//! In-flight root turn execution for the TUI.
//!
//! [`run`] drives an agent turn end-to-end — seeding from a user prompt or
//! steered agent messages, then threading queued follow-ups and pending child
//! prompts. [`mid`] holds the mid-turn terminal-event and active-input handling
//! the run loop delegates to while an agent step is in flight.

mod mid;
mod run;

pub(super) use run::{run_pending_child_prompts, run_ready_root_inbound, run_turn_and_pending};
