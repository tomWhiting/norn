//! Session event rendering and structured output.
//!
//! This module is the dispatch layer that turns a [`SessionEvent`] into
//! styled terminal output. Each event variant routes to one of a handful
//! of focused renderers:
//!
//! - [`render_user_message`] — coloured `> ` prefix for human input.
//! - [`render_thinking`] — dim/italic reasoning content, gated by the
//!   [`DisplayToggles::thinking_visible`] flag.
//! - [`render_assistant_message`] — thinking (when visible) followed by
//!   the assistant text, run through the streaming
//!   [`MarkdownRenderer`]. When the assistant content parses as a JSON
//!   object with multiple fields, the labeled-section pattern from
//!   [`render_structured`] takes over.
//! - [`render_tool_call`] — per-tool renderer dispatch via
//!   [`crate::tools::renderer::renderer_for`]. Unknown tools fall back
//!   to a single-line `{tool_name}: {json}` representation.
//! - [`render_structured`] — labeled-section pattern for schema-driven
//!   event types. Primary field renders by default; secondary fields
//!   render below `─── {field_name} ───` separators when the visibility
//!   toggle is set.
//!
//! The module is pure: every function is stateless, takes its inputs by
//! reference, and returns a `String`. No I/O, no event-loop wiring (that
//! is NT-011), no scroll-region writes (the caller invokes
//! [`crate::render::scroll_region::write_to_scroll`] on the returned
//! string).
//!
//! All function signatures take a [`TerminalCaps`] so that styling
//! decisions adapt to the terminal — true colour vs palette, italic vs
//! underline, OSC 8 hyperlinks vs bracketed fallback — by way of the
//! shared style helpers.

use serde_json::{Map, Value};
use termina::escape::csi::{Csi, Sgr};
use termina::style::{ColorSpec, Intensity, RgbColor};

use norn::session::events::SessionEvent;

use crate::render::markdown::MarkdownRenderer;
use crate::render::style::colour_for;
use crate::render::thinking::render_thinking as render_thinking_block;
use crate::terminal::caps::TerminalCaps;
use crate::tools::renderer::renderer_for;

/// Foreground colour for the user-message prefix.
///
/// A mid-saturation blue that reads as "input" without colliding with
/// the inline-code colour in [`crate::render::markdown`] or the
/// generating-indicator amber in [`crate::render::fixed_panel`].
pub(crate) const USER_PREFIX_COLOUR: RgbColor = RgbColor::new(80, 160, 220);

/// Priority list for selecting the primary field of a structured value.
///
/// Iterated in order before falling back to the first string-typed
/// field, then the first field in iteration order.
const PRIMARY_KEY_PRIORITY: &[&str] = &["text", "content", "written", "response"];

/// Visibility toggles for rendering output.
///
/// Thinking is visible by default so provider reasoning summaries are
/// not silently dropped from the live TUI. Secondary structured fields
/// remain hidden initially to avoid expanding every structured event.
/// Pressing Ctrl+E toggles the whole extra-output layer off or on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayToggles {
    /// Whether thinking content is rendered into the scroll region.
    pub thinking_visible: bool,
    /// Whether secondary structured-output fields are rendered.
    pub secondary_fields_visible: bool,
}

impl Default for DisplayToggles {
    fn default() -> Self {
        Self {
            thinking_visible: true,
            secondary_fields_visible: false,
        }
    }
}

impl DisplayToggles {
    /// Construct with thinking visible and secondary fields hidden.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            thinking_visible: true,
            secondary_fields_visible: false,
        }
    }

    /// Toggle the extra-output layer.
    ///
    /// Turning it off hides both thinking and secondary structured
    /// fields. Turning it on shows both.
    pub fn toggle(&mut self) {
        let visible = !self.thinking_visible;
        self.thinking_visible = visible;
        self.secondary_fields_visible = visible;
    }

    /// Render the human-readable status string shown momentarily after
    /// the Ctrl+E keystroke (e.g. `"thinking: on, details: on"`).
    #[must_use]
    pub fn status_text(&self) -> String {
        let thinking = if self.thinking_visible { "on" } else { "off" };
        let details = if self.secondary_fields_visible {
            "on"
        } else {
            "off"
        };
        format!("thinking: {thinking}, details: {details}")
    }
}

/// Render a [`SessionEvent`] into styled terminal output.
///
/// Dispatches to the appropriate renderer based on the variant. The
/// match is exhaustive — adding a variant to [`SessionEvent`] forces a
/// compile error here so no event type ever renders as silent nothing.
#[must_use]
pub fn render_event(
    event: &SessionEvent,
    caps: &TerminalCaps,
    toggles: DisplayToggles,
    terminal_width: u16,
) -> String {
    match event {
        SessionEvent::UserMessage { content, .. } => render_user_message(content, caps),
        SessionEvent::AssistantMessage {
            content, thinking, ..
        } => render_assistant_message(content, thinking, caps, toggles, terminal_width),
        SessionEvent::SpokenResponse { content, .. } => render_structured(
            content,
            None,
            caps,
            toggles.secondary_fields_visible,
            terminal_width,
        ),
        SessionEvent::ToolResult {
            tool_name,
            output,
            duration_ms,
            ..
        } => render_tool_call(tool_name, &Value::Null, output, *duration_ms, caps),
        SessionEvent::ModelChange {
            old_model,
            new_model,
            ..
        } => render_dim_status_line(&format!("model: {old_model} → {new_model}")),
        SessionEvent::Compaction { summary, .. } => {
            render_dim_status_line(&format!("compaction: {summary}"))
        }
        SessionEvent::Fork {
            forked_session_id, ..
        } => render_dim_status_line(&format!("forked → {forked_session_id}")),
        SessionEvent::ForkComplete {
            forked_session_id,
            duration_ms,
            ..
        } => render_dim_status_line(&format!(
            "fork complete ← {forked_session_id} ({duration_ms}ms)"
        )),
        SessionEvent::Label {
            label, description, ..
        } => match description {
            Some(d) => render_dim_status_line(&format!("label: {label} — {d}")),
            None => render_dim_status_line(&format!("label: {label}")),
        },
        SessionEvent::Custom {
            event_type, data, ..
        } => {
            let mut out = secondary_separator(event_type);
            out.push('\n');
            out.push_str(&render_structured(
                data,
                None,
                caps,
                toggles.secondary_fields_visible,
                terminal_width,
            ));
            out
        }
        SessionEvent::RuleInjection {
            rule_id, content, ..
        } => {
            let header = render_dim_status_line(&format!("rule: {rule_id}"));
            if content.is_empty() {
                header
            } else {
                format!("{header}\n{content}")
            }
        }
    }
}

/// Render a user message with a coloured `> ` prefix on the first line.
///
/// Continuation lines (after `\n`) are indented two spaces so the text
/// column aligns with the prefix column. The first line is wrapped in a
/// foreground-colour SGR pair; continuation lines are unstyled.
#[must_use]
pub fn render_user_message(content: &str, caps: &TerminalCaps) -> String {
    let colour = colour_for(USER_PREFIX_COLOUR, caps);
    let reset = Csi::Sgr(Sgr::Foreground(ColorSpec::Reset)).to_string();
    if content.is_empty() {
        return format!("{colour}> {reset}\n");
    }
    let mut out = String::new();
    for (i, line) in content.split('\n').enumerate() {
        if i == 0 {
            out.push_str(&colour);
            out.push_str("> ");
            out.push_str(line);
            out.push_str(&reset);
        } else {
            out.push_str("  ");
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// Render thinking content with the dim SGR attribute.
///
/// Returns an empty string when `toggles.thinking_visible` is false, so
/// the caller can unconditionally append the result to its output
/// buffer. Replayed `AssistantMessage.thinking` strings use this block
/// renderer; live `ThinkingDelta` chunks are buffered and then rendered
/// through the same path from `app::streaming`.
///
/// GPT-style markdown-heading summaries (`**Heading**\n\nBody`) render
/// as a compact `Thought about ...` block. Other text gets the legacy
/// `thinking: ` prefix and is otherwise preserved verbatim. The whole
/// block is bracketed by dim, italic (or underline fallback), and reset
/// SGR escapes.
#[must_use]
pub fn render_thinking(
    text: &str,
    caps: &TerminalCaps,
    toggles: DisplayToggles,
    terminal_width: u16,
) -> String {
    if !toggles.thinking_visible || text.is_empty() {
        return String::new();
    }
    render_thinking_block(text, caps, terminal_width)
}

/// Render an assistant message — thinking (when visible) followed by
/// the assistant text.
///
/// When `content` parses as a JSON object with two or more keys the
/// rendering delegates to [`render_structured`] (the labeled-section
/// pattern). Otherwise `content` is run through the streaming
/// [`MarkdownRenderer`] for one-shot rendering.
#[must_use]
pub fn render_assistant_message(
    content: &str,
    thinking: &str,
    caps: &TerminalCaps,
    toggles: DisplayToggles,
    terminal_width: u16,
) -> String {
    let mut out = render_thinking(thinking, caps, toggles, terminal_width);
    if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(content)
        && map.len() > 1
    {
        let value = Value::Object(map);
        out.push_str(&render_structured(
            &value,
            None,
            caps,
            toggles.secondary_fields_visible,
            terminal_width,
        ));
        return out;
    }
    if !content.is_empty() {
        let mut renderer = MarkdownRenderer::new(caps.clone(), terminal_width);
        let feed_out = renderer.feed(content);
        let tail = renderer.finalize();
        let mut md_out = format!("{}{}", feed_out.styled, tail.styled);
        if !md_out.is_empty() && !md_out.ends_with('\n') {
            md_out.push('\n');
        }
        out.push_str(&md_out);
    }
    out
}

/// Render a tool call: header line, then optional body.
///
/// Dispatches to the per-tool renderer keyed on `tool_name`. Unknown
/// tools fall back to a single-line `{tool_name}: {json}` summary so
/// they never render as silent nothing.
#[must_use]
pub fn render_tool_call(
    tool_name: &str,
    args: &Value,
    result: &Value,
    duration_ms: u64,
    caps: &TerminalCaps,
) -> String {
    let dim_border = "\x1b[2m│\x1b[22m ";
    let Some(renderer) = renderer_for(tool_name) else {
        let json = serde_json::to_string(result).unwrap_or_default();
        return format!("{dim_border}{tool_name}: {json}\n");
    };
    let header = renderer.header_line(args, result, duration_ms, caps);
    let mut out = format!("{dim_border}{header}");
    if !out.ends_with('\n') {
        out.push('\n');
    }
    if let Some(body) = renderer.body(args, result, caps)
        && !body.is_empty()
    {
        for line in body.lines() {
            out.push_str(dim_border);
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Render any JSON value against an optional schema.
///
/// For object values, a primary key is selected via
/// [`pick_primary_key_ordered`] and rendered first. When
/// `secondary_visible` is true, remaining keys render below
/// `─── {field_name} ───` separators in schema-property order (when a
/// schema is provided) or map iteration order (alphabetical for
/// `serde_json::Map` without the `preserve_order` feature, which the
/// workspace does not enable).
///
/// Non-object values render as pretty-printed JSON.
///
/// The function is defensive: a schema missing `properties`, or with
/// keys absent from `value`, is silently tolerated — validation is the
/// runtime's job, not the renderer's.
#[must_use]
pub fn render_structured(
    value: &Value,
    schema: Option<&Value>,
    caps: &TerminalCaps,
    secondary_visible: bool,
    terminal_width: u16,
) -> String {
    let Value::Object(map) = value else {
        let mut pretty = serde_json::to_string_pretty(value).unwrap_or_default();
        if !pretty.ends_with('\n') {
            pretty.push('\n');
        }
        return pretty;
    };

    if map.is_empty() {
        return String::new();
    }

    let key_order = resolve_key_order(map, schema);
    let primary_key = pick_primary_key_ordered(map, &key_order);
    let mut out = String::new();

    if let Some(primary) = map.get(&primary_key) {
        out.push_str(&render_field_value(primary, caps, terminal_width));
        if !out.ends_with('\n') {
            out.push('\n');
        }
    }

    if !secondary_visible {
        return out;
    }

    for key in &key_order {
        if key == &primary_key {
            continue;
        }
        let Some(secondary) = map.get(key) else {
            continue;
        };
        out.push_str(&secondary_separator(key));
        out.push('\n');
        out.push_str(&render_field_value(secondary, caps, terminal_width));
        if !out.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

/// Build the ordered key list for a structured value.
///
/// When a schema is provided, schema-property order takes precedence
/// (filtered to keys that actually exist in `map`), then any
/// map-only keys are appended in map order so no field is silently
/// dropped.
fn resolve_key_order(map: &Map<String, Value>, schema: Option<&Value>) -> Vec<String> {
    let Some(schema_keys) = schema_property_order(schema) else {
        return map.keys().cloned().collect();
    };
    let mut order: Vec<String> = schema_keys
        .into_iter()
        .filter(|k| map.contains_key(k))
        .collect();
    for k in map.keys() {
        if !order.iter().any(|x| x == k) {
            order.push(k.clone());
        }
    }
    order
}

/// Pick the primary key of an ordered key set.
///
/// Tries the priority list ([`PRIMARY_KEY_PRIORITY`]) first, then the
/// first string-typed field in iteration order, then the first field
/// in iteration order. Returns an empty string only when `key_order`
/// itself is empty, which the caller filters out.
fn pick_primary_key_ordered(map: &Map<String, Value>, key_order: &[String]) -> String {
    for &candidate in PRIMARY_KEY_PRIORITY {
        if map.contains_key(candidate) && key_order.iter().any(|k| k == candidate) {
            return candidate.to_string();
        }
    }
    for key in key_order {
        if matches!(map.get(key), Some(Value::String(_))) {
            return key.clone();
        }
    }
    key_order.first().cloned().unwrap_or_default()
}

/// Render a single field value: strings through the markdown pipeline,
/// other JSON values pretty-printed.
fn render_field_value(value: &Value, caps: &TerminalCaps, terminal_width: u16) -> String {
    match value {
        Value::String(s) => {
            let mut renderer = MarkdownRenderer::new(caps.clone(), terminal_width);
            let feed_out = renderer.feed(s);
            let tail = renderer.finalize();
            let mut out = format!("{}{}", feed_out.styled, tail.styled);
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out
        }
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

/// Format a labeled-section separator: `─── {name} ───`.
fn secondary_separator(name: &str) -> String {
    format!("─── {name} ───")
}

/// Render a dim status line with a trailing newline.
fn render_dim_status_line(text: &str) -> String {
    let dim = Csi::Sgr(Sgr::Intensity(Intensity::Dim)).to_string();
    let normal = Csi::Sgr(Sgr::Intensity(Intensity::Normal)).to_string();
    format!("{dim}{text}{normal}\n")
}

/// Extract the property-key order from a JSON Schema object, if any.
///
/// Returns `Some(keys)` when `schema.properties` is an object; `None`
/// when the schema is missing or has no `properties` field. Note that
/// `serde_json::Map` iteration is alphabetical unless the
/// `preserve_order` feature is enabled (the workspace does not enable
/// it) — schema authors who need insertion order must enable that
/// feature workspace-wide.
fn schema_property_order(schema: Option<&Value>) -> Option<Vec<String>> {
    schema?
        .get("properties")
        .and_then(Value::as_object)
        .map(|m| m.keys().cloned().collect())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use chrono::Utc;
    use norn::session::events::{EventBase, EventId, EventUsage};
    use serde_json::json;

    fn caps() -> TerminalCaps {
        TerminalCaps::baseline()
    }

    fn base() -> EventBase {
        EventBase {
            id: EventId::new(),
            parent_id: None,
            timestamp: Utc::now(),
        }
    }

    // ---------------- DisplayToggles (R3) ----------------

    #[test]
    fn display_toggles_default_shows_thinking_only() {
        let t = DisplayToggles::default();
        assert!(t.thinking_visible);
        assert!(!t.secondary_fields_visible);
    }

    #[test]
    fn display_toggles_toggle_flips_extra_output_layer() {
        let mut t = DisplayToggles::default();
        t.toggle();
        assert_eq!(
            t,
            DisplayToggles {
                thinking_visible: false,
                secondary_fields_visible: false,
            }
        );
        t.toggle();
        assert_eq!(
            t,
            DisplayToggles {
                thinking_visible: true,
                secondary_fields_visible: true,
            }
        );
    }

    #[test]
    fn display_toggles_status_text_reports_both() {
        let on = DisplayToggles {
            thinking_visible: true,
            secondary_fields_visible: true,
        };
        assert_eq!(on.status_text(), "thinking: on, details: on");
        let mut off = on;
        off.toggle();
        assert_eq!(off.status_text(), "thinking: off, details: off");
    }

    // ---------------- render_user_message (R5) ----------------

    #[test]
    fn user_message_contains_literal_prefix_and_text() {
        let out = render_user_message("hello", &caps());
        assert!(out.contains("> hello"), "got: {out:?}");
    }

    #[test]
    fn user_message_uses_foreground_colour_escape() {
        let out = render_user_message("hello", &caps());
        // Baseline caps → palette index escape.
        assert!(out.contains("38;5;"), "got: {out:?}");
    }

    #[test]
    fn user_message_multi_line_prefix_first_line_only() {
        let out = render_user_message("first\nsecond\nthird", &caps());
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].contains("> first"), "got: {:?}", lines[0]);
        assert_eq!(lines[1], "  second");
        assert_eq!(lines[2], "  third");
    }

    #[test]
    fn user_message_empty_still_renders_prefix() {
        let out = render_user_message("", &caps());
        assert!(out.contains("> "), "got: {out:?}");
    }

    // ---------------- render_thinking (R2) ----------------

    #[test]
    fn thinking_invisible_returns_empty() {
        let toggles = DisplayToggles {
            thinking_visible: false,
            secondary_fields_visible: false,
        };
        let out = render_thinking("inner monologue", &caps(), toggles, 80);
        assert!(out.is_empty(), "got: {out:?}");
    }

    #[test]
    fn thinking_visible_emits_dim_italic_sgr_and_prefix() {
        let toggles = DisplayToggles::default();
        let mut caps = caps();
        caps.italic_support = true;
        let out = render_thinking("considering", &caps, toggles, 80);
        assert!(out.contains("\x1b[2m"), "expected dim SGR: {out:?}");
        assert!(out.contains("\x1b[3m"), "expected italic SGR: {out:?}");
        assert!(out.contains("\x1b[23m"), "expected italic reset: {out:?}");
        assert!(out.contains("thinking: considering"));
    }

    #[test]
    fn thinking_preserves_blank_line_first_character() {
        let toggles = DisplayToggles::default();
        let out = render_thinking("one\n\nI need", &caps(), toggles, 80);
        assert!(out.contains("thinking: one\n\nI need"), "got: {out:?}");
        assert!(!out.contains("\n\n need"), "got: {out:?}");
    }

    #[test]
    fn thinking_falls_back_to_underline_without_italic_support() {
        let toggles = DisplayToggles::default();
        let out = render_thinking("considering", &caps(), toggles, 80);
        assert!(
            out.contains("\x1b[4m"),
            "expected underline fallback: {out:?}"
        );
        assert!(
            out.contains("\x1b[24m"),
            "expected underline reset: {out:?}"
        );
    }

    #[test]
    fn thinking_empty_visible_returns_empty() {
        let toggles = DisplayToggles::default();
        let out = render_thinking("", &caps(), toggles, 80);
        assert!(out.is_empty());
    }

    #[test]
    fn thinking_markdown_summary_renders_thought_about_heading() {
        let toggles = DisplayToggles::default();
        let out = render_thinking(
            "**Creating a markdown table**\n\nI need",
            &caps(),
            toggles,
            80,
        );
        assert!(out.contains("Thought about"), "got: {out:?}");
        assert!(out.contains("Creating a markdown table"), "got: {out:?}");
        assert!(out.contains("│"), "got: {out:?}");
        assert!(out.contains("I need"), "got: {out:?}");
        assert!(!out.contains("thinking:"), "got: {out:?}");
    }

    // ---------------- render_assistant_message (R1, R4) ----------------

    #[test]
    fn assistant_message_renders_markdown() {
        let out = render_assistant_message("**bold**", "", &caps(), DisplayToggles::default(), 80);
        assert!(out.contains("\x1b[1m"), "expected bold SGR: {out:?}");
    }

    #[test]
    fn assistant_message_includes_thinking_when_visible() {
        let toggles = DisplayToggles::default();
        let out = render_assistant_message("answer", "deliberating", &caps(), toggles, 80);
        assert!(out.contains("thinking: deliberating"), "got: {out:?}");
        assert!(out.contains("answer"), "got: {out:?}");
    }

    #[test]
    fn assistant_message_omits_thinking_when_hidden() {
        let toggles = DisplayToggles {
            thinking_visible: false,
            secondary_fields_visible: false,
        };
        let out = render_assistant_message("answer", "deliberating", &caps(), toggles, 80);
        assert!(!out.contains("deliberating"), "got: {out:?}");
    }

    #[test]
    fn structured_assistant_message_renders_primary_only_by_default() {
        let json = r#"{"text": "primary value", "extra": "secondary value"}"#;
        let out = render_assistant_message(json, "", &caps(), DisplayToggles::default(), 80);
        assert!(out.contains("primary value"), "got: {out:?}");
        assert!(!out.contains("secondary value"), "got: {out:?}");
        assert!(!out.contains("───"), "got: {out:?}");
    }

    #[test]
    fn structured_assistant_message_renders_secondary_when_visible() {
        let toggles = DisplayToggles {
            thinking_visible: true,
            secondary_fields_visible: true,
        };
        let json = r#"{"text": "primary value", "extra": "secondary value"}"#;
        let out = render_assistant_message(json, "", &caps(), toggles, 80);
        assert!(out.contains("primary value"), "got: {out:?}");
        assert!(out.contains("─── extra ───"), "got: {out:?}");
        assert!(out.contains("secondary value"), "got: {out:?}");
    }

    // ---------------- render_tool_call (R1) ----------------

    #[test]
    fn tool_call_dispatches_to_known_renderer() {
        let args = json!({"command": "echo hi"});
        let result = json!({"exit_code": 0, "stdout": "hi\n", "stderr": ""});
        let out = render_tool_call("bash", &args, &result, 42, &caps());
        assert!(out.contains("$ echo hi"), "got: {out:?}");
        assert!(out.contains("0.04s"), "got: {out:?}");
    }

    #[test]
    fn tool_call_unknown_falls_back_to_summary_line() {
        let args = Value::Null;
        let result = json!({"ok": true});
        let out = render_tool_call("mystery_tool", &args, &result, 0, &caps());
        assert!(out.contains("mystery_tool: "), "got: {out:?}");
        assert!(out.contains("\"ok\":true"), "got: {out:?}");
        assert!(out.ends_with('\n'), "got: {out:?}");
    }

    // ---------------- render_structured (R6) ----------------

    #[test]
    fn structured_primary_and_secondary_with_schema() {
        let value = json!({"summary": "short", "detail": "long form"});
        let schema = json!({
            "type": "object",
            "properties": {
                "summary": {"type": "string"},
                "detail": {"type": "string"},
            }
        });
        let out = render_structured(&value, Some(&schema), &caps(), true, 80);
        assert!(out.contains("short"), "got: {out:?}");
        assert!(out.contains("─── detail ───"), "got: {out:?}");
        assert!(out.contains("long form"), "got: {out:?}");
    }

    #[test]
    fn structured_secondary_hidden_omits_secondary_fields() {
        let value = json!({"summary": "short", "detail": "long form"});
        let schema = json!({
            "type": "object",
            "properties": {
                "summary": {"type": "string"},
                "detail": {"type": "string"},
            }
        });
        let out = render_structured(&value, Some(&schema), &caps(), false, 80);
        assert!(out.contains("short"), "got: {out:?}");
        assert!(!out.contains("long form"), "got: {out:?}");
        assert!(!out.contains("─── detail ───"), "got: {out:?}");
    }

    #[test]
    fn structured_picks_priority_key_when_present() {
        let value = json!({"extra": "secondary", "text": "primary"});
        let out = render_structured(&value, None, &caps(), false, 80);
        assert!(out.contains("primary"), "got: {out:?}");
        assert!(!out.contains("secondary"), "got: {out:?}");
    }

    #[test]
    fn structured_non_object_renders_pretty_json() {
        let value = json!(42);
        let out = render_structured(&value, None, &caps(), false, 80);
        assert!(out.contains("42"), "got: {out:?}");
        assert!(out.ends_with('\n'), "got: {out:?}");
    }

    #[test]
    fn structured_empty_object_renders_empty() {
        let value = json!({});
        let out = render_structured(&value, None, &caps(), true, 80);
        assert!(out.is_empty(), "got: {out:?}");
    }

    #[test]
    fn structured_picks_first_string_when_no_priority_match() {
        let value = json!({"alpha": 1, "beta": "the body", "gamma": 2});
        let out = render_structured(&value, None, &caps(), false, 80);
        assert!(out.contains("the body"), "got: {out:?}");
    }

    // ---------------- render_event (R1) ----------------

    #[test]
    fn render_event_dispatches_user_message() {
        let event = SessionEvent::UserMessage {
            base: base(),
            content: "hi".to_owned(),
        };
        let out = render_event(&event, &caps(), DisplayToggles::default(), 80);
        assert!(out.contains("> hi"), "got: {out:?}");
    }

    #[test]
    fn render_event_dispatches_assistant_message_to_markdown() {
        let event = SessionEvent::AssistantMessage {
            base: base(),
            content: "**bold**".to_owned(),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: vec![],
            usage: EventUsage::default(),
            stop_reason: String::new(),
            response_id: None,
        };
        let out = render_event(&event, &caps(), DisplayToggles::default(), 80);
        assert!(out.contains("\x1b[1m"), "got: {out:?}");
    }

    #[test]
    fn render_event_dispatches_tool_result_to_renderer() {
        let event = SessionEvent::ToolResult {
            base: base(),
            tool_call_id: "tc_1".to_owned(),
            tool_name: "bash".to_owned(),
            output: json!({"exit_code": 0, "stdout": "ok\n", "stderr": ""}),
            duration_ms: 100,
        };
        let out = render_event(&event, &caps(), DisplayToggles::default(), 80);
        assert!(out.contains("0.10s"), "got: {out:?}");
    }

    #[test]
    fn render_event_dispatches_spoken_response_to_structured() {
        let event = SessionEvent::SpokenResponse {
            base: base(),
            content: json!({"text": "spoken", "details": "extra"}),
        };
        let out = render_event(&event, &caps(), DisplayToggles::default(), 80);
        assert!(out.contains("spoken"), "got: {out:?}");
        assert!(!out.contains("extra"), "got: {out:?}");
    }

    #[test]
    fn render_event_dispatches_custom_with_event_type_header() {
        let event = SessionEvent::Custom {
            base: base(),
            event_type: "review".to_owned(),
            data: json!({"text": "looks good"}),
        };
        let out = render_event(&event, &caps(), DisplayToggles::default(), 80);
        assert!(out.contains("─── review ───"), "got: {out:?}");
        assert!(out.contains("looks good"), "got: {out:?}");
    }

    #[test]
    fn render_event_dispatches_model_change() {
        let event = SessionEvent::ModelChange {
            base: base(),
            old_model: "o".to_owned(),
            new_model: "n".to_owned(),
        };
        let out = render_event(&event, &caps(), DisplayToggles::default(), 80);
        assert!(out.contains("model: o → n"), "got: {out:?}");
        assert!(out.contains("\x1b[2m"), "expected dim: {out:?}");
    }

    #[test]
    fn render_event_dispatches_compaction() {
        let event = SessionEvent::Compaction {
            base: base(),
            summary: "rolled up 5".to_owned(),
            replaced_event_ids: vec![],
        };
        let out = render_event(&event, &caps(), DisplayToggles::default(), 80);
        assert!(out.contains("compaction: rolled up 5"), "got: {out:?}");
    }

    #[test]
    fn render_event_dispatches_fork() {
        let event = SessionEvent::Fork {
            base: base(),
            source_event_id: EventId::new(),
            forked_session_id: "sess_abc".to_owned(),
        };
        let out = render_event(&event, &caps(), DisplayToggles::default(), 80);
        assert!(out.contains("forked → sess_abc"), "got: {out:?}");
    }

    #[test]
    fn render_event_dispatches_label_with_description() {
        let event = SessionEvent::Label {
            base: base(),
            label: "checkpoint".to_owned(),
            description: Some("phase one done".to_owned()),
        };
        let out = render_event(&event, &caps(), DisplayToggles::default(), 80);
        assert!(
            out.contains("label: checkpoint — phase one done"),
            "got: {out:?}"
        );
    }

    #[test]
    fn render_event_dispatches_label_without_description() {
        let event = SessionEvent::Label {
            base: base(),
            label: "checkpoint".to_owned(),
            description: None,
        };
        let out = render_event(&event, &caps(), DisplayToggles::default(), 80);
        assert!(out.contains("label: checkpoint"), "got: {out:?}");
    }
}
