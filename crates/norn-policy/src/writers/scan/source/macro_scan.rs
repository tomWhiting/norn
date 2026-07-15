//! Macro invocation and `macro_rules!` expansion-body coverage.

use std::collections::BTreeSet;

use tree_sitter::Node;

use super::{RecordedSink, Scanner, callable};
use crate::writers::WriterCandidateForm;
use crate::writers::imports::ImportResolution;
use crate::writers::input::WriterScanError;
use crate::writers::model::{SinkDiscovery, UnknownSinkReason};
use crate::writers::syntax::{canonical_identifier, macro_identifier_nodes, normalized_path};

impl Scanner<'_, '_, '_> {
    pub(super) fn macro_invocation(&mut self, node: Node<'_>) -> Result<(), WriterScanError> {
        let (resolved, candidate) = self.resolve_macro_path(node)?;
        let recorded = resolved
            .as_deref()
            .and_then(|path| self.registry.macro_sink(path))
            .map(RecordedSink::from_spec);
        let registered = recorded.is_some();
        if let Some(sink) = recorded {
            self.operation(node, sink, SinkDiscovery::MacroInvocation);
        } else if resolved
            .as_deref()
            .is_some_and(|path| self.registry.has_terminal(super::support::terminal(path)))
        {
            self.unknown(
                node,
                &candidate,
                UnknownSinkReason::UnresolvedAlias,
                WriterCandidateForm::MacroInvocation,
            )?;
        }
        let mut cursor = node.walk();
        for token_tree in node
            .named_children(&mut cursor)
            .filter(|child| child.kind() == "token_tree")
        {
            self.macro_tokens(
                token_tree,
                UnknownSinkReason::MacroTokenCandidate,
                WriterCandidateForm::MacroToken,
            )?;
            if !registered {
                self.macro_escapes(token_tree, &candidate)?;
            }
        }
        Ok(())
    }

    fn resolve_macro_path(
        &mut self,
        node: Node<'_>,
    ) -> Result<(Option<String>, String), WriterScanError> {
        let Some(path) = node
            .child_by_field_name("macro")
            .and_then(|macro_path| normalized_path(macro_path, self.bytes))
        else {
            return Ok((None, "macro".to_owned()));
        };
        let candidate = super::support::terminal(&path).to_owned();
        match self.imports.resolve(node, &path, self.registry) {
            ImportResolution::Exact(resolved) => Ok((Some(resolved), candidate)),
            ImportResolution::Ambiguous | ImportResolution::AmbiguousReexport => {
                self.unknown(
                    node,
                    &candidate,
                    UnknownSinkReason::AmbiguousAlias,
                    WriterCandidateForm::MacroInvocation,
                )?;
                Ok((None, candidate))
            }
            ImportResolution::Wildcard | ImportResolution::WildcardReexport => {
                self.unknown(
                    node,
                    &candidate,
                    UnknownSinkReason::WildcardImport,
                    WriterCandidateForm::MacroInvocation,
                )?;
                Ok((None, candidate))
            }
        }
    }

    pub(super) fn macro_definition(&mut self, node: Node<'_>) -> Result<(), WriterScanError> {
        let candidate = node
            .child_by_field_name("name")
            .and_then(|name| canonical_identifier(name, self.bytes))
            .unwrap_or_else(|| "macro".to_owned());
        let mut cursor = node.walk();
        for rule in node
            .named_children(&mut cursor)
            .filter(|child| child.kind() == "macro_rule")
        {
            if let Some(body) = rule.child_by_field_name("right") {
                self.macro_tokens(
                    body,
                    UnknownSinkReason::MacroDefinitionCandidate,
                    WriterCandidateForm::MacroDefinition,
                )?;
                self.macro_escapes(body, &candidate)?;
            }
        }
        Ok(())
    }

    fn macro_tokens(
        &mut self,
        token_tree: Node<'_>,
        reason: UnknownSinkReason,
        form: WriterCandidateForm,
    ) -> Result<(), WriterScanError> {
        let terminals: BTreeSet<String> = self
            .registry
            .terminal_names()
            .into_iter()
            .map(str::to_owned)
            .collect();
        for identifier in macro_identifier_nodes(token_tree, self.bytes) {
            let Some(name) = canonical_identifier(identifier, self.bytes) else {
                continue;
            };
            if terminals.contains(&name) {
                self.unknown(identifier, &name, reason, form)?;
            }
        }
        Ok(())
    }

    fn macro_escapes(
        &mut self,
        token_tree: Node<'_>,
        macro_candidate: &str,
    ) -> Result<(), WriterScanError> {
        for identifier in macro_identifier_nodes(token_tree, self.bytes) {
            let Some(name) = canonical_identifier(identifier, self.bytes) else {
                continue;
            };
            if self.bindings.contains_value(identifier, &name) {
                self.unknown(
                    identifier,
                    macro_candidate,
                    UnknownSinkReason::AuthorityArgument,
                    WriterCandidateForm::AuthorityEscape,
                )?;
            }
            let Some(binding) = callable::resolve(
                identifier,
                self.bytes,
                self.imports,
                self.registry,
                self.path,
                &self.callables,
            ) else {
                continue;
            };
            let candidate = self.callable_candidate(identifier, &binding);
            self.unknown(
                identifier,
                &candidate,
                UnknownSinkReason::CallableEscape,
                WriterCandidateForm::CallableEscape,
            )?;
        }
        Ok(())
    }
}
