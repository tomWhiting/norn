//! Sub-struct sections merged field-by-field with scalar precedence:
//! provider, agent, retry, session, and tools (including the deep-merged
//! `tools.write` and `tools.skill` blocks).
//!
//! Each merger returns [`None`] only when every layer is [`None`]; when
//! any layer contributes, the result is `Some(merged_sub)` with each inner
//! [`Option`] field resolved by [`pick_scalar`].

use crate::config::types::{
    AgentSettings, ProviderSettings, RetrySettings, SessionSettings, SkillToolSettings,
    ToolSettings, WriteToolSettings,
};

use super::primitives::pick_scalar;

/// Merge the `provider` section scalar-wise across the four layers.
pub(super) fn merge_provider(
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
        api_key_env: pick_scalar(
            &mut usr.api_key_env,
            &mut prj.api_key_env,
            &mut lcl.api_key_env,
            &mut ovr.api_key_env,
        ),
        auth: pick_scalar(&mut usr.auth, &mut prj.auth, &mut lcl.auth, &mut ovr.auth),
        rate_limit: pick_scalar(
            &mut usr.rate_limit,
            &mut prj.rate_limit,
            &mut lcl.rate_limit,
            &mut ovr.rate_limit,
        ),
        rate_limit_interval: pick_scalar(
            &mut usr.rate_limit_interval,
            &mut prj.rate_limit_interval,
            &mut lcl.rate_limit_interval,
            &mut ovr.rate_limit_interval,
        ),
        retry_backoff: pick_scalar(
            &mut usr.retry_backoff,
            &mut prj.retry_backoff,
            &mut lcl.retry_backoff,
            &mut ovr.retry_backoff,
        ),
        retry_after_ceiling: pick_scalar(
            &mut usr.retry_after_ceiling,
            &mut prj.retry_after_ceiling,
            &mut lcl.retry_after_ceiling,
            &mut ovr.retry_after_ceiling,
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

/// Merge the `agent` section scalar-wise across the four layers.
pub(super) fn merge_agent(
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
        auto_compact_reserve_tokens: pick_scalar(
            &mut usr.auto_compact_reserve_tokens,
            &mut prj.auto_compact_reserve_tokens,
            &mut lcl.auto_compact_reserve_tokens,
            &mut ovr.auto_compact_reserve_tokens,
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
        service_tier: pick_scalar(
            &mut usr.service_tier,
            &mut prj.service_tier,
            &mut lcl.service_tier,
            &mut ovr.service_tier,
        ),
        prompt_command_timeout: pick_scalar(
            &mut usr.prompt_command_timeout,
            &mut prj.prompt_command_timeout,
            &mut lcl.prompt_command_timeout,
            &mut ovr.prompt_command_timeout,
        ),
    })
}

/// Merge the `retry` section scalar-wise across the four layers.
pub(super) fn merge_retry(
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

/// Merge the `session` section scalar-wise across the four layers.
pub(super) fn merge_session(
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

/// Merge the `tools` section: `write` and `skill` deep-merge
/// field-by-field while the opaque `bash` / `edit` values follow scalar
/// precedence.
pub(super) fn merge_tools(
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
        skill: merge_skill(
            &mut usr.skill,
            &mut prj.skill,
            &mut lcl.skill,
            &mut ovr.skill,
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

/// Deep-merge the `tools.write` block field-by-field across the four
/// layers; sibling keys contributed at different layers are preserved.
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

/// Deep-merge the `tools.skill` block field-by-field across the four
/// layers, mirroring [`merge_write`]: a layer that omits
/// `shell_execution` inherits it from the lower-precedence layer.
fn merge_skill(
    usr: &mut Option<SkillToolSettings>,
    prj: &mut Option<SkillToolSettings>,
    lcl: &mut Option<SkillToolSettings>,
    ovr: &mut Option<SkillToolSettings>,
) -> Option<SkillToolSettings> {
    if usr.is_none() && prj.is_none() && lcl.is_none() && ovr.is_none() {
        return None;
    }
    let mut usr = usr.take().unwrap_or_default();
    let mut prj = prj.take().unwrap_or_default();
    let mut lcl = lcl.take().unwrap_or_default();
    let mut ovr = ovr.take().unwrap_or_default();
    Some(SkillToolSettings {
        shell_execution: pick_scalar(
            &mut usr.shell_execution,
            &mut prj.shell_execution,
            &mut lcl.shell_execution,
            &mut ovr.shell_execution,
        ),
    })
}
