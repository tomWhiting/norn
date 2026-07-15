//! Deterministic, lexical Rust import and alias resolution for writer sinks.

mod collect;
mod reexports;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tree_sitter::Node;

use super::registry::SinkRegistry;
use crate::path::RepositoryPath;
use crate::rust::SourceRange;

pub(crate) use reexports::ReexportGraph;
pub(crate) type Reexports = Arc<ReexportGraph>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Imports {
    scopes: Vec<ImportScope>,
    by_range: BTreeMap<(usize, usize), usize>,
    modules: ModuleArena,
    reexports: Reexports,
    source_crate: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ImportScope {
    parent: Option<usize>,
    module: Option<usize>,
    aliases: BTreeMap<String, BTreeSet<String>>,
    wildcards: BTreeSet<String>,
}

impl Imports {
    pub(crate) fn collect(
        root: Node<'_>,
        bytes: &[u8],
        excluded: &[SourceRange],
        source: &RepositoryPath,
        reexports: &Reexports,
    ) -> Self {
        collect::collect(root, bytes, excluded, source, reexports)
    }

    pub(crate) fn resolve(
        &self,
        node: Node<'_>,
        path: &str,
        registry: &SinkRegistry,
    ) -> ImportResolution {
        let path = path.strip_prefix("::").unwrap_or(path);
        let (first, suffix) = path
            .split_once("::")
            .map_or((path, None), |(head, tail)| (head, Some(tail)));
        let Some(mut scope) = self.scope_index(node) else {
            return ImportResolution::Ambiguous;
        };
        let lexical_module = self.scopes[scope].module;
        loop {
            let current = &self.scopes[scope];
            if let Some(targets) = current.aliases.get(first) {
                if targets.len() != 1 {
                    return ImportResolution::Ambiguous;
                }
                let Some(target) = targets.first() else {
                    return ImportResolution::Ambiguous;
                };
                let resolved =
                    suffix.map_or_else(|| target.clone(), |tail| format!("{target}::{tail}"));
                return self
                    .reexports
                    .resolve(&self.source_crate, resolved, registry);
            }
            if suffix.is_none()
                && current
                    .wildcards
                    .iter()
                    .any(|prefix| registry.has_path_prefix(prefix))
                && registry.has_terminal(first)
            {
                return ImportResolution::Wildcard;
            }
            let Some(parent) = current.parent else {
                let resolved = self.modules.absolute(lexical_module, path);
                return self
                    .reexports
                    .resolve(&self.source_crate, resolved, registry);
            };
            scope = parent;
        }
    }

    pub(crate) fn local_path(&self, node: Node<'_>, path: &str) -> Option<String> {
        let scope = self.scope_index(node)?;
        let module = self.scopes.get(scope)?.module;
        if matches!(path, "crate" | "self" | "super")
            || path.starts_with("crate::")
            || path.starts_with("self::")
            || path.starts_with("super::")
        {
            Some(self.modules.absolute(module, path))
        } else {
            Some(self.modules.join(module, path))
        }
    }

    pub(crate) fn canonical_item(&self, node: Node<'_>, name: &str) -> Option<String> {
        self.local_path(node, name)
    }

    fn scope_index(&self, node: Node<'_>) -> Option<usize> {
        let mut current = Some(node);
        while let Some(candidate) = current {
            if let Some(index) = self
                .by_range
                .get(&(candidate.start_byte(), candidate.end_byte()))
            {
                return Some(*index);
            }
            current = candidate.parent();
        }
        None
    }

    fn push_scope(
        &mut self,
        node: Node<'_>,
        parent: Option<usize>,
        module: Option<usize>,
    ) -> usize {
        let index = self.scopes.len();
        self.scopes.push(ImportScope {
            parent,
            module,
            aliases: BTreeMap::new(),
            wildcards: BTreeSet::new(),
        });
        self.by_range
            .insert((node.start_byte(), node.end_byte()), index);
        index
    }

    fn add_alias(&mut self, scope: usize, alias: String, target: String) {
        self.scopes[scope]
            .aliases
            .entry(alias)
            .or_default()
            .insert(target);
    }

    fn add_wildcard(&mut self, scope: usize, target: String) {
        self.scopes[scope].wildcards.insert(target);
    }

    fn absolute_in_scope(&self, scope: usize, path: &str) -> String {
        self.modules.absolute(self.scopes[scope].module, path)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ImportResolution {
    Exact(String),
    Ambiguous,
    Wildcard,
    AmbiguousReexport,
    WildcardReexport,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ModuleArena {
    segments: Vec<ModuleSegment>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ModuleSegment {
    parent: Option<usize>,
    value: String,
}

impl ModuleArena {
    fn extend(&mut self, parent: Option<usize>, value: String) -> usize {
        let index = self.segments.len();
        self.segments.push(ModuleSegment { parent, value });
        index
    }

    fn join(&self, module: Option<usize>, tail: &str) -> String {
        let mut parts = self.render(module);
        parts.extend(tail.split("::").map(str::to_owned));
        parts.join("::")
    }

    fn absolute(&self, module: Option<usize>, path: &str) -> String {
        let mut parts = path.split("::");
        let Some(first) = parts.next() else {
            return path.to_owned();
        };
        let mut output = match first {
            "crate" => vec!["crate".to_owned()],
            "self" => self.render(module),
            "super" => {
                let mut parent = self.render(module);
                parent.pop();
                parent
            }
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

    fn render(&self, module: Option<usize>) -> Vec<String> {
        let mut output = Vec::new();
        let mut current = module;
        while let Some(index) = current {
            let Some(segment) = self.segments.get(index) else {
                return Vec::new();
            };
            output.push(segment.value.clone());
            current = segment.parent;
        }
        output.reverse();
        output
    }
}

fn source_module(source: &RepositoryPath) -> Vec<String> {
    let components: Vec<&str> = source.as_str().split('/').collect();
    let Some(src) = components.iter().rposition(|component| *component == "src") else {
        return vec!["crate".to_owned()];
    };
    let mut module = vec!["crate".to_owned()];
    let tail = &components[src + 1..];
    for (index, component) in tail.iter().enumerate() {
        let is_last = index + 1 == tail.len();
        let stem = component.strip_suffix(".rs").unwrap_or(component);
        if is_last && matches!(stem, "lib" | "main" | "mod") {
            continue;
        }
        module.push(stem.to_owned());
    }
    module
}

pub(crate) fn source_crate(source: &RepositoryPath) -> String {
    let path = source.as_str();
    if path.starts_with("src/") {
        return ".".to_owned();
    }
    path.split_once("/src/")
        .map_or_else(|| path.to_owned(), |(root, _)| root.to_owned())
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use tree_sitter::Node;

    use super::{ImportResolution, Imports, ReexportGraph};
    use crate::path::RepositoryPath;
    use crate::rust::RustSource;
    use crate::writers::registry::SinkRegistry;
    use crate::writers::syntax::canonical_identifier;

    #[test]
    fn sibling_module_aliases_do_not_merge() -> Result<(), Box<dyn Error>> {
        let source = r"
mod first {
    use crate::alpha::write as persist;
    fn run() { persist(); }
}
mod second {
    use crate::beta::write as persist;
    fn run() { persist(); }
}
";
        let (parsed, imports) = collect(source)?;
        let calls = call_identifiers(parsed.root_node(), parsed.bytes(), "persist");
        assert_eq!(calls.len(), 2);
        assert_eq!(
            resolve(&imports, calls[0], "persist")?,
            "crate::alpha::write"
        );
        assert_eq!(
            resolve(&imports, calls[1], "persist")?,
            "crate::beta::write"
        );
        Ok(())
    }

    #[test]
    fn block_import_does_not_escape_to_its_parent() -> Result<(), Box<dyn Error>> {
        let source = r"
fn run() {
    { use crate::alpha::write as persist; persist(); }
    persist();
}
";
        let (parsed, imports) = collect(source)?;
        let calls = call_identifiers(parsed.root_node(), parsed.bytes(), "persist");
        assert_eq!(calls.len(), 2);
        assert_eq!(
            resolve(&imports, calls[0], "persist")?,
            "crate::alpha::write"
        );
        assert_eq!(resolve(&imports, calls[1], "persist")?, "persist");
        Ok(())
    }

    #[test]
    fn import_scopes_use_heap_at_twenty_thousand_modules() -> Result<(), Box<dyn Error>> {
        const DEPTH: usize = 20_000;

        let mut source = String::with_capacity(DEPTH * 14 + 96);
        for _ in 0..DEPTH {
            source.push_str("mod layer {");
        }
        source.push_str("use crate::Target as Alias; fn run() { Alias(); }");
        source.extend(std::iter::repeat_n('}', DEPTH));

        let (parsed, imports) = collect(&source)?;
        let calls = call_identifiers(parsed.root_node(), parsed.bytes(), "Alias");
        assert_eq!(calls.len(), 1);
        assert_eq!(resolve(&imports, calls[0], "Alias")?, "crate::Target");
        Ok(())
    }

    fn collect(source: &str) -> Result<(RustSource, Imports), Box<dyn Error>> {
        let parsed = RustSource::parse(source.as_bytes().to_vec())?;
        let path = RepositoryPath::parse("crates/sample/src/lib.rs")?;
        let reexports = std::sync::Arc::new(ReexportGraph::default());
        let imports = Imports::collect(parsed.root_node(), parsed.bytes(), &[], &path, &reexports);
        Ok((parsed, imports))
    }

    fn resolve(imports: &Imports, node: Node<'_>, path: &str) -> Result<String, Box<dyn Error>> {
        let registry = SinkRegistry::try_new(1, Vec::new())?;
        let ImportResolution::Exact(path) = imports.resolve(node, path, &registry) else {
            return Err("import did not resolve exactly".into());
        };
        Ok(path)
    }

    fn call_identifiers<'tree>(
        root: Node<'tree>,
        bytes: &[u8],
        expected: &str,
    ) -> Vec<Node<'tree>> {
        let mut output = Vec::new();
        let mut pending = vec![root];
        while let Some(node) = pending.pop() {
            if node.kind() == "identifier"
                && canonical_identifier(node, bytes).as_deref() == Some(expected)
                && node
                    .parent()
                    .is_some_and(|parent| parent.kind() == "call_expression")
            {
                output.push(node);
            }
            for index in (0..node.named_child_count()).rev() {
                if let Some(child) = node.named_child(index) {
                    pending.push(child);
                }
            }
        }
        output.sort_by_key(Node::start_byte);
        output
    }
}
