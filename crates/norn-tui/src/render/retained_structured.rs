//! Direct assistant-field rendering with provenance back to the original JSON bytes.

use std::collections::HashMap;
use std::ops::Range;

use serde_json::{Map, Value};

use super::retained_markdown::{
    MarkdownError, RenderedMarkdown, SourceDisplaySpan, SourceMapping, render_markdown,
    render_plain,
};
use super::retained_text::{
    StyleSpan, StyledText, TextAttribute, TextAttributes, TextError, TextStyle,
};
use super::syntax::SyntaxHighlighter;

/// Structured adapters refuse inconsistent offsets without quoting body contents.
#[derive(Debug, thiserror::Error)]
pub enum StructuredError {
    /// Validated JSON and its source-token map contradicted one another.
    #[error("structured source mapping is inconsistent at original range {range:?}")]
    Mapping {
        /// Original source range involved in the contradiction.
        range: Range<usize>,
    },
    /// A token failed decoding after the containing JSON had been validated.
    #[error("structured token decoding failed at original byte {offset}")]
    Decode {
        /// Original token start, never a decoded/display offset.
        offset: usize,
        /// Original decoder failure retained for diagnostics.
        source: serde_json::Error,
    },
    /// Direct Markdown parsing or mapping failed.
    #[error(transparent)]
    Markdown(#[from] MarkdownError),
    /// Final styled text failed safety/range validation.
    #[error(transparent)]
    Text(#[from] TextError),
}

/// Render the existing assistant primary/secondary-field convention directly.
///
/// Complete objects with multiple fields select text/content/written/response,
/// then the first string, then the first key in the actual JSON map order.
/// Decoded string fields retain Markdown styles; nonstrings show their faithful
/// original JSON token. Secondary separators are generated and have no source.
/// All mappings refer to this supplied original body, never a synthesized body.
///
/// Other complete values and ordinary prose keep their original Markdown view.
/// Partial/invalid text with an object key and colon stays original plain text
/// beneath an explicit JSON-like diagnostic. This is syntax evidence, not a
/// validated output contract. Other incomplete input, including bracket-led
/// Markdown links and lists, retains Markdown; no field is invented from it.
/// The caller supplies validated UTF-8 and tags the result with its exact body
/// revision and visibility preference; this function owns no I/O or cache.
///
/// # Errors
/// Returns located token/mapping failures or direct Markdown/style errors.
pub fn render_structured(
    original: &str,
    show_secondary: bool,
    highlighter: &SyntaxHighlighter,
) -> Result<RenderedMarkdown, StructuredError> {
    if !original.trim_start().starts_with('{') {
        return Ok(render_markdown(original, highlighter)?);
    }
    let value: Value = match serde_json::from_str(original) {
        Ok(value) => value,
        Err(error) => {
            if !has_object_field_prefix(original) {
                return Ok(render_markdown(original, highlighter)?);
            }
            let label = if error.is_eof() {
                "Partial JSON-like text — original text\n".to_owned()
            } else {
                format!(
                    "Invalid JSON-like text at line {}, column {} — original text\n",
                    error.line(),
                    error.column()
                )
            };
            let mut output = Output::new();
            output.generated(&label)?;
            output.append(render_plain(original)?)?;
            return output.finish();
        }
    };
    let Value::Object(map) = value else {
        return Ok(render_markdown(original, highlighter)?);
    };
    if map.len() < 2 {
        return Ok(render_markdown(original, highlighter)?);
    }
    let tokens = object_tokens(original)?;
    let primary = primary_key(&map).ok_or_else(|| inconsistent(0..original.len()))?;
    let mut output = Output::new();
    let primary_value = map
        .get(primary)
        .ok_or_else(|| inconsistent(0..original.len()))?;
    output.field(original, &tokens, primary, primary_value, highlighter)?;
    if show_secondary {
        for (key, value) in &map {
            if key != primary {
                output.generated(&format!("─── {key} ───\n"))?;
                output.field(original, &tokens, key, value, highlighter)?;
            }
        }
    }
    output.finish()
}

fn primary_key(map: &Map<String, Value>) -> Option<&str> {
    ["text", "content", "written", "response"]
        .into_iter()
        .find(|key| map.contains_key(*key))
        .or_else(|| {
            map.iter()
                .find(|(_, value)| value.is_string())
                .map(|(key, _)| key.as_str())
        })
        .or_else(|| map.keys().next().map(String::as_str))
}

struct Output {
    text: String,
    styles: Vec<StyleSpan>,
    spans: Vec<SourceDisplaySpan>,
}

impl Output {
    fn new() -> Self {
        Self {
            text: String::new(),
            styles: Vec::new(),
            spans: Vec::new(),
        }
    }

    fn append(&mut self, rendered: RenderedMarkdown) -> Result<(), StructuredError> {
        let offset = self.text.len();
        self.text.push_str(rendered.styled.text());
        for style in rendered.styled.spans() {
            self.styles.push(StyleSpan {
                range: shift(&style.range, offset)?,
                style: style.style,
            });
        }
        for span in rendered.spans {
            self.spans.push(SourceDisplaySpan {
                display: shift(&span.display, offset)?,
                source: span.source,
            });
        }
        Ok(())
    }

    fn generated(&mut self, text: &str) -> Result<(), StructuredError> {
        let mut rendered = render_plain(text)?;
        for span in &mut rendered.spans {
            span.source = SourceMapping::Generated;
        }
        let text = rendered.styled.text().to_owned();
        let styles = if text.is_empty() {
            Vec::new()
        } else {
            vec![StyleSpan {
                range: 0..text.len(),
                style: TextStyle {
                    attributes: TextAttributes::default().with(TextAttribute::Dim),
                    ..TextStyle::default()
                },
            }]
        };
        rendered.styled = StyledText::new(text, styles)?;
        self.append(rendered)
    }

    fn field(
        &mut self,
        original: &str,
        tokens: &HashMap<String, Range<usize>>,
        key: &str,
        value: &Value,
        highlighter: &SyntaxHighlighter,
    ) -> Result<(), StructuredError> {
        let range = tokens
            .get(key)
            .ok_or_else(|| inconsistent(0..original.len()))?;
        let rendered = if let Value::String(expected) = value {
            let decoded = decode_string(original, range.clone())?;
            if decoded.text != *expected {
                return Err(inconsistent(range.clone()));
            }
            let rendered = render_markdown(&decoded.text, highlighter)?;
            compose(rendered, &decoded.runs)?
        } else {
            let token = original
                .get(range.clone())
                .ok_or_else(|| inconsistent(range.clone()))?;
            let mut rendered = render_plain(token)?;
            for span in &mut rendered.spans {
                match &mut span.source {
                    SourceMapping::Exact { original } | SourceMapping::Transformed { original } => {
                        *original = shift(original, range.start)?;
                    }
                    SourceMapping::Generated => {}
                }
            }
            rendered
        };
        self.append(rendered)?;
        if !self.text.ends_with('\n') {
            self.generated("\n")?;
        }
        Ok(())
    }

    fn finish(self) -> Result<RenderedMarkdown, StructuredError> {
        Ok(RenderedMarkdown {
            styled: StyledText::new(self.text, self.styles)?,
            spans: self.spans,
        })
    }
}

/// Find top-level tokens after serde has validated syntax, retaining last duplicate values.
fn object_tokens(original: &str) -> Result<HashMap<String, Range<usize>>, StructuredError> {
    let mut tokens = HashMap::new();
    let bytes = original.as_bytes();
    let mut position = whitespace(bytes, 0);
    if bytes.get(position) != Some(&b'{') {
        return Err(inconsistent(position..position));
    }
    position += 1;
    loop {
        position = whitespace(bytes, position);
        if bytes.get(position) == Some(&b'}') {
            break;
        }
        let key_start = position;
        let key_end = string_end(bytes, key_start)?;
        let key = serde_json::from_str(&original[key_start..key_end]).map_err(|source| {
            StructuredError::Decode {
                offset: key_start,
                source,
            }
        })?;
        position = whitespace(bytes, key_end);
        if bytes.get(position) != Some(&b':') {
            return Err(inconsistent(position..position));
        }
        let start = whitespace(bytes, position + 1);
        let end = value_end(bytes, start)?;
        tokens.insert(key, start..end);
        position = whitespace(bytes, end);
        match bytes.get(position) {
            Some(b',') => position += 1,
            Some(b'}') => break,
            _ => return Err(inconsistent(position..position)),
        }
    }
    Ok(tokens)
}

fn whitespace(bytes: &[u8], mut position: usize) -> usize {
    while bytes.get(position).is_some_and(u8::is_ascii_whitespace) {
        position += 1;
    }
    position
}

fn string_end(bytes: &[u8], start: usize) -> Result<usize, StructuredError> {
    quoted_end(bytes, start).ok_or_else(|| inconsistent(start..bytes.len()))
}

/// Recognize only a quoted object field followed by a colon, never a contract.
fn has_object_field_prefix(original: &str) -> bool {
    let bytes = original.as_bytes();
    let start = whitespace(bytes, 0);
    if bytes.get(start) != Some(&b'{') {
        return false;
    }
    let key_start = whitespace(bytes, start + 1);
    quoted_end(bytes, key_start).is_some_and(|end| bytes.get(whitespace(bytes, end)) == Some(&b':'))
}

/// Locate closing quotes while treating escaped quotes as part of the string.
fn quoted_end(bytes: &[u8], start: usize) -> Option<usize> {
    if bytes.get(start) != Some(&b'"') {
        return None;
    }
    let mut position = start + 1;
    while let Some(byte) = bytes.get(position) {
        match byte {
            b'"' => return Some(position + 1),
            b'\\' => position += 2,
            _ => position += 1,
        }
    }
    None
}

fn value_end(bytes: &[u8], start: usize) -> Result<usize, StructuredError> {
    if bytes.get(start) == Some(&b'"') {
        return string_end(bytes, start);
    }
    let mut position = start;
    let mut depth = 0usize;
    while let Some(byte) = bytes.get(position) {
        match byte {
            b'"' => {
                position = string_end(bytes, position)?;
                continue;
            }
            b'{' | b'[' => depth += 1,
            b'}' | b']' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    return Ok(position + 1);
                }
            }
            b'}' | b',' if depth == 0 => break,
            byte if depth == 0 && byte.is_ascii_whitespace() => break,
            _ => {}
        }
        position += 1;
    }
    if position == start {
        Err(inconsistent(start..position))
    } else {
        Ok(position)
    }
}

struct Decoded {
    text: String,
    runs: Vec<DecodeRun>,
}

struct DecodeRun {
    decoded: Range<usize>,
    original: Range<usize>,
    exact: bool,
}

fn decode_string(original: &str, range: Range<usize>) -> Result<Decoded, StructuredError> {
    let bytes = original.as_bytes();
    let end = range
        .end
        .checked_sub(1)
        .ok_or_else(|| inconsistent(range.clone()))?;
    let mut position = range.start + 1;
    let mut decoded = Decoded {
        text: String::new(),
        runs: Vec::new(),
    };
    while position < end {
        let start = position;
        let display_start = decoded.text.len();
        let exact = bytes[position] != b'\\';
        if exact {
            position = original[position..end]
                .find('\\')
                .map_or(end, |relative| start + relative);
            decoded.text.push_str(&original[start..position]);
        } else {
            position += if bytes.get(position + 1) == Some(&b'u') {
                6
            } else {
                2
            };
            // A high-surrogate escape and its required low surrogate encode one scalar.
            if bytes.get(start + 1) == Some(&b'u')
                && matches!(bytes.get(start + 2), Some(b'd' | b'D'))
                && matches!(
                    bytes.get(start + 3),
                    Some(b'8' | b'9' | b'a'..=b'b' | b'A'..=b'B')
                )
            {
                position += 6;
            }
            let raw = original
                .get(start..position)
                .ok_or_else(|| inconsistent(start..position))?;
            let scalar: String = serde_json::from_str(&format!("\"{raw}\"")).map_err(|source| {
                StructuredError::Decode {
                    offset: start,
                    source,
                }
            })?;
            decoded.text.push_str(&scalar);
        }
        decoded.runs.push(DecodeRun {
            decoded: display_start..decoded.text.len(),
            original: start..position,
            exact,
        });
    }
    if position != end {
        return Err(inconsistent(range));
    }
    Ok(decoded)
}

fn compose(
    mut rendered: RenderedMarkdown,
    runs: &[DecodeRun],
) -> Result<RenderedMarkdown, StructuredError> {
    let mut mapped = Vec::new();
    for span in rendered.spans {
        match span.source {
            SourceMapping::Generated => mapped.push(SourceDisplaySpan {
                display: span.display,
                source: SourceMapping::Generated,
            }),
            SourceMapping::Exact { original } => {
                for (run, overlap) in overlapping(runs, &original)? {
                    let display = shift(
                        &(overlap.start - original.start..overlap.end - original.start),
                        span.display.start,
                    )?;
                    let source = if run.exact {
                        SourceMapping::Exact {
                            original: run_original(run, &overlap)?,
                        }
                    } else {
                        SourceMapping::Transformed {
                            original: run.original.clone(),
                        }
                    };
                    mapped.push(SourceDisplaySpan { display, source });
                }
            }
            SourceMapping::Transformed { original } => {
                let overlaps = overlapping(runs, &original)?;
                let first = overlaps
                    .first()
                    .ok_or_else(|| inconsistent(original.clone()))?;
                let last = overlaps
                    .last()
                    .ok_or_else(|| inconsistent(original.clone()))?;
                let raw =
                    run_original(first.0, &first.1)?.start..run_original(last.0, &last.1)?.end;
                mapped.push(SourceDisplaySpan {
                    display: span.display,
                    source: SourceMapping::Transformed { original: raw },
                });
            }
        }
    }
    rendered.spans = mapped;
    Ok(rendered)
}

fn overlapping<'a>(
    runs: &'a [DecodeRun],
    range: &Range<usize>,
) -> Result<Vec<(&'a DecodeRun, Range<usize>)>, StructuredError> {
    let first = runs.partition_point(|run| run.decoded.end <= range.start);
    let mut cursor = range.start;
    let mut overlaps = Vec::new();
    for run in &runs[first..] {
        if run.decoded.start >= range.end {
            break;
        }
        let overlap = run.decoded.start.max(range.start)..run.decoded.end.min(range.end);
        if overlap.start != cursor {
            return Err(inconsistent(range.clone()));
        }
        cursor = overlap.end;
        overlaps.push((run, overlap));
    }
    if cursor != range.end || range.is_empty() {
        return Err(inconsistent(range.clone()));
    }
    Ok(overlaps)
}

fn run_original(run: &DecodeRun, overlap: &Range<usize>) -> Result<Range<usize>, StructuredError> {
    if run.exact {
        shift(
            &(overlap.start - run.decoded.start..overlap.end - run.decoded.start),
            run.original.start,
        )
    } else {
        Ok(run.original.clone())
    }
}

fn shift(range: &Range<usize>, by: usize) -> Result<Range<usize>, StructuredError> {
    let start = range
        .start
        .checked_add(by)
        .ok_or_else(|| inconsistent(range.clone()))?;
    let end = range
        .end
        .checked_add(by)
        .ok_or_else(|| inconsistent(range.clone()))?;
    Ok(start..end)
}

fn inconsistent(range: Range<usize>) -> StructuredError {
    StructuredError::Mapping { range }
}

#[cfg(test)]
#[path = "retained_structured_tests.rs"]
mod tests;
