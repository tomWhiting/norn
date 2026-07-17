use norn::agent_loop::config::AgentStepResult;
use norn::provider::usage::Usage;

use crate::TuiError;
use crate::render::scroll_region::write_to_scroll;
use crate::terminal::setup::TerminalGuard;

use super::AppState;
use crate::app::helpers::flush_terminal;

/// Extract the usage field from any completed agent-step outcome.
pub fn extract_usage(result: &AgentStepResult) -> Usage {
    match result {
        AgentStepResult::Completed { usage, .. }
        | AgentStepResult::Refused { usage, .. }
        | AgentStepResult::SchemaUnreachable { usage, .. }
        | AgentStepResult::MaxIterationsReached { usage, .. }
        | AgentStepResult::Cancelled { usage, .. }
        | AgentStepResult::TimedOut { usage, .. }
        | AgentStepResult::Truncated { usage, .. } => usage.clone(),
    }
}

/// Write a red error line into the scroll region.
pub(crate) fn write_error_line(
    state: &AppState,
    guard: &mut TerminalGuard,
    message: &str,
) -> Result<(), TuiError> {
    let red = crate::render::style::colour_for(
        termina::style::RgbColor::new(200, 80, 80),
        &state.terminal_caps,
    );
    let reset = termina::escape::csi::Csi::Sgr(termina::escape::csi::Sgr::Reset).to_string();
    let line = format!("{red}error: {message}{reset}\n");
    write_to_scroll(&line, guard.terminal_mut())?;
    guard.note_scroll_newlines(&line)?;
    flush_terminal(guard)
}
