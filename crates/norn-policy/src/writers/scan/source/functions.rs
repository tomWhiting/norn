//! Exact local-function authority-parameter proofs.

use std::collections::{BTreeMap, BTreeSet};

use tree_sitter::Node;

use super::FlowValue;
use super::support::{collect_functions, enclosing_impl_type, first_expression};
use crate::path::RepositoryPath;
use crate::rust::RustSource;
use crate::writers::imports::{ImportResolution, Imports, Reexports, source_crate};
use crate::writers::input::{WriterScanError, WriterSource};
use crate::writers::model::FlowClass;
use crate::writers::registry::SinkRegistry;
use crate::writers::syntax::{
    binding_name, canonical_identifier, function_name, node_text, normalized_path,
};

#[derive(Clone, Debug)]
struct FunctionProof {
    authority_parameters: Vec<bool>,
}

#[derive(Clone, Debug, Default)]
pub(in crate::writers::scan) struct FunctionIndex {
    functions: BTreeMap<String, BTreeMap<String, Vec<FunctionProof>>>,
}

pub(super) enum FunctionResolution<'a> {
    Absent,
    Exact(&'a [bool]),
    Ambiguous,
}

impl FunctionIndex {
    pub(in crate::writers::scan) fn build(
        sources: &[&WriterSource],
        registry: &SinkRegistry,
        reexports: &Reexports,
    ) -> Result<Self, WriterScanError> {
        let mut index = Self::default();
        for source in sources {
            let parsed = RustSource::parse(source.bytes())?;
            let excluded = parsed.test_only_ranges()?;
            let root = parsed.root_node();
            let imports =
                Imports::collect(root, parsed.bytes(), &excluded, source.path(), reexports);
            let mut functions = Vec::new();
            collect_functions(root, &excluded, &mut functions);
            for function in functions {
                if enclosing_impl_type(function, parsed.bytes()).is_some()
                    || has_enclosing_function(function)
                {
                    continue;
                }
                let Some(name) = function_name(function, parsed.bytes()) else {
                    continue;
                };
                let Some(path) = imports.canonical_item(function, &name) else {
                    continue;
                };
                let proof = prove_function(function, parsed.bytes(), &imports, registry, source);
                index
                    .functions
                    .entry(source_crate(source.path()))
                    .or_default()
                    .entry(path)
                    .or_default()
                    .push(proof);
            }
        }
        Ok(index)
    }

    pub(super) fn resolve<'a>(
        &'a self,
        call: Node<'_>,
        resolved: &str,
        imports: &Imports,
        source: &RepositoryPath,
    ) -> FunctionResolution<'a> {
        let crate_key = source_crate(source);
        let mut keys = BTreeSet::new();
        keys.insert(resolved.to_owned());
        if let Some(local) = imports.local_path(call, resolved) {
            keys.insert(local);
        }
        let Some(crate_functions) = self.functions.get(&crate_key) else {
            return FunctionResolution::Absent;
        };
        let mut matches = keys
            .iter()
            .filter_map(|key| crate_functions.get(key.as_str()))
            .flat_map(|proofs| proofs.iter());
        let Some(first) = matches.next() else {
            return FunctionResolution::Absent;
        };
        if matches.next().is_some() {
            return FunctionResolution::Ambiguous;
        }
        FunctionResolution::Exact(&first.authority_parameters)
    }
}

pub(super) fn type_flow(
    node: Node<'_>,
    bytes: &[u8],
    imports: &Imports,
    registry: &SinkRegistry,
    source: &RepositoryPath,
) -> Option<FlowValue> {
    let path = type_path(node, bytes)?;
    let ImportResolution::Exact(resolved) = imports.resolve(node, &path, registry) else {
        return None;
    };
    let local = imports
        .local_path(node, &resolved)
        .unwrap_or_else(|| resolved.clone());
    if let Some(receiver) = registry.definition_receiver(source, &resolved, &local) {
        return Some(FlowValue::with_receiver(FlowClass::RootAuthority, receiver));
    }
    let class = match_exact_type(&resolved).or_else(|| match_exact_type(&local))?;
    Some(FlowValue::from_class(class))
}

fn prove_function(
    function: Node<'_>,
    bytes: &[u8],
    imports: &Imports,
    registry: &SinkRegistry,
    source: &WriterSource,
) -> FunctionProof {
    let parameters = function
        .child_by_field_name("parameters")
        .map(|parameters| parameter_proofs(parameters, bytes, imports, registry, source))
        .unwrap_or_default();
    let structurally_provable = function.child_by_field_name("type_parameters").is_none()
        && has_unit_return(function, bytes)
        && !has_modifier(function, "async");
    let Some(body) = function.child_by_field_name("body") else {
        return FunctionProof {
            authority_parameters: vec![false; parameters.len()],
        };
    };
    let authority_parameters = parameters
        .iter()
        .map(|parameter| {
            structurally_provable
                && !parameter.name.is_empty()
                && parameter.flow.as_ref().is_some_and(|flow| {
                    !has_shadow(body, bytes, &parameter.name)
                        && occurrences_are_contained(body, bytes, &parameter.name, flow, registry)
                })
        })
        .collect();
    FunctionProof {
        authority_parameters,
    }
}

struct ParameterProof {
    name: String,
    flow: Option<FlowValue>,
}

fn parameter_proofs(
    parameters: Node<'_>,
    bytes: &[u8],
    imports: &Imports,
    registry: &SinkRegistry,
    source: &WriterSource,
) -> Vec<ParameterProof> {
    let mut output = Vec::new();
    let mut cursor = parameters.walk();
    for parameter in parameters.named_children(&mut cursor) {
        if parameter.kind() != "parameter" {
            if matches!(parameter.kind(), "self_parameter" | "variadic_parameter") {
                output.push(ParameterProof {
                    name: String::new(),
                    flow: None,
                });
            }
            continue;
        }
        let (Some(pattern), Some(kind)) = (
            parameter.child_by_field_name("pattern"),
            parameter.child_by_field_name("type"),
        ) else {
            output.push(ParameterProof {
                name: String::new(),
                flow: None,
            });
            continue;
        };
        output.push(ParameterProof {
            name: binding_name(pattern, bytes).unwrap_or_default(),
            flow: type_flow(kind, bytes, imports, registry, source.path()),
        });
    }
    output
}

fn occurrences_are_contained(
    body: Node<'_>,
    bytes: &[u8],
    name: &str,
    flow: &FlowValue,
    registry: &SinkRegistry,
) -> bool {
    let mut pending = vec![body];
    while let Some(node) = pending.pop() {
        if node.id() != body.id() && node.kind() == "function_item" {
            continue;
        }
        if node.kind() == "identifier"
            && canonical_identifier(node, bytes).as_deref() == Some(name)
            && (inside_closure(node, body) || !is_safe_method_receiver(node, bytes, flow, registry))
        {
            return false;
        }
        for index in (0..node.named_child_count()).rev() {
            if let Some(child) = node.named_child(index) {
                pending.push(child);
            }
        }
    }
    true
}

fn is_safe_method_receiver(
    identifier: Node<'_>,
    bytes: &[u8],
    flow: &FlowValue,
    registry: &SinkRegistry,
) -> bool {
    let Some(field) = identifier
        .parent()
        .filter(|parent| parent.kind() == "field_expression")
    else {
        return false;
    };
    if field
        .child_by_field_name("value")
        .is_none_or(|value| value.id() != identifier.id())
    {
        return false;
    }
    let Some(call) = field
        .parent()
        .filter(|parent| parent.kind() == "call_expression")
    else {
        return false;
    };
    if call
        .child_by_field_name("function")
        .is_none_or(|function| function.id() != field.id())
    {
        return false;
    }
    let Some(name) = field
        .child_by_field_name("field")
        .and_then(|name| canonical_identifier(name, bytes))
    else {
        return false;
    };
    registry
        .method(&name, flow.class, flow.receiver.as_ref())
        .is_some()
        || SinkRegistry::reviewed_authority_method(&name, flow.class).is_some()
}

fn has_shadow(body: Node<'_>, bytes: &[u8], name: &str) -> bool {
    let mut pending = vec![body];
    while let Some(node) = pending.pop() {
        if node.id() != body.id() && node.kind() == "function_item" {
            continue;
        }
        if matches!(
            node.kind(),
            "let_declaration" | "for_expression" | "match_arm"
        ) && node
            .child_by_field_name("pattern")
            .is_some_and(|pattern| pattern_contains(pattern, bytes, name))
        {
            return true;
        }
        if node.kind() == "closure_expression"
            && node
                .child_by_field_name("parameters")
                .is_some_and(|parameters| pattern_contains(parameters, bytes, name))
        {
            return true;
        }
        for index in (0..node.named_child_count()).rev() {
            if let Some(child) = node.named_child(index) {
                pending.push(child);
            }
        }
    }
    false
}

fn inside_closure(node: Node<'_>, body: Node<'_>) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.id() == body.id() {
            return false;
        }
        if matches!(parent.kind(), "closure_expression" | "async_block") {
            return true;
        }
        current = parent.parent();
    }
    false
}

fn pattern_contains(pattern: Node<'_>, bytes: &[u8], name: &str) -> bool {
    let mut pending = vec![pattern];
    while let Some(node) = pending.pop() {
        if node.kind() == "identifier" && canonical_identifier(node, bytes).as_deref() == Some(name)
        {
            return true;
        }
        for index in (0..node.named_child_count()).rev() {
            if let Some(child) = node.named_child(index) {
                pending.push(child);
            }
        }
    }
    false
}

fn type_path(mut node: Node<'_>, bytes: &[u8]) -> Option<String> {
    loop {
        if let Some(path) = normalized_path(node, bytes) {
            return Some(path);
        }
        node = node
            .child_by_field_name("type")
            .or_else(|| first_expression(node))?;
    }
}

fn match_exact_type(path: &str) -> Option<FlowClass> {
    match path {
        "std::fs::File"
        | "tokio::fs::File"
        | "crate::session::persistence::io::AdmittedSessionFile" => Some(FlowClass::WritableHandle),
        "tempfile::NamedTempFile" => Some(FlowClass::TemporaryHandle),
        "std::fs::OpenOptions" => Some(FlowClass::StandardOpenBuilder),
        "tokio::fs::OpenOptions" => Some(FlowClass::TokioOpenBuilder),
        "tempfile::Builder" => Some(FlowClass::TempfileBuilder),
        _ => None,
    }
}

fn has_unit_return(function: Node<'_>, bytes: &[u8]) -> bool {
    function
        .child_by_field_name("return_type")
        .is_none_or(|kind| node_text(kind, bytes).is_some_and(|text| text.trim() == "()"))
}

fn has_modifier(function: Node<'_>, modifier: &str) -> bool {
    let body = function.child_by_field_name("body").map(|node| node.id());
    let mut pending = vec![function];
    while let Some(node) = pending.pop() {
        if node.kind() == modifier {
            return true;
        }
        for index in (0..node.child_count()).rev() {
            if let Some(child) = node.child(index)
                && Some(child.id()) != body
            {
                pending.push(child);
            }
        }
    }
    false
}

fn has_enclosing_function(function: Node<'_>) -> bool {
    let mut current = function.parent();
    while let Some(parent) = current {
        if parent.kind() == "function_item" {
            return true;
        }
        current = parent.parent();
    }
    false
}
