//! Norn configuration: directory resolution and typed settings schema.
//!
//! - [`paths`] — `~/.norn/` directory layout, honouring the `NORN_HOME`
//!   override. Lifted from `norn-cli` so libnorn consumers (profile
//!   loader, session manager, task store, REPL history) can resolve paths
//!   without depending on the CLI crate.
//! - [`types`] — typed schema for `settings.json` files. The loader, merger,
//!   and builder integration (NC-003 / NC-004) all reference these types;
//!   nothing in this module performs I/O.
//! - [`loader`] — file discovery and JSON parsing for the three on-disk
//!   layers (user, project, local). Missing files are tolerated;
//!   malformed JSON produces typed errors.
//! - [`merge`] — five-layer precedence merge with field-type-specific
//!   strategies (scalar override, deny-additive, allow-concatenate,
//!   hook-extend, MCP-by-name, tool-deep-merge). Runtime callers validate raw
//!   working-directory authority before invoking this mechanical merge.
//! - [`validate_working_directory_authority`] — provenance validation for raw
//!   user/project/local layers before repository-controlled values can grant
//!   credential, backend, command, or eager-file-read authority.
//! - [`validate`] — semantic validation of a merged settings value:
//!   duration strings, permission patterns, MCP server shape.
//! - [`permissions`] — compiled [`PermissionPolicy`] evaluating tool
//!   calls against the merged allow/deny/ask patterns; consumed by tool
//!   dispatch as the runtime consent boundary.

pub mod loader;
pub mod mcp;
pub mod mcp_approval;
mod mcp_local;
mod mcp_patch;
pub mod mcp_state;
mod mcp_state_types;
mod mcp_workspace_write;
pub(crate) mod merge;
pub mod paths;
pub mod permissions;
mod provider_security;
pub mod types;
pub mod validate;

pub use loader::{local_settings_path, project_settings_path};
pub use mcp::{
    McpConfigSource, McpDefinitionFingerprint, McpRuntimeOverrides, ResolvedMcpServer,
    ResolvedMcpServers, ResolvedSettings, load_resolved_settings,
};
pub use mcp_approval::{McpApprovalState, McpApprovalStore};
pub use mcp_local::project_local_mcp_settings_path;
pub use mcp_state::{McpConfigState, McpDefinitions};
pub use mcp_state_types::{
    EffectiveMcpServer, McpConfigLayer, McpConfigSnapshot, McpLayerEntry, McpPersistentChange,
    McpPersistentMutation, McpPersistentScope, McpServerInspection, McpSessionEntry,
};
pub(crate) use merge::merge_settings;
pub use permissions::{PermissionDecision, PermissionPolicy};
pub(crate) use provider_security::validate_working_directory_authority;
pub use types::{
    AgentSettings, AutoCompactReserve, ContextSettings, HookEntry, HookSettings,
    LengthOverrideEntry, McpServerSettings, ModelAliasSelection, ModelAliasSettings, NornSettings,
    PermissionSettings, ProviderProfileSettings, ProviderSettings, RetrySettings, SessionSettings,
    SkillToolSettings, SkillsSettings, ToolSettings, WriteToolSettings,
};
pub use validate::validate_settings;
