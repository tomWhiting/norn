//! Cross-source, crate-partitioned public re-export resolution.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tree_sitter::Node;

use super::collect::{UseBinding, is_excluded, use_bindings};
use super::{ImportResolution, Reexports, source_crate, source_module};
use crate::rust::{RustSource, SourceRange};
use crate::writers::input::{WriterScanError, WriterSource};
use crate::writers::registry::SinkRegistry;
use crate::writers::syntax::canonical_identifier;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ReexportGraph {
    crates: BTreeMap<String, CrateExports>,
    crate_names: BTreeMap<String, BTreeSet<String>>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct CrateExports {
    aliases: BTreeMap<String, BTreeSet<String>>,
    wildcards: BTreeMap<String, BTreeSet<String>>,
    known_modules: BTreeSet<String>,
    known_items: BTreeSet<String>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum PathState {
    External(String),
    Local {
        crate_key: String,
        path: String,
        public_prefix: Option<String>,
    },
}

enum ExactResolution {
    Exact(PathState),
    Ambiguous,
}

impl ReexportGraph {
    pub(crate) fn build(sources: &[&WriterSource]) -> Result<Reexports, WriterScanError> {
        let mut graph = Self::default();
        for source in sources {
            let crate_key = source_crate(source.path());
            graph
                .crate_names
                .entry(crate_name(&crate_key))
                .or_default()
                .insert(crate_key.clone());
            let parsed = RustSource::parse(source.bytes())?;
            let excluded = parsed.test_only_ranges()?;
            let exports = graph.crates.entry(crate_key).or_default();
            collect_known_paths(
                parsed.root_node(),
                parsed.bytes(),
                &excluded,
                source_module(source.path()),
                &mut exports.known_modules,
                &mut exports.known_items,
            );
        }
        for source in sources {
            graph.collect_source(source)?;
        }
        Ok(Arc::new(graph))
    }

    pub(crate) fn resolve(
        &self,
        source_crate: &str,
        path: String,
        registry: &SinkRegistry,
    ) -> ImportResolution {
        let state = match self.initial_state(source_crate, path) {
            ExactResolution::Exact(state) => state,
            ExactResolution::Ambiguous => return ImportResolution::AmbiguousReexport,
        };
        let state = match self.resolve_exact(state) {
            ExactResolution::Exact(state) => state,
            ExactResolution::Ambiguous => return ImportResolution::AmbiguousReexport,
        };
        if self.has_relevant_wildcard(&state, registry) {
            return ImportResolution::WildcardReexport;
        }
        ImportResolution::Exact(render_state(state))
    }

    fn collect_source(&mut self, source: &WriterSource) -> Result<(), WriterScanError> {
        let parsed = RustSource::parse(source.bytes())?;
        let excluded = parsed.test_only_ranges()?;
        let crate_key = source_crate(source.path());
        let base = source_module(source.path());
        let mut pending = vec![(parsed.root_node(), base)];
        while let Some((container, module)) = pending.pop() {
            let mut cursor = container.walk();
            for child in container.named_children(&mut cursor) {
                if is_excluded(child, &excluded) {
                    continue;
                }
                if child.kind() == "mod_item" {
                    if let Some((body, nested)) = nested_module(child, parsed.bytes(), &module) {
                        pending.push((body, nested));
                    }
                    continue;
                }
                if child.kind() != "use_declaration" || !has_visibility(child) {
                    continue;
                }
                let Some(argument) = child.child_by_field_name("argument") else {
                    return Err(WriterScanError::ReexportSyntax);
                };
                let Some(bindings) = use_bindings(argument, parsed.bytes()) else {
                    return Err(WriterScanError::ReexportSyntax);
                };
                self.insert_bindings(&crate_key, &module, bindings);
            }
        }
        Ok(())
    }

    fn insert_bindings(&mut self, crate_key: &str, module: &[String], bindings: Vec<UseBinding>) {
        for binding in bindings {
            match binding {
                UseBinding::Alias { alias, target } => {
                    let target = self.canonical_target(crate_key, module, &target);
                    let exported = join_path(module, &alias);
                    self.crates
                        .entry(crate_key.to_owned())
                        .or_default()
                        .aliases
                        .entry(exported)
                        .or_default()
                        .insert(target);
                }
                UseBinding::Wildcard { target } => {
                    let target = self.canonical_target(crate_key, module, &target);
                    self.crates
                        .entry(crate_key.to_owned())
                        .or_default()
                        .wildcards
                        .entry(module.join("::"))
                        .or_default()
                        .insert(target);
                }
                UseBinding::Anonymous => {}
            }
        }
    }

    fn canonical_target(&self, crate_key: &str, module: &[String], target: &str) -> String {
        if starts_locally(target) {
            return absolute_path(module, target);
        }
        let Some(exports) = self.crates.get(crate_key) else {
            return target.to_owned();
        };
        let relative = join_path(module, target);
        if is_known_local(exports, &relative, &module.join("::")) {
            return relative;
        }
        let rooted = format!("crate::{target}");
        if is_known_local(exports, &rooted, "crate") {
            return rooted;
        }
        target.to_owned()
    }

    fn initial_state(&self, source_crate: &str, path: String) -> ExactResolution {
        if path == "crate" || path.starts_with("crate::") {
            return ExactResolution::Exact(PathState::Local {
                crate_key: source_crate.to_owned(),
                path,
                public_prefix: None,
            });
        }
        let first = path.split("::").next().unwrap_or(path.as_str());
        let Some(crates) = self.crate_names.get(first) else {
            return ExactResolution::Exact(PathState::External(path));
        };
        let mut matches = crates.iter();
        let Some(crate_key) = matches.next() else {
            return ExactResolution::Ambiguous;
        };
        if matches.next().is_some() {
            return ExactResolution::Ambiguous;
        }
        let suffix = path.strip_prefix(first).unwrap_or_default();
        ExactResolution::Exact(PathState::Local {
            crate_key: crate_key.clone(),
            path: format!("crate{suffix}"),
            public_prefix: Some(first.to_owned()),
        })
    }

    fn resolve_exact(&self, mut state: PathState) -> ExactResolution {
        let mut visited = BTreeSet::new();
        loop {
            let PathState::Local {
                crate_key,
                path,
                public_prefix,
            } = &state
            else {
                return ExactResolution::Exact(state);
            };
            let Some(exports) = self.crates.get(crate_key) else {
                return ExactResolution::Exact(state);
            };
            let Some((alias, targets)) = longest_alias(&exports.aliases, path) else {
                return ExactResolution::Exact(state);
            };
            if !visited.insert((crate_key.clone(), alias.to_owned())) || targets.len() != 1 {
                return ExactResolution::Ambiguous;
            }
            let Some(target) = targets.first() else {
                return ExactResolution::Ambiguous;
            };
            let tail = path.strip_prefix(alias).unwrap_or_default();
            let replacement = format!("{target}{tail}");
            let inherited_prefix = public_prefix.clone();
            state = match self.initial_state(crate_key, replacement) {
                ExactResolution::Exact(PathState::Local {
                    crate_key: next_crate,
                    path: next_path,
                    public_prefix,
                }) => PathState::Local {
                    public_prefix: public_prefix.or(inherited_prefix),
                    crate_key: next_crate,
                    path: next_path,
                },
                ExactResolution::Exact(state) => state,
                ExactResolution::Ambiguous => return ExactResolution::Ambiguous,
            };
        }
    }

    fn has_relevant_wildcard(&self, state: &PathState, registry: &SinkRegistry) -> bool {
        let PathState::Local {
            crate_key, path, ..
        } = state
        else {
            return false;
        };
        let Some(exports) = self.crates.get(crate_key) else {
            return false;
        };
        exports.wildcards.iter().any(|(module, targets)| {
            let Some(tail) = path
                .strip_prefix(module)
                .and_then(|tail| tail.strip_prefix("::"))
            else {
                return false;
            };
            targets.iter().any(|target| {
                let candidate = format!("{target}::{tail}");
                let state = self.initial_state(crate_key, candidate);
                match state {
                    ExactResolution::Exact(state) => match self.resolve_exact(state) {
                        ExactResolution::Exact(resolved) => is_writer_path(&resolved, registry),
                        ExactResolution::Ambiguous => true,
                    },
                    ExactResolution::Ambiguous => true,
                }
            })
        })
    }
}

fn collect_known_paths(
    root: Node<'_>,
    bytes: &[u8],
    excluded: &[SourceRange],
    base: Vec<String>,
    modules: &mut BTreeSet<String>,
    items: &mut BTreeSet<String>,
) {
    modules.insert(base.join("::"));
    let mut pending = vec![(root, base)];
    while let Some((container, module)) = pending.pop() {
        let mut cursor = container.walk();
        for child in container.named_children(&mut cursor) {
            if is_excluded(child, excluded) {
                continue;
            }
            if child.kind() == "mod_item" {
                if let Some(name) = child
                    .child_by_field_name("name")
                    .and_then(|name| canonical_identifier(name, bytes))
                {
                    let nested = appended(&module, &name);
                    modules.insert(nested.join("::"));
                    if let Some(body) = child.child_by_field_name("body") {
                        pending.push((body, nested));
                    }
                }
                continue;
            }
            if is_named_module_item(child.kind())
                && let Some(name) = child
                    .child_by_field_name("name")
                    .and_then(|name| canonical_identifier(name, bytes))
            {
                items.insert(join_path(&module, &name));
            }
        }
    }
}

fn nested_module<'tree>(
    node: Node<'tree>,
    bytes: &[u8],
    module: &[String],
) -> Option<(Node<'tree>, Vec<String>)> {
    let name = node
        .child_by_field_name("name")
        .and_then(|name| canonical_identifier(name, bytes))?;
    Some((node.child_by_field_name("body")?, appended(module, &name)))
}

fn longest_alias<'a>(
    aliases: &'a BTreeMap<String, BTreeSet<String>>,
    path: &str,
) -> Option<(&'a str, &'a BTreeSet<String>)> {
    aliases
        .iter()
        .filter(|(alias, _)| path == alias.as_str() || path.starts_with(&format!("{alias}::")))
        .max_by_key(|(alias, _)| alias.len())
        .map(|(alias, targets)| (alias.as_str(), targets))
}

fn is_writer_path(state: &PathState, registry: &SinkRegistry) -> bool {
    let path = match state {
        PathState::External(path) => path.as_str(),
        PathState::Local { path, .. } => path.as_str(),
    };
    registry.is_function_candidate(path) || registry.macro_sink(path).is_some()
}

fn render_state(state: PathState) -> String {
    match state {
        PathState::External(path) => path,
        PathState::Local {
            path,
            public_prefix: Some(prefix),
            ..
        } => format!(
            "{prefix}{}",
            path.strip_prefix("crate").unwrap_or(path.as_str())
        ),
        PathState::Local { path, .. } => path,
    }
}

fn has_visibility(node: Node<'_>) -> bool {
    let mut cursor = node.walk();
    let visible = node
        .named_children(&mut cursor)
        .any(|child| child.kind() == "visibility_modifier");
    visible
}

fn starts_locally(path: &str) -> bool {
    matches!(path, "crate" | "self" | "super")
        || path.starts_with("crate::")
        || path.starts_with("self::")
        || path.starts_with("super::")
}

fn absolute_path(module: &[String], path: &str) -> String {
    let mut parts = path.split("::");
    let Some(first) = parts.next() else {
        return path.to_owned();
    };
    let mut output = match first {
        "crate" => vec!["crate".to_owned()],
        "self" => module.to_vec(),
        "super" => module[..module.len().saturating_sub(1)].to_vec(),
        _ => return path.to_owned(),
    };
    for part in parts {
        if part == "super" {
            output.pop();
        } else if part != "self" {
            output.push(part.to_owned());
        }
    }
    output.join("::")
}

fn is_known_local(exports: &CrateExports, candidate: &str, base: &str) -> bool {
    if exports.known_items.contains(candidate) {
        return true;
    }
    let descendant = format!("{base}::");
    exports.known_modules.iter().any(|module| {
        module.starts_with(&descendant)
            && (candidate == module || candidate.starts_with(&format!("{module}::")))
    })
}

fn appended(module: &[String], value: &str) -> Vec<String> {
    let mut output = module.to_vec();
    output.push(value.to_owned());
    output
}

fn join_path(module: &[String], tail: &str) -> String {
    format!("{}::{tail}", module.join("::"))
}

fn crate_name(crate_key: &str) -> String {
    crate_key
        .rsplit('/')
        .next()
        .unwrap_or(crate_key)
        .replace('-', "_")
}

fn is_named_module_item(kind: &str) -> bool {
    matches!(
        kind,
        "const_item"
            | "enum_item"
            | "function_item"
            | "macro_definition"
            | "static_item"
            | "struct_item"
            | "trait_item"
            | "type_item"
            | "union_item"
    )
}
