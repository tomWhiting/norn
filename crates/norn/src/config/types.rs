//! Typed settings schema for `~/.norn/settings.json`, `.norn/settings.json`,
//! and `.norn/settings.local.json`.
//!
//! This module is the shared vocabulary for the loader, merger, and builder.
//! Every field is [`Option`] so partial JSON deserialises cleanly and merge
//! semantics can treat `None` as "inherit from the lower-precedence layer".
//! Duration fields are stored as [`Option<String>`] and parsed by
//! `humantime::parse_duration` at validation time (NC-003) rather than at
//! deserialisation time — that keeps this module dependency-free and lets
//! the loader produce typed errors that name the offending field.
//!
//! No `Default` implementation is hand-written: deriving `Default` yields
//! all-[`None`] / empty fields, which is the only correct behaviour under
//! Tom's NO ASSUMED DEFAULTS edict — every concrete default must come from
//! either compiled-in constants or the layer above.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Root
// ---------------------------------------------------------------------------

/// Root settings object as deserialised from a single `settings.json` file.
///
/// One [`NornSettings`] value is produced per file (user, project, local) and
/// the loader (NC-003) folds them together according to the five-layer
/// precedence rule from `DESIGN.md` D2:
///
/// ```text
/// compiled defaults < user < project < local < CLI overrides
/// ```
///
/// Field ordering matches `DESIGN.md` and the brief's R1 acceptance list
/// verbatim — the merger reads fields left-to-right and downstream snapshot
/// tests rely on stable serialisation order.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NornSettings {
    /// Default model identifier (e.g. `"gpt-5.5"`). Maps to the `--model`
    /// CLI flag and to [`crate::profile::Profile::model`] when no profile
    /// override is present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// User-defined short aliases for model selections. Aliases are resolved
    /// before provider construction; exact model IDs still win over aliases
    /// when the name is ambiguous.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_aliases: Option<BTreeMap<String, ModelAliasSettings>>,

    /// Named provider deployment profiles. A profile selects an API shape
    /// and supplies provider connection settings for that deployment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_profiles: Option<BTreeMap<String, ProviderProfileSettings>>,

    /// Provider connection, retry, and runner-binary settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<ProviderSettings>,

    /// Agent-loop configuration: turn limits, timeouts, compaction
    /// thresholds, reasoning hints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentSettings>,

    /// Provider-call retry policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetrySettings>,

    /// Consent-boundary permission rules — `allow`, `deny`, `ask` patterns.
    /// Distinct from the capability-boundary `tools` allow-list on
    /// [`crate::profile::Profile`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<PermissionSettings>,

    /// Hook entries grouped by the five [`HookRegistry`](crate::integration::hooks)
    /// trait slots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hooks: Option<HookSettings>,

    /// Per-tool configuration namespaced by tool name. See [`ToolSettings`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolSettings>,

    /// MCP server definitions keyed by user-chosen server name.
    ///
    /// Uses [`BTreeMap`] (not [`std::collections::HashMap`]) so JSON
    /// round-trips emit a stable, deterministic ordering — required for the
    /// snapshot tests in NC-003 and beyond.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_servers: Option<BTreeMap<String, McpServerSettings>>,

    /// Skill discovery configuration — see [`SkillsSettings`]. The internal
    /// structure of skills is owned by a separate cluster (NG3 in
    /// `DESIGN.md`); this struct only exposes the search-path surface.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<SkillsSettings>,

    /// Context-discovery configuration — see [`ContextSettings`]. Internal
    /// structure of context is owned by Harry's context cluster (NG2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextSettings>,

    /// Session-scope settings (cleanup retention, REPL history capacity).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionSettings>,

    /// Opaque pass-through value reserved for the TUI cluster (NG6).
    ///
    /// Stored as raw [`serde_json::Value`] so this crate does not need to
    /// understand the TUI schema. Chop Suey's cluster defines its own
    /// typed view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tui: Option<serde_json::Value>,

    /// Environment-variable overrides applied to spawned subprocesses
    /// (provider runners, hook commands, MCP transports). Uses
    /// [`BTreeMap`] for deterministic JSON ordering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
}

/// User-defined model alias target.
///
/// The compact string form is convenient for simple aliases:
///
/// ```json
/// { "model_aliases": { "55": "gpt-5.5" } }
/// ```
///
/// The object form reserves room for full backend selections:
///
/// ```json
/// {
///   "model_aliases": {
///     "local": {
///       "provider_profile": "lmstudio_local",
///       "api_shape": "openai_chat_completions",
///       "model": "google/gemma-4-e4b"
///     }
///   }
/// }
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ModelAliasSettings {
    /// Alias targets only a model id.
    Model(String),
    /// Alias targets a future full backend selection.
    Selection(ModelAliasSelection),
}

impl ModelAliasSettings {
    /// Returns the aliased model id.
    #[must_use]
    pub fn model(&self) -> &str {
        match self {
            Self::Model(model) => model,
            Self::Selection(selection) => &selection.model,
        }
    }

    /// Returns the optional provider profile id.
    #[must_use]
    pub fn provider_profile(&self) -> Option<&str> {
        match self {
            Self::Model(_) => None,
            Self::Selection(selection) => selection.provider_profile.as_deref(),
        }
    }

    /// Returns the optional API shape id.
    #[must_use]
    pub fn api_shape(&self) -> Option<&str> {
        match self {
            Self::Model(_) => None,
            Self::Selection(selection) => selection.api_shape.as_deref(),
        }
    }
}

/// Full model alias target shape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelAliasSelection {
    /// Optional provider profile id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_profile: Option<String>,
    /// Optional API shape id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_shape: Option<String>,
    /// Provider model id.
    pub model: String,
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

/// Named provider deployment profile.
///
/// The provider fields are flattened intentionally so a profile has the same
/// spelling as the top-level `provider` section, plus an optional `api_shape`:
///
/// ```json
/// {
///   "provider_profiles": {
///     "lmstudio": {
///       "api_shape": "openai_chat_completions",
///       "base_url": "http://localhost:1234/v1",
///       "api_key_env": "NORN_OPENAI_COMPAT_API_KEY"
///     }
///   }
/// }
/// ```
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProviderProfileSettings {
    /// API shape exposed by this provider profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_shape: Option<String>,

    /// Provider connection settings for this profile.
    #[serde(flatten)]
    pub provider: ProviderSettings,
}

/// Provider connection settings.
///
/// Each field maps to a key in the existing
/// [`norn_cli::config::assembly::ProviderConfigOverrides`](../../../norn_cli/src/config/assembly.rs)
/// surface: the loader (NC-003) folds [`ProviderSettings`] onto that struct
/// before CLI overrides are layered on top.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProviderSettings {
    /// Base URL of the provider endpoint (e.g.
    /// `"https://api.openai.com/v1"`). Maps to `ConfigOverrides.base_url`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,

    /// Per-request timeout as a `humantime` duration string (e.g.
    /// `"30s"`, `"2m"`). Parsed by `humantime::parse_duration` in NC-003;
    /// stored as [`String`] here. Maps to `ConfigOverrides.request_timeout`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,

    /// Provider-level maximum retry count (distinct from
    /// [`RetrySettings::max_retries`], which governs the agent-loop retry
    /// policy). Maps to `ConfigOverrides.max_retries`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u32>,

    /// Provider-specific extension knobs. Mirrors
    /// `ConfigOverrides.provider_options`: a free-form JSON object the
    /// downstream provider can interpret as it pleases.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<serde_json::Value>,

    /// Environment variable that holds an API key for API-key based
    /// providers. This stores the variable name only, never the secret
    /// value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,

    /// Authentication mode selector. Recognised values are `"oauth"`,
    /// `"api_key"`, and `"env"` per `DESIGN.md` D12. Validation of the
    /// string and the actual secret resolution (env var lookup, codex
    /// auth.json read) live in NC-003 and runtime wiring — this struct
    /// holds the raw string only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<String>,

    /// Rate-limit cap in requests per minute. Provides an override for the
    /// hardcoded `60 req/min` in `provider/openai/mod.rs`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<u32>,

    /// Replenishment window over which [`Self::rate_limit`] permits are
    /// granted, as a `humantime` duration string (e.g. `"60s"`, `"1m"`).
    /// Maps to `ProviderConfig::rate_limit_interval`; absent defers to the
    /// library's owner-approved 60-second default (permits-per-minute
    /// semantics). Must be non-zero — validated in NC-003.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_interval: Option<String>,

    /// Backoff applied to a `429` response that carries no parseable
    /// `Retry-After` header, as a `humantime` duration string (e.g.
    /// `"1s"`). Maps to `ProviderConfig::retry_backoff`; absent defers to
    /// the library's owner-approved 1-second default. Must be non-zero —
    /// validated in NC-003.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_backoff: Option<String>,

    /// Optional ceiling on accepted server-supplied `Retry-After` waits,
    /// as a `humantime` duration string (e.g. `"2m"`). Maps to
    /// `ProviderConfig::retry_after_ceiling`; absent honors the header
    /// as-is (the library deliberately has no built-in ceiling). Must be
    /// non-zero — validated in NC-003.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ceiling: Option<String>,

    /// Filesystem path to the Claude Runner binary (overrides the default
    /// `"claude"` lookup in `print/provider.rs`). Stored as [`String`]; the
    /// CLI converts to [`std::path::PathBuf`] at the runtime boundary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_path: Option<String>,

    /// Directory in which `--debug-api` writes the JSONL request/response
    /// dump. Stored as [`String`] (file-derived); the CLI's
    /// `ConfigOverrides.debug_dump_dir` is `Option<PathBuf>` — a different,
    /// downstream layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debug_dump_dir: Option<String>,
}

// ---------------------------------------------------------------------------
// Agent loop
// ---------------------------------------------------------------------------

/// Agent-loop configuration.
///
/// Mirrors the agent-loop subset of
/// [`norn_cli::config::assembly::ConfigOverrides`](../../../norn_cli/src/config/assembly.rs):
/// every field corresponds 1:1 to a `-c key=value` CLI override. Duration
/// fields are [`Option<String>`] (`humantime` format) and translated in
/// NC-003; the reasoning hints are stored as raw strings and translated to
/// [`crate::provider::request::ReasoningEffort`] /
/// [`crate::provider::request::ReasoningSummary`] there too.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AgentSettings {
    /// Hard cap on the number of model turns per agent run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,

    /// Per-step timeout as a `humantime` duration string (e.g. `"30s"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_timeout: Option<String>,

    /// Maximum number of validation/repair attempts for a single structured
    /// output (mirrors `ConfigOverrides.schema_budget`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_budget: Option<u32>,

    /// Token-based context window size hint. Used by the auto-compaction
    /// trigger in the loop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,

    /// Compaction trigger as a fraction of [`Self::context_window`] (e.g.
    /// `0.85`). Range `0.0..1.0`; validated in NC-003.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact_threshold: Option<f64>,

    /// Number of trailing turns preserved verbatim when compaction fires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact_keep_turns: Option<usize>,

    /// Conversation state policy (`manual` or `provider_threaded`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_state: Option<String>,

    /// Absolute server-side compaction threshold in rendered tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_compaction_threshold_tokens: Option<u64>,

    /// Reasoning-effort hint, stored as a raw string here (e.g. `"low"`,
    /// `"medium"`, `"high"`). Translated to
    /// [`crate::provider::request::ReasoningEffort`] in NC-003.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,

    /// Reasoning-summary verbosity, stored as a raw string here.
    /// Translated to [`crate::provider::request::ReasoningSummary`] in
    /// NC-003.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_summary: Option<String>,

    /// Service tier hint, stored as a raw string here (e.g. `"fast"`).
    /// Translated to [`crate::provider::request::ServiceTier`] in
    /// runtime assembly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,

    /// Timeout for `prompt_commands` shell invocations as a `humantime`
    /// duration string. Overrides the hardcoded `5s` in
    /// `loop/loop_context.rs`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_command_timeout: Option<String>,
}

// ---------------------------------------------------------------------------
// Retry policy
// ---------------------------------------------------------------------------

/// Retry-policy settings consumed by [`crate::r#loop::retry`].
///
/// Distinct from [`ProviderSettings::max_retries`]: the provider layer
/// retries connection-level failures, while [`RetrySettings`] governs the
/// agent-loop's response to provider-reported errors.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RetrySettings {
    /// Maximum retry attempts. Overrides the hardcoded `2` in
    /// `loop/retry.rs:16`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u32>,

    /// Base delay between retries as a `humantime` duration string (e.g.
    /// `"1s"`). Overrides the hardcoded `1s` backoff in `loop/retry.rs:18`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_delay: Option<String>,

    /// Exponential backoff multiplier applied between successive retries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backoff_multiplier: Option<f64>,
}

// ---------------------------------------------------------------------------
// Permissions (consent boundary)
// ---------------------------------------------------------------------------

/// Consent-boundary permission rules.
///
/// Patterns follow the Claude Code rule syntax (`tool_name`,
/// `tool_name(pattern)`, `*` wildcards in both segments). Evaluation order
/// is deny > ask > allow, first match wins; this struct only holds the raw
/// patterns — [`crate::config::permissions::PermissionPolicy`] compiles
/// them and the tool dispatch path enforces the result before every tool
/// execution. See the [`crate::config::permissions`] module docs for the
/// exact grammar (argument patterns match the call's top-level string
/// argument values).
///
/// Runtime behaviour:
///
/// - **deny** — the call is blocked with a structured tool error the
///   model sees; a deny match cannot be un-denied by any allow or ask
///   pattern (lower-precedence layers union into `deny`, CO6).
/// - **ask** — the embedded loop has no interactive consent surface, so
///   an ask match is routed through the registered
///   [`PreToolHook`](crate::integration::hooks::PreToolHook) chain when
///   at least one pre-tool hook exists: a `Block` outcome refuses the
///   call, any other outcome is taken as consent. When **no** pre-tool
///   hook is registered the call is blocked with a
///   "requires consent; no interactive handler" error.
/// - **allow** — short-circuits evaluation and permits the call. Calls
///   matching no pattern at all also proceed; permissions add deny/ask
///   restrictions on top of the capability boundary, they are not a
///   second availability gate.
///
/// Distinct from the *capability* boundary (the
/// [`crate::profile::Profile::tools`] allow-list), which controls which
/// tools the model sees. [`PermissionSettings`] controls whether a tool
/// call is allowed to execute.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PermissionSettings {
    /// Patterns that auto-allow a tool call without prompting. Concatenated
    /// across precedence layers (deduplicated) per `DESIGN.md` D3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow: Option<Vec<String>>,

    /// Patterns that hard-block a tool call. Unioned across layers — a
    /// lower-precedence deny cannot be un-denied by a higher layer (D3,
    /// constraint CO6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deny: Option<Vec<String>>,

    /// Patterns that require operator confirmation before a tool call
    /// executes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask: Option<Vec<String>>,
}

// ---------------------------------------------------------------------------
// Hooks
// ---------------------------------------------------------------------------

/// Hook entries grouped by all
/// [`HookRegistry`](crate::integration::hooks) trait slots.
///
/// Field names map 1:1 to the storage names on `HookRegistry` and the
/// `snake_case` [`crate::integration::hooks::HookEventType`] variant names
/// (`pre_tool`, `post_tool`, …). Hook arrays merge by extending (not
/// replacing) lower-precedence layers, per `DESIGN.md` D3 / D8.
///
/// NH-003 extended this struct from the original five slots (NC-002) to
/// thirteen, covering every lifecycle event the design enumerates. Field
/// ordering matches [`crate::integration::hooks::HookEventType`] variant
/// order and the `DESIGN.md` D6 taxonomy table — downstream snapshot tests
/// and the merger rely on stable order.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HookSettings {
    /// Hooks fired before each tool invocation. Matched by tool-name
    /// pattern in [`HookEntry::matcher`]. Can block execution (exit 2) or
    /// modify tool arguments (NH-002 `HookOutcome::Modify`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_tool: Option<Vec<HookEntry>>,

    /// Hooks fired after each tool invocation succeeds. Matched by
    /// tool-name pattern. Observation only — return value does not block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_tool: Option<Vec<HookEntry>>,

    /// Hooks fired after a tool invocation returns an error output.
    /// Matched by tool-name pattern. Observation only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_tool_failure: Option<Vec<HookEntry>>,

    /// Hooks fired before each LLM request. Matched by model name. Can
    /// block (exit 2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_llm: Option<Vec<HookEntry>>,

    /// Hooks fired after each LLM response. Matched by model name.
    /// Observation only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_llm: Option<Vec<HookEntry>>,

    /// Hooks fired on every session-event append. Matched by event
    /// variant name (e.g. `"UserMessage"`, `"ToolResult"`). Shell-command
    /// hooks fire fire-and-forget; trait hooks are synchronous (CO8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_event: Option<Vec<HookEntry>>,

    /// Hooks fired when a user (or orchestrator) prompt enters the agent
    /// loop, before initial message construction. Can block (exit 2).
    /// No matcher input — always fires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_prompt: Option<Vec<HookEntry>>,

    /// Hooks fired when the model would stop. Can block (exit 2) to force
    /// the agent loop to continue. No matcher input — always fires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<HookEntry>>,

    /// Hooks fired when a sub-agent is launched. Matched by profile or
    /// agent-type string. Observation only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_start: Option<Vec<HookEntry>>,

    /// Hooks fired when a sub-agent would complete. Matched by profile or
    /// agent-type string. Can block (exit 2) to keep the sub-agent
    /// running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_stop: Option<Vec<HookEntry>>,

    /// Hooks fired at session construction. Observation only.
    /// No matcher input — always fires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_start: Option<Vec<HookEntry>>,

    /// Hooks fired at session teardown. Observation only.
    /// No matcher input — always fires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_end: Option<Vec<HookEntry>>,

    /// Hooks fired before auto-compaction runs. Can block (exit 2) to
    /// prevent compaction. No matcher input — always fires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_compaction: Option<Vec<HookEntry>>,
}

/// A single hook entry — inline shell command plus optional matcher and
/// timeout.
///
/// [`Self::command`] is REQUIRED (not [`Option`]) — a hook without a command
/// has no behaviour and is rejected at load time. NC-003 catches the empty
/// string and produces a typed error.
///
/// Per `DESIGN.md` D5, [`Self::command`] is an inline shell string, not a
/// file path. If the operator wants a script, they reference it explicitly:
/// `command: "/path/to/script.sh --flag"`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HookEntry {
    /// Tool-name or event-type pattern restricting when this hook fires.
    /// [`None`] means "fire for every event in this slot".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,

    /// Shell command executed when the hook fires. Required (not
    /// optional) — empty / absent commands are rejected by NC-003.
    pub command: String,

    /// Optional execution timeout in milliseconds (D5 is silent on the
    /// unit; the brief specifies `u64` and defers the humantime-vs-integer
    /// choice). [`None`] means "use the runtime default".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
}

// ---------------------------------------------------------------------------
// Tools (per-tool configuration)
// ---------------------------------------------------------------------------

/// Per-tool configuration, namespaced by tool name.
///
/// The `write` slot has a typed schema ([`WriteToolSettings`]) because its
/// fields are consumed by [`crate::tools::write`]. `bash` and `edit` are
/// deliberately opaque [`serde_json::Value`] for forward compatibility —
/// downstream tool clusters will replace them with typed sub-structs as
/// their schemas stabilise.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ToolSettings {
    /// Configuration for the `write` tool. See [`WriteToolSettings`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write: Option<WriteToolSettings>,

    /// Configuration for the `bash` tool. Reserved opaque object; the
    /// `bash` cluster will replace this with a typed sub-struct.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bash: Option<serde_json::Value>,

    /// Configuration for the `edit` tool. Reserved opaque object; the
    /// `edit` cluster will replace this with a typed sub-struct.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit: Option<serde_json::Value>,
}

/// Settings for the `write` tool — global default for the per-profile
/// `tool_config.write` map.
///
/// The libnorn-level mirror of
/// [`norn_cli::runtime::wiring::WriteToolSpec`](../../../norn_cli/src/runtime/wiring.rs).
/// Downstream (NC-004) replaces the CLI-side `WriteToolSpec` with this type
/// so settings and CLI overrides share one shape.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WriteToolSettings {
    /// Default ceiling on the number of code lines a single `write`
    /// invocation may emit. Overrides the implicit "no limit" default in
    /// [`crate::tools::write::LengthLimit`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_code_lines: Option<usize>,

    /// Per-path overrides applied on top of [`Self::max_code_lines`]. The
    /// first matching glob wins (matching is performed by NC-004 when this
    /// list is folded into the runtime's [`crate::tools::write::LengthLimit`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length_overrides: Option<Vec<LengthOverrideEntry>>,
}

/// A single per-path length-override entry.
///
/// Mirrors
/// [`norn_cli::runtime::wiring::LengthOverrideSpec`](../../../norn_cli/src/runtime/wiring.rs).
/// Both fields are required — a partially-specified entry has no meaning,
/// and validation in NC-003 rejects missing patterns or limits via serde's
/// own required-field handling.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LengthOverrideEntry {
    /// Glob pattern that selects the paths this override applies to.
    /// Required.
    pub pattern: String,

    /// Code-line limit for files matching [`Self::pattern`]. Required.
    pub limit: usize,
}

// ---------------------------------------------------------------------------
// MCP servers
// ---------------------------------------------------------------------------

/// A single MCP server definition.
///
/// Both transport styles (subprocess `stdio` and remote `http`/`sse`) are
/// representable: subprocess servers populate [`Self::command`]/[`Self::args`]
/// and remote servers populate [`Self::url`]/[`Self::headers`]. Validation
/// of the combination lives in NC-003 / the MCP cluster (NG4).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct McpServerSettings {
    /// Transport identifier (e.g. `"stdio"`, `"sse"`, `"http"`). The MCP
    /// cluster owns the canonical list of values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,

    /// Subprocess executable path or name (used when
    /// [`Self::transport`] selects a stdio-style transport).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// Subprocess arguments passed to [`Self::command`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,

    /// Endpoint URL for remote transports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,

    /// Environment variables injected into the subprocess. [`BTreeMap`]
    /// for deterministic JSON ordering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,

    /// HTTP headers attached to remote-transport requests. [`BTreeMap`]
    /// for deterministic JSON ordering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
}

// ---------------------------------------------------------------------------
// Skills / Context / Session
// ---------------------------------------------------------------------------

/// Skill-discovery configuration.
///
/// Only the search-path surface is exposed here — discovery logic,
/// activation rules, and the on-disk skill format are owned by the skills
/// cluster (NG3 in `DESIGN.md`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SkillsSettings {
    /// Additional directories searched for skill definitions, in addition
    /// to the built-in locations the skills cluster ships.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_paths: Option<Vec<String>>,
}

/// Context-discovery configuration.
///
/// Only the search-path surface is exposed here — internal context
/// structure is owned by Harry's context cluster (NG2 in `DESIGN.md`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ContextSettings {
    /// Directories scanned for context fragments (e.g. CLAUDE.md, AGENTS.md).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_paths: Option<Vec<String>>,
}

/// Session-scope settings.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SessionSettings {
    /// Retention window, in days, for session JSONL files under
    /// `~/.norn/sessions/`. The session cluster's cleanup task deletes
    /// records older than this. [`None`] means "retain indefinitely".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup_days: Option<u32>,

    /// REPL history capacity (entries). Overrides the hardcoded `1000` in
    /// `repl/history.rs:18`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_capacity: Option<usize>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_const_for_fn,
    clippy::needless_pass_by_value,
    clippy::uninlined_format_args,
    clippy::unnecessary_literal_bound
)]
mod tests {
    use super::*;

    #[test]
    fn default_yields_all_none() {
        let settings = NornSettings::default();
        assert!(settings.model.is_none());
        assert!(settings.provider.is_none());
        assert!(settings.agent.is_none());
        assert!(settings.retry.is_none());
        assert!(settings.permissions.is_none());
        assert!(settings.hooks.is_none());
        assert!(settings.tools.is_none());
        assert!(settings.mcp_servers.is_none());
        assert!(settings.skills.is_none());
        assert!(settings.context.is_none());
        assert!(settings.session.is_none());
        assert!(settings.tui.is_none());
        assert!(settings.env.is_none());

        // Spot-check nested defaults too — each sub-struct's Default must
        // be all-None per Tom's NO ASSUMED DEFAULTS edict.
        let prov = ProviderSettings::default();
        assert!(prov.base_url.is_none());
        assert!(prov.timeout.is_none());
        assert!(prov.max_retries.is_none());
        assert!(prov.options.is_none());
        assert!(prov.api_key_env.is_none());
        assert!(prov.auth.is_none());
        assert!(prov.rate_limit.is_none());
        assert!(prov.rate_limit_interval.is_none());
        assert!(prov.retry_backoff.is_none());
        assert!(prov.retry_after_ceiling.is_none());
        assert!(prov.runner_path.is_none());
        assert!(prov.debug_dump_dir.is_none());

        let agent = AgentSettings::default();
        assert!(agent.max_turns.is_none());
        assert!(agent.step_timeout.is_none());
        assert!(agent.schema_budget.is_none());
        assert!(agent.context_window.is_none());
        assert!(agent.compact_threshold.is_none());
        assert!(agent.compact_keep_turns.is_none());
        assert!(agent.reasoning_effort.is_none());
        assert!(agent.reasoning_summary.is_none());
        assert!(agent.service_tier.is_none());
        assert!(agent.prompt_command_timeout.is_none());

        let retry = RetrySettings::default();
        assert!(retry.max_retries.is_none());
        assert!(retry.base_delay.is_none());
        assert!(retry.backoff_multiplier.is_none());

        let perm = PermissionSettings::default();
        assert!(perm.allow.is_none());
        assert!(perm.deny.is_none());
        assert!(perm.ask.is_none());

        let hooks = HookSettings::default();
        assert!(hooks.pre_tool.is_none());
        assert!(hooks.post_tool.is_none());
        assert!(hooks.post_tool_failure.is_none());
        assert!(hooks.pre_llm.is_none());
        assert!(hooks.post_llm.is_none());
        assert!(hooks.session_event.is_none());
        assert!(hooks.user_prompt.is_none());
        assert!(hooks.stop.is_none());
        assert!(hooks.subagent_start.is_none());
        assert!(hooks.subagent_stop.is_none());
        assert!(hooks.session_start.is_none());
        assert!(hooks.session_end.is_none());
        assert!(hooks.pre_compaction.is_none());

        let tools = ToolSettings::default();
        assert!(tools.write.is_none());
        assert!(tools.bash.is_none());
        assert!(tools.edit.is_none());

        let write = WriteToolSettings::default();
        assert!(write.max_code_lines.is_none());
        assert!(write.length_overrides.is_none());

        let mcp = McpServerSettings::default();
        assert!(mcp.transport.is_none());
        assert!(mcp.command.is_none());
        assert!(mcp.args.is_none());
        assert!(mcp.url.is_none());
        assert!(mcp.env.is_none());
        assert!(mcp.headers.is_none());

        let sk = SkillsSettings::default();
        assert!(sk.search_paths.is_none());

        let ctx = ContextSettings::default();
        assert!(ctx.search_paths.is_none());

        let sess = SessionSettings::default();
        assert!(sess.cleanup_days.is_none());
        assert!(sess.history_capacity.is_none());
    }

    #[test]
    fn empty_json_deserialises_to_default() {
        let s: NornSettings = serde_json::from_str("{}").unwrap();
        // Equality on NornSettings is structural via fields-are-None.
        // We assert each top-level field individually rather than deriving
        // PartialEq for the public struct (serde_json::Value isn't Eq).
        assert!(s.model.is_none());
        assert!(s.model_aliases.is_none());
        assert!(s.provider_profiles.is_none());
        assert!(s.provider.is_none());
        assert!(s.agent.is_none());
        assert!(s.retry.is_none());
        assert!(s.permissions.is_none());
        assert!(s.hooks.is_none());
        assert!(s.tools.is_none());
        assert!(s.mcp_servers.is_none());
        assert!(s.skills.is_none());
        assert!(s.context.is_none());
        assert!(s.session.is_none());
        assert!(s.tui.is_none());
        assert!(s.env.is_none());
    }

    #[test]
    fn partial_json_deserialises_cleanly() {
        // Only `model` and `agent.max_turns` are set — everything else
        // must come back None. This is the canonical NC-002 brief check
        // for partial-JSON tolerance.
        let json = r#"{"model":"gpt-5.5","agent":{"max_turns":5}}"#;
        let s: NornSettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.model.as_deref(), Some("gpt-5.5"));
        let agent = s.agent.expect("agent must deserialise");
        assert_eq!(agent.max_turns, Some(5));
        assert!(agent.step_timeout.is_none());
        assert!(agent.schema_budget.is_none());
        assert!(agent.context_window.is_none());
        assert!(agent.compact_threshold.is_none());
        assert!(agent.compact_keep_turns.is_none());
        assert!(agent.reasoning_effort.is_none());
        assert!(agent.reasoning_summary.is_none());
        assert!(agent.service_tier.is_none());
        assert!(agent.prompt_command_timeout.is_none());
        assert!(s.provider.is_none());
        assert!(s.retry.is_none());
        assert!(s.permissions.is_none());
        assert!(s.hooks.is_none());
        assert!(s.tools.is_none());
        assert!(s.mcp_servers.is_none());
        assert!(s.skills.is_none());
        assert!(s.context.is_none());
        assert!(s.session.is_none());
        assert!(s.tui.is_none());
        assert!(s.env.is_none());
    }

    #[test]
    fn serde_round_trip_through_json() {
        // Construct a fully-populated NornSettings — every public field
        // exercised so the round-trip catches field-name typos in serde
        // derive and any silent omissions.
        let mut mcp = BTreeMap::new();
        mcp.insert(
            "fs".to_owned(),
            McpServerSettings {
                transport: Some("stdio".to_owned()),
                command: Some("mcp-fs".to_owned()),
                args: Some(vec!["--root".to_owned(), "/tmp".to_owned()]),
                url: None,
                env: Some({
                    let mut m = BTreeMap::new();
                    m.insert("LOG".to_owned(), "info".to_owned());
                    m
                }),
                headers: None,
            },
        );

        let mut env = BTreeMap::new();
        env.insert("OPENAI_LOG".to_owned(), "debug".to_owned());

        let mut model_aliases = BTreeMap::new();
        model_aliases.insert(
            "55".to_owned(),
            ModelAliasSettings::Model("gpt-5.5".to_owned()),
        );
        model_aliases.insert(
            "local".to_owned(),
            ModelAliasSettings::Selection(ModelAliasSelection {
                provider_profile: Some("lmstudio_local".to_owned()),
                api_shape: Some("openai_chat_completions".to_owned()),
                model: "google/gemma-4-e4b".to_owned(),
            }),
        );

        let mut provider_profiles = BTreeMap::new();
        provider_profiles.insert(
            "lmstudio_local".to_owned(),
            ProviderProfileSettings {
                api_shape: Some("openai_chat_completions".to_owned()),
                provider: ProviderSettings {
                    base_url: Some("http://localhost:1234/v1".to_owned()),
                    api_key_env: Some("NORN_OPENAI_COMPAT_API_KEY".to_owned()),
                    ..ProviderSettings::default()
                },
            },
        );

        let original = NornSettings {
            model: Some("gpt-5.5".to_owned()),
            model_aliases: Some(model_aliases),
            provider_profiles: Some(provider_profiles),
            provider: Some(ProviderSettings {
                base_url: Some("https://api.example.com".to_owned()),
                timeout: Some("30s".to_owned()),
                max_retries: Some(3),
                options: Some(serde_json::json!({"alpha":1})),
                api_key_env: Some("LOCAL_AI_KEY".to_owned()),
                auth: Some("oauth".to_owned()),
                rate_limit: Some(120),
                rate_limit_interval: Some("90s".to_owned()),
                retry_backoff: Some("500ms".to_owned()),
                retry_after_ceiling: Some("2m".to_owned()),
                runner_path: Some("/usr/local/bin/claude".to_owned()),
                debug_dump_dir: Some("/tmp/norn-debug".to_owned()),
            }),
            agent: Some(AgentSettings {
                max_turns: Some(10),
                step_timeout: Some("45s".to_owned()),
                schema_budget: Some(4),
                context_window: Some(200_000),
                compact_threshold: Some(0.85),
                compact_keep_turns: Some(8),
                conversation_state: Some("provider_threaded".to_owned()),
                server_compaction_threshold_tokens: Some(200_000),
                reasoning_effort: Some("high".to_owned()),
                reasoning_summary: Some("detailed".to_owned()),
                service_tier: Some("fast".to_owned()),
                prompt_command_timeout: Some("10s".to_owned()),
            }),
            retry: Some(RetrySettings {
                max_retries: Some(5),
                base_delay: Some("2s".to_owned()),
                backoff_multiplier: Some(1.5),
            }),
            permissions: Some(PermissionSettings {
                allow: Some(vec!["read".to_owned(), "edit".to_owned()]),
                deny: Some(vec!["bash(rm -rf*)".to_owned()]),
                ask: Some(vec!["bash(git push*)".to_owned()]),
            }),
            hooks: Some(HookSettings {
                pre_tool: Some(vec![HookEntry {
                    matcher: Some("write".to_owned()),
                    command: "lint-check.sh".to_owned(),
                    timeout: Some(5000),
                }]),
                post_tool: None,
                post_tool_failure: None,
                pre_llm: None,
                post_llm: None,
                session_event: Some(vec![HookEntry {
                    matcher: Some("start".to_owned()),
                    command: "log-start.sh".to_owned(),
                    timeout: None,
                }]),
                user_prompt: None,
                stop: None,
                subagent_start: None,
                subagent_stop: None,
                session_start: None,
                session_end: None,
                pre_compaction: None,
            }),
            tools: Some(ToolSettings {
                write: Some(WriteToolSettings {
                    max_code_lines: Some(500),
                    length_overrides: Some(vec![LengthOverrideEntry {
                        pattern: "**/*.md".to_owned(),
                        limit: 2000,
                    }]),
                }),
                bash: Some(serde_json::json!({"timeout":"60s"})),
                edit: None,
            }),
            mcp_servers: Some(mcp),
            skills: Some(SkillsSettings {
                search_paths: Some(vec!["./skills".to_owned()]),
            }),
            context: Some(ContextSettings {
                search_paths: Some(vec!["./docs".to_owned()]),
            }),
            session: Some(SessionSettings {
                cleanup_days: Some(30),
                history_capacity: Some(500),
            }),
            tui: Some(serde_json::json!({"theme":"dark"})),
            env: Some(env),
        };

        let json = serde_json::to_string(&original).unwrap();
        let roundtripped: NornSettings = serde_json::from_str(&json).unwrap();

        assert_eq!(roundtripped.model, original.model);
        assert_eq!(roundtripped.model_aliases, original.model_aliases);
        let rprofiles = roundtripped.provider_profiles.as_ref().unwrap();
        let oprofiles = original.provider_profiles.as_ref().unwrap();
        assert_eq!(
            rprofiles["lmstudio_local"].api_shape,
            oprofiles["lmstudio_local"].api_shape,
        );
        assert_eq!(
            rprofiles["lmstudio_local"].provider.base_url,
            oprofiles["lmstudio_local"].provider.base_url,
        );
        assert_eq!(
            rprofiles["lmstudio_local"].provider.api_key_env,
            oprofiles["lmstudio_local"].provider.api_key_env,
        );
        let rp = roundtripped.provider.as_ref().unwrap();
        let op = original.provider.as_ref().unwrap();
        assert_eq!(rp.base_url, op.base_url);
        assert_eq!(rp.timeout, op.timeout);
        assert_eq!(rp.api_key_env, op.api_key_env);
        assert_eq!(rp.rate_limit, op.rate_limit);
        assert_eq!(rp.rate_limit_interval, op.rate_limit_interval);
        assert_eq!(rp.retry_backoff, op.retry_backoff);
        assert_eq!(rp.retry_after_ceiling, op.retry_after_ceiling);
        assert_eq!(rp.runner_path, op.runner_path);
        let ra = roundtripped.agent.as_ref().unwrap();
        let oa = original.agent.as_ref().unwrap();
        assert_eq!(ra.max_turns, oa.max_turns);
        assert_eq!(ra.step_timeout, oa.step_timeout);
        assert_eq!(ra.reasoning_effort, oa.reasoning_effort);
        let rr = roundtripped.retry.as_ref().unwrap();
        let or_ = original.retry.as_ref().unwrap();
        assert_eq!(rr.max_retries, or_.max_retries);
        assert_eq!(rr.base_delay, or_.base_delay);
        let rperm = roundtripped.permissions.as_ref().unwrap();
        let operm = original.permissions.as_ref().unwrap();
        assert_eq!(rperm.deny, operm.deny);
        let rh = roundtripped.hooks.as_ref().unwrap();
        let oh = original.hooks.as_ref().unwrap();
        assert_eq!(
            rh.pre_tool.as_ref().unwrap()[0].command,
            oh.pre_tool.as_ref().unwrap()[0].command
        );
        let rt = roundtripped.tools.as_ref().unwrap().write.as_ref().unwrap();
        let ot = original.tools.as_ref().unwrap().write.as_ref().unwrap();
        assert_eq!(rt.max_code_lines, ot.max_code_lines);
        assert_eq!(
            rt.length_overrides.as_ref().unwrap()[0].pattern,
            ot.length_overrides.as_ref().unwrap()[0].pattern,
        );
        assert_eq!(
            roundtripped
                .mcp_servers
                .as_ref()
                .unwrap()
                .get("fs")
                .unwrap()
                .command,
            original
                .mcp_servers
                .as_ref()
                .unwrap()
                .get("fs")
                .unwrap()
                .command,
        );
        assert_eq!(
            roundtripped.skills.as_ref().unwrap().search_paths,
            original.skills.as_ref().unwrap().search_paths,
        );
        assert_eq!(
            roundtripped.session.as_ref().unwrap().history_capacity,
            original.session.as_ref().unwrap().history_capacity,
        );
        assert_eq!(
            roundtripped.env.as_ref().unwrap().get("OPENAI_LOG"),
            original.env.as_ref().unwrap().get("OPENAI_LOG"),
        );
        assert_eq!(roundtripped.tui, original.tui);
    }
}
