//! Tool-row presentation delegates to the shared retained summary contract.

use norn::session_view::ToolView;

/// Resolve the caller-owned expansion setting without reading any body.
#[must_use]
pub fn label(tool: &ToolView, expanded: bool) -> String {
    crate::tools::summary::summarize(tool, expanded).header()
}
