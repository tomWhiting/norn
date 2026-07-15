//! Strict Rust attribute metadata used by debt analysis.

mod formula;
mod parser;

use crate::debt::model::{DebtConstructKind, DebtScanError};

use self::{formula::is_impossible, parser::MetaParser};
use super::meta_lex::lex;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AttributeDebt {
    pub(super) construct: DebtConstructKind,
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) normalized: Vec<u8>,
}

pub(super) fn analyze_attribute(text: &str) -> Result<Vec<AttributeDebt>, DebtScanError> {
    let (body, body_offset) = attribute_body(text).ok_or(DebtScanError::Attribute { offset: 0 })?;
    if !has_relevant_root(body) {
        return Ok(Vec::new());
    }
    let tokens = lex(body).map_err(|offset| DebtScanError::Attribute {
        offset: body_offset + offset,
    })?;
    let mut parser = MetaParser::new(&tokens);
    let mut meta = parser
        .parse_meta()
        .map_err(|offset| DebtScanError::Attribute {
            offset: body_offset + offset,
        })?;
    if !parser.is_finished() {
        return Err(DebtScanError::Attribute {
            offset: body_offset + parser.offset(),
        });
    }
    meta.shift(body_offset);
    let mut findings = Vec::new();
    inspect_meta(&meta, &mut findings)?;
    Ok(findings)
}

fn inspect_meta(tree: &MetaTree, findings: &mut Vec<AttributeDebt>) -> Result<(), DebtScanError> {
    let mut pending = vec![tree.root];
    while let Some(meta_id) = pending.pop() {
        let meta = tree.node(meta_id)?;
        let Some(name) = meta.simple_name() else {
            continue;
        };
        let construct = match name {
            "allow" => Some(DebtConstructKind::AllowAttribute),
            "expect" => Some(DebtConstructKind::ExpectAttribute),
            "ignore" => Some(DebtConstructKind::IgnoreAttribute),
            _ => None,
        };
        if let Some(construct) = construct {
            findings.push(AttributeDebt {
                construct,
                start: meta.start,
                end: meta.end,
                normalized: tree.normalized(meta_id)?,
            });
        }
        match (name, &meta.form) {
            ("cfg", MetaForm::List(parts)) if parts.len() == 1 => {
                if is_impossible(tree, parts[0], meta.start)? {
                    findings.push(AttributeDebt {
                        construct: DebtConstructKind::ImpossibleCfg,
                        start: meta.start,
                        end: meta.end,
                        normalized: tree.normalized(meta_id)?,
                    });
                }
            }
            ("cfg_attr", MetaForm::List(parts)) if parts.len() >= 2 => {
                let condition_id = parts[0];
                let condition = tree.node(condition_id)?;
                if is_impossible(tree, condition_id, meta.start)? {
                    findings.push(AttributeDebt {
                        construct: DebtConstructKind::ImpossibleCfg,
                        start: condition.start,
                        end: condition.end,
                        normalized: tree.normalized(condition_id)?,
                    });
                }
                pending.extend(parts[1..].iter().rev().copied());
            }
            ("cfg" | "cfg_attr", _) => {
                return Err(DebtScanError::Attribute { offset: meta.start });
            }
            _ => {}
        }
    }
    Ok(())
}

fn attribute_body(text: &str) -> Option<(&str, usize)> {
    let trimmed_start = text.len() - text.trim_start().len();
    let trimmed = text.trim();
    let (body, prefix) = if let Some(body) = trimmed.strip_prefix("#![") {
        (body, 3)
    } else {
        (trimmed.strip_prefix("#[")?, 2)
    };
    let body = body.strip_suffix(']')?;
    let leading = body.len() - body.trim_start().len();
    Some((body.trim(), trimmed_start + prefix + leading))
}

fn has_relevant_root(body: &str) -> bool {
    let trimmed = body.trim_start();
    let canonical = trimmed.strip_prefix("r#").unwrap_or(trimmed);
    let root: String = canonical
        .chars()
        .take_while(|character| character.is_ascii_alphanumeric() || *character == '_')
        .collect();
    matches!(
        root.as_str(),
        "allow" | "expect" | "ignore" | "cfg" | "cfg_attr"
    )
}

type MetaId = usize;

#[derive(Debug, Eq, PartialEq)]
struct MetaTree {
    nodes: Vec<Meta>,
    root: MetaId,
}

impl MetaTree {
    fn node(&self, meta_id: MetaId) -> Result<&Meta, DebtScanError> {
        self.nodes
            .get(meta_id)
            .ok_or(DebtScanError::Attribute { offset: 0 })
    }

    fn shift(&mut self, amount: usize) {
        for meta in &mut self.nodes {
            meta.start += amount;
            meta.end += amount;
        }
    }

    fn normalized(&self, meta_id: MetaId) -> Result<Vec<u8>, DebtScanError> {
        let mut output = Vec::new();
        let mut pending = vec![NormalizationAction::Meta(meta_id)];
        while let Some(action) = pending.pop() {
            match action {
                NormalizationAction::Byte(byte) => output.push(byte),
                NormalizationAction::Meta(current_id) => {
                    let meta = self.node(current_id)?;
                    for (index, segment) in meta.path.iter().enumerate() {
                        if index != 0 {
                            output.extend_from_slice(b"::");
                        }
                        output.extend_from_slice(segment.as_bytes());
                    }
                    match &meta.form {
                        MetaForm::Word => {}
                        MetaForm::Equals(value) => {
                            output.push(b'=');
                            output.extend_from_slice(value);
                        }
                        MetaForm::List(parts) => {
                            output.push(b'(');
                            pending.push(NormalizationAction::Byte(b')'));
                            for (index, part) in parts.iter().enumerate().rev() {
                                pending.push(NormalizationAction::Meta(*part));
                                if index != 0 {
                                    pending.push(NormalizationAction::Byte(b','));
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(output)
    }
}

#[derive(Clone, Copy)]
enum NormalizationAction {
    Meta(MetaId),
    Byte(u8),
}

#[derive(Debug, Eq, PartialEq)]
struct Meta {
    path: Vec<String>,
    start: usize,
    end: usize,
    form: MetaForm,
}

impl Meta {
    fn simple_name(&self) -> Option<&str> {
        match self.path.as_slice() {
            [name] => Some(name.as_str()),
            _ => None,
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
enum MetaForm {
    Word,
    Equals(Vec<u8>),
    List(Vec<MetaId>),
}

#[cfg(test)]
#[path = "meta/meta_tests.rs"]
mod tests;
