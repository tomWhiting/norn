//! The [`ToolRenderer`] trait and the [`renderer_for`] factory.
//!
//! Every tool that produces output the human should see gets a renderer.
//! A renderer turns the JSON-shaped tool arguments and result — the
//! shapes emitted by `crates/norn/src/tools/*` — into ANSI-styled text:
//! a one-line header (which doubles as the collapsed view) and an
//! optional multi-line body.
//!
//! The trait is deliberately object-safe — every method takes `&self`,
//! has no generic parameters, and never returns `Self` — so
//! [`renderer_for`] can hand back a `Box<dyn ToolRenderer>` keyed on the
//! upstream `Tool::name()`.

use serde_json::Value;

use super::compact::{
    LspRenderer, SkillRenderer, TaskRenderer, ToolSearchRenderer, WebFetchRenderer,
    WebSearchRenderer, WriteRenderer,
};
use super::minimal::{
    CloseAgentRenderer, ForkRenderer, SendMessageRenderer, SpawnAgentRenderer, WaitAgentRenderer,
};
use super::rich::{ApplyPatchRenderer, BashRenderer, EditRenderer, ReadRenderer, SearchRenderer};
use crate::render::content::ContentBlock;
use crate::terminal::caps::TerminalCaps;

/// Renders a single tool call's header line and optional expandable body.
///
/// Implementors are fed the JSON arguments the model supplied and the
/// JSON result the tool produced (`ProviderEvent::ToolResult.output`).
/// All styling decisions route through [`TerminalCaps`] so output adapts
/// to the terminal's colour depth.
pub trait ToolRenderer {
    /// One-line header for a completed tool call.
    ///
    /// `args` come from `ToolCallComplete` or accumulated
    /// `ToolCallDelta` chunks; `result` is `ProviderEvent::ToolResult`'s
    /// `output`. The returned string is both the live header and the
    /// collapsed view.
    fn header_line(
        &self,
        args: &Value,
        result: &Value,
        duration_ms: u64,
        caps: &TerminalCaps,
    ) -> String;

    /// Expandable body for a completed tool call.
    ///
    /// Returns `None` for tools that have no expanded view, or when the
    /// completed call produced nothing worth showing.
    fn body(&self, args: &Value, result: &Value, caps: &TerminalCaps) -> Option<String>;

    /// Semantic content blocks for the tool body.
    ///
    /// Returns structured [`ContentBlock`] values that the rendering
    /// pipeline turns into syntax-highlighted, diff-coloured, or
    /// severity-styled output. The default wraps [`Self::body`] in a
    /// [`ContentBlock::Plain`]; tool renderers that carry a file path
    /// override this to produce [`ContentBlock::Code`] or
    /// [`ContentBlock::Diff`] for proper highlighting.
    fn body_blocks<'a>(
        &self,
        _args: &'a Value,
        _result: &'a Value,
        _caps: &TerminalCaps,
    ) -> Option<Vec<ContentBlock<'a>>> {
        None
    }

    /// Header shown in the scroll region while the tool is still
    /// executing, before its `ToolResult` arrives.
    ///
    /// `partial_args` is the best-effort accumulation of `ToolCallDelta`
    /// argument fragments — it may not yet be valid JSON.
    fn streaming_header(&self, name: &str, partial_args: &str, caps: &TerminalCaps) -> String;
}

/// Returns the [`ToolRenderer`] for `tool_name`, or `None` for tools
/// with no renderer.
///
/// Keys match the upstream `Tool::name()` exactly.
#[must_use]
pub fn renderer_for(tool_name: &str) -> Option<Box<dyn ToolRenderer>> {
    match tool_name {
        // Tier 1 — rich
        "bash" => Some(Box::new(BashRenderer)),
        "edit" => Some(Box::new(EditRenderer)),
        "apply_patch" => Some(Box::new(ApplyPatchRenderer)),
        "search" => Some(Box::new(SearchRenderer)),
        "read" => Some(Box::new(ReadRenderer)),
        // Tier 2 — compact
        "write" => Some(Box::new(WriteRenderer)),
        "web_search" => Some(Box::new(WebSearchRenderer)),
        "web_fetch" => Some(Box::new(WebFetchRenderer)),
        "lsp" => Some(Box::new(LspRenderer)),
        "task" => Some(Box::new(TaskRenderer)),
        "skill" => Some(Box::new(SkillRenderer)),
        "tool_search" => Some(Box::new(ToolSearchRenderer)),
        // Tier 3 — minimal
        "spawn_agent" => Some(Box::new(SpawnAgentRenderer)),
        "fork" => Some(Box::new(ForkRenderer)),
        "send_message" => Some(Box::new(SendMessageRenderer)),
        "wait_agent" => Some(Box::new(WaitAgentRenderer)),
        "close_agent" => Some(Box::new(CloseAgentRenderer)),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn factory_returns_renderer_for_known_tools() {
        let all = [
            "bash",
            "edit",
            "apply_patch",
            "search",
            "read",
            "write",
            "web_search",
            "web_fetch",
            "lsp",
            "task",
            "skill",
            "tool_search",
            "spawn_agent",
            "fork",
            "send_message",
            "wait_agent",
            "close_agent",
        ];
        for name in all {
            assert!(
                renderer_for(name).is_some(),
                "expected a renderer for `{name}`",
            );
        }
    }

    #[test]
    fn factory_returns_none_for_unknown_tool() {
        assert!(renderer_for("unknown").is_none());
        assert!(renderer_for("").is_none());
        assert!(renderer_for("applypatch").is_none());
    }

    #[test]
    fn renderer_is_object_safe() {
        let renderer: Box<dyn ToolRenderer> = renderer_for("bash").unwrap();
        let header = renderer.streaming_header("bash", "{}", &TerminalCaps::baseline());
        assert!(header.contains('$'));
    }
}
