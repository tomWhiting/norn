//! Top-level [`merge_settings`] entry point folding the four
//! [`NornSettings`] layers field-by-field via the per-section mergers.
//!
//! Parameters are taken by `&mut` reference so inner fields can be moved
//! out via [`Option::take`] — zero-copy for all moved fields, and the
//! parameter types are small pointer-sized references rather than the
//! large [`NornSettings`] struct itself.

use crate::config::types::NornSettings;

use super::collection_sections::{
    merge_context, merge_env_map, merge_hooks, merge_mcp_servers, merge_model_aliases,
    merge_permissions, merge_provider_profiles, merge_skills, merge_variants,
};
use super::primitives::pick_scalar;
use super::scalar_sections::{
    merge_agent, merge_provider, merge_retry, merge_session, merge_tools,
};

/// Merge four [`NornSettings`] layers in precedence order:
/// `user < project < local < cli`.
///
/// See [module documentation](super) for per-field merge semantics.
#[must_use]
pub(crate) fn merge_settings(
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
        model_aliases: merge_model_aliases(
            &mut usr.model_aliases,
            &mut prj.model_aliases,
            &mut lcl.model_aliases,
            &mut ovr.model_aliases,
        ),
        provider_profiles: merge_provider_profiles(
            &mut usr.provider_profiles,
            &mut prj.provider_profiles,
            &mut lcl.provider_profiles,
            &mut ovr.provider_profiles,
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
        variants: merge_variants(
            &mut usr.variants,
            &mut prj.variants,
            &mut lcl.variants,
            &mut ovr.variants,
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
