//! Conditional item reachability and path-attribute branch expansion.

use std::collections::BTreeSet;

use tree_sitter::Node;

use super::super::{CfgTruth, evaluate_cfg, identifier};
use super::literal::decode_rust_string;
use super::model::ModuleDiagnosticCode;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AnalysisMode {
    Production,
    Test,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AttributePlan {
    pub(super) paths: BTreeSet<Option<String>>,
}

impl AttributePlan {
    pub(super) fn is_reachable(&self) -> bool {
        !self.paths.is_empty()
    }

    pub(super) fn is_default_path(&self) -> bool {
        self.paths.len() == 1 && self.paths.contains(&None)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct AttributeFailure {
    pub(super) code: ModuleDiagnosticCode,
    pub(super) offset: usize,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Branch {
    path: Option<String>,
    has_path: bool,
}

pub(super) fn plan(
    attributes: &[Node<'_>],
    bytes: &[u8],
    mode: AnalysisMode,
) -> Result<AttributePlan, AttributeFailure> {
    let mut branches = vec![Branch {
        path: None,
        has_path: false,
    }];
    for attribute in attributes {
        let text = node_text(*attribute, bytes)?;
        let meta = strip_attribute(text).ok_or(AttributeFailure {
            code: ModuleDiagnosticCode::AttributeUnsupported,
            offset: attribute.start_byte(),
        })?;
        apply_meta(&mut branches, meta, attribute.start_byte(), mode)?;
        branches.sort();
        branches.dedup();
    }
    Ok(AttributePlan {
        paths: branches.into_iter().map(|branch| branch.path).collect(),
    })
}

fn apply_meta(
    branches: &mut Vec<Branch>,
    meta: &str,
    offset: usize,
    mode: AnalysisMode,
) -> Result<(), AttributeFailure> {
    if has_meta_name(meta, "cfg") {
        let predicate = invocation_body(meta, "cfg").ok_or(AttributeFailure {
            code: ModuleDiagnosticCode::CfgUnsupported,
            offset,
        })?;
        if evaluate(predicate, mode, offset)? == CfgTruth::False {
            branches.clear();
        }
        return Ok(());
    }
    if has_meta_name(meta, "cfg_attr") {
        let arguments = invocation_body(meta, "cfg_attr").ok_or(AttributeFailure {
            code: ModuleDiagnosticCode::CfgUnsupported,
            offset,
        })?;
        let parts = split_top_level(arguments).ok_or(AttributeFailure {
            code: ModuleDiagnosticCode::CfgUnsupported,
            offset,
        })?;
        let Some((condition, generated)) = parts.split_first() else {
            return Err(AttributeFailure {
                code: ModuleDiagnosticCode::CfgUnsupported,
                offset,
            });
        };
        if generated.is_empty() {
            return Err(AttributeFailure {
                code: ModuleDiagnosticCode::CfgUnsupported,
                offset,
            });
        }
        apply_cfg_attr(branches, condition, generated, offset, mode)?;
        return Ok(());
    }
    if has_meta_name(meta, "path") {
        apply_path(branches, meta, offset)?;
    }
    Ok(())
}

fn apply_cfg_attr(
    branches: &mut Vec<Branch>,
    condition: &str,
    generated: &[&str],
    offset: usize,
    mode: AnalysisMode,
) -> Result<(), AttributeFailure> {
    let condition = evaluate(condition, mode, offset)?;
    if condition == CfgTruth::False {
        return Ok(());
    }
    let original = branches.clone();
    let mut applied = original.clone();
    for nested in generated {
        apply_meta(&mut applied, nested, offset, mode)?;
    }
    *branches = match condition {
        CfgTruth::False => original,
        CfgTruth::True => applied,
        CfgTruth::Possible => original.into_iter().chain(applied).collect(),
    };
    Ok(())
}

fn apply_path(branches: &mut [Branch], meta: &str, offset: usize) -> Result<(), AttributeFailure> {
    let Some(remainder) = identifier::name_remainder(meta, "path") else {
        return Err(AttributeFailure {
            code: ModuleDiagnosticCode::PathNonliteral,
            offset,
        });
    };
    let Some(raw) = remainder.trim_start().strip_prefix('=') else {
        return Err(AttributeFailure {
            code: ModuleDiagnosticCode::PathNonliteral,
            offset,
        });
    };
    let path = decode_rust_string(raw).ok_or(AttributeFailure {
        code: ModuleDiagnosticCode::PathNonliteral,
        offset,
    })?;
    for branch in branches {
        if branch.has_path {
            return Err(AttributeFailure {
                code: ModuleDiagnosticCode::PathConflict,
                offset,
            });
        }
        branch.path = Some(path.clone());
        branch.has_path = true;
    }
    Ok(())
}

fn evaluate(
    predicate: &str,
    mode: AnalysisMode,
    offset: usize,
) -> Result<CfgTruth, AttributeFailure> {
    let owned;
    let predicate = match mode {
        AnalysisMode::Production => predicate,
        AnalysisMode::Test => {
            owned = replace_test_predicates(predicate);
            &owned
        }
    };
    let Ok(value) = evaluate_cfg(predicate) else {
        return Err(AttributeFailure {
            code: ModuleDiagnosticCode::CfgUnsupported,
            offset,
        });
    };
    Ok(value)
}

fn replace_test_predicates(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] == b'"' {
            let end = string_end(bytes, cursor);
            output.push_str(&input[cursor..end]);
            cursor = end;
            continue;
        }
        if is_identifier_start(bytes[cursor]) {
            let start = cursor;
            let Some((token, end)) = identifier::token_at(input, cursor) else {
                output.push(char::from(bytes[cursor]));
                cursor += 1;
                continue;
            };
            cursor = end;
            let next = input[cursor..].trim_start().as_bytes().first().copied();
            if token == "test" && !matches!(next, Some(b'=' | b'(')) {
                output.push_str("all()");
            } else {
                output.push_str(&input[start..cursor]);
            }
            continue;
        }
        output.push(char::from(bytes[cursor]));
        cursor += 1;
    }
    output
}

fn string_end(bytes: &[u8], start: usize) -> usize {
    let mut cursor = start + 1;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'\\' => cursor = (cursor + 2).min(bytes.len()),
            b'"' => return cursor + 1,
            _ => cursor += 1,
        }
    }
    bytes.len()
}

fn node_text<'a>(node: Node<'_>, bytes: &'a [u8]) -> Result<&'a str, AttributeFailure> {
    let Ok(text) = std::str::from_utf8(&bytes[node.byte_range()]) else {
        return Err(AttributeFailure {
            code: ModuleDiagnosticCode::SourceNotUtf8,
            offset: node.start_byte(),
        });
    };
    Ok(text)
}

fn strip_attribute(attribute: &str) -> Option<&str> {
    let trimmed = attribute.trim();
    let body = trimmed
        .strip_prefix("#![")
        .or_else(|| trimmed.strip_prefix("#["))?;
    body.strip_suffix(']').map(str::trim)
}

fn has_meta_name(meta: &str, name: &str) -> bool {
    identifier::name_remainder(meta, name).is_some()
}

fn invocation_body<'a>(meta: &'a str, name: &str) -> Option<&'a str> {
    let remainder = identifier::name_remainder(meta, name)?.trim_start();
    remainder
        .strip_prefix('(')?
        .strip_suffix(')')
        .map(str::trim)
}

fn split_top_level(value: &str) -> Option<Vec<&str>> {
    let mut depth = 0_u32;
    let mut in_string = false;
    let mut escaped = false;
    let mut start = 0;
    let mut parts = Vec::new();
    for (index, character) in value.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        match character {
            '"' => in_string = true,
            '(' | '[' | '{' => depth = depth.checked_add(1)?,
            ')' | ']' | '}' => depth = depth.checked_sub(1)?,
            ',' if depth == 0 => {
                parts.push(value[start..index].trim());
                start = index + 1;
            }
            _ => {}
        }
    }
    if in_string || depth != 0 {
        return None;
    }
    parts.push(value[start..].trim());
    (!parts.iter().any(|part| part.is_empty())).then_some(parts)
}

const fn is_identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}
