//! Per-source alias, handle-flow, macro, and wrapper analysis.

mod callable;
mod definitions;
mod flow;
mod functions;
mod macro_scan;
mod record;
mod state;
mod support;

use std::collections::{BTreeMap, BTreeSet};

use tree_sitter::Node;

use super::{RawCandidate, RawOperation};
use crate::path::RepositoryPath;
use crate::rust::{RustSource, SourceRange};
use crate::writers::WriterCandidateForm;
use crate::writers::imports::{ImportResolution, Imports, Reexports};
use crate::writers::input::{WriterScanError, WriterSource};
use crate::writers::model::{
    FlowClass, OperationKind, SinkDiscovery, UnknownSinkReason, WriterRole, WriterToken,
};
use crate::writers::registry::{DefinitionReceiver, SinkRegistry, SinkSpec};
use crate::writers::syntax::{
    canonical_identifier, enclosing_is_generic, identifier_name, normalized_path,
};
use state::{LocalLookup, ScopedBindings};
use support::{
    EventKind, collect_events, collect_functions, local_definition_path, peel_callable, terminal,
};

pub(super) use functions::FunctionIndex;

pub(super) fn scan_source(
    source: &WriterSource,
    registry: &SinkRegistry,
    reexports: &Reexports,
    functions: &FunctionIndex,
    operations: &mut Vec<RawOperation>,
    candidates: &mut Vec<RawCandidate>,
    observed: &mut BTreeSet<WriterToken>,
) -> Result<(), WriterScanError> {
    let parsed = RustSource::parse(source.bytes())?;
    let excluded = parsed.test_only_ranges()?;
    let root = parsed.root_node();
    let imports = Imports::collect(root, parsed.bytes(), &excluded, source.path(), reexports);
    let mut source_functions = Vec::new();
    collect_functions(root, &excluded, &mut source_functions);

    let context = SourceContext {
        path: source.path(),
        bytes: parsed.bytes(),
        excluded: &excluded,
        imports: &imports,
        registry,
        functions,
    };
    let mut output = ScanOutput {
        operations,
        candidates,
        observed,
    };
    definitions::observe(source, &source_functions, registry, &mut output);
    scan_container(root, false, &context, &mut output)?;
    for function in source_functions {
        scan_container(function, true, &context, &mut output)?;
    }
    Ok(())
}

struct SourceContext<'a> {
    path: &'a RepositoryPath,
    bytes: &'a [u8],
    excluded: &'a [SourceRange],
    imports: &'a Imports,
    registry: &'a SinkRegistry,
    functions: &'a FunctionIndex,
}

struct ScanOutput<'a> {
    operations: &'a mut Vec<RawOperation>,
    candidates: &'a mut Vec<RawCandidate>,
    observed: &'a mut BTreeSet<WriterToken>,
}

struct Scanner<'context, 'output, 'tree> {
    path: &'context RepositoryPath,
    bytes: &'context [u8],
    registry: &'context SinkRegistry,
    functions: &'context FunctionIndex,
    imports: &'context Imports,
    bindings: ScopedBindings<FlowValue>,
    callables: ScopedBindings<callable::CallableBinding>,
    call_flows: BTreeMap<(usize, usize), FlowValue>,
    operations: &'output mut Vec<RawOperation>,
    candidates: &'output mut Vec<RawCandidate>,
    observed: &'output mut BTreeSet<WriterToken>,
    container: Node<'tree>,
}

struct RecordedSink {
    id: WriterToken,
    kind: OperationKind,
    role: WriterRole,
    definition_bound: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FlowValue {
    class: FlowClass,
    receiver: Option<DefinitionReceiver>,
    uncertain: bool,
}

impl FlowValue {
    fn from_class(class: FlowClass) -> Self {
        Self {
            class,
            receiver: None,
            uncertain: false,
        }
    }

    fn with_receiver(class: FlowClass, receiver: DefinitionReceiver) -> Self {
        Self {
            class,
            receiver: Some(receiver),
            uncertain: false,
        }
    }

    fn merge(values: Vec<Self>, absent: bool) -> Self {
        let mut values = values.into_iter();
        let Some(mut merged) = values.next() else {
            return Self::from_class(FlowClass::None);
        };
        merged.uncertain |= absent;
        for value in values {
            if value.class != merged.class || value.receiver != merged.receiver || value.uncertain {
                merged.uncertain = true;
            }
        }
        merged
    }
}

impl RecordedSink {
    fn from_spec(spec: &SinkSpec) -> Self {
        Self {
            id: spec.id().clone(),
            kind: spec.kind(),
            role: spec.role(),
            definition_bound: spec.definition().is_some(),
        }
    }
}

fn scan_container(
    container: Node<'_>,
    include_function: bool,
    context: &SourceContext<'_>,
    output: &mut ScanOutput<'_>,
) -> Result<(), WriterScanError> {
    let mut events = Vec::new();
    collect_events(container, include_function, context.excluded, &mut events);
    events.sort_by_key(|event| (event.node.end_byte(), event.kind, event.node.start_byte()));
    let mut scanner = Scanner {
        path: context.path,
        bytes: context.bytes,
        registry: context.registry,
        functions: context.functions,
        imports: context.imports,
        bindings: ScopedBindings::new(),
        callables: ScopedBindings::new(),
        call_flows: BTreeMap::new(),
        operations: &mut *output.operations,
        candidates: &mut *output.candidates,
        observed: &mut *output.observed,
        container,
    };
    scanner.seed_parameters();
    for event in events {
        match event.kind {
            EventKind::Call => scanner.call(event.node)?,
            EventKind::Macro => scanner.macro_invocation(event.node)?,
            EventKind::MacroDefinition => scanner.macro_definition(event.node)?,
            EventKind::Binding => scanner.binding(event.node)?,
            EventKind::Assignment => scanner.assignment(event.node)?,
            EventKind::Return => scanner.return_escape(event.node)?,
            EventKind::StaticStorage => scanner.static_storage(event.node)?,
        }
    }
    scanner.implicit_callable_escape()?;
    scanner.new_wrapper_candidate()?;
    Ok(())
}

impl Scanner<'_, '_, '_> {
    fn call(&mut self, node: Node<'_>) -> Result<(), WriterScanError> {
        let Some(function) = node.child_by_field_name("function") else {
            return Ok(());
        };
        self.callable_arguments(node)?;
        let function = peel_callable(function);
        let flow = if function.kind() == "field_expression" {
            self.method_call(node, function)?
        } else {
            self.function_call(node, function)?
        };
        self.call_flows
            .insert((node.start_byte(), node.end_byte()), flow);
        Ok(())
    }

    fn function_call(
        &mut self,
        call: Node<'_>,
        function: Node<'_>,
    ) -> Result<FlowValue, WriterScanError> {
        let syntactically_bare = function.kind() == "identifier";
        if let Some(name) = identifier_name(function, self.bytes) {
            match self.callables.local_lookup(function, &name) {
                LocalLookup::Exact(binding) => return self.bound_callable(call, binding),
                LocalLookup::Ambiguous => {
                    self.unknown(
                        call,
                        &name,
                        UnknownSinkReason::AmbiguousAlias,
                        WriterCandidateForm::FunctionCall,
                    )?;
                    self.authority_arguments(call, &name)?;
                    return Ok(FlowValue::from_class(FlowClass::None));
                }
                LocalLookup::Shadowed => {
                    self.authority_arguments(call, &name)?;
                    return Ok(FlowValue::from_class(FlowClass::None));
                }
                LocalLookup::Unbound => {}
            }
        }
        let Some(path) = normalized_path(function, self.bytes) else {
            self.authority_arguments(call, "unresolved_callee")?;
            return Ok(FlowValue::from_class(FlowClass::None));
        };
        let resolution = self.imports.resolve(function, &path, self.registry);
        let resolved = match resolution {
            ImportResolution::Exact(path) => path,
            ImportResolution::Ambiguous | ImportResolution::AmbiguousReexport => {
                self.unknown(
                    call,
                    terminal(&path),
                    UnknownSinkReason::AmbiguousAlias,
                    WriterCandidateForm::FunctionCall,
                )?;
                self.authority_arguments(call, terminal(&path))?;
                return Ok(FlowValue::from_class(FlowClass::None));
            }
            ImportResolution::Wildcard | ImportResolution::WildcardReexport => {
                self.unknown(
                    call,
                    terminal(&path),
                    UnknownSinkReason::WildcardImport,
                    WriterCandidateForm::FunctionCall,
                )?;
                self.authority_arguments(call, terminal(&path))?;
                return Ok(FlowValue::from_class(FlowClass::None));
            }
        };
        let local_item = local_definition_path(call, self.bytes, &resolved);
        if let Some(spec) = self.registry.function(&resolved, self.path, &local_item) {
            let returns = self.return_flow(spec, call, &FlowValue::from_class(FlowClass::None));
            let recorded = RecordedSink::from_spec(spec);
            self.operation(call, recorded, SinkDiscovery::Function);
            return Ok(returns);
        }
        if let Some(returns) = SinkRegistry::reviewed_authority_function(&resolved) {
            return Ok(if returns == FlowClass::FirstArgument {
                self.first_argument_flow(call)
            } else {
                FlowValue::from_class(returns)
            });
        }
        match self
            .functions
            .resolve(call, &resolved, self.imports, self.path)
        {
            functions::FunctionResolution::Exact(parameters) => {
                let name = terminal(&resolved);
                self.local_authority_arguments(call, name, parameters)?;
                return Ok(FlowValue::from_class(FlowClass::None));
            }
            functions::FunctionResolution::Ambiguous => {
                self.unknown(
                    call,
                    terminal(&resolved),
                    UnknownSinkReason::AmbiguousAlias,
                    WriterCandidateForm::FunctionCall,
                )?;
                self.authority_arguments(call, terminal(&resolved))?;
                return Ok(FlowValue::from_class(FlowClass::None));
            }
            functions::FunctionResolution::Absent => {}
        }
        self.authority_arguments(call, terminal(&resolved))?;
        if SinkRegistry::is_reviewed_non_writer_function(&resolved) {
            return Ok(FlowValue::from_class(FlowClass::None));
        }
        let name = terminal(&resolved);
        if self.registry.is_function_candidate(&resolved) {
            self.unknown(
                call,
                name,
                UnknownSinkReason::KnownNamespaceCandidate,
                WriterCandidateForm::FunctionCall,
            )?;
        } else if syntactically_bare && self.registry.has_terminal(name) {
            self.unknown(
                call,
                name,
                UnknownSinkReason::UnresolvedAlias,
                WriterCandidateForm::FunctionCall,
            )?;
        }
        Ok(FlowValue::from_class(FlowClass::None))
    }

    fn bound_callable(
        &mut self,
        call: Node<'_>,
        binding: callable::CallableBinding,
    ) -> Result<FlowValue, WriterScanError> {
        match binding {
            callable::CallableBinding::Registered(path) => {
                let local_item = local_definition_path(call, self.bytes, &path);
                let Some(spec) = self.registry.function(&path, self.path, &local_item) else {
                    self.unknown(
                        call,
                        terminal(&path),
                        UnknownSinkReason::UnresolvedAlias,
                        WriterCandidateForm::FunctionCall,
                    )?;
                    return Ok(FlowValue::from_class(FlowClass::None));
                };
                let returns = self.return_flow(spec, call, &FlowValue::from_class(FlowClass::None));
                let recorded = RecordedSink::from_spec(spec);
                self.operation(call, recorded, SinkDiscovery::Function);
                Ok(returns)
            }
            callable::CallableBinding::Unknown { candidate, reason } => {
                self.unknown(call, &candidate, reason, WriterCandidateForm::FunctionCall)?;
                self.authority_arguments(call, &candidate)?;
                Ok(FlowValue::from_class(FlowClass::None))
            }
        }
    }

    fn callable_arguments(&mut self, call: Node<'_>) -> Result<(), WriterScanError> {
        let Some(arguments) = call.child_by_field_name("arguments") else {
            return Ok(());
        };
        let mut cursor = arguments.walk();
        for argument in arguments.named_children(&mut cursor) {
            let mut bindings = Vec::new();
            callable::collect(
                argument,
                self.bytes,
                self.imports,
                self.registry,
                self.path,
                &self.callables,
                &mut bindings,
            );
            for (node, binding) in bindings {
                match binding {
                    callable::CallableBinding::Registered(path) => {
                        let local_item = local_definition_path(node, self.bytes, &path);
                        let candidate = self
                            .registry
                            .function(&path, self.path, &local_item)
                            .map_or_else(
                                || terminal(&path).to_owned(),
                                |spec| spec.id().as_str().to_owned(),
                            );
                        self.unknown(
                            node,
                            &candidate,
                            UnknownSinkReason::CallableEscape,
                            WriterCandidateForm::CallableEscape,
                        )?;
                    }
                    callable::CallableBinding::Unknown { candidate, reason } => {
                        self.unknown(
                            node,
                            &candidate,
                            reason,
                            WriterCandidateForm::CallableEscape,
                        )?;
                    }
                }
            }
        }
        Ok(())
    }

    fn method_call(
        &mut self,
        call: Node<'_>,
        function: Node<'_>,
    ) -> Result<FlowValue, WriterScanError> {
        let (Some(receiver), Some(field)) = (
            function.child_by_field_name("value"),
            function.child_by_field_name("field"),
        ) else {
            return Ok(FlowValue::from_class(FlowClass::None));
        };
        let Some(name) = canonical_identifier(field, self.bytes) else {
            return Ok(FlowValue::from_class(FlowClass::None));
        };
        let receiver_flow = self.flow_value_of(receiver);
        if receiver_flow.uncertain && receiver_flow.class != FlowClass::None {
            self.authority_arguments(call, &name)?;
            self.unknown(
                call,
                &name,
                UnknownSinkReason::AuthorityMethod,
                WriterCandidateForm::MethodCall,
            )?;
            return Ok(FlowValue::from_class(FlowClass::None));
        }
        if let Some(spec) =
            self.registry
                .method(&name, receiver_flow.class, receiver_flow.receiver.as_ref())
        {
            let returns = self.return_flow(spec, call, &receiver_flow);
            let recorded = RecordedSink::from_spec(spec);
            self.operation(call, recorded, SinkDiscovery::Method);
            return Ok(returns);
        }
        if let Some(returns) = SinkRegistry::reviewed_authority_method(&name, receiver_flow.class) {
            self.authority_arguments(call, &name)?;
            return Ok(if returns == FlowClass::SameReceiver {
                receiver_flow
            } else {
                FlowValue::from_class(returns)
            });
        }
        self.authority_arguments(call, &name)?;
        if receiver_flow.class != FlowClass::None {
            self.unknown(
                call,
                &name,
                UnknownSinkReason::AuthorityMethod,
                WriterCandidateForm::MethodCall,
            )?;
            return Ok(FlowValue::from_class(FlowClass::None));
        }
        if self.registry.has_method_name(&name) {
            let reason = if enclosing_is_generic(call) {
                UnknownSinkReason::GenericReceiver
            } else {
                UnknownSinkReason::DynamicReceiver
            };
            self.unknown(call, &name, reason, WriterCandidateForm::MethodCall)?;
        }
        Ok(FlowValue::from_class(FlowClass::None))
    }
}
