//! CLI-side runtime wiring that maps onto library [`AgentBuilder`] inputs.
//!
//! After R1 collapsed runtime assembly onto the single library-owned
//! [`AgentBuilder`](norn::agent::AgentBuilder), this module holds only the
//! genuinely CLI-side helpers that translate the parsed CLI surface into
//! builder inputs or read off the assembled
//! [`AgentParts`](norn::agent::AgentParts):
//!
//! 1. [`cli_coordination_envelope`] — the CLI's deliberate
//!    [`CoordinationEnvelope`] (child policy + channel capacities) each
//!    driver passes to `.child_policy()` / `.child_result_capacity()` /
//!    `.inbound_capacity()`.
//! 2. [`length_limit_from_profile`] / [`build_write_tool`] — resolve the
//!    profile `[tool_config.write]` section and the `-c
//!    write.max_code_lines=N` override into a length-limited [`WriteTool`]
//!    the driver overlays via `.tool(..)`.
//! 3. [`build_slash_state_from_bundle`] / [`build_slash_state_with_schema`]
//!    — build the slash-command surface from the decoupled
//!    [`SlashStateInputs`] read off `AgentParts`.

use std::sync::Arc;

use norn::agent::child_policy::{
    ChildPolicy, CoordinationEnvelope, DelegationBudget, MessagingScope,
};
use norn::agent_loop::commands::SlashCommandRegistry;
use norn::profile::Profile;
use norn::session::store::EventStore;
use norn::tool::registry::ToolRegistry;
use norn::tools::write::{LengthLimit, WriteTool};
use serde::Deserialize;
use serde_json::Value;

use crate::cli::BuildError;
use crate::cli::Cli;
use crate::commands::slash::state::SlashStateSeed;
use crate::commands::slash::{SlashState, build_slash_registry};
use crate::config::AppliedOverrides;
use crate::config::ConfigOverrides;
use crate::config::parse_inline_or_file;
use crate::config::parse_kv;
use crate::config::session_data_dir;

#[cfg(test)]
mod inline_tests;

/// The owner-ruled default root delegation depth when neither the `[agent]
/// delegation_depth` setting nor `-c delegation_depth=<u32>` is set: `2`
/// (children may spawn one level of their own; grandchildren are leaves).
/// DECISIONS §0.6(d).
pub const DEFAULT_DELEGATION_DEPTH: u32 = 2;

/// The CLI's deliberate [`CoordinationEnvelope`]: the child policy and
/// channel capacities every agent spawned from a CLI-assembled runtime
/// runs under.
///
/// The values are the Wave 3 design's documented proposals — a conscious
/// per-deployment choice by the CLI, never a library default (the library
/// requires every embedder to supply its own envelope):
///
/// - `messaging: SiblingsAndParent` — the audit trail and the steer/update
///   split are the safety mechanism, not isolation (DECISION M1).
/// - `remaining_depth: delegation_depth` — the root's own delegation
///   budget, resolved from `[agent] delegation_depth` / `-c
///   delegation_depth`, defaulting to [`DEFAULT_DELEGATION_DEPTH`] (`2`,
///   owner-ruled — DECISIONS §0.6(d)). The inherit-with-decrement and
///   narrowing-only invariants are unchanged; this only seeds the root.
/// - `max_concurrent_children: 32` — today's production-proven concurrency
///   cap (DECISION R1).
/// - `inbound_capacity: 32` — the per-child inbound backpressure buffer
///   (DECISION M4).
/// - `child_result_capacity: 256` — the child-result channel buffer
///   (DECISION R3); the CLI's root result channel is sized from this same
///   value so the two cannot drift.
#[must_use]
pub fn cli_coordination_envelope(delegation_depth: u32) -> CoordinationEnvelope {
    CoordinationEnvelope {
        child_policy: ChildPolicy {
            messaging: MessagingScope::SiblingsAndParent,
            delegation: DelegationBudget {
                remaining_depth: delegation_depth,
                max_concurrent_children: 32,
            },
            inbound_capacity: 32,
            loop_config: None,
        },
        child_result_capacity: 256,
    }
}

/// The `(flag, name)` pairs from `--allowed-tools` / `--disallowed-tools`
/// that match no physically-registered tool in the assembled agent. Pure
/// core of [`warn_unmatched_tool_flag_names`], split out for testing.
///
/// The reference is [`ToolRegistry::is_registered`] — the *physical*
/// registration, not the gated [`ToolRegistry::names`] view — so a
/// correctly denied tool (removed from the available set by
/// `--disallowed-tools`) is never reported: only names matching no real
/// tool at all appear.
fn unmatched_tool_flag_names<'a>(
    registry: &ToolRegistry,
    applied: &'a AppliedOverrides,
) -> Vec<(&'static str, &'a str)> {
    let mut unmatched = Vec::new();
    for (flag, names) in [
        ("--allowed-tools", &applied.allowed_tools),
        ("--disallowed-tools", &applied.disallowed_tools),
    ] {
        for name in names {
            if !registry.is_registered(name) {
                unmatched.push((flag, name.as_str()));
            }
        }
    }
    unmatched
}

/// Emit a visible stderr warning for every `--allowed-tools` /
/// `--disallowed-tools` name that matches no tool in the assembled agent.
///
/// Tool gating is exact-name, so a partial typo (`--allowed-tools
/// read,serch` silently narrows to `read`) or a wrong-case name would
/// otherwise enforce a different tool set than the user asked for with no
/// feedback. Run against the assembled agent's registry after `build()`.
/// It is a warning rather than a hard error because a name may legitimately
/// match a tool an `--extension` MCP server registers later.
pub fn warn_unmatched_tool_flag_names(registry: &ToolRegistry, applied: &AppliedOverrides) {
    for (flag, name) in unmatched_tool_flag_names(registry, applied) {
        eprintln!(
            "norn: warning: {flag} name '{name}' matches no registered tool \
             (names are case-sensitive and matched exactly); it takes effect \
             only if a tool with that exact name is registered later (e.g. by \
             an --extension MCP server)",
        );
    }
}

/// Resolve the [`LengthLimit`] applied to the `Write` tool from the
/// profile's `[tool_config.write]` section and an optional CLI override.
///
/// Resolution order:
///
/// 1. If the profile has no `tool_config.write` section, start from
///    [`LengthLimit::none`].
/// 2. Otherwise, deserialise it into [`WriteToolSpec`] — `max_code_lines`
///    becomes the default and `length_overrides` populate the
///    glob/limit pairs (in source order — first match wins per
///    [`LengthLimit::limit_for`]).
/// 3. If `cli_override` is `Some`, replace `default` with the CLI value.
///    Glob overrides from the profile are preserved.
///
/// # Errors
///
/// Returns [`BuildError::Argument`] when the profile section cannot be
/// deserialised (missing fields, wrong types) or when any
/// `length_overrides[i].pattern` fails [`glob::Pattern::new`].
pub fn length_limit_from_profile(
    profile: &Profile,
    cli_override: Option<usize>,
) -> Result<LengthLimit, BuildError> {
    let mut limit = match profile
        .settings
        .get("tool_config")
        .and_then(|tool_config| tool_config.get("write"))
    {
        Some(raw) => {
            let spec: WriteToolSpec = serde_json::from_value(raw.clone()).map_err(|err| {
                BuildError::Argument(format!(
                    "invalid [tool_config.write] profile section: {err} (expected optional \
                     max_code_lines: usize and optional length_overrides: \
                     [{{ pattern: string, limit: usize }}])",
                ))
            })?;
            spec.into_length_limit()?
        }
        None => LengthLimit::none(),
    };
    if let Some(value) = cli_override {
        limit.default = Some(value);
    }
    Ok(limit)
}

/// Construct a [`WriteTool`] whose [`LengthLimit`] is resolved from the
/// profile's `[tool_config.write]` section and the `-c
/// write.max_code_lines=N` override carried on [`ConfigOverrides`].
///
/// The returned tool is overlaid onto the library-assembled default
/// `WriteTool` via [`AgentBuilder::tool`](norn::agent::AgentBuilder::tool);
/// [`ToolRegistry::register`] keys on the tool name, so the configured
/// tool replaces the default-limit one.
///
/// # Errors
///
/// Propagates [`length_limit_from_profile`] errors verbatim.
pub fn build_write_tool(
    profile: &Profile,
    overrides: &ConfigOverrides,
) -> Result<WriteTool, BuildError> {
    let limit = length_limit_from_profile(profile, overrides.write_max_code_lines)?;
    Ok(WriteTool::with_length_limit(limit))
}

/// The assembled inputs a slash-command surface reads off an agent
/// bundle: the gated tool registry, the resolved model, and the resolved
/// service tier / reasoning effort. Decoupled from any concrete assembly
/// bundle so the assembled [`AgentParts`](norn::agent::AgentParts) feed the
/// same builder.
#[derive(Clone, Copy)]
pub struct SlashStateInputs<'a> {
    /// The gated tool registry whose names/descriptions populate the
    /// slash `/tools` surface.
    pub registry: &'a ToolRegistry,
    /// The resolved model identifier shown in the status surface and
    /// swapped by `/model`.
    pub model: &'a str,
    /// The resolved service tier, when set.
    pub service_tier: Option<norn::provider::request::ServiceTier>,
    /// The resolved reasoning effort, when set.
    pub reasoning_effort: Option<norn::provider::request::ReasoningEffort>,
}

/// Build a [`SlashState`] and [`SlashCommandRegistry`] from the assembled
/// slash inputs.
///
/// `index_lock_deadline` is the driver-resolved session index-lock
/// deadline (`ResolvedInvocation::index_lock_deadline`); the slash
/// handlers apply it to every lock-taking `SessionManager` they
/// construct (`/name`'s index rename), so a wedged sibling process can
/// never hang the interactive surface inside a handler.
pub fn build_slash_state_from_bundle(
    cli: &Cli,
    inputs: SlashStateInputs<'_>,
    store: Arc<EventStore>,
    session_id: Option<String>,
    index_lock_deadline: std::time::Duration,
) -> Result<(SlashState, SlashCommandRegistry), BuildError> {
    Ok(build_slash_state_inner(
        cli,
        inputs,
        store,
        session_id,
        index_lock_deadline,
        None,
        session_data_dir()?,
    ))
}

#[cfg(test)]
fn build_slash_state_from_bundle_at(
    cli: &Cli,
    inputs: SlashStateInputs<'_>,
    store: Arc<EventStore>,
    session_id: Option<String>,
    index_lock_deadline: std::time::Duration,
    data_dir: std::path::PathBuf,
) -> (SlashState, SlashCommandRegistry) {
    build_slash_state_inner(
        cli,
        inputs,
        store,
        session_id,
        index_lock_deadline,
        None,
        data_dir,
    )
}

/// Variant that accepts a pre-parsed output schema, avoiding a
/// redundant re-parse when the caller has already validated the
/// `--output-schema` flag (e.g. the print-mode orchestrator).
pub fn build_slash_state_with_schema(
    cli: &Cli,
    inputs: SlashStateInputs<'_>,
    store: Arc<EventStore>,
    session_id: Option<String>,
    index_lock_deadline: std::time::Duration,
    output_schema: Option<Value>,
) -> Result<(SlashState, SlashCommandRegistry), BuildError> {
    Ok(build_slash_state_inner(
        cli,
        inputs,
        store,
        session_id,
        index_lock_deadline,
        output_schema,
        session_data_dir()?,
    ))
}

fn build_slash_state_inner(
    cli: &Cli,
    inputs: SlashStateInputs<'_>,
    store: Arc<EventStore>,
    session_id: Option<String>,
    index_lock_deadline: std::time::Duration,
    output_schema_override: Option<Value>,
    data_dir: std::path::PathBuf,
) -> (SlashState, SlashCommandRegistry) {
    let tools: Vec<(String, String)> = inputs
        .registry
        .names()
        .filter_map(|name| {
            inputs
                .registry
                .get(name)
                .map(|tool| (tool.name().to_owned(), tool.description().to_owned()))
        })
        .collect();

    let variable_pairs = parse_variable_pairs(&cli.variables);
    let output_schema = output_schema_override
        .or_else(|| parse_output_schema_for_state(cli.output_schema.as_deref()));

    let seed = SlashStateSeed {
        model: inputs.model.to_owned(),
        service_tier: inputs.service_tier,
        reasoning_effort: inputs.reasoning_effort,
        output_schema,
        session_name: cli.session_name.clone(),
        session_id,
        data_dir,
        no_session: cli.no_session,
        index_lock_deadline,
        variable_pairs,
        tools,
        store,
    };
    let state = SlashState::new(seed);
    let registry = build_slash_registry(&state, None);
    (state, registry)
}

fn parse_variable_pairs(raw: &[String]) -> Vec<(String, String)> {
    raw.iter()
        .filter_map(|pair| match parse_kv(pair) {
            Ok(kv) => Some(kv),
            Err(err) => {
                tracing::warn!(
                    pair = %pair,
                    error = %err,
                    "skipping malformed --variables pair when building slash state",
                );
                None
            }
        })
        .collect()
}

fn parse_output_schema_for_state(raw: Option<&str>) -> Option<serde_json::Value> {
    let value = raw?;
    match parse_inline_or_file(value) {
        Ok(parsed) => Some(parsed),
        Err(err) => {
            tracing::warn!(
                raw = %value,
                error = %err,
                "skipping unparseable --output-schema when building slash state",
            );
            None
        }
    }
}

/// CLI-side mirror of the `[tool_config.write]` profile section.
///
/// Both fields are optional — a profile may set just `max_code_lines`,
/// just `length_overrides`, both, or neither — and any combination
/// resolves to a valid [`LengthLimit`].
#[derive(Debug, Default, Deserialize)]
struct WriteToolSpec {
    #[serde(default)]
    max_code_lines: Option<usize>,
    #[serde(default)]
    length_overrides: Vec<LengthOverrideSpec>,
}

/// One entry in `[tool_config.write.length_overrides]`. The `pattern`
/// must compile via [`glob::Pattern::new`]; failures surface as
/// [`BuildError::Argument`] naming the offending pattern.
#[derive(Debug, Deserialize)]
struct LengthOverrideSpec {
    pattern: String,
    limit: usize,
}

impl WriteToolSpec {
    fn into_length_limit(self) -> Result<LengthLimit, BuildError> {
        let mut limit = LengthLimit {
            default: self.max_code_lines,
            overrides: Vec::with_capacity(self.length_overrides.len()),
        };
        for entry in self.length_overrides {
            let pattern = glob::Pattern::new(&entry.pattern).map_err(|err| {
                BuildError::Argument(format!(
                    "invalid glob pattern '{}' in [tool_config.write.length_overrides]: {err}",
                    entry.pattern,
                ))
            })?;
            limit.overrides.push((pattern, entry.limit));
        }
        Ok(limit)
    }
}
