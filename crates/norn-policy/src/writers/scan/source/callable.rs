//! Local callable binding and parenthesized function resolution.

use tree_sitter::Node;

use super::state::{LocalLookup, ScopedBindings, is_scope};
use super::support::local_definition_path;
use super::support::{peel_callable, terminal};
use crate::path::RepositoryPath;
use crate::writers::imports::{ImportResolution, Imports};
use crate::writers::model::UnknownSinkReason;
use crate::writers::registry::SinkRegistry;
use crate::writers::syntax::{identifier_name, normalized_path};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum CallableBinding {
    Registered(String),
    Unknown {
        candidate: String,
        reason: UnknownSinkReason,
    },
}

pub(super) fn resolve(
    node: Node<'_>,
    bytes: &[u8],
    imports: &Imports,
    registry: &SinkRegistry,
    source: &RepositoryPath,
    locals: &ScopedBindings<CallableBinding>,
) -> Option<CallableBinding> {
    resolve_at(node, node, bytes, imports, registry, source, locals)
}

fn resolve_at(
    node: Node<'_>,
    lexical_anchor: Node<'_>,
    bytes: &[u8],
    imports: &Imports,
    registry: &SinkRegistry,
    source: &RepositoryPath,
    locals: &ScopedBindings<CallableBinding>,
) -> Option<CallableBinding> {
    let callable = peel_callable(node);
    if let Some(name) = identifier_name(callable, bytes) {
        match locals.local_lookup(lexical_anchor, &name) {
            LocalLookup::Exact(binding) => return Some(binding),
            LocalLookup::Ambiguous => {
                return Some(CallableBinding::Unknown {
                    candidate: name,
                    reason: UnknownSinkReason::AmbiguousAlias,
                });
            }
            LocalLookup::Shadowed => return None,
            LocalLookup::Unbound => {}
        }
    }
    if !matches!(callable.kind(), "identifier" | "scoped_identifier") {
        return None;
    }
    let path = normalized_path(callable, bytes)?;
    match imports.resolve(lexical_anchor, &path, registry) {
        ImportResolution::Exact(resolved) => {
            exact(resolved, registry, source, lexical_anchor, bytes)
        }
        ImportResolution::Ambiguous if registry.has_terminal(terminal(&path)) => {
            Some(CallableBinding::Unknown {
                candidate: terminal(&path).to_owned(),
                reason: UnknownSinkReason::AmbiguousAlias,
            })
        }
        ImportResolution::Wildcard if registry.has_terminal(terminal(&path)) => {
            Some(CallableBinding::Unknown {
                candidate: terminal(&path).to_owned(),
                reason: UnknownSinkReason::WildcardImport,
            })
        }
        ImportResolution::AmbiguousReexport => Some(CallableBinding::Unknown {
            candidate: terminal(&path).to_owned(),
            reason: UnknownSinkReason::AmbiguousAlias,
        }),
        ImportResolution::WildcardReexport => Some(CallableBinding::Unknown {
            candidate: terminal(&path).to_owned(),
            reason: UnknownSinkReason::WildcardImport,
        }),
        ImportResolution::Ambiguous | ImportResolution::Wildcard => None,
    }
}

pub(super) fn collect<'tree>(
    node: Node<'tree>,
    bytes: &[u8],
    imports: &Imports,
    registry: &SinkRegistry,
    source: &RepositoryPath,
    locals: &ScopedBindings<CallableBinding>,
    output: &mut Vec<(Node<'tree>, CallableBinding)>,
) {
    let mut pending = vec![(node, node)];
    while let Some((current, lexical_anchor)) = pending.pop() {
        if let Some(binding) = resolve_at(
            current,
            lexical_anchor,
            bytes,
            imports,
            registry,
            source,
            locals,
        ) {
            output.push((current, binding));
            continue;
        }
        let peeled = peel_callable(current);
        if peeled.id() != current.id() {
            pending.push((peeled, lexical_anchor));
            continue;
        }
        if matches!(current.kind(), "macro_invocation" | "macro_definition") {
            continue;
        }
        if current.kind() == "scoped_identifier" {
            // A qualified value is one expression. Its path prefixes are not
            // independent callable values and must not be reinterpreted as
            // candidate functions when the complete path did not resolve.
            continue;
        }
        if current.kind() == "call_expression" {
            // Arguments are scanned by their own call event. Descending here
            // would duplicate escaped callables from an enclosing expression.
            continue;
        }
        for index in (0..current.named_child_count()).rev() {
            if let Some(child) = current.named_child(index) {
                let child_anchor = if is_scope(child) {
                    child
                } else {
                    lexical_anchor
                };
                pending.push((child, child_anchor));
            }
        }
    }
}

fn exact(
    path: String,
    registry: &SinkRegistry,
    source: &RepositoryPath,
    node: Node<'_>,
    bytes: &[u8],
) -> Option<CallableBinding> {
    let local_item = local_definition_path(node, bytes, &path);
    if registry.function(&path, source, &local_item).is_some() {
        return Some(CallableBinding::Registered(path));
    }
    if registry.is_function_candidate(&path)
        && !SinkRegistry::is_reviewed_non_writer_function(&path)
    {
        return Some(CallableBinding::Unknown {
            candidate: terminal(&path).to_owned(),
            reason: UnknownSinkReason::KnownNamespaceCandidate,
        });
    }
    None
}
