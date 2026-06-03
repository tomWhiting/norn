//! Five-layer settings merge.
//!
//! [`merge_settings`] folds four [`NornSettings`] layers (user, project,
//! local, CLI) into a single resolved [`NornSettings`]. The compiled-default
//! floor (the fifth layer in `DESIGN.md` D2) is represented implicitly:
//! every field is [`Option`], and the merger never invents values, so an
//! all-`None` result correctly cedes the field to downstream code that
//! consults its built-in default.
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
//! CLI flags into a [`NornSettings`] shell before invoking this function.
//!
//! Parameters are taken by `&mut` reference so inner fields can be moved
//! out via [`Option::take`] — zero-copy for all moved fields, and the
//! parameter types are small pointer-sized references rather than the
//! large `NornSettings` struct itself.

use std::collections::BTreeMap;

use crate::config::types::{
    AgentSettings, ContextSettings, HookEntry, HookSettings, McpServerSettings, NornSettings,
    PermissionSettings, ProviderSettings, RetrySettings, SessionSettings, SkillsSettings,
    ToolSettings, WriteToolSettings,
};

/// Merge four [`NornSettings`] layers in precedence order:
/// `user < project < local < cli`.
///
/// See module documentation for per-field merge semantics.
#[must_use]
pub fn merge_settings(
    usr: &mut NornSettings,
    prj: &mut NornSettings,
    lcl: &mut NornSettings,
    ovr: &mut NornSettings,
) -> NornSettings {
    NornSettings {
        model: pick_scalar(
            &mut usr.model,
            &mut prj.model,
            &mut lcl.model,
            &mut ovr.model,
        ),
        provider: merge_provider(
            &mut usr.provider,
            &mut prj.provider,
            &mut lcl.provider,
            &mut ovr.provider,
        ),
        agent: merge_agent(
            &mut usr.agent,
            &mut prj.agent,
            &mut lcl.agent,
            &mut ovr.agent,
        ),
        retry: merge_retry(
            &mut usr.retry,
            &mut prj.retry,
            &mut lcl.retry,
            &mut ovr.retry,
        ),
        permissions: merge_permissions(
            &mut usr.permissions,
            &mut prj.permissions,
            &mut lcl.permissions,
            &mut ovr.permissions,
        ),
        hooks: merge_hooks(
            &mut usr.hooks,
            &mut prj.hooks,
            &mut lcl.hooks,
            &mut ovr.hooks,
        ),
        tools: merge_tools(
            &mut usr.tools,
            &mut prj.tools,
            &mut lcl.tools,
            &mut ovr.tools,
        ),
        mcp_servers: merge_mcp_servers(
            &mut usr.mcp_servers,
            &mut prj.mcp_servers,
            &mut lcl.mcp_servers,
            &mut ovr.mcp_servers,
        ),
        skills: merge_skills(
            &mut usr.skills,
            &mut prj.skills,
            &mut lcl.skills,
            &mut ovr.skills,
        ),
        context: merge_context(
            &mut usr.context,
            &mut prj.context,
            &mut lcl.context,
            &mut ovr.context,
        ),
        session: merge_session(
            &mut usr.session,
            &mut prj.session,
            &mut lcl.session,
            &mut ovr.session,
        ),
        tui: pick_scalar(&mut usr.tui, &mut prj.tui, &mut lcl.tui, &mut ovr.tui),
        env: merge_env_map(&mut usr.env, &mut prj.env, &mut lcl.env, &mut ovr.env),
    }
}

/// Scalar precedence: highest-layer `Some` wins; `None` falls through.
///
/// Uses [`Option::take`] on the winning layer to move without cloning.
fn pick_scalar<T>(
    usr: &mut Option<T>,
    prj: &mut Option<T>,
    lcl: &mut Option<T>,
    ovr: &mut Option<T>,
) -> Option<T> {
    ovr.take()
        .or_else(|| lcl.take())
        .or_else(|| prj.take())
        .or_else(|| usr.take())
}

fn merge_provider(
    usr: &mut Option<ProviderSettings>,
    prj: &mut Option<ProviderSettings>,
    lcl: &mut Option<ProviderSettings>,
    ovr: &mut Option<ProviderSettings>,
) -> Option<ProviderSettings> {
    if usr.is_none() && prj.is_none() && lcl.is_none() && ovr.is_none() {
        return None;
    }
    let mut usr = usr.take().unwrap_or_default();
    let mut prj = prj.take().unwrap_or_default();
    let mut lcl = lcl.take().unwrap_or_default();
    let mut ovr = ovr.take().unwrap_or_default();
    Some(ProviderSettings {
        base_url: pick_scalar(
            &mut usr.base_url,
            &mut prj.base_url,
            &mut lcl.base_url,
            &mut ovr.base_url,
        ),
        timeout: pick_scalar(
            &mut usr.timeout,
            &mut prj.timeout,
            &mut lcl.timeout,
            &mut ovr.timeout,
        ),
        max_retries: pick_scalar(
            &mut usr.max_retries,
            &mut prj.max_retries,
            &mut lcl.max_retries,
            &mut ovr.max_retries,
        ),
        options: pick_scalar(
            &mut usr.options,
            &mut prj.options,
            &mut lcl.options,
            &mut ovr.options,
        ),
        auth: pick_scalar(&mut usr.auth, &mut prj.auth, &mut lcl.auth, &mut ovr.auth),
        rate_limit: pick_scalar(
            &mut usr.rate_limit,
            &mut prj.rate_limit,
            &mut lcl.rate_limit,
            &mut ovr.rate_limit,
        ),
        runner_path: pick_scalar(
            &mut usr.runner_path,
            &mut prj.runner_path,
            &mut lcl.runner_path,
            &mut ovr.runner_path,
        ),
        debug_dump_dir: pick_scalar(
            &mut usr.debug_dump_dir,
            &mut prj.debug_dump_dir,
            &mut lcl.debug_dump_dir,
            &mut ovr.debug_dump_dir,
        ),
    })
}

fn merge_agent(
    usr: &mut Option<AgentSettings>,
    prj: &mut Option<AgentSettings>,
    lcl: &mut Option<AgentSettings>,
    ovr: &mut Option<AgentSettings>,
) -> Option<AgentSettings> {
    if usr.is_none() && prj.is_none() && lcl.is_none() && ovr.is_none() {
        return None;
    }
    let mut usr = usr.take().unwrap_or_default();
    let mut prj = prj.take().unwrap_or_default();
    let mut lcl = lcl.take().unwrap_or_default();
    let mut ovr = ovr.take().unwrap_or_default();
    Some(AgentSettings {
        max_turns: pick_scalar(
            &mut usr.max_turns,
            &mut prj.max_turns,
            &mut lcl.max_turns,
            &mut ovr.max_turns,
        ),
        step_timeout: pick_scalar(
            &mut usr.step_timeout,
            &mut prj.step_timeout,
            &mut lcl.step_timeout,
            &mut ovr.step_timeout,
        ),
        schema_budget: pick_scalar(
            &mut usr.schema_budget,
            &mut prj.schema_budget,
            &mut lcl.schema_budget,
            &mut ovr.schema_budget,
        ),
        context_window: pick_scalar(
            &mut usr.context_window,
            &mut prj.context_window,
            &mut lcl.context_window,
            &mut ovr.context_window,
        ),
        compact_threshold: pick_scalar(
            &mut usr.compact_threshold,
            &mut prj.compact_threshold,
            &mut lcl.compact_threshold,
            &mut ovr.compact_threshold,
        ),
        compact_keep_turns: pick_scalar(
            &mut usr.compact_keep_turns,
            &mut prj.compact_keep_turns,
            &mut lcl.compact_keep_turns,
            &mut ovr.compact_keep_turns,
        ),
        conversation_state: pick_scalar(
            &mut usr.conversation_state,
            &mut prj.conversation_state,
            &mut lcl.conversation_state,
            &mut ovr.conversation_state,
        ),
        server_compaction_threshold_tokens: pick_scalar(
            &mut usr.server_compaction_threshold_tokens,
            &mut prj.server_compaction_threshold_tokens,
            &mut lcl.server_compaction_threshold_tokens,
            &mut ovr.server_compaction_threshold_tokens,
        ),
        reasoning_effort: pick_scalar(
            &mut usr.reasoning_effort,
            &mut prj.reasoning_effort,
            &mut lcl.reasoning_effort,
            &mut ovr.reasoning_effort,
        ),
        reasoning_summary: pick_scalar(
            &mut usr.reasoning_summary,
            &mut prj.reasoning_summary,
            &mut lcl.reasoning_summary,
            &mut ovr.reasoning_summary,
        ),
        prompt_command_timeout: pick_scalar(
            &mut usr.prompt_command_timeout,
            &mut prj.prompt_command_timeout,
            &mut lcl.prompt_command_timeout,
            &mut ovr.prompt_command_timeout,
        ),
    })
}

fn merge_retry(
    usr: &mut Option<RetrySettings>,
    prj: &mut Option<RetrySettings>,
    lcl: &mut Option<RetrySettings>,
    ovr: &mut Option<RetrySettings>,
) -> Option<RetrySettings> {
    if usr.is_none() && prj.is_none() && lcl.is_none() && ovr.is_none() {
        return None;
    }
    let mut usr = usr.take().unwrap_or_default();
    let mut prj = prj.take().unwrap_or_default();
    let mut lcl = lcl.take().unwrap_or_default();
    let mut ovr = ovr.take().unwrap_or_default();
    Some(RetrySettings {
        max_retries: pick_scalar(
            &mut usr.max_retries,
            &mut prj.max_retries,
            &mut lcl.max_retries,
            &mut ovr.max_retries,
        ),
        base_delay: pick_scalar(
            &mut usr.base_delay,
            &mut prj.base_delay,
            &mut lcl.base_delay,
            &mut ovr.base_delay,
        ),
        backoff_multiplier: pick_scalar(
            &mut usr.backoff_multiplier,
            &mut prj.backoff_multiplier,
            &mut lcl.backoff_multiplier,
            &mut ovr.backoff_multiplier,
        ),
    })
}

fn merge_permissions(
    usr: &mut Option<PermissionSettings>,
    prj: &mut Option<PermissionSettings>,
    lcl: &mut Option<PermissionSettings>,
    ovr: &mut Option<PermissionSettings>,
) -> Option<PermissionSettings> {
    if usr.is_none() && prj.is_none() && lcl.is_none() && ovr.is_none() {
        return None;
    }
    let usr = usr.take().unwrap_or_default();
    let prj = prj.take().unwrap_or_default();
    let lcl = lcl.take().unwrap_or_default();
    let ovr = ovr.take().unwrap_or_default();
    Some(PermissionSettings {
        allow: merge_dedup(&[&usr.allow, &prj.allow, &lcl.allow, &ovr.allow]),
        deny: merge_dedup(&[&usr.deny, &prj.deny, &lcl.deny, &ovr.deny]),
        ask: merge_dedup(&[&usr.ask, &prj.ask, &lcl.ask, &ovr.ask]),
    })
}

/// Concatenate the layers' string vectors in precedence order
/// (`user -> project -> local -> cli`), preserving the first occurrence of
/// each string and discarding subsequent duplicates.
///
/// Returns [`None`] when *every* layer's slot is [`None`]. A layer with
/// `Some(empty_vec)` still counts as a present (empty) contribution, so the
/// result is `Some([])` in that case --- which is what we need for the
/// deny-additive contract: an explicit empty list at one layer cannot mask
/// entries from another layer that did contribute.
fn merge_dedup(layers: &[&Option<Vec<String>>]) -> Option<Vec<String>> {
    if layers.iter().all(|opt| opt.is_none()) {
        return None;
    }
    let mut out: Vec<String> = Vec::new();
    for layer in layers {
        let Some(items) = layer.as_ref() else {
            continue;
        };
        for item in items {
            if !out.iter().any(|existing| existing == item) {
                out.push(item.clone());
            }
        }
    }
    Some(out)
}

fn merge_hooks(
    usr: &mut Option<HookSettings>,
    prj: &mut Option<HookSettings>,
    lcl: &mut Option<HookSettings>,
    ovr: &mut Option<HookSettings>,
) -> Option<HookSettings> {
    if usr.is_none() && prj.is_none() && lcl.is_none() && ovr.is_none() {
        return None;
    }
    let usr = usr.take().unwrap_or_default();
    let prj = prj.take().unwrap_or_default();
    let lcl = lcl.take().unwrap_or_default();
    let ovr = ovr.take().unwrap_or_default();
    Some(HookSettings {
        pre_tool: concat_hook_slot(&[&usr.pre_tool, &prj.pre_tool, &lcl.pre_tool, &ovr.pre_tool]),
        post_tool: concat_hook_slot(&[
            &usr.post_tool,
            &prj.post_tool,
            &lcl.post_tool,
            &ovr.post_tool,
        ]),
        post_tool_failure: concat_hook_slot(&[
            &usr.post_tool_failure,
            &prj.post_tool_failure,
            &lcl.post_tool_failure,
            &ovr.post_tool_failure,
        ]),
        pre_llm: concat_hook_slot(&[&usr.pre_llm, &prj.pre_llm, &lcl.pre_llm, &ovr.pre_llm]),
        post_llm: concat_hook_slot(&[&usr.post_llm, &prj.post_llm, &lcl.post_llm, &ovr.post_llm]),
        session_event: concat_hook_slot(&[
            &usr.session_event,
            &prj.session_event,
            &lcl.session_event,
            &ovr.session_event,
        ]),
        user_prompt: concat_hook_slot(&[
            &usr.user_prompt,
            &prj.user_prompt,
            &lcl.user_prompt,
            &ovr.user_prompt,
        ]),
        stop: concat_hook_slot(&[&usr.stop, &prj.stop, &lcl.stop, &ovr.stop]),
        subagent_start: concat_hook_slot(&[
            &usr.subagent_start,
            &prj.subagent_start,
            &lcl.subagent_start,
            &ovr.subagent_start,
        ]),
        subagent_stop: concat_hook_slot(&[
            &usr.subagent_stop,
            &prj.subagent_stop,
            &lcl.subagent_stop,
            &ovr.subagent_stop,
        ]),
        session_start: concat_hook_slot(&[
            &usr.session_start,
            &prj.session_start,
            &lcl.session_start,
            &ovr.session_start,
        ]),
        session_end: concat_hook_slot(&[
            &usr.session_end,
            &prj.session_end,
            &lcl.session_end,
            &ovr.session_end,
        ]),
        pre_compaction: concat_hook_slot(&[
            &usr.pre_compaction,
            &prj.pre_compaction,
            &lcl.pre_compaction,
            &ovr.pre_compaction,
        ]),
    })
}

/// Concatenate hook-entry vectors across layers, preserving precedence
/// order. Identical-looking entries are *not* deduplicated --- the same
/// command at the same matcher may be an intentional duplicate (e.g. the
/// operator wants a hook to fire twice). Returns [`None`] when every layer
/// is [`None`].
fn concat_hook_slot(layers: &[&Option<Vec<HookEntry>>]) -> Option<Vec<HookEntry>> {
    if layers.iter().all(|opt| opt.is_none()) {
        return None;
    }
    let mut out: Vec<HookEntry> = Vec::new();
    for layer in layers {
        if let Some(entries) = layer.as_ref() {
            out.extend(entries.iter().cloned());
        }
    }
    Some(out)
}

fn merge_mcp_servers(
    usr: &mut Option<BTreeMap<String, McpServerSettings>>,
    prj: &mut Option<BTreeMap<String, McpServerSettings>>,
    lcl: &mut Option<BTreeMap<String, McpServerSettings>>,
    ovr: &mut Option<BTreeMap<String, McpServerSettings>>,
) -> Option<BTreeMap<String, McpServerSettings>> {
    if usr.is_none() && prj.is_none() && lcl.is_none() && ovr.is_none() {
        return None;
    }
    let mut out: BTreeMap<String, McpServerSettings> = BTreeMap::new();
    for map in [usr.take(), prj.take(), lcl.take(), ovr.take()]
        .into_iter()
        .flatten()
    {
        for (name, def) in map {
            out.insert(name, def);
        }
    }
    Some(out)
}

fn merge_tools(
    usr: &mut Option<ToolSettings>,
    prj: &mut Option<ToolSettings>,
    lcl: &mut Option<ToolSettings>,
    ovr: &mut Option<ToolSettings>,
) -> Option<ToolSettings> {
    if usr.is_none() && prj.is_none() && lcl.is_none() && ovr.is_none() {
        return None;
    }
    let mut usr = usr.take().unwrap_or_default();
    let mut prj = prj.take().unwrap_or_default();
    let mut lcl = lcl.take().unwrap_or_default();
    let mut ovr = ovr.take().unwrap_or_default();
    Some(ToolSettings {
        write: merge_write(
            &mut usr.write,
            &mut prj.write,
            &mut lcl.write,
            &mut ovr.write,
        ),
        // Opaque values: scalar precedence (highest-non-None wins). The
        // schema for `bash`/`edit` is not yet stable, so we do not attempt
        // an in-value merge --- that would require knowledge of which fields
        // are mergeable. Tom's NO ASSUMED DEFAULTS edict applies: do not
        // invent merge semantics for opaque values.
        bash: pick_scalar(&mut usr.bash, &mut prj.bash, &mut lcl.bash, &mut ovr.bash),
        edit: pick_scalar(&mut usr.edit, &mut prj.edit, &mut lcl.edit, &mut ovr.edit),
    })
}

fn merge_write(
    usr: &mut Option<WriteToolSettings>,
    prj: &mut Option<WriteToolSettings>,
    lcl: &mut Option<WriteToolSettings>,
    ovr: &mut Option<WriteToolSettings>,
) -> Option<WriteToolSettings> {
    if usr.is_none() && prj.is_none() && lcl.is_none() && ovr.is_none() {
        return None;
    }
    let mut usr = usr.take().unwrap_or_default();
    let mut prj = prj.take().unwrap_or_default();
    let mut lcl = lcl.take().unwrap_or_default();
    let mut ovr = ovr.take().unwrap_or_default();
    Some(WriteToolSettings {
        max_code_lines: pick_scalar(
            &mut usr.max_code_lines,
            &mut prj.max_code_lines,
            &mut lcl.max_code_lines,
            &mut ovr.max_code_lines,
        ),
        length_overrides: pick_scalar(
            &mut usr.length_overrides,
            &mut prj.length_overrides,
            &mut lcl.length_overrides,
            &mut ovr.length_overrides,
        ),
    })
}

fn merge_skills(
    usr: &mut Option<SkillsSettings>,
    prj: &mut Option<SkillsSettings>,
    lcl: &mut Option<SkillsSettings>,
    ovr: &mut Option<SkillsSettings>,
) -> Option<SkillsSettings> {
    if usr.is_none() && prj.is_none() && lcl.is_none() && ovr.is_none() {
        return None;
    }
    let usr = usr.take().unwrap_or_default();
    let prj = prj.take().unwrap_or_default();
    let lcl = lcl.take().unwrap_or_default();
    let ovr = ovr.take().unwrap_or_default();
    Some(SkillsSettings {
        search_paths: concat_string_paths(&[
            &usr.search_paths,
            &prj.search_paths,
            &lcl.search_paths,
            &ovr.search_paths,
        ]),
    })
}

fn merge_context(
    usr: &mut Option<ContextSettings>,
    prj: &mut Option<ContextSettings>,
    lcl: &mut Option<ContextSettings>,
    ovr: &mut Option<ContextSettings>,
) -> Option<ContextSettings> {
    if usr.is_none() && prj.is_none() && lcl.is_none() && ovr.is_none() {
        return None;
    }
    let usr = usr.take().unwrap_or_default();
    let prj = prj.take().unwrap_or_default();
    let lcl = lcl.take().unwrap_or_default();
    let ovr = ovr.take().unwrap_or_default();
    Some(ContextSettings {
        search_paths: concat_string_paths(&[
            &usr.search_paths,
            &prj.search_paths,
            &lcl.search_paths,
            &ovr.search_paths,
        ]),
    })
}

fn merge_session(
    usr: &mut Option<SessionSettings>,
    prj: &mut Option<SessionSettings>,
    lcl: &mut Option<SessionSettings>,
    ovr: &mut Option<SessionSettings>,
) -> Option<SessionSettings> {
    if usr.is_none() && prj.is_none() && lcl.is_none() && ovr.is_none() {
        return None;
    }
    let mut usr = usr.take().unwrap_or_default();
    let mut prj = prj.take().unwrap_or_default();
    let mut lcl = lcl.take().unwrap_or_default();
    let mut ovr = ovr.take().unwrap_or_default();
    Some(SessionSettings {
        cleanup_days: pick_scalar(
            &mut usr.cleanup_days,
            &mut prj.cleanup_days,
            &mut lcl.cleanup_days,
            &mut ovr.cleanup_days,
        ),
        history_capacity: pick_scalar(
            &mut usr.history_capacity,
            &mut prj.history_capacity,
            &mut lcl.history_capacity,
            &mut ovr.history_capacity,
        ),
    })
}

/// Concatenate path-string vectors in precedence order, deduplicating to
/// keep the first occurrence of each path. Behaviour matches
/// [`merge_dedup`] --- kept as a separate helper for readability since the
/// motivation (path discovery extends, not replaces) is documented next to
/// the call.
fn concat_string_paths(layers: &[&Option<Vec<String>>]) -> Option<Vec<String>> {
    merge_dedup(layers)
}

fn merge_env_map(
    usr: &mut Option<BTreeMap<String, String>>,
    prj: &mut Option<BTreeMap<String, String>>,
    lcl: &mut Option<BTreeMap<String, String>>,
    ovr: &mut Option<BTreeMap<String, String>>,
) -> Option<BTreeMap<String, String>> {
    if usr.is_none() && prj.is_none() && lcl.is_none() && ovr.is_none() {
        return None;
    }
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for map in [usr.take(), prj.take(), lcl.take(), ovr.take()]
        .into_iter()
        .flatten()
    {
        for (key, val) in map {
            out.insert(key, val);
        }
    }
    Some(out)
}

#[cfg(test)]
#[path = "merge_tests.rs"]
mod tests;
