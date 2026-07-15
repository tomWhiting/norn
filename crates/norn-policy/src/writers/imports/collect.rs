//! Heap-backed import-scope and use-tree collection.

use std::collections::BTreeMap;

use tree_sitter::Node;

use super::{Imports, ModuleArena, Reexports, source_crate, source_module};
use crate::path::RepositoryPath;
use crate::rust::SourceRange;
use crate::writers::syntax::{canonical_identifier, normalized_path};

#[derive(Default)]
struct PrefixArena {
    segments: Vec<PrefixSegment>,
}

struct PrefixSegment {
    parent: Option<usize>,
    value: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum UseBinding {
    Alias { alias: String, target: String },
    Wildcard { target: String },
    Anonymous,
}

impl PrefixArena {
    fn extend(&mut self, parent: Option<usize>, value: String) -> usize {
        let index = self.segments.len();
        self.segments.push(PrefixSegment { parent, value });
        index
    }

    fn render(&self, prefix: Option<usize>, leaf: Option<&str>) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(leaf) = leaf {
            parts.push(leaf);
        }
        let mut current = prefix;
        while let Some(index) = current {
            let segment = self.segments.get(index)?;
            parts.push(segment.value.as_str());
            current = segment.parent;
        }
        if parts.is_empty() {
            return None;
        }
        parts.reverse();
        Some(parts.join("::"))
    }
}

pub(super) fn collect(
    root: Node<'_>,
    bytes: &[u8],
    excluded: &[SourceRange],
    source: &RepositoryPath,
    reexports: &Reexports,
) -> Imports {
    let mut imports = Imports {
        scopes: Vec::new(),
        by_range: BTreeMap::default(),
        modules: ModuleArena::default(),
        reexports: reexports.clone(),
        source_crate: source_crate(source),
    };
    let mut base_module = None;
    for component in source_module(source) {
        base_module = Some(imports.modules.extend(base_module, component));
    }
    let mut pending = vec![(root, None, base_module)];
    while let Some((node, inherited_scope, inherited_module)) = pending.pop() {
        if is_excluded(node, excluded) {
            continue;
        }
        let module = if node.kind() == "mod_item" {
            node.child_by_field_name("name")
                .and_then(|name| canonical_identifier(name, bytes))
                .map_or(inherited_module, |name| {
                    Some(imports.modules.extend(inherited_module, name))
                })
        } else {
            inherited_module
        };
        let scope = if is_scope(node) {
            imports.push_scope(node, inherited_scope, module)
        } else if let Some(scope) = inherited_scope {
            scope
        } else {
            imports.push_scope(node, None, module)
        };
        if node.kind() == "use_declaration" {
            if let Some(argument) = node.child_by_field_name("argument") {
                collect_argument(argument, bytes, scope, &mut imports);
            }
            continue;
        }
        for index in (0..node.named_child_count()).rev() {
            if let Some(child) = node.named_child(index) {
                pending.push((child, Some(scope), module));
            }
        }
    }
    imports
}

fn collect_argument(node: Node<'_>, bytes: &[u8], scope: usize, imports: &mut Imports) {
    let Some(bindings) = use_bindings(node, bytes) else {
        return;
    };
    for binding in bindings {
        match binding {
            UseBinding::Alias { alias, target } => add_alias(scope, alias, &target, imports),
            UseBinding::Wildcard { target } => {
                let target = imports.absolute_in_scope(scope, &target);
                imports.add_wildcard(scope, target);
            }
            UseBinding::Anonymous => {}
        }
    }
}

pub(super) fn use_bindings(node: Node<'_>, bytes: &[u8]) -> Option<Vec<UseBinding>> {
    let mut prefixes = PrefixArena::default();
    let mut pending = vec![(node, None)];
    let mut bindings = Vec::new();
    while let Some((current, prefix)) = pending.pop() {
        match current.kind() {
            "scoped_use_list" => {
                let path = current.child_by_field_name("path")?;
                let path = normalized_path(path, bytes)?;
                let combined = prefixes.extend(prefix, path);
                let list = current.child_by_field_name("list")?;
                pending.push((list, Some(combined)));
            }
            "use_list" => {
                for index in (0..current.named_child_count()).rev() {
                    if let Some(child) = current.named_child(index) {
                        pending.push((child, prefix));
                    }
                }
            }
            "use_as_clause" => {
                bindings.push(alias_binding(current, bytes, prefix, &prefixes)?);
            }
            "use_wildcard" => {
                bindings.push(wildcard_binding(current, bytes, prefix, &prefixes)?);
            }
            "self" => bindings.push(self_binding(prefix, &prefixes)?),
            _ => bindings.push(leaf_binding(current, bytes, prefix, &prefixes)?),
        }
    }
    Some(bindings)
}

fn alias_binding(
    node: Node<'_>,
    bytes: &[u8],
    prefix: Option<usize>,
    prefixes: &PrefixArena,
) -> Option<UseBinding> {
    let (path, alias) = (
        node.child_by_field_name("path"),
        node.child_by_field_name("alias"),
    );
    let (path, alias) = (
        path.and_then(|path| normalized_path(path, bytes)),
        alias.and_then(|alias| canonical_identifier(alias, bytes)),
    );
    let (Some(path), Some(alias)) = (path, alias) else {
        return None;
    };
    if alias == "_" {
        return Some(UseBinding::Anonymous);
    }
    let target = if path == "self" && prefix.is_some() {
        prefixes.render(prefix, None)?
    } else {
        prefixes.render(prefix, Some(&path))?
    };
    Some(UseBinding::Alias { alias, target })
}

fn wildcard_binding(
    node: Node<'_>,
    bytes: &[u8],
    prefix: Option<usize>,
    prefixes: &PrefixArena,
) -> Option<UseBinding> {
    let mut cursor = node.walk();
    let path = node
        .named_children(&mut cursor)
        .next()
        .and_then(|child| normalized_path(child, bytes));
    let target = prefixes.render(prefix, path.as_deref())?;
    Some(UseBinding::Wildcard { target })
}

fn self_binding(prefix: Option<usize>, prefixes: &PrefixArena) -> Option<UseBinding> {
    let target = prefixes.render(prefix, None)?;
    let alias = target.rsplit("::").next()?.to_owned();
    Some(UseBinding::Alias { alias, target })
}

fn leaf_binding(
    node: Node<'_>,
    bytes: &[u8],
    prefix: Option<usize>,
    prefixes: &PrefixArena,
) -> Option<UseBinding> {
    let path = normalized_path(node, bytes)?;
    let target = prefixes.render(prefix, Some(&path))?;
    let alias = path.rsplit("::").next()?.to_owned();
    Some(UseBinding::Alias { alias, target })
}

fn add_alias(scope: usize, alias: String, target: &str, imports: &mut Imports) {
    let target = imports.absolute_in_scope(scope, target);
    imports.add_alias(scope, alias, target);
}

fn is_scope(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "source_file" | "declaration_list" | "block" | "match_arm" | "closure_expression"
    )
}

pub(super) fn is_excluded(node: Node<'_>, excluded: &[SourceRange]) -> bool {
    excluded
        .iter()
        .any(|range| range.contains(node.start_byte(), node.end_byte()))
}
