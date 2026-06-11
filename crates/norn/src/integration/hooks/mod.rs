//! Lifecycle hooks for pre/post tool, pre/post LLM, session events, and the
//! extended lifecycle boundaries added in NH-002.
//!
//! See [`traits`] for the original five hook traits, the [`HookOutcome`] enum,
//! and the [`HookRegistry`] that dispatches them. See [`new_traits`] for the
//! six additional hook traits covering user prompts, stop, sub-agent
//! lifecycle, session lifecycle, compaction, and tool failure.
//!
//! NH-003 adds the config-driven plumbing: [`config`] defines the
//! [`HookEventType`] taxonomy that maps `snake_case` config names to trait
//! dispatch; [`matchers`] compiles regex matchers at settings-load time;
//! [`loader`] reads the three settings tiers and produces a merged
//! [`crate::config::types::HookSettings`] ready for NH-005 (`ShellCommandHook`)
//! and NH-006 (CLI wiring).
//!
//! NH-004 adds the JSON wire protocol used between Norn and a shell hook
//! child process: [`input`] defines [`HookInput`] (serialised to stdin)
//! plus the `NORN_*` environment variable names; [`output`] defines
//! [`HookOutput`] (parsed from stdout) and its decision → [`HookOutcome`]
//! mapping. NH-005 (`ShellCommandHook`) composes these types — this layer
//! only owns the schema.

pub mod config;
pub mod input;
pub mod loader;
pub mod matchers;
pub mod merge;
pub mod new_traits;
pub mod output;
pub mod shell;
pub mod traits;

pub use config::HookEventType;
pub use input::{
    HookInput, NORN_AGENT_ID, NORN_HOOK_EVENT, NORN_PROFILE, NORN_PROJECT_DIR, NORN_SESSION_ID,
};
pub use loader::load_hooks_from_settings;
pub use matchers::HookMatcher;
pub use new_traits::{
    CompactionHook, PostToolFailureHook, SessionLifecycleHook, StopHook, SubagentHook,
    UserPromptHook,
};
pub use output::HookOutput;
pub use shell::{HookContext, ShellCommandHook};
pub use traits::{
    Hook, HookOutcome, HookRegistry, LlmCallSummary, PostLlmHook, PostToolHook, PreLlmHook,
    PreToolHook, SessionEventHook,
};
