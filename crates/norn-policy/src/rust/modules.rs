//! Pure Rust module and include reachability over an owned snapshot.

mod analyze;
mod attributes;
mod generated;
mod literal;
mod model;
mod path;
mod plans;
mod scan;
mod trybuild;
mod walk;

pub use analyze::analyze_modules;
pub(crate) use analyze::analyze_modules_with_cargo;
pub use generated::generated_invocation_digest;
pub use model::{
    CompileTestExpectation, CompileTestFixtureFact, FileReachability, GeneratedIncludeRegistration,
    GeneratedIncludeRegistry, HashedSourceInput, ModuleAnalysis, ModuleDiagnostic,
    ModuleDiagnosticCode, ModuleTargetIdentity, ModuleTargetKind, SourceSpan,
};
