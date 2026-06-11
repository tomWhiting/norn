//! Five-layer settings merge.
//!
//! [`merge_settings`] folds four [`crate::config::types::NornSettings`]
//! layers (user, project, local, CLI) into a single resolved value. The
//! compiled-default floor (the fifth layer in `DESIGN.md` D2) is
//! represented implicitly: every field is [`Option`], and the merger never
//! invents values, so an all-`None` result correctly cedes the field to
//! downstream code that consults its built-in default.
//!
//! Merge rules vary by field type (`DESIGN.md` D3):
//!
//! - **Scalars** — higher-precedence non-`None` wins.
//! - **`permissions.deny`** — union across all layers (additive). CO6:
//!   you cannot un-deny.
//! - **`permissions.allow` / `.ask`** — concatenate across layers,
//!   deduplicate (first-seen wins).
//! - **`hooks.*`** — concatenate by event slot. Project hooks extend user
//!   hooks; they do not replace.
//! - **`mcp_servers`** — merge by name. Same-name later-layer entry
//!   *replaces* the earlier definition wholesale (no deep merge).
//! - **`tools.write`** — deep merge field-by-field. Sibling keys at
//!   different layers are preserved.
//! - **`tools.bash` / `tools.edit`** — opaque [`serde_json::Value`];
//!   scalar-wise override (no in-value merge).
//! - **Sub-structs** — when any layer has [`Some`], the result is
//!   `Some(merged_sub)` with each inner [`Option`] field merged
//!   scalar-wise.
//!
//! The CLI layer is the highest-precedence input. Callers (NC-004) project
//! CLI flags into a settings shell before invoking [`merge_settings`].
//!
//! Module layout:
//!
//! - `primitives` — layer-combination building blocks (scalar precedence
//!   pick, deduplicating concatenation, hook-slot concat).
//! - `scalar_sections` — sub-structs merged field-by-field with scalar
//!   precedence (provider, agent, retry, session, tools).
//! - `collection_sections` — sections with additive / keyed semantics
//!   (permissions, hooks, MCP servers, skills, context, env).
//! - `settings` — the [`merge_settings`] entry point.

mod collection_sections;
mod primitives;
mod scalar_sections;
mod settings;

pub use settings::merge_settings;

#[cfg(test)]
mod tests;
