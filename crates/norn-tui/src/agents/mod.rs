//! Agent status lines, tree collapse, and multi-agent tabs.

pub mod activity_log;
pub mod status_line;
pub mod tabs;
pub mod tree;

pub use activity_log::{ActivityLog, ActivityLogEntry, IDLE_FADE};
pub use status_line::{AgentActivity, AgentStatusPanel, HOLD_DURATION, icon_for};
pub use tabs::TabState;
pub use tree::{CandidateEntry, CollapsedView, MAX_VISIBLE, RECENT_CHANGE_WINDOW, collapse};
