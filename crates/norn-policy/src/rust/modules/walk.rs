//! Recursive module/include traversal over immutable source bytes.

use std::collections::BTreeMap;

use tree_sitter::Node;

use super::super::{RustSource, RustSourceError};
use super::analyze::{Analyzer, ReachModes, VisitKey};
use super::model::{ModuleDiagnosticCode, ModuleTargetIdentity, SourceSpan};
use super::path::{
    Directory, package_authority, resolve_directory_from, resolve_from, resolve_literal,
};
use super::plans::ModePlans;
use super::scan::{include_literal, is_include, module_body, module_name, scan_container, span};
use crate::{EntryKind, RepositoryPath};

struct WalkContext<'a> {
    package_root: Option<&'a RepositoryPath>,
    target: &'a ModuleTargetIdentity,
    source: &'a RepositoryPath,
    module_directory: &'a Directory,
    // Outside inline modules rustc resolves `#[path]` from the physical source parent.
    path_attribute_base: &'a Directory,
    bytes: &'a [u8],
}

impl Analyzer<'_, '_> {
    pub(super) fn visit_file(
        &mut self,
        package_root: Option<&RepositoryPath>,
        target: &ModuleTargetIdentity,
        path: &RepositoryPath,
        module_directory: &Directory,
        modes: ReachModes,
    ) {
        if !modes.any() {
            return;
        }
        if self.stack.contains(path) {
            self.problem(
                ModuleDiagnosticCode::ResolutionCycle,
                path,
                None,
                None,
                Some(target.clone()),
                None,
            );
            return;
        }
        let key = VisitKey {
            path: path.clone(),
            module_directory: module_directory.clone(),
            target: target.clone(),
            modes,
        };
        if !self.visited.insert(key) {
            return;
        }
        let Some(entry) = self.snapshot.get(path) else {
            self.problem(
                ModuleDiagnosticCode::SourceMissing,
                path,
                None,
                None,
                Some(target.clone()),
                None,
            );
            return;
        };
        if entry.kind() != EntryKind::Regular {
            self.problem(
                ModuleDiagnosticCode::EntryNotRegular,
                path,
                None,
                None,
                Some(target.clone()),
                None,
            );
            return;
        }
        let owned_bytes = entry.bytes().to_vec();
        self.record(package_root, target, path, modes);
        let source = match RustSource::parse(owned_bytes) {
            Ok(source) => source,
            Err(error) => {
                self.source_error(path, target, &error);
                return;
            }
        };
        let stack_depth = self.stack.len();
        self.stack.push(path.clone());
        let path_attribute_base = Directory::parent_of(path);
        let context = WalkContext {
            package_root,
            target,
            source: path,
            module_directory,
            path_attribute_base: &path_attribute_base,
            bytes: source.bytes(),
        };
        self.process_container(&context, modes, source.root_node(), None);
        self.stack.truncate(stack_depth);
    }

    fn process_container(
        &mut self,
        context: &WalkContext<'_>,
        parent_modes: ReachModes,
        container: Node<'_>,
        enclosing_item: Option<SourceSpan>,
    ) {
        let scanned = match scan_container(container) {
            Ok(scanned) => scanned,
            Err(error_span) => {
                self.problem(
                    ModuleDiagnosticCode::AttributeUnsupported,
                    context.source,
                    Some(error_span),
                    None,
                    Some(context.target.clone()),
                    None,
                );
                return;
            }
        };
        let modes = self.inner_modes(
            context.source,
            context.target,
            parent_modes,
            &scanned.inner_attributes,
            context.bytes,
        );
        if !modes.any() {
            return;
        }
        for item in scanned.items {
            self.process_item(context, modes, item.node, &item.attributes, enclosing_item);
        }
    }

    fn process_item(
        &mut self,
        context: &WalkContext<'_>,
        parent_modes: ReachModes,
        node: Node<'_>,
        attributes: &[Node<'_>],
        enclosing_item: Option<SourceSpan>,
    ) {
        let plans = self.attribute_plans(
            context.source,
            context.target,
            parent_modes,
            attributes,
            context.bytes,
        );
        if node.kind() == "mod_item" {
            self.process_module(context, node, &plans);
        } else if is_include(node, context.bytes) {
            self.process_include(
                context,
                node,
                &plans,
                enclosing_item.unwrap_or_else(|| span(node)),
            );
        } else {
            let modes = self.default_modes(context.source, context.target, node, &plans);
            if !modes.any() || node.kind() == "macro_invocation" {
                return;
            }
            let next_enclosing = if node.kind().ends_with("_item") {
                Some(span(node))
            } else {
                enclosing_item
            };
            self.process_container(context, modes, node, next_enclosing);
        }
    }

    fn process_module(&mut self, context: &WalkContext<'_>, node: Node<'_>, plans: &ModePlans) {
        let Some(name) = module_name(node, context.bytes) else {
            self.problem(
                ModuleDiagnosticCode::ModuleNameMissing,
                context.source,
                Some(span(node)),
                None,
                Some(context.target.clone()),
                None,
            );
            return;
        };
        let Some(default_child_directory) = context.module_directory.child(&name) else {
            self.problem(
                ModuleDiagnosticCode::AuthorityEscape,
                context.source,
                Some(span(node)),
                None,
                Some(context.target.clone()),
                None,
            );
            return;
        };
        let alternatives = alternative_modes(plans);
        let authority = package_authority(context.package_root);
        if let Some(body) = module_body(node) {
            let mut directories = BTreeMap::new();
            for (ordinal, (raw, modes)) in alternatives.into_iter().enumerate() {
                let directory = raw.map_or_else(
                    || Some(default_child_directory.clone()),
                    |raw| resolve_directory_from(context.path_attribute_base, &raw, &authority),
                );
                let Some(directory) = directory else {
                    self.problem(
                        ModuleDiagnosticCode::AuthorityEscape,
                        context.source,
                        Some(span(node)),
                        None,
                        Some(context.target.clone()),
                        Some(ordinal),
                    );
                    continue;
                };
                merge_directory(&mut directories, directory, modes);
            }
            for (directory, modes) in directories {
                let child_context = WalkContext {
                    package_root: context.package_root,
                    target: context.target,
                    source: context.source,
                    module_directory: &directory,
                    path_attribute_base: &directory,
                    bytes: context.bytes,
                };
                self.process_container(&child_context, modes, body, Some(span(node)));
            }
            return;
        }
        let mut resolved = BTreeMap::new();
        for (ordinal, (raw, modes)) in alternatives.into_iter().enumerate() {
            if let Some(raw) = raw {
                match resolve_from(context.path_attribute_base, &raw, &authority) {
                    Some(path) if self.snapshot.contains_path(&path) => {
                        merge_path(&mut resolved, path, modes);
                    }
                    Some(path) => self.problem(
                        ModuleDiagnosticCode::ModuleMissing,
                        context.source,
                        Some(span(node)),
                        Some(path),
                        Some(context.target.clone()),
                        Some(ordinal),
                    ),
                    None => self.problem(
                        ModuleDiagnosticCode::AuthorityEscape,
                        context.source,
                        Some(span(node)),
                        None,
                        Some(context.target.clone()),
                        Some(ordinal),
                    ),
                }
            } else {
                self.resolve_standard(context, node, &name, modes, &mut resolved);
            }
        }
        for (path, modes) in resolved {
            let module_directory = if path.file_name() == "mod.rs" {
                Directory::parent_of(&path)
            } else {
                default_child_directory.clone()
            };
            self.visit_file(
                context.package_root,
                context.target,
                &path,
                &module_directory,
                modes,
            );
        }
    }

    fn process_include(
        &mut self,
        context: &WalkContext<'_>,
        node: Node<'_>,
        plans: &ModePlans,
        enclosing_item: SourceSpan,
    ) {
        let modes = self.default_modes(context.source, context.target, node, plans);
        if !modes.any() {
            return;
        }
        let Some(raw) = include_literal(node, context.bytes) else {
            self.generated.encounter(
                context.source,
                context.target,
                node,
                enclosing_item,
                context.bytes,
                &mut self.diagnostics,
            );
            return;
        };
        let authority = package_authority(context.package_root);
        let Some(path) = resolve_literal(context.source, &raw, &authority) else {
            self.problem(
                ModuleDiagnosticCode::AuthorityEscape,
                context.source,
                Some(span(node)),
                None,
                Some(context.target.clone()),
                None,
            );
            return;
        };
        if !self.snapshot.contains_path(&path) {
            self.problem(
                ModuleDiagnosticCode::IncludeMissing,
                context.source,
                Some(span(node)),
                Some(path),
                Some(context.target.clone()),
                None,
            );
            return;
        }
        self.visit_file(
            context.package_root,
            context.target,
            &path,
            context.module_directory,
            modes,
        );
    }

    fn resolve_standard(
        &mut self,
        context: &WalkContext<'_>,
        node: Node<'_>,
        name: &str,
        modes: ReachModes,
        resolved: &mut BTreeMap<RepositoryPath, ReachModes>,
    ) {
        let direct = context.module_directory.file(&format!("{name}.rs"));
        let nested = context
            .module_directory
            .child(name)
            .and_then(|directory| directory.file("mod.rs"));
        let candidates: Vec<_> = [direct, nested]
            .into_iter()
            .flatten()
            .filter(|path| self.snapshot.contains_path(path))
            .collect();
        match candidates.as_slice() {
            [path] => merge_path(resolved, path.clone(), modes),
            [] => self.problem(
                ModuleDiagnosticCode::ModuleMissing,
                context.source,
                Some(span(node)),
                None,
                Some(context.target.clone()),
                None,
            ),
            _ => {
                for (ordinal, path) in candidates.into_iter().enumerate() {
                    self.problem(
                        ModuleDiagnosticCode::ModuleAmbiguous,
                        context.source,
                        Some(span(node)),
                        Some(path),
                        Some(context.target.clone()),
                        Some(ordinal),
                    );
                }
            }
        }
    }

    fn source_error(
        &mut self,
        path: &RepositoryPath,
        target: &ModuleTargetIdentity,
        error: &RustSourceError,
    ) {
        let code = match error {
            RustSourceError::Utf8(_) => ModuleDiagnosticCode::SourceNotUtf8,
            RustSourceError::Language(_)
            | RustSourceError::Parse
            | RustSourceError::Syntax
            | RustSourceError::Cfg(_)
            | RustSourceError::Attribute { .. } => ModuleDiagnosticCode::SourceParse,
        };
        self.problem(code, path, None, None, Some(target.clone()), None);
    }
}

fn alternative_modes(plans: &ModePlans) -> BTreeMap<Option<String>, ReachModes> {
    let mut alternatives = BTreeMap::new();
    if let Some(plan) = &plans.production {
        for path in &plan.paths {
            merge_alternative(
                &mut alternatives,
                path.clone(),
                ReachModes {
                    production: true,
                    test: false,
                    fixture: false,
                },
            );
        }
    }
    if let Some(plan) = &plans.test {
        for path in &plan.paths {
            merge_alternative(
                &mut alternatives,
                path.clone(),
                ReachModes {
                    production: false,
                    test: true,
                    fixture: false,
                },
            );
        }
    }
    if let Some(plan) = &plans.fixture {
        for path in &plan.paths {
            merge_alternative(
                &mut alternatives,
                path.clone(),
                ReachModes {
                    production: false,
                    test: false,
                    fixture: true,
                },
            );
        }
    }
    alternatives
}

fn merge_alternative(
    alternatives: &mut BTreeMap<Option<String>, ReachModes>,
    path: Option<String>,
    modes: ReachModes,
) {
    alternatives
        .entry(path)
        .and_modify(|existing| existing.merge(modes))
        .or_insert(modes);
}

fn merge_path(
    paths: &mut BTreeMap<RepositoryPath, ReachModes>,
    path: RepositoryPath,
    modes: ReachModes,
) {
    paths
        .entry(path)
        .and_modify(|existing| existing.merge(modes))
        .or_insert(modes);
}

fn merge_directory(
    directories: &mut BTreeMap<Directory, ReachModes>,
    directory: Directory,
    modes: ReachModes,
) {
    directories
        .entry(directory)
        .and_modify(|existing| existing.merge(modes))
        .or_insert(modes);
}
