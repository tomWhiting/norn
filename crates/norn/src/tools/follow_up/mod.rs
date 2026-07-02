//! `follow_up` — execute a deferred action registered by a prior tool call.
//!
//! Tools register [`FollowUpAction`](crate::tool::follow_up::FollowUpAction)s
//! at the end of their lifecycle (e.g. `undo`, `apply_structural`). The model
//! executes one by calling `follow_up` with the original `tool_call_id` and
//! the action's `action` name. The tool looks the reference up in the session
//! action log, checks the action's expiry against current file/turn state,
//! merges the action's pre-populated argument overrides onto the original
//! call's arguments, and dispatches the target tool through the registry's
//! full lifecycle. This eliminates re-generating large arguments (patch text,
//! file content) by reading them from the action log.
//!
//! Submodules:
//! * [`tool`] — the [`FollowUpTool`] trait surface.
//! * [`lookup`] — action-log query and action selection.
//! * [`expiry`] — expiry evaluation against current file and turn state.
//! * [`merge`] — shallow argument-override merge.
//! * [`dispatch`] — orchestration and target-tool lifecycle dispatch.

pub mod dispatch;
pub mod expiry;
pub mod lookup;
pub mod merge;
mod tool;

#[cfg(test)]
mod tests;

pub use self::dispatch::{CurrentTurnId, SharedToolRegistry};
pub use self::tool::FollowUpTool;
