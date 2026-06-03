//! `norn init …` subcommand dispatcher (NTC-004).
//!
//! Thin dispatcher: matches the parsed [`InitCmd`] and delegates to the
//! per-target handler. Generation logic lives in the sibling modules
//! (e.g. [`conventions`]) so this file stays a routing seam.

pub mod conventions;
pub mod upgrade;

use crate::cli::{ExitCode, InitCmd};

/// Top-level dispatcher for `norn init`.
pub fn run_init(cmd: InitCmd) -> ExitCode {
    match cmd {
        InitCmd::Conventions {
            upgrade,
            input,
            output,
        } => {
            if upgrade {
                upgrade::run_upgrade(input, output)
            } else {
                conventions::run_conventions(output)
            }
        }
    }
}
