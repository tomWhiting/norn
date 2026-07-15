//! Heap-backed evaluation of nested `cfg_attr` metadata.

use std::collections::BTreeMap;

use super::{RustSourceError, combine};
use crate::rust::identifier;
use crate::rust::{CfgTruth, evaluate_cfg};

type Range = (usize, usize);

enum Task {
    Evaluate(Range),
    Finish {
        condition: CfgTruth,
        generated: usize,
    },
}

pub(super) fn meta_truth(meta: &str, offset: usize) -> Result<CfgTruth, RustSourceError> {
    let index = MetaIndex::new(meta).ok_or(RustSourceError::Attribute { offset })?;
    let mut tasks = vec![Task::Evaluate((0, meta.len()))];
    let mut values = Vec::new();
    while let Some(task) = tasks.pop() {
        match task {
            Task::Evaluate(range) => {
                if let Some(body) = index.invocation(range, "cfg") {
                    values.push(evaluate_cfg(index.text(body))?);
                    continue;
                }
                let Some(body) = index.invocation(range, "cfg_attr") else {
                    let text = index.text(index.trim(range));
                    if has_meta_name(text, "cfg") || has_meta_name(text, "cfg_attr") {
                        return Err(RustSourceError::Attribute { offset });
                    }
                    values.push(CfgTruth::True);
                    continue;
                };
                let parts = index
                    .parts(body)
                    .ok_or(RustSourceError::Attribute { offset })?;
                let Some((condition, generated)) = parts.split_first() else {
                    return Err(RustSourceError::Attribute { offset });
                };
                if generated.is_empty() {
                    return Err(RustSourceError::Attribute { offset });
                }
                let condition = evaluate_cfg(index.text(*condition))?;
                tasks.push(Task::Finish {
                    condition,
                    generated: generated.len(),
                });
                tasks.extend(generated.iter().rev().copied().map(Task::Evaluate));
            }
            Task::Finish {
                condition,
                generated,
            } => {
                let start = values
                    .len()
                    .checked_sub(generated)
                    .ok_or(RustSourceError::Attribute { offset })?;
                let generated_truth = values[start..]
                    .iter()
                    .copied()
                    .fold(CfgTruth::True, combine);
                values.truncate(start);
                values.push(match condition {
                    CfgTruth::False => CfgTruth::True,
                    CfgTruth::True => generated_truth,
                    CfgTruth::Possible if generated_truth == CfgTruth::True => CfgTruth::True,
                    CfgTruth::Possible => CfgTruth::Possible,
                });
            }
        }
    }
    let [truth] = values.as_slice() else {
        return Err(RustSourceError::Attribute { offset });
    };
    Ok(*truth)
}

struct MetaIndex<'a> {
    input: &'a str,
    matching: BTreeMap<usize, usize>,
    commas: BTreeMap<usize, Vec<usize>>,
}

impl<'a> MetaIndex<'a> {
    fn new(input: &'a str) -> Option<Self> {
        let bytes = input.as_bytes();
        let mut matching = BTreeMap::new();
        let mut commas: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        let mut delimiters = Vec::new();
        let mut cursor = 0;
        let mut in_string = false;
        while cursor < bytes.len() {
            let byte = bytes[cursor];
            if in_string {
                match byte {
                    b'\\' => cursor = cursor.checked_add(2)?,
                    b'"' => {
                        in_string = false;
                        cursor += 1;
                    }
                    _ => cursor += 1,
                }
                continue;
            }
            match byte {
                b'"' => in_string = true,
                b'(' | b'[' | b'{' => delimiters.push((cursor, byte)),
                b')' | b']' | b'}' => {
                    let (open, opener) = delimiters.pop()?;
                    if !matching_pair(opener, byte) {
                        return None;
                    }
                    matching.insert(open, cursor);
                }
                b',' => {
                    if let Some((open, _)) = delimiters.last() {
                        commas.entry(*open).or_default().push(cursor);
                    }
                }
                _ => {}
            }
            cursor += 1;
        }
        if in_string || !delimiters.is_empty() {
            return None;
        }
        Some(Self {
            input,
            matching,
            commas,
        })
    }

    fn invocation(&self, range: Range, name: &str) -> Option<Range> {
        let range = self.trim(range);
        let text = self.text(range);
        let remainder = identifier::name_remainder(text, name)?;
        let consumed = text.len().checked_sub(remainder.len())?;
        let whitespace = remainder.len().checked_sub(remainder.trim_start().len())?;
        let open = range.0.checked_add(consumed)?.checked_add(whitespace)?;
        if self.input.as_bytes().get(open) != Some(&b'(') {
            return None;
        }
        let close = *self.matching.get(&open)?;
        (close + 1 == range.1).then_some((open + 1, close))
    }

    fn parts(&self, body: Range) -> Option<Vec<Range>> {
        let open = body.0.checked_sub(1)?;
        let mut parts = Vec::new();
        let mut start = body.0;
        for comma in self.commas.get(&open).into_iter().flatten() {
            if *comma < body.1 {
                let part = self.trim((start, *comma));
                if part.0 == part.1 {
                    return None;
                }
                parts.push(part);
                start = comma + 1;
            }
        }
        let part = self.trim((start, body.1));
        if part.0 == part.1 {
            return None;
        }
        parts.push(part);
        Some(parts)
    }

    fn trim(&self, (mut start, mut end): Range) -> Range {
        let bytes = self.input.as_bytes();
        while start < end && bytes[start].is_ascii_whitespace() {
            start += 1;
        }
        while start < end && bytes[end - 1].is_ascii_whitespace() {
            end -= 1;
        }
        (start, end)
    }

    fn text(&self, range: Range) -> &'a str {
        &self.input[range.0..range.1]
    }
}

const fn matching_pair(open: u8, close: u8) -> bool {
    matches!((open, close), (b'(', b')') | (b'[', b']') | (b'{', b'}'))
}

fn has_meta_name(meta: &str, name: &str) -> bool {
    identifier::name_remainder(meta, name).is_some()
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use crate::rust::CfgTruth;

    use super::meta_truth;

    #[test]
    fn nested_cfg_attr_uses_heap_at_twenty_thousand_levels() -> Result<(), Box<dyn Error>> {
        const DEPTH: usize = 20_000;

        let mut meta = "cfg_attr(any(),".repeat(DEPTH);
        meta.push_str("cfg(test)");
        meta.extend(std::iter::repeat_n(')', DEPTH));
        assert_eq!(meta_truth(&meta, 0)?, CfgTruth::True);
        Ok(())
    }
}
