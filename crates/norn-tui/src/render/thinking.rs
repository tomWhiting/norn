//! Rendering for reasoning-summary text.
//!
//! Reasoning summaries arrive as plain text, but current `OpenAI` examples
//! commonly shape them as `**Heading**\n\nBody`. This renderer treats
//! that as a presentation hint rather than a protocol guarantee: matching
//! summaries become compact "Thought about ..." blocks, while all other
//! text falls back to the plain `thinking:` form. Multiple heading/body
//! sections in one summary render as separate blocks so model-supplied
//! section boundaries do not get folded into body prose.

use termina::escape::csi::{Csi, Sgr};
use termina::style::Intensity;
use unicode_width::{UnicodeWidthChar as _, UnicodeWidthStr as _};

use crate::render::style::{italic, italic_off};
use crate::terminal::caps::TerminalCaps;

const THOUGHT_HEADER_PREFIX: &str = "╭┄Thought about ";
const THOUGHT_BODY_PREFIX: &str = "│";
const THOUGHT_BODY_TEXT_PREFIX: &str = "│ ";
const THOUGHT_FOOTER: &str = "╰┄";

/// Render a reasoning summary for the scroll region.
#[must_use]
pub(crate) fn render_thinking(text: &str, caps: &TerminalCaps, width: u16) -> String {
    if text.is_empty() {
        return String::new();
    }
    if let Some(summaries) = split_markdown_summaries(text) {
        return render_thought_blocks(&summaries, caps, width);
    }
    render_plain_thinking(text, caps)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MarkdownSummary<'a> {
    heading: &'a str,
    body: &'a str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HeadingMatch<'a> {
    start: usize,
    heading: &'a str,
    body_start: usize,
}

fn split_markdown_summaries(text: &str) -> Option<Vec<MarkdownSummary<'_>>> {
    let first = heading_at(text, 0)?;
    let mut summaries = Vec::new();
    let mut current = first;

    loop {
        let next = find_next_heading(text, current.body_start);
        let raw_body = next.map_or(&text[current.body_start..], |h| {
            &text[current.body_start..h.start]
        });
        summaries.push(MarkdownSummary {
            heading: current.heading,
            body: trim_section_body(raw_body),
        });

        let Some(next_heading) = next else {
            break;
        };
        current = next_heading;
    }

    Some(summaries)
}

fn heading_at(text: &str, start: usize) -> Option<HeadingMatch<'_>> {
    let rest = text.get(start..)?.strip_prefix("**")?;
    let heading_end = rest.find("**")?;
    let heading = rest[..heading_end].trim();
    if heading.is_empty() || heading.contains('\n') || heading.contains('\r') {
        return None;
    }
    let after_heading = &rest[heading_end + 2..];
    let body_start = if after_heading.starts_with("\n\n") {
        start + 2 + heading_end + 2 + 2
    } else if after_heading.starts_with("\r\n\r\n") {
        start + 2 + heading_end + 2 + 4
    } else {
        return None;
    };
    Some(HeadingMatch {
        start,
        heading,
        body_start,
    })
}

fn find_next_heading(text: &str, from: usize) -> Option<HeadingMatch<'_>> {
    let mut search_from = from;
    while let Some(rel) = text.get(search_from..)?.find("**") {
        let absolute = search_from + rel;
        if let Some(heading) = heading_at(text, absolute) {
            return Some(heading);
        }
        search_from = absolute + "**".len();
    }
    None
}

fn trim_section_body(body: &str) -> &str {
    body.trim_matches(['\r', '\n'])
}

fn render_plain_thinking(text: &str, caps: &TerminalCaps) -> String {
    let dim = Csi::Sgr(Sgr::Intensity(Intensity::Dim)).to_string();
    let normal = Csi::Sgr(Sgr::Intensity(Intensity::Normal)).to_string();
    let italic_on = Csi::Sgr(italic(caps)).to_string();
    let italic_off = Csi::Sgr(italic_off(caps)).to_string();
    let mut out = String::new();
    out.push_str(&dim);
    out.push_str(&italic_on);
    out.push_str("thinking: ");
    out.push_str(text);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&italic_off);
    out.push_str(&normal);
    out
}

fn render_thought_blocks(
    summaries: &[MarkdownSummary<'_>],
    caps: &TerminalCaps,
    width: u16,
) -> String {
    let dim = Csi::Sgr(Sgr::Intensity(Intensity::Dim)).to_string();
    let normal = Csi::Sgr(Sgr::Intensity(Intensity::Normal)).to_string();
    let bold = Csi::Sgr(Sgr::Intensity(Intensity::Bold)).to_string();
    let italic_on = Csi::Sgr(italic(caps)).to_string();
    let italic_off = Csi::Sgr(italic_off(caps)).to_string();
    let width = usize::from(width).max(1);
    let body_width = width
        .saturating_sub(THOUGHT_BODY_TEXT_PREFIX.width())
        .max(1);

    let mut out = String::new();
    out.push_str(&dim);
    for summary in summaries {
        write_wrapped_heading(&mut out, summary.heading, width, &bold, &normal, &dim);
        out.push_str(THOUGHT_BODY_PREFIX);
        out.push('\n');
        for line in wrap_text(summary.body, body_width) {
            if line.is_empty() {
                out.push_str(THOUGHT_BODY_PREFIX);
                out.push('\n');
            } else {
                out.push_str(THOUGHT_BODY_TEXT_PREFIX);
                out.push_str(&italic_on);
                out.push_str(&line);
                out.push_str(&italic_off);
                out.push('\n');
            }
        }
        out.push_str(THOUGHT_FOOTER);
        out.push('\n');
    }
    out.push('\n');
    out.push_str(&normal);
    out
}

fn write_wrapped_heading(
    out: &mut String,
    heading: &str,
    width: usize,
    bold: &str,
    normal: &str,
    dim: &str,
) {
    let first_width = width.saturating_sub(THOUGHT_HEADER_PREFIX.width()).max(1);
    let mut lines = wrap_text(heading, first_width).into_iter();
    let first = lines.next().unwrap_or_default();
    out.push_str(THOUGHT_HEADER_PREFIX);
    out.push_str(bold);
    out.push_str(&first);
    out.push_str(normal);
    out.push_str(dim);
    out.push('\n');

    let continuation_width = width
        .saturating_sub(THOUGHT_BODY_TEXT_PREFIX.width())
        .max(1);
    for line in lines.flat_map(|line| wrap_text(&line, continuation_width)) {
        out.push_str(THOUGHT_BODY_TEXT_PREFIX);
        out.push_str(bold);
        out.push_str(&line);
        out.push_str(normal);
        out.push_str(dim);
        out.push('\n');
    }
}

fn wrap_text(text: &str, width: usize) -> Vec<String> {
    text.split('\n')
        .flat_map(move |line| wrap_logical_line(line, width))
        .collect()
}

fn wrap_logical_line(line: &str, width: usize) -> Vec<String> {
    if line.is_empty() {
        return vec![String::new()];
    }
    let mut out = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    for word in line.split_whitespace() {
        let word_width = word.width();
        if current.is_empty() {
            push_word_or_split(&mut out, &mut current, &mut current_width, word, width);
        } else if current_width + 1 + word_width <= width {
            current.push(' ');
            current.push_str(word);
            current_width += 1 + word_width;
        } else {
            out.push(std::mem::take(&mut current));
            current_width = 0;
            push_word_or_split(&mut out, &mut current, &mut current_width, word, width);
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

fn push_word_or_split(
    out: &mut Vec<String>,
    current: &mut String,
    current_width: &mut usize,
    word: &str,
    width: usize,
) {
    if word.width() <= width {
        current.push_str(word);
        *current_width = word.width();
        return;
    }
    for ch in word.chars() {
        let ch_width = ch.width().unwrap_or(0);
        if !current.is_empty() && *current_width + ch_width > width {
            out.push(std::mem::take(current));
            *current_width = 0;
        }
        current.push(ch);
        *current_width += ch_width;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn strip_ansi(input: &str) -> String {
        let mut out = String::new();
        let mut chars = input.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '\x1b' && chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
                continue;
            }
            out.push(ch);
        }
        out
    }

    #[test]
    fn split_markdown_summaries_accepts_heading_body_shape() {
        let parsed = split_markdown_summaries("**Creating a markdown table**\n\nI need").unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].heading, "Creating a markdown table");
        assert_eq!(parsed[0].body, "I need");
    }

    #[test]
    fn split_markdown_summaries_rejects_plain_text() {
        assert!(split_markdown_summaries("thinking normally").is_none());
        assert!(split_markdown_summaries("**not closed\n\nbody").is_none());
    }

    #[test]
    fn split_markdown_summaries_accepts_multiple_heading_body_sections() {
        let parsed = split_markdown_summaries(
            "**Exploring agent tasks**\n\nFirst body.**Testing signal agents**\n\nSecond body.",
        )
        .unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].heading, "Exploring agent tasks");
        assert_eq!(parsed[0].body, "First body.");
        assert_eq!(parsed[1].heading, "Testing signal agents");
        assert_eq!(parsed[1].body, "Second body.");
    }

    #[test]
    fn render_markdown_summary_as_thought_block() {
        let out = render_thinking(
            "**Creating a markdown table**\n\nI need to prepare an answer.",
            &TerminalCaps::baseline(),
            80,
        );
        let plain = strip_ansi(&out);
        assert!(plain.contains("╭┄Thought about Creating a markdown table"));
        assert!(plain.contains("│"));
        assert!(plain.contains("│ I need to prepare an answer."));
        assert!(plain.contains("╰┄"));
        assert!(!plain.contains("thinking:"));
        assert!(!plain.contains("**Creating"));
        assert!(plain.ends_with("\n\n"));
    }

    #[test]
    fn render_multiple_markdown_sections_as_separate_thought_blocks() {
        let out = render_thinking(
            "**Exploring agent tasks**\n\nFirst body.**Testing signal agents**\n\nSecond body.",
            &TerminalCaps::baseline(),
            80,
        );
        let plain = strip_ansi(&out);
        assert!(plain.contains("╭┄Thought about Exploring agent tasks"));
        assert!(plain.contains("│ First body."));
        assert!(plain.contains("╭┄Thought about Testing signal agents"));
        assert!(plain.contains("│ Second body."));
        assert!(!plain.contains("First body.**Testing signal agents**"));
    }

    #[test]
    fn render_thought_block_wraps_body_with_gutter() {
        let out = render_thinking(
            "**Heading**\n\nalpha beta gamma delta epsilon",
            &TerminalCaps::baseline(),
            18,
        );
        let plain = strip_ansi(&out);
        assert!(plain.contains("│ alpha beta gamma"));
        assert!(plain.contains("│ delta epsilon"));
    }

    #[test]
    fn render_plain_thinking_keeps_prefix() {
        let out = render_thinking("plain text", &TerminalCaps::baseline(), 80);
        let plain = strip_ansi(&out);
        assert!(plain.contains("thinking: plain text"));
    }
}
