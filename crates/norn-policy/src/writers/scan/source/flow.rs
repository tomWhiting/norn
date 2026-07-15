//! Local writer-authority and callable propagation.

use tree_sitter::Node;

use super::state::is_scope;
use super::support::{definition_paths, first_expression, last_expression, local_definition_path};
use super::{FlowValue, Scanner, callable, functions};
use crate::writers::WriterCandidateForm;
use crate::writers::input::WriterScanError;
use crate::writers::model::{FlowClass, UnknownSinkReason};
use crate::writers::registry::SinkSpec;
use crate::writers::syntax::{binding_name, canonical_identifier, function_name, identifier_name};

impl Scanner<'_, '_, '_> {
    pub(super) fn binding(&mut self, node: Node<'_>) -> Result<(), WriterScanError> {
        let (Some(pattern), Some(value)) = (
            node.child_by_field_name("pattern"),
            node.child_by_field_name("value"),
        ) else {
            return Ok(());
        };
        let Some(name) = binding_name(pattern, self.bytes) else {
            self.unsupported_storage(value)?;
            return Ok(());
        };
        let flow = self.flow_value_of(value);
        let stored = (flow.class != FlowClass::None).then_some(flow);
        if stored.is_none() && self.contains_authority(value) {
            self.authority_candidate(value, "authority", UnknownSinkReason::AuthorityStorage)?;
        }
        self.bindings.declare(node, name.clone(), stored);
        self.bind_callable(node, name, value, false)?;
        Ok(())
    }

    pub(super) fn assignment(&mut self, node: Node<'_>) -> Result<(), WriterScanError> {
        let (Some(left), Some(right)) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        ) else {
            return Ok(());
        };
        let Some(name) = identifier_name(left, self.bytes) else {
            self.unsupported_storage(right)?;
            return Ok(());
        };
        let flow = self.flow_value_of(right);
        let stored = (flow.class != FlowClass::None).then_some(flow);
        if stored.is_none() && self.contains_authority(right) {
            self.authority_candidate(right, "authority", UnknownSinkReason::AuthorityStorage)?;
        }
        self.bindings.assign(node, name.clone(), stored);
        self.bind_callable(node, name, right, true)?;
        Ok(())
    }

    fn bind_callable(
        &mut self,
        scope: Node<'_>,
        name: String,
        value: Node<'_>,
        assignment: bool,
    ) -> Result<(), WriterScanError> {
        let binding = callable::resolve(
            value,
            self.bytes,
            self.imports,
            self.registry,
            self.path,
            &self.callables,
        );
        let escaped = binding.is_none();
        if assignment {
            self.callables.assign(scope, name, binding);
        } else {
            self.callables.declare(scope, name, binding);
        }
        if escaped {
            self.record_callable_escapes(value)?;
        }
        Ok(())
    }

    pub(super) fn flow_value_of(&self, node: Node<'_>) -> FlowValue {
        self.flow_value_at(node, node)
    }

    fn flow_value_at<'tree>(
        &self,
        node: Node<'tree>,
        mut lexical_anchor: Node<'tree>,
    ) -> FlowValue {
        let mut current = node;
        loop {
            match current.kind() {
                "identifier" | "self" => {
                    let Some(name) = canonical_identifier(current, self.bytes) else {
                        return FlowValue::from_class(FlowClass::None);
                    };
                    return self.bindings.values(lexical_anchor, &name).map_or_else(
                        || FlowValue::from_class(FlowClass::None),
                        |(values, absent)| FlowValue::merge(values, absent),
                    );
                }
                "call_expression" => {
                    return self
                        .call_flows
                        .get(&(current.start_byte(), current.end_byte()))
                        .cloned()
                        .unwrap_or_else(|| FlowValue::from_class(FlowClass::None));
                }
                "reference_expression" => {
                    let Some(child) = current.child_by_field_name("value") else {
                        return FlowValue::from_class(FlowClass::None);
                    };
                    current = child;
                }
                "await_expression"
                | "try_expression"
                | "parenthesized_expression"
                | "return_expression" => {
                    let Some(child) = first_expression(current) else {
                        return FlowValue::from_class(FlowClass::None);
                    };
                    current = child;
                }
                "block" => {
                    lexical_anchor = current;
                    let Some(child) = last_expression(current) else {
                        return FlowValue::from_class(FlowClass::None);
                    };
                    current = child;
                }
                _ => return FlowValue::from_class(FlowClass::None),
            }
        }
    }

    pub(super) fn contains_authority(&self, node: Node<'_>) -> bool {
        let mut pending = vec![(node, node)];
        while let Some((current, lexical_anchor)) = pending.pop() {
            if matches!(current.kind(), "identifier" | "self" | "call_expression")
                && self.flow_value_at(current, lexical_anchor).class != FlowClass::None
            {
                return true;
            }
            if matches!(
                current.kind(),
                "call_expression" | "macro_invocation" | "macro_definition"
            ) {
                continue;
            }
            let transparent_child = match current.kind() {
                "reference_expression" => current.child_by_field_name("value"),
                "await_expression"
                | "try_expression"
                | "parenthesized_expression"
                | "return_expression" => first_expression(current),
                _ => None,
            };
            if let Some(child) = transparent_child {
                pending.push((child, lexical_anchor));
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
        false
    }

    pub(super) fn return_escape(&mut self, node: Node<'_>) -> Result<(), WriterScanError> {
        if let Some(value) = first_expression(node) {
            if self.contains_authority(value) && !self.registered_container() {
                self.authority_candidate(value, "authority", UnknownSinkReason::AuthorityReturn)?;
            }
            self.record_callable_escapes(value)?;
        }
        Ok(())
    }

    pub(super) fn implicit_callable_escape(&mut self) -> Result<(), WriterScanError> {
        if self.container.kind() != "function_item" {
            return Ok(());
        }
        let Some(body) = self.container.child_by_field_name("body") else {
            return Ok(());
        };
        let Some(value) = last_expression(body) else {
            return Ok(());
        };
        if value.kind() != "return_expression" {
            self.record_callable_escapes(value)?;
        }
        Ok(())
    }

    pub(super) fn has_implicit_return_authority(&self) -> bool {
        let Some(body) = self.container.child_by_field_name("body") else {
            return false;
        };
        last_expression(body).is_some_and(|value| {
            value.kind() != "return_expression" && self.contains_authority(value)
        })
    }

    pub(super) fn registered_container(&self) -> bool {
        let Some(name) = function_name(self.container, self.bytes) else {
            return false;
        };
        definition_paths(self.container, self.bytes, &name)
            .iter()
            .any(|item| self.registry.has_definition(self.path, item))
    }

    pub(super) fn static_storage(&mut self, node: Node<'_>) -> Result<(), WriterScanError> {
        if let Some(value) = node.child_by_field_name("value") {
            self.unsupported_storage(value)?;
        }
        Ok(())
    }

    fn unsupported_storage(&mut self, value: Node<'_>) -> Result<(), WriterScanError> {
        if self.contains_authority(value) {
            self.authority_candidate(value, "authority", UnknownSinkReason::AuthorityStorage)?;
        }
        self.record_callable_escapes(value)
    }

    pub(super) fn record_callable_escapes(
        &mut self,
        value: Node<'_>,
    ) -> Result<(), WriterScanError> {
        let mut escapes = Vec::new();
        callable::collect(
            value,
            self.bytes,
            self.imports,
            self.registry,
            self.path,
            &self.callables,
            &mut escapes,
        );
        for (node, binding) in escapes {
            let candidate = self.callable_candidate(node, &binding);
            self.unknown(
                node,
                &candidate,
                UnknownSinkReason::CallableEscape,
                WriterCandidateForm::CallableEscape,
            )?;
        }
        Ok(())
    }

    pub(super) fn callable_candidate(
        &self,
        node: Node<'_>,
        binding: &callable::CallableBinding,
    ) -> String {
        match binding {
            callable::CallableBinding::Registered(path) => {
                let local_item = local_definition_path(node, self.bytes, path);
                self.registry
                    .function(path, self.path, &local_item)
                    .map_or_else(
                        || super::support::terminal(path).to_owned(),
                        |spec| spec.id().as_str().to_owned(),
                    )
            }
            callable::CallableBinding::Unknown { candidate, .. } => candidate.clone(),
        }
    }

    pub(super) fn authority_arguments(
        &mut self,
        call: Node<'_>,
        candidate: &str,
    ) -> Result<(), WriterScanError> {
        let Some(arguments) = call.child_by_field_name("arguments") else {
            return Ok(());
        };
        let mut cursor = arguments.walk();
        for argument in arguments.named_children(&mut cursor) {
            if self.contains_authority(argument) {
                self.authority_candidate(
                    argument,
                    candidate,
                    UnknownSinkReason::AuthorityArgument,
                )?;
            }
        }
        Ok(())
    }

    pub(super) fn local_authority_arguments(
        &mut self,
        call: Node<'_>,
        candidate: &str,
        contained_parameters: &[bool],
    ) -> Result<(), WriterScanError> {
        let Some(arguments) = call.child_by_field_name("arguments") else {
            return Ok(());
        };
        let mut cursor = arguments.walk();
        for (index, argument) in arguments.named_children(&mut cursor).enumerate() {
            let contained = contained_parameters.get(index).copied().unwrap_or(false);
            if !contained && self.contains_authority(argument) {
                self.authority_candidate(
                    argument,
                    candidate,
                    UnknownSinkReason::AuthorityArgument,
                )?;
            }
        }
        Ok(())
    }

    pub(super) fn return_flow(
        &self,
        spec: &SinkSpec,
        call: Node<'_>,
        receiver: &FlowValue,
    ) -> FlowValue {
        match spec.returns() {
            FlowClass::SameReceiver => receiver.clone(),
            FlowClass::FirstArgument => self.first_argument_flow(call),
            FlowClass::RootAuthority => spec
                .definition()
                .and_then(crate::writers::registry::DefinitionSpec::receiver)
                .map_or_else(
                    || FlowValue::from_class(FlowClass::RootAuthority),
                    |provenance| FlowValue::with_receiver(FlowClass::RootAuthority, provenance),
                ),
            flow => FlowValue::from_class(flow),
        }
    }

    pub(super) fn first_argument_flow(&self, call: Node<'_>) -> FlowValue {
        call.child_by_field_name("arguments")
            .and_then(first_expression)
            .map_or_else(
                || FlowValue::from_class(FlowClass::None),
                |argument| self.flow_value_of(argument),
            )
    }

    pub(super) fn seed_parameters(&mut self) {
        if self.container.kind() != "function_item" {
            return;
        }
        let Some(body) = self.container.child_by_field_name("body") else {
            return;
        };
        if let Some(parameters) = self.container.child_by_field_name("parameters") {
            let mut cursor = parameters.walk();
            for parameter in parameters.named_children(&mut cursor) {
                self.seed_parameter(body, parameter);
            }
        }
        if let Some(kind) = enclosing_impl_type_node(self.container)
            && let Some(flow) =
                functions::type_flow(kind, self.bytes, self.imports, self.registry, self.path)
        {
            self.bindings.declare(body, "self".to_owned(), Some(flow));
        }
        self.seed_lexical_shadows();
    }

    fn seed_parameter(&mut self, body: Node<'_>, parameter: Node<'_>) {
        let (Some(pattern), Some(kind)) = (
            parameter.child_by_field_name("pattern"),
            parameter.child_by_field_name("type"),
        ) else {
            return;
        };
        for name in binding_names(pattern, self.bytes) {
            self.callables.declare(body, name, None);
        }
        let Some(name) = binding_name(pattern, self.bytes) else {
            return;
        };
        let flow = functions::type_flow(kind, self.bytes, self.imports, self.registry, self.path);
        self.bindings.declare(body, name, flow);
    }

    fn seed_lexical_shadows(&mut self) {
        let mut pending = vec![self.container];
        while let Some(node) = pending.pop() {
            if node.id() != self.container.id()
                && matches!(
                    node.kind(),
                    "function_item" | "struct_item" | "const_item" | "static_item"
                )
                && let Some(name) = node
                    .child_by_field_name("name")
                    .and_then(|name| canonical_identifier(name, self.bytes))
            {
                self.callables.declare(node, name, None);
            }
            if node.id() != self.container.id() && node.kind() == "function_item" {
                continue;
            }
            let binding = match node.kind() {
                "closure_expression" => node
                    .child_by_field_name("parameters")
                    .map(|pattern| (node, pattern)),
                "for_expression" => node
                    .child_by_field_name("body")
                    .zip(node.child_by_field_name("pattern")),
                "match_arm" => node
                    .child_by_field_name("pattern")
                    .map(|pattern| (node, pattern)),
                "let_condition" => node
                    .child_by_field_name("pattern")
                    .and_then(|pattern| let_binding_scope(node).map(|scope| (scope, pattern))),
                _ => None,
            };
            if let Some((scope, pattern)) = binding {
                for name in binding_names(pattern, self.bytes) {
                    self.bindings.declare(scope, name.clone(), None);
                    self.callables.declare(scope, name, None);
                }
            }
            for index in (0..node.named_child_count()).rev() {
                if let Some(child) = node.named_child(index) {
                    pending.push(child);
                }
            }
        }
    }
}

fn enclosing_impl_type_node(node: Node<'_>) -> Option<Node<'_>> {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "impl_item" {
            return parent.child_by_field_name("type");
        }
        current = parent.parent();
    }
    None
}

fn binding_names(pattern: Node<'_>, bytes: &[u8]) -> Vec<String> {
    let mut names = Vec::new();
    let mut pending = vec![pattern];
    while let Some(node) = pending.pop() {
        if node.kind() == "identifier"
            && let Some(name) = canonical_identifier(node, bytes)
        {
            names.push(name);
            continue;
        }
        for index in (0..node.named_child_count()).rev() {
            if let Some(child) = node.named_child(index) {
                pending.push(child);
            }
        }
    }
    names.sort();
    names.dedup();
    names
}

fn let_binding_scope(node: Node<'_>) -> Option<Node<'_>> {
    let mut current = node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "if_expression" => return parent.child_by_field_name("consequence"),
            "while_expression" => return parent.child_by_field_name("body"),
            "function_item" | "closure_expression" => return None,
            _ => current = parent.parent(),
        }
    }
    None
}
