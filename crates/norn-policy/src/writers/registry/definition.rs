//! Exact project-wrapper definition authority.

use tree_sitter::Node;

use super::{RegistryError, validate_rust_path};
use crate::digest::Digest;
use crate::path::RepositoryPath;
use crate::rust::RustSource;
use crate::writers::syntax::{function_name, function_signature_digest, normalized_node_digest};

/// Exact source item, signature, and implementation required by a project-wrapper row.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct DefinitionSpec {
    source: RepositoryPath,
    item: String,
    signature: Digest,
    implementation: Digest,
}

/// Exact source-local receiver identity for a definition-backed method.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct DefinitionReceiver {
    source: RepositoryPath,
    item: String,
}

impl DefinitionReceiver {
    pub(crate) fn key(&self) -> String {
        format!("{}:{}", self.source.as_str(), self.item)
    }
}

impl DefinitionSpec {
    /// Bind a project wrapper to one complete Rust function implementation.
    ///
    /// # Errors
    ///
    /// Rejects invalid repository paths, item paths, incomplete or multiple
    /// definitions, and definitions whose function name differs from the item.
    pub fn from_function_source(
        source: &str,
        item: &str,
        definition: &str,
    ) -> Result<Self, RegistryError> {
        let Ok(source) = RepositoryPath::parse(source) else {
            return Err(RegistryError::DefinitionPath);
        };
        validate_rust_path(item)?;
        let definition = definition.trim();
        let (snippet, expected_start, expected_end) = if item.contains("::") {
            let prefix = "struct DefinitionAuthority; impl DefinitionAuthority { ";
            (
                format!("{prefix}{definition} }}\n"),
                prefix.len(),
                prefix.len() + definition.len(),
            )
        } else {
            (format!("{definition}\n"), 0, definition.len())
        };
        let Ok(parsed) = RustSource::parse(snippet.as_bytes()) else {
            return Err(RegistryError::DefinitionAuthority);
        };
        let functions = collect_functions(parsed.root_node());
        let [function] = functions.as_slice() else {
            return Err(RegistryError::DefinitionAuthority);
        };
        if function.start_byte() != expected_start || function.end_byte() != expected_end {
            return Err(RegistryError::DefinitionAuthority);
        }
        if function.child_by_field_name("body").is_none() {
            return Err(RegistryError::DefinitionAuthority);
        }
        let name =
            function_name(*function, parsed.bytes()).ok_or(RegistryError::DefinitionAuthority)?;
        if item.rsplit("::").next() != Some(name.as_str()) {
            return Err(RegistryError::DefinitionAuthority);
        }
        Ok(Self {
            source,
            item: item.to_owned(),
            signature: function_signature_digest(*function, parsed.bytes()),
            implementation: normalized_node_digest(*function, parsed.bytes()),
        })
    }

    pub(super) fn reviewed_function(
        source: &str,
        item: &str,
        signature: &str,
        implementation: Digest,
    ) -> Result<Self, RegistryError> {
        let synthetic = format!("{signature} {{}} ");
        let mut definition = Self::from_function_source(source, item, &synthetic)?;
        definition.implementation = implementation;
        Ok(definition)
    }

    /// Return the exact repository source containing the definition.
    #[must_use]
    pub const fn source(&self) -> &RepositoryPath {
        &self.source
    }

    /// Return the exact free-function or `Type::method` item path.
    #[must_use]
    pub fn item(&self) -> &str {
        &self.item
    }

    pub(crate) const fn signature(&self) -> Digest {
        self.signature
    }

    pub(crate) const fn implementation(&self) -> Digest {
        self.implementation
    }

    pub(crate) fn receiver(&self) -> Option<DefinitionReceiver> {
        self.item
            .rsplit_once("::")
            .map(|(receiver, _)| DefinitionReceiver {
                source: self.source.clone(),
                item: receiver.to_owned(),
            })
    }

    pub(crate) fn matches_receiver_type(
        &self,
        source: &RepositoryPath,
        resolved: &str,
        local: &str,
    ) -> bool {
        self.source == *source
            && self
                .receiver()
                .is_some_and(|receiver| receiver.item == resolved || receiver.item == local)
    }
}

fn collect_functions(root: Node<'_>) -> Vec<Node<'_>> {
    let mut functions = Vec::new();
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if node.kind() == "function_item" {
            functions.push(node);
            continue;
        }
        let mut cursor = node.walk();
        let children = node.named_children(&mut cursor).collect::<Vec<_>>();
        pending.extend(children.into_iter().rev());
    }
    functions
}
