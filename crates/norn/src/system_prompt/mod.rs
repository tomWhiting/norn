//! System prompt construction for tool-equipped agents.
//!
//! Assembles the Norn base system prompt from identity, harness
//! capabilities, tool guidance (dynamic from the registry), safety rules,
//! agent coordination patterns, and communication style (mode-dependent).
//! Profile instructions layer on top after the base prompt.
//!
//! ## Assembly order
//!
//! 1. **Identity** — static one-liner, varies by execution mode.
//! 2. **Harness capabilities** — Norn-specific runtime behaviors (schema
//!    enforcement, tool lifecycle, session context, rules engine).
//!    Conditional on which features are configured.
//! 3. **Tools** — unified section dynamically generated from the tool
//!    registry. Groups tools by [`ToolCategory`](crate::tool::traits::ToolCategory)
//!    and includes each tool's description and usage guidance.
//! 4. **Safety** — universal action guidance (reversibility, confirmation).
//! 5. **Agent Coordination** — strategic guidance on fork/spawn/team
//!    patterns. Conditional on Agent-category tools being registered.
//! 6. **Communication** — output style, varies by execution mode.
//!
//! The following sections are dynamic (refreshed per-iteration) and
//! injected by the runner, not by the builder:
//!
//! - **Environment** — cwd, platform, time, git branch, session ID.
//! - **Collaboration Mode** — autonomous, plan, or default mode guidance.
//!
//! Profile `system_instructions` and dynamic sections (prompt commands,
//! rule injections, environment) are appended by the caller after the
//! base prompt.

pub mod builder;
pub mod environment;
pub mod sections;

pub use builder::{
    CollaborationMode, ExecutionMode, SystemPromptInputs, ToolPromptEntry, build_system_prompt,
};
pub use environment::{EnvironmentConfig, format_environment_section};
