//! Compact retained tool facts; formatting never reads a body or invents a description.

use norn::session_view::{DisplayText, ToolState, ToolView};

/// Borrowed metadata for a known or unknown call at its existing expansion setting.
#[derive(Debug)]
pub struct ToolSummary<'a> {
    /// Original terminal-safe tool name, if supplied.
    pub name: Option<&'a DisplayText>,
    /// Original terminal-safe description, if supplied.
    pub description: Option<&'a DisplayText>,
    /// Recorded reason description extraction failed, separate from absence.
    pub description_error: Option<&'a DisplayText>,
    /// Actual call ID; a streaming item ID is never substituted.
    pub call_id: Option<&'a str>,
    /// Observed lifecycle, including incomplete coverage.
    pub state: ToolState,
    /// Observed result outcome even when invocation coverage is incomplete.
    pub result_state: Option<ToolState>,
    /// Reported duration; absence is not zero milliseconds.
    pub duration_ms: Option<u64>,
    /// Explicit commitment, independent of failure or diagnostics.
    pub committed: Option<bool>,
    /// Caller-resolved individual override or global expansion preference.
    pub expanded: bool,
}

/// Borrow only approved metadata; unknown tools follow the same compact policy.
#[must_use]
pub fn summarize(tool: &ToolView, expanded: bool) -> ToolSummary<'_> {
    ToolSummary {
        name: tool.name.as_ref(),
        description: tool.description.as_ref(),
        description_error: tool.description_error.as_ref(),
        call_id: tool.call_id.as_deref(),
        state: tool.state,
        result_state: tool.result_state,
        duration_ms: tool.duration_ms,
        committed: tool.committed,
        expanded,
    }
}

impl ToolSummary<'_> {
    /// Compact description and lifecycle, or full metadata when expanded.
    ///
    /// Newlines and tabs are visible escapes here; the borrowed original
    /// description remains intact for expansion and exact-body selection.
    #[must_use]
    pub fn header(&self) -> String {
        if self.expanded {
            return self.details_header();
        }
        let name = self.name_label();
        let description = self.description.map_or_else(
            || "description unavailable".to_owned(),
            |description| single_line(description.as_str()),
        );
        let mut parts = vec![
            format!("{name}: {description}"),
            state_label(self.state).to_owned(),
        ];
        if let Some(result) = self.result_state
            && result != self.state
        {
            parts.push(format!("result {}", state_label(result)));
        }
        if let Some(duration) = self.duration_ms {
            parts.push(format!("{duration}ms"));
        }
        parts.join(" · ")
    }

    /// Exact terminal-safe name used as the first field in either header form.
    #[must_use]
    pub fn name_label(&self) -> String {
        self.name.map_or_else(
            || "tool name unavailable".to_owned(),
            |name| single_line(name.as_str()),
        )
    }

    /// Full exact metadata for expanded or selected-call details.
    ///
    /// Missing evidence remains explicit here without repeating technical
    /// bookkeeping in every collapsed conversation row. This does not change
    /// the caller's individual expansion preference.
    #[must_use]
    pub fn details_header(&self) -> String {
        let name = self.name_label();
        let description = self.description.map_or_else(
            || "description unavailable".to_owned(),
            |description| single_line(description.as_str()),
        );
        let call = self.call_id.map_or_else(
            || "call ID unavailable".to_owned(),
            |call| format!("call {}", single_line(call)),
        );
        let duration = self.duration_ms.map_or_else(
            || "duration unavailable".to_owned(),
            |duration| format!("{duration} ms"),
        );
        let commitment = match self.committed {
            Some(true) => "committed",
            Some(false) => "not committed",
            None => "commit evidence unavailable",
        };
        let mut parts = vec![
            name,
            description,
            call,
            state_label(self.state).to_owned(),
            commitment.to_owned(),
            duration,
        ];
        if let Some(result) = self.result_state
            && result != self.state
        {
            parts.push(format!("result {}", state_label(result)));
        }
        if let Some(error) = self.description_error {
            parts.push(format!(
                "description error: {}",
                single_line(error.as_str())
            ));
        }
        parts.join(" · ")
    }
}

/// Exact lifecycle wording shared by compact rows and read-only call evidence.
#[must_use]
pub fn state_label(state: ToolState) -> &'static str {
    match state {
        ToolState::Assembling => "assembling",
        ToolState::Running => "running",
        ToolState::Completed => "completed",
        ToolState::Failed => "failed",
        ToolState::Blocked => "blocked",
        ToolState::Cancelled => "cancelled",
        ToolState::Incomplete => "incomplete",
    }
}

fn single_line(text: &str) -> String {
    DisplayText::new(text)
        .as_str()
        .replace('\n', "\\n")
        .replace('\t', "\\t")
}

#[cfg(test)]
#[path = "summary_tests.rs"]
mod tests;
