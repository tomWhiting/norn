//! Post-tool diagnostic check.
//!
//! Folder module split (LD-014): the entry point and per-mutation logic
//! live in `post_check`; supporting concerns live in dedicated siblings
//! to keep each file under the project-wide 500-LoC clippy gate.

mod adapters;
mod findings;
mod infra;
mod loc;
mod lsp_diagnostics;
mod lsp_test_exec;
mod lsp_tests;
mod post_check;
mod remediation;
mod server_query;
mod stop_hook;
mod trigger;

#[cfg(test)]
mod tests;

pub use self::infra::DiagnosticInfra;
pub use self::post_check::{
    DiagnosticsPostCheck, errors_to_diagnostic_json, run_diagnostics_for_trigger,
};
pub use self::stop_hook::DiagnosticStopHook;
