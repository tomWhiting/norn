//! Thin TUI adapter for the shared live MCP command surface.

use norn::integration::{McpControlHandle, execute_live_mcp_command, parse_live_mcp_command};

use crate::TuiError;
use crate::terminal::setup::TerminalGuard;

use super::slash::write_dim_line;

pub(super) async fn handle_mcp(
    arguments: &str,
    control: Option<&McpControlHandle>,
    guard: &mut TerminalGuard,
) -> Result<(), TuiError> {
    let result = match parse_live_mcp_command(arguments) {
        Ok(command) => execute_live_mcp_command(control, command).await,
        Err(error) => Err(error),
    };
    match result {
        Ok(lines) => {
            for line in lines {
                write_dim_line(&line, guard)?;
            }
        }
        Err(error) => write_dim_line(&format!("norn: {error}"), guard)?,
    }
    Ok(())
}
