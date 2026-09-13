//! Actual-App input and exit under continuous typed agent-message traffic.

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
fn message_flood_keeps_draft_editable_and_allows_confirmed_exit() -> TestResult {
    workspace_support::verify_message_flood()
}
