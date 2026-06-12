//! Core tool implementations.

pub mod action_log;
mod action_log_scope_resolve;
pub mod agent;
pub mod agents;
mod ast;
pub mod bash;

mod confinement;
pub mod context_paths;
pub mod conventions;
pub mod diagnostics_check;
pub mod diagnostics_infra;
pub mod edit;
mod file_commit;
pub mod follow_up;
pub mod lsp;
pub mod patch;
mod patch_apply;
mod patch_cc;
mod patch_commit;
pub(crate) mod patch_entity;
mod patch_eol;
mod patch_followup;
mod patch_gate;
mod patch_hunk;
mod patch_match;
mod patch_modes;
mod patch_resolve;
mod patch_stage;

mod patch_parse;
pub mod read;
pub mod registry_builder;
pub mod search;
pub mod skill;
pub mod task;
pub mod tool_search;
pub mod validation;
pub mod web;
pub mod write;
pub use self::action_log::ActionLogTool;
pub use self::agent::{
    AgentHandle, AgentHandles, AgentToolInfra, CloseAgentTool, ForkTool, SendMessageTool,
    SpawnAgentTool,
};
pub use self::agents::AgentsTool;
pub use self::bash::BashTool;
pub use self::context_paths::ContextSearchPaths;
pub use self::conventions::ConventionsConfig;
pub use self::follow_up::FollowUpTool;

/// Public diagnostics API facade for post-validation infrastructure.
pub mod diagnostics {
    pub use super::diagnostics_check::{
        DiagnosticInfra, DiagnosticStopHook, DiagnosticsPostCheck, errors_to_diagnostic_json,
        run_diagnostics_for_trigger,
    };
    pub use super::diagnostics_infra::build_diagnostic_infra;
}

pub use self::lsp::{
    LspBackend, LspBackendError, LspDiagnostic, LspDiagnosticSeverity, LspHover, LspLocation,
    LspSymbol, LspSymbolKind, LspTool,
};
pub use self::patch::ApplyPatchTool;
#[cfg(feature = "libyggd-ast")]
pub use self::patch_entity::LibygdEntityExtractor;
pub use self::patch_entity::{EntityExtractor, ExtractedEntity};
pub use self::registry_builder::register_standard_tools;
pub use self::search::SearchTool;

pub use self::skill::{SkillSearchPaths, SkillTool};
pub use self::task::{
    DiskTaskStore, InMemoryTaskStore, SharedTaskStore, TaskEntry, TaskStatus, TaskStore, TaskTool,
};
pub use self::tool_search::ToolSearchTool;
pub use self::web::{WEB_SEARCH_TOOL_NAME, WebFetchTool, WebSearchTool};
