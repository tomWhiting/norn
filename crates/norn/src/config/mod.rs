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
//!   hook-extend, MCP-by-name, tool-deep-merge).
//! - [`validate`] — semantic validation of a merged settings value:
//!   duration strings, permission patterns, MCP server shape.
//! - [`permissions`] — compiled [`PermissionPolicy`] evaluating tool
//!   calls against the merged allow/deny/ask patterns; consumed by tool
//!   dispatch as the runtime consent boundary.

pub mod loader;
pub mod merge;
pub mod paths;
pub mod permissions;
pub mod types;
pub mod validate;

pub use loader::{LoadedSettings, load_settings, local_settings_path, project_settings_path};
pub use merge::merge_settings;
pub use permissions::{PermissionDecision, PermissionPolicy};
pub use types::{
    AgentSettings, ContextSettings, HookEntry, HookSettings, LengthOverrideEntry,
    McpServerSettings, NornSettings, PermissionSettings, ProviderSettings, RetrySettings,
    SessionSettings, SkillsSettings, ToolSettings, WriteToolSettings,
};
pub use validate::validate_settings;
