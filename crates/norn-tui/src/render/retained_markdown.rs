//! Direct Markdown styles and honest original/display byte mappings; no terminal escapes or I/O.

use std::ops::Range;

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use super::retained_text::{StyleSpan, TextAttribute, TextStyle};
use super::syntax::SyntaxHighlighter;

const OPTIONS: Options = Options::ENABLE_TABLES
    .union(Options::ENABLE_STRIKETHROUGH)
    .union(Options::ENABLE_TASKLISTS)
    .union(Options::ENABLE_MATH)
    .union(Options::ENABLE_SMART_PUNCTUATION);
const CODE_FOREGROUND: [u8; 3] = [0, 175, 175];

#[path = "retained_markdown/mapping.rs"]
mod mapping;

pub use mapping::{
    BoundaryAffinity, MarkdownError, RenderedMarkdown, SourceBoundary, SourceDisplaySpan,
    SourceMapping,
};

/// Render exactly the currently supplied Markdown, including incomplete streaming syntax.
/// Reparse on content revision; this function makes no claim that an unclosed construct is final.
/// The frontend owns one initialized highlighter and reuses it across body renders.
///
/// # Errors
/// Reports contradictory parser structure, numbering overflow or unsafe final display text.
pub fn render_markdown(
    original: &str,
    highlighter: &SyntaxHighlighter,
) -> Result<RenderedMarkdown, MarkdownError> {
    let mut output = Builder::new(original);
    for (event, range) in Parser::new_ext(original, OPTIONS).into_offset_iter() {
        output.event(event, range, highlighter)?;
    }
    output.finish()
}

/// Render plain original text through the same control escaping and source mapping contract.
///
/// # Errors
/// Propagates styled-text validation failures without echoing original content.
pub fn render_plain(original: &str) -> Result<RenderedMarkdown, MarkdownError> {
    let mut output = Builder::new(original);
    output.emit(
        original,
        &SourceMapping::Exact {
            original: 0..original.len(),
        },
        TextStyle::default(),
    );
    output.finish()
}

struct Builder<'a> {
    original: &'a str,
    text: String,
    styles: Vec<StyleSpan>,
    spans: Vec<SourceDisplaySpan>,
    style: TextStyle,
    stack: Vec<(TagEnd, TextStyle)>,
    lists: Vec<(Option<u64>, u64)>,
    quote_depth: usize,
    item_prefix: Option<usize>,
    table_cell: usize,
    links: Vec<(String, Range<usize>)>,
    pending_code: Option<PendingCode>,
}

struct PendingCode {
    language: Option<String>,
    text: String,
    spans: Vec<SourceDisplaySpan>,
}

impl<'a> Builder<'a> {
    fn new(original: &'a str) -> Self {
        Self {
            original,
            text: String::new(),
            styles: Vec::new(),
            spans: Vec::new(),
            style: TextStyle::default(),
            stack: Vec::new(),
            lists: Vec::new(),
            quote_depth: 0,
            item_prefix: None,
            table_cell: 0,
            links: Vec::new(),
            pending_code: None,
        }
    }

    fn event(
        &mut self,
        event: Event<'_>,
        range: Range<usize>,
        highlighter: &SyntaxHighlighter,
    ) -> Result<(), MarkdownError> {
        match event {
            Event::Start(tag) => self.start(tag, range)?,
            Event::End(end) => self.end(end, range, highlighter)?,
            Event::Text(text) if self.pending_code.is_some() => self.stage_code(&text, range)?,
            Event::Text(text) => self.parsed(&text, range, self.style, true)?,
            Event::Code(text) | Event::InlineMath(text) => {
                self.parsed(&text, range, code(self.style), false)?;
            }
            Event::DisplayMath(text) => {
                self.line();
                self.parsed(&text, range, code(self.style), false)?;
                self.line();
            }
            Event::Html(text) | Event::InlineHtml(text) => self.parsed(
                &text,
                range,
                attribute(self.style, TextAttribute::Dim),
                false,
            )?,
            Event::SoftBreak | Event::HardBreak => self.parsed("\n", range, self.style, false)?,
            Event::Rule => {
                self.line();
                self.generated("───");
                self.line();
            }
            Event::TaskListMarker(checked) => {
                let start = self
                    .item_prefix
                    .take()
                    .ok_or_else(|| MarkdownError::Structure {
                        range: range.clone(),
                    })?;
                self.truncate(start);
                self.emit(
                    if checked { "☑ " } else { "☐ " },
                    &SourceMapping::Transformed { original: range },
                    self.style,
                );
            }
            Event::FootnoteReference(text) => self.parsed(&text, range, self.style, false)?,
        }
        Ok(())
    }

    fn start(&mut self, tag: Tag<'_>, range: Range<usize>) -> Result<(), MarkdownError> {
        self.stack.push((tag.to_end(), self.style));
        match tag {
            Tag::Paragraph if self.lists.is_empty() => self.line(),
            Tag::Heading { level, .. } => {
                self.line();
                self.style = attribute(
                    self.style,
                    if matches!(
                        level,
                        HeadingLevel::H4 | HeadingLevel::H5 | HeadingLevel::H6
                    ) {
                        TextAttribute::Dim
                    } else {
                        TextAttribute::Bold
                    },
                );
            }
            Tag::Emphasis => self.style = attribute(self.style, TextAttribute::Italic),
            Tag::Strong | Tag::TableHead => self.style = attribute(self.style, TextAttribute::Bold),
            Tag::Strikethrough => self.style = attribute(self.style, TextAttribute::Strike),
            Tag::CodeBlock(kind) => {
                self.line();
                self.pending_code = Some(PendingCode {
                    language: match kind {
                        CodeBlockKind::Fenced(info) => {
                            info.split_whitespace().next().map(str::to_owned)
                        }
                        CodeBlockKind::Indented => None,
                    },
                    text: String::new(),
                    spans: Vec::new(),
                });
            }
            Tag::BlockQuote(_) => {
                self.line();
                self.quote_depth += 1;
            }
            Tag::List(number) => {
                self.line();
                self.lists.push((number, 0));
            }
            Tag::Item => {
                self.line();
                for _ in 1..self.lists.len() {
                    self.generated("  ");
                }
                let (number, count) =
                    self.lists
                        .last_mut()
                        .ok_or_else(|| MarkdownError::Structure {
                            range: range.clone(),
                        })?;
                let marker = match number {
                    Some(first) => format!(
                        "{}. ",
                        first
                            .checked_add(*count)
                            .ok_or_else(|| MarkdownError::ListOverflow {
                                range: range.clone()
                            })?
                    ),
                    None => "• ".to_owned(),
                };
                *count = count
                    .checked_add(1)
                    .ok_or(MarkdownError::ListOverflow { range })?;
                self.item_prefix = Some(self.text.len());
                self.generated(&marker);
            }
            Tag::Link { dest_url, .. } => {
                self.links.push((dest_url.into_string(), range));
                self.style = attribute(self.style, TextAttribute::Underline);
            }
            Tag::Image { .. } => {
                self.generated("[image: ");
                self.style = attribute(self.style, TextAttribute::Dim);
            }
            Tag::Table(_) | Tag::TableRow => {
                self.line();
                self.table_cell = 0;
            }
            Tag::TableCell => {
                if self.table_cell != 0 {
                    self.generated(" │ ");
                }
                self.table_cell += 1;
            }
            Tag::Paragraph | Tag::HtmlBlock => {}
            _ => return Err(MarkdownError::Structure { range }),
        }
        Ok(())
    }

    fn end(
        &mut self,
        end: TagEnd,
        range: Range<usize>,
        highlighter: &SyntaxHighlighter,
    ) -> Result<(), MarkdownError> {
        let (expected, previous) = self.stack.pop().ok_or_else(|| MarkdownError::Structure {
            range: range.clone(),
        })?;
        if end != expected {
            return Err(MarkdownError::Structure { range });
        }
        match end {
            TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::HtmlBlock | TagEnd::Table => {
                self.line();
            }
            TagEnd::CodeBlock => {
                self.finish_code(highlighter, &range)?;
                self.line();
            }
            TagEnd::Item => {
                self.line();
                self.item_prefix = None;
            }
            TagEnd::List(_) => {
                self.lists.pop().ok_or_else(|| MarkdownError::Structure {
                    range: range.clone(),
                })?;
            }
            TagEnd::BlockQuote(_) => {
                self.quote_depth =
                    self.quote_depth
                        .checked_sub(1)
                        .ok_or_else(|| MarkdownError::Structure {
                            range: range.clone(),
                        })?;
                self.line();
            }
            TagEnd::Link => {
                let (url, source) = self.links.pop().ok_or_else(|| MarkdownError::Structure {
                    range: range.clone(),
                })?;
                self.generated(" (");
                self.emit(
                    &url,
                    &SourceMapping::Transformed { original: source },
                    attribute(previous, TextAttribute::Dim),
                );
                self.generated(")");
            }
            TagEnd::Image => self.generated("]"),
            TagEnd::TableHead | TagEnd::TableRow => {
                self.line();
                self.table_cell = 0;
            }
            _ => {}
        }
        self.style = previous;
        Ok(())
    }

    fn parsed(
        &mut self,
        text: &str,
        range: Range<usize>,
        style: TextStyle,
        escape: bool,
    ) -> Result<(), MarkdownError> {
        let raw = self
            .original
            .get(range.clone())
            .ok_or_else(|| MarkdownError::Structure {
                range: range.clone(),
            })?;
        if raw.contains('\0')
            && raw
                .chars()
                .map(|character| {
                    if character == '\0' {
                        '\u{fffd}'
                    } else {
                        character
                    }
                })
                .eq(text.chars())
        {
            // CommonMark replaces literal NUL; display the actual control visibly instead.
            self.emit(raw, &SourceMapping::Exact { original: range }, style);
        } else if raw != text {
            self.emit(text, &SourceMapping::Transformed { original: range }, style);
        } else if escape
            && !self.stack.iter().any(|(end, _)| *end == TagEnd::CodeBlock)
            && range.start > 0
            && self.original.as_bytes().get(range.start - 1) == Some(&b'\\')
            && text.starts_with(|character: char| character.is_ascii_punctuation())
        {
            let end = range.start + 1;
            self.emit(
                &text[..1],
                &SourceMapping::Transformed {
                    original: range.start - 1..end,
                },
                style,
            );
            self.emit(
                &text[1..],
                &SourceMapping::Exact {
                    original: end..range.end,
                },
                style,
            );
        } else {
            self.emit(text, &SourceMapping::Exact { original: range }, style);
        }
        Ok(())
    }

    fn stage_code(&mut self, text: &str, range: Range<usize>) -> Result<(), MarkdownError> {
        let raw = self
            .original
            .get(range.clone())
            .ok_or_else(|| MarkdownError::Structure {
                range: range.clone(),
            })?;
        let text = if raw.contains('\0')
            && raw
                .chars()
                .map(|character| {
                    if character == '\0' {
                        '\u{fffd}'
                    } else {
                        character
                    }
                })
                .eq(text.chars())
        {
            raw
        } else {
            text
        };
        let code = self
            .pending_code
            .as_mut()
            .ok_or_else(|| MarkdownError::Structure {
                range: range.clone(),
            })?;
        let start = code.text.len();
        code.text.push_str(text);
        if !text.is_empty() {
            code.spans.push(SourceDisplaySpan {
                display: start..code.text.len(),
                source: if raw == text {
                    SourceMapping::Exact { original: range }
                } else {
                    SourceMapping::Transformed { original: range }
                },
            });
        }
        Ok(())
    }

    fn finish_code(
        &mut self,
        highlighter: &SyntaxHighlighter,
        range: &Range<usize>,
    ) -> Result<(), MarkdownError> {
        let code = self
            .pending_code
            .take()
            .ok_or_else(|| MarkdownError::Structure {
                range: range.clone(),
            })?;
        let styles = highlighter.highlight_spans(&code.text, code.language.as_deref())?;
        let mut index = 0;
        for chunk in &code.spans {
            let mut offset = chunk.display.start;
            while offset < chunk.display.end {
                let style = styles.get(index).ok_or_else(|| MarkdownError::Structure {
                    range: range.clone(),
                })?;
                if style.range.end <= offset {
                    index += 1;
                    continue;
                }
                if style.range.start > offset {
                    return Err(MarkdownError::Structure {
                        range: range.clone(),
                    });
                }
                let end = style.range.end.min(chunk.display.end);
                let text = code
                    .text
                    .get(offset..end)
                    .ok_or_else(|| MarkdownError::Structure {
                        range: range.clone(),
                    })?;
                let source = match &chunk.source {
                    SourceMapping::Exact { original } => SourceMapping::Exact {
                        original: original.start + (offset - chunk.display.start)
                            ..original.start + (end - chunk.display.start),
                    },
                    source => source.clone(),
                };
                self.emit(text, &source, style.style);
                offset = end;
            }
        }
        Ok(())
    }

    fn generated(&mut self, text: &str) {
        self.emit(text, &SourceMapping::Generated, self.style);
    }

    fn line(&mut self) {
        if !self.text.is_empty() && !self.text.ends_with('\n') {
            self.generated("\n");
        }
    }
}

fn attribute(mut style: TextStyle, attribute: TextAttribute) -> TextStyle {
    style.attributes = style.attributes.with(attribute);
    style
}

fn code(mut style: TextStyle) -> TextStyle {
    style.foreground = Some(CODE_FOREGROUND);
    style
}

#[cfg(test)]
#[path = "retained_markdown_tests.rs"]
mod tests;
