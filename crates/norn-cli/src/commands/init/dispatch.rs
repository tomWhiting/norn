//! `norn init …` top-level dispatch.
//!
//! Matches the parsed [`InitCmd`] and delegates to the per-target handler.
//! Generation logic lives in the sibling modules (e.g.
//! [`conventions`](super::conventions)) so this routing seam stays thin.

use crate::cli::{ExitCode, InitCmd};

use super::{conventions, upgrade};

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
