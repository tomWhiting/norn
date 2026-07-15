//! Exact root-level trybuild harness grammar.

use tree_sitter::Node;

use super::super::super::{RustSource, identifier};
use super::super::literal::decode_rust_string;
use super::super::model::{CompileTestExpectation, SourceSpan};
use super::super::scan::{scan_container, span};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum SelectorArgument {
    Literal(String),
    Unsupported,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SelectorObservation {
    pub(super) span: SourceSpan,
    pub(super) expectation: CompileTestExpectation,
    pub(super) argument: SelectorArgument,
}

pub(super) fn scan_selectors(source: &RustSource) -> Vec<SelectorObservation> {
    let Ok(root) = scan_container(source.root_node()) else {
        return Vec::new();
    };
    if !root.inner_attributes.is_empty() || root.items.len() != 1 {
        return Vec::new();
    }
    let item = &root.items[0];
    if item.node.kind() != "function_item"
        || item.attributes.len() != 1
        || !exact_test_attribute(item.attributes[0], source.bytes())
        || !exact_function_signature(item.node, source.bytes())
    {
        return Vec::new();
    }
    let Some(body) = item.node.child_by_field_name("body") else {
        return Vec::new();
    };
    scan_body(body, source.bytes())
}

fn scan_body(body: Node<'_>, bytes: &[u8]) -> Vec<SelectorObservation> {
    let Ok(scanned) = scan_container(body) else {
        return Vec::new();
    };
    if !scanned.inner_attributes.is_empty() || scanned.items.len() < 2 {
        return Vec::new();
    }
    let binding = &scanned.items[0];
    if !binding.attributes.is_empty() {
        return Vec::new();
    }
    let Some(name) = constructor_binding(binding.node, bytes) else {
        return Vec::new();
    };
    let mut selectors = Vec::new();
    for item in &scanned.items[1..] {
        if !item.attributes.is_empty() {
            return Vec::new();
        }
        let Some(selector) = selector_statement(item.node, bytes, &name) else {
            return Vec::new();
        };
        selectors.push(selector);
    }
    selectors
}

fn exact_test_attribute(node: Node<'_>, bytes: &[u8]) -> bool {
    node_text(node, bytes).is_some_and(|text| text.trim() == "#[test]")
}

fn exact_function_signature(node: Node<'_>, bytes: &[u8]) -> bool {
    let Some(parameters) = node.child_by_field_name("parameters") else {
        return false;
    };
    let Some(name) = node.child_by_field_name("name") else {
        return false;
    };
    let Some(body) = node.child_by_field_name("body") else {
        return false;
    };
    let name = identifier_text(name, bytes);
    let header = &bytes[node.start_byte()..body.start_byte()];
    node.named_child_count() == 3
        && parameters.named_child_count() == 0
        && simple_identifier(&name)
        && compact_ascii_whitespace(header) == format!("fn{name}()")
        && node.child_by_field_name("return_type").is_none()
        && node.child_by_field_name("type_parameters").is_none()
}

fn constructor_binding(node: Node<'_>, bytes: &[u8]) -> Option<String> {
    if node.kind() != "let_declaration"
        || node.named_child_count() != 2
        || node.child_by_field_name("type").is_some()
    {
        return None;
    }
    let pattern = node.child_by_field_name("pattern")?;
    let value = node.child_by_field_name("value")?;
    if pattern.kind() != "identifier" || !exact_constructor(value, bytes) {
        return None;
    }
    let name = identifier_text(pattern, bytes);
    let source = &bytes[node.byte_range()];
    (simple_identifier(&name)
        && compact_ascii_whitespace(source) == format!("let{name}=trybuild::TestCases::new();"))
    .then_some(name)
}

fn selector_statement(node: Node<'_>, bytes: &[u8], binding: &str) -> Option<SelectorObservation> {
    if node.kind() != "expression_statement" || node.named_child_count() != 1 {
        return None;
    }
    let call = node.named_child(0)?;
    if call.kind() != "call_expression" {
        return None;
    }
    let function = call.child_by_field_name("function")?;
    if call.named_child_count() != 2
        || function.kind() != "field_expression"
        || function.named_child_count() != 2
    {
        return None;
    }
    let receiver = function.child_by_field_name("value")?;
    let method = function.child_by_field_name("field")?;
    if receiver.kind() != "identifier" || identifier_text(receiver, bytes) != binding {
        return None;
    }
    let expectation = match identifier_text(method, bytes).as_str() {
        "compile_fail" => CompileTestExpectation::CompileFail,
        "pass" => CompileTestExpectation::Pass,
        _ => return None,
    };
    let arguments = call.child_by_field_name("arguments")?;
    let mut cursor = arguments.walk();
    let mut children = arguments.named_children(&mut cursor);
    let argument = match (children.next(), children.next()) {
        (Some(argument), None) => node_text(argument, bytes)
            .and_then(decode_rust_string)
            .map_or(SelectorArgument::Unsupported, SelectorArgument::Literal),
        _ => SelectorArgument::Unsupported,
    };
    Some(SelectorObservation {
        span: span(call),
        expectation,
        argument,
    })
}

fn exact_constructor(node: Node<'_>, bytes: &[u8]) -> bool {
    if node.kind() != "call_expression" || node.named_child_count() != 2 {
        return false;
    }
    let Some(function) = node.child_by_field_name("function") else {
        return false;
    };
    let Some(arguments) = node.child_by_field_name("arguments") else {
        return false;
    };
    let mut path = Vec::new();
    scoped_identifiers(function, bytes, &mut path)
        && path == ["trybuild", "TestCases", "new"]
        && arguments.named_child_count() == 0
}

fn scoped_identifiers(node: Node<'_>, bytes: &[u8], output: &mut Vec<String>) -> bool {
    if matches!(node.kind(), "identifier" | "type_identifier") {
        output.push(identifier_text(node, bytes));
        return true;
    }
    if node.kind() != "scoped_identifier" || node.named_child_count() != 2 {
        return false;
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .all(|child| scoped_identifiers(child, bytes, output))
}

fn identifier_text(node: Node<'_>, bytes: &[u8]) -> String {
    let Some(text) = node_text(node, bytes) else {
        return String::new();
    };
    identifier::canonical_text(text).to_owned()
}

fn simple_identifier(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn compact_ascii_whitespace(bytes: &[u8]) -> String {
    bytes
        .iter()
        .filter(|byte| !byte.is_ascii_whitespace())
        .map(|byte| char::from(*byte))
        .collect()
}

fn node_text<'bytes>(node: Node<'_>, bytes: &'bytes [u8]) -> Option<&'bytes str> {
    let Ok(text) = std::str::from_utf8(&bytes[node.byte_range()]) else {
        return None;
    };
    Some(text)
}
