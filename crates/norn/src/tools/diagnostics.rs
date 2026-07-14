//! Public diagnostics API facade for post-validation infrastructure.

pub use super::diagnostics_check::{
    DiagnosticInfra, DiagnosticStopHook, DiagnosticsPostCheck, errors_to_diagnostic_json,
    run_diagnostics_for_trigger,
};
pub use super::diagnostics_infra::build_diagnostic_infra;
