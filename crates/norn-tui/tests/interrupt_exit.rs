//! Actual-App exit under active provider work preserves drafts and restores the terminal.

#[path = "support/retained_screen.rs"]
pub mod retained_screen;
#[path = "support/retained_workspace.rs"]
pub mod workspace_support;

use workspace_support::TestResult;

#[test]
fn retained_workspace_child_entrypoint() -> TestResult {
    workspace_support::child_entrypoint()
}

#[test]
fn active_cancel_keeps_draft_and_confirms_exit_without_releasing_provider() -> TestResult {
    workspace_support::verify_interrupt_exit(false)
}

#[test]
fn queued_ctrl_c_pair_exits_active_turn_and_restores_terminal() -> TestResult {
    workspace_support::verify_interrupt_exit(true)
}
