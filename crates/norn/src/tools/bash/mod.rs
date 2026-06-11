//! Bash tool — streaming subprocess execution with risk classification.
//!
//! Executes shell commands via `sh -c` using [`tokio::process::Command`],
//! spawned into their own process group on Unix so a timeout kills the
//! entire process tree. Stdout and stderr are drained concurrently and
//! captured into the result (bounded by a grace period after process
//! exit so backgrounded children cannot stall the tool), with each line
//! emitted as a `tracing::debug!` event for progress observability. The
//! compile-time pre-validate phase classifies the command's risk tier
//! using [`crate::tool::risk::classify_risk`] and embeds it in the
//! output metadata; blocking on risk is left to orchestrator policy.
//!
//! The model-supplied `working_dir` argument resolves through the tool
//! context (tilde expansion, relative paths against the agent working
//! directory) and is checked against the workspace-confinement root when
//! one is configured. The executed command itself is *not* confined —
//! shell commands can `cd` anywhere and touch absolute paths; that gap is
//! a known, documented limitation (see [`BashTool`]).

mod cd_track;
mod output;
mod process;
mod tool;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_follow_up;

pub use tool::BashTool;
