//! Sections with additive or keyed merge semantics: permissions
//! (deny-additive, allow/ask deduplicating concat), hooks (extend by event
//! slot), MCP servers and env (keyed replace, later layer wins per key),
//! and the skills / context search-path lists (deduplicating concat).

use std::collections::BTreeMap;

use crate::config::types::{
    ContextSettings, HookSettings, McpServerSettings, ModelAliasSettings, PermissionSettings,
    ProviderProfileSettings, SkillsSettings,
};

use super::primitives::{concat_hook_slot, concat_string_paths, merge_dedup};

/// Merge the `permissions` section: `deny` is a cross-layer union (CO6:
/// you cannot un-deny), `allow` / `ask` concatenate with first-seen
/// deduplication.
pub(super) fn merge_permissions(
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

/// Merge the `hooks` section by concatenating each event slot across the
/// four layers: project hooks extend user hooks, they do not replace.
pub(super) fn merge_hooks(
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

/// Merge the `mcp_servers` map by name: a same-name entry at a later layer
/// replaces the earlier definition wholesale (no deep merge).
pub(super) fn merge_mcp_servers(
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

/// Merge model aliases by alias name: a same-name entry at a later layer
/// replaces the earlier definition wholesale (no deep merge).
pub(super) fn merge_model_aliases(
    usr: &mut Option<BTreeMap<String, ModelAliasSettings>>,
    prj: &mut Option<BTreeMap<String, ModelAliasSettings>>,
    lcl: &mut Option<BTreeMap<String, ModelAliasSettings>>,
    ovr: &mut Option<BTreeMap<String, ModelAliasSettings>>,
) -> Option<BTreeMap<String, ModelAliasSettings>> {
    if usr.is_none() && prj.is_none() && lcl.is_none() && ovr.is_none() {
        return None;
    }
    let mut out: BTreeMap<String, ModelAliasSettings> = BTreeMap::new();
    for map in [usr.take(), prj.take(), lcl.take(), ovr.take()]
        .into_iter()
        .flatten()
    {
        for (name, alias) in map {
            out.insert(name, alias);
        }
    }
    Some(out)
}

/// Merge provider profiles by profile id: a same-name entry at a later layer
/// replaces the earlier definition wholesale (no deep merge).
pub(super) fn merge_provider_profiles(
    usr: &mut Option<BTreeMap<String, ProviderProfileSettings>>,
    prj: &mut Option<BTreeMap<String, ProviderProfileSettings>>,
    lcl: &mut Option<BTreeMap<String, ProviderProfileSettings>>,
    ovr: &mut Option<BTreeMap<String, ProviderProfileSettings>>,
) -> Option<BTreeMap<String, ProviderProfileSettings>> {
    if usr.is_none() && prj.is_none() && lcl.is_none() && ovr.is_none() {
        return None;
    }
    let mut out: BTreeMap<String, ProviderProfileSettings> = BTreeMap::new();
    for map in [usr.take(), prj.take(), lcl.take(), ovr.take()]
        .into_iter()
        .flatten()
    {
        for (name, profile) in map {
            out.insert(name, profile);
        }
    }
    Some(out)
}

/// Merge the `skills` section: search paths extend across layers
/// (deduplicating concat), they do not replace.
pub(super) fn merge_skills(
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

/// Merge the `context` section: search paths extend across layers
/// (deduplicating concat), they do not replace.
pub(super) fn merge_context(
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

/// Merge the `env` map by key: a same-key entry at a later layer replaces
/// the earlier value.
pub(super) fn merge_env_map(
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
