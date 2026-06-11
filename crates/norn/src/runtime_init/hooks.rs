//! Merging of programmatic and settings-declared (shell) hook registries.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::config::HookSettings;
use crate::error::NornError;
use crate::integration::hooks::{
    Hook, HookContext, HookEventType, HookMatcher, HookRegistry, ShellCommandHook,
};
use crate::profile::Profile;

use super::base::invalid_config;

/// Merge programmatic hooks with the settings-declared shell hooks into a
/// single registry.
///
/// **Precedence:** programmatic hooks register first, settings shell hooks
/// after. Dispatch is first-`Block`-wins in registration order, so on a
/// conflicting outcome the programmatic (embedder-supplied) hook wins.
///
/// Neither source is ever dropped: when the programmatic `Arc` has
/// outstanding clones it is folded in via
/// [`HookRegistry::merge_shared`] instead of being silently replaced with
/// an empty registry. This is the single library-owned implementation; the
/// CLI's duplicate (`norn-cli/src/runtime/builder.rs`) should delegate here
/// once the assembly paths converge (REVIEW R1).
///
/// # Errors
///
/// Returns [`NornError::Config`] for an invalid hook matcher regex or a
/// settings hook entry missing its required timeout.
pub fn assemble_hook_registry(
    programmatic: Option<Arc<HookRegistry>>,
    settings: &HookSettings,
    profile: &Profile,
    cwd: &Path,
) -> Result<Option<Arc<HookRegistry>>, NornError> {
    let shell_total = settings_total_entries(settings);
    if programmatic.is_none() && shell_total == 0 {
        return Ok(None);
    }
    if shell_total == 0 {
        return Ok(programmatic);
    }

    let mut registry = match programmatic {
        Some(arc) => match Arc::try_unwrap(arc) {
            Ok(owned) => owned,
            Err(shared) => {
                // The caller retained a clone of its programmatic registry.
                // Fold it in by reference so every programmatic hook still
                // fires (and still fires first), instead of silently
                // substituting an empty registry.
                let mut fresh = HookRegistry::new();
                fresh.merge_shared(shared);
                fresh
            }
        },
        None => HookRegistry::new(),
    };
    let context = HookContext {
        session_id: String::new(),
        cwd: cwd.display().to_string(),
        agent_id: String::new(),
        profile_name: profile.name.clone(),
    };

    register_shell_hooks(
        &mut registry,
        settings.pre_tool.as_ref(),
        HookEventType::PreTool,
        &context,
        |h| Hook::PreTool(Box::new(h)),
    )?;
    register_shell_hooks(
        &mut registry,
        settings.post_tool.as_ref(),
        HookEventType::PostTool,
        &context,
        |h| Hook::PostTool(Box::new(h)),
    )?;
    register_shell_hooks(
        &mut registry,
        settings.post_tool_failure.as_ref(),
        HookEventType::PostToolFailure,
        &context,
        |h| Hook::PostToolFailure(Box::new(h)),
    )?;
    register_shell_hooks(
        &mut registry,
        settings.pre_llm.as_ref(),
        HookEventType::PreLlm,
        &context,
        |h| Hook::PreLlm(Box::new(h)),
    )?;
    register_shell_hooks(
        &mut registry,
        settings.post_llm.as_ref(),
        HookEventType::PostLlm,
        &context,
        |h| Hook::PostLlm(Box::new(h)),
    )?;
    register_shell_hooks(
        &mut registry,
        settings.session_event.as_ref(),
        HookEventType::SessionEvent,
        &context,
        |h| Hook::SessionEvent(Box::new(h)),
    )?;
    register_shell_hooks(
        &mut registry,
        settings.user_prompt.as_ref(),
        HookEventType::UserPrompt,
        &context,
        |h| Hook::UserPrompt(Box::new(h)),
    )?;
    register_shell_hooks(
        &mut registry,
        settings.stop.as_ref(),
        HookEventType::Stop,
        &context,
        |h| Hook::Stop(Box::new(h)),
    )?;
    register_shell_hooks(
        &mut registry,
        settings.subagent_start.as_ref(),
        HookEventType::SubagentStart,
        &context,
        |h| Hook::Subagent(Box::new(h)),
    )?;
    register_shell_hooks(
        &mut registry,
        settings.subagent_stop.as_ref(),
        HookEventType::SubagentStop,
        &context,
        |h| Hook::Subagent(Box::new(h)),
    )?;
    register_shell_hooks(
        &mut registry,
        settings.session_start.as_ref(),
        HookEventType::SessionStart,
        &context,
        |h| Hook::SessionLifecycle(Box::new(h)),
    )?;
    register_shell_hooks(
        &mut registry,
        settings.session_end.as_ref(),
        HookEventType::SessionEnd,
        &context,
        |h| Hook::SessionLifecycle(Box::new(h)),
    )?;
    register_shell_hooks(
        &mut registry,
        settings.pre_compaction.as_ref(),
        HookEventType::PreCompaction,
        &context,
        |h| Hook::Compaction(Box::new(h)),
    )?;

    Ok(Some(Arc::new(registry)))
}

fn register_shell_hooks<F>(
    registry: &mut HookRegistry,
    entries: Option<&Vec<crate::config::HookEntry>>,
    event_type: HookEventType,
    context: &HookContext,
    wrap: F,
) -> Result<(), NornError>
where
    F: Fn(ShellCommandHook) -> Hook,
{
    let Some(entries) = entries else {
        return Ok(());
    };
    for entry in entries {
        let timeout_ms = entry.timeout.ok_or_else(|| {
            invalid_config(format!(
                "hook {:?} command '{}' is missing required timeout",
                event_type, entry.command
            ))
        })?;
        let timeout = Duration::from_millis(timeout_ms);
        let matcher = HookMatcher::new(entry.matcher.as_deref())?;
        registry.register(wrap(ShellCommandHook::new(
            entry.command.clone(),
            matcher,
            timeout,
            event_type,
            context.clone(),
        )));
    }
    Ok(())
}

fn settings_total_entries(settings: &HookSettings) -> usize {
    settings.pre_tool.as_ref().map_or(0, Vec::len)
        + settings.post_tool.as_ref().map_or(0, Vec::len)
        + settings.post_tool_failure.as_ref().map_or(0, Vec::len)
        + settings.pre_llm.as_ref().map_or(0, Vec::len)
        + settings.post_llm.as_ref().map_or(0, Vec::len)
        + settings.session_event.as_ref().map_or(0, Vec::len)
        + settings.user_prompt.as_ref().map_or(0, Vec::len)
        + settings.stop.as_ref().map_or(0, Vec::len)
        + settings.subagent_start.as_ref().map_or(0, Vec::len)
        + settings.subagent_stop.as_ref().map_or(0, Vec::len)
        + settings.session_start.as_ref().map_or(0, Vec::len)
        + settings.session_end.as_ref().map_or(0, Vec::len)
        + settings.pre_compaction.as_ref().map_or(0, Vec::len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HookEntry;
    use crate::integration::hooks::{Hook, HookOutcome, StopHook};

    struct BlockingStop;

    #[async_trait::async_trait]
    impl StopHook for BlockingStop {
        async fn on_stop(&self, _final_text: &str) -> HookOutcome {
            HookOutcome::Block {
                reason: "programmatic".to_owned(),
            }
        }
    }

    fn programmatic_with_stop() -> HookRegistry {
        let mut registry = HookRegistry::new();
        registry.register(Hook::Stop(Box::new(BlockingStop)));
        registry
    }

    fn settings_with_one_pre_tool() -> HookSettings {
        HookSettings {
            pre_tool: Some(vec![HookEntry {
                matcher: None,
                command: "true".to_owned(),
                timeout: Some(1_000),
            }]),
            ..HookSettings::default()
        }
    }

    /// H13 regression: a *shared* programmatic registry (outstanding `Arc`
    /// clone, exactly what `AgentBuilder::build` used to produce) must not
    /// be silently replaced by an empty registry — both the programmatic
    /// stop hook and the settings shell hook survive the merge.
    #[tokio::test]
    async fn assemble_merges_shared_programmatic_with_shell_hooks() -> Result<(), NornError> {
        let programmatic = Arc::new(programmatic_with_stop());
        let outstanding_clone = Arc::clone(&programmatic);
        let profile = Profile::default();

        let merged = assemble_hook_registry(
            Some(programmatic),
            &settings_with_one_pre_tool(),
            &profile,
            Path::new("/tmp"),
        )?
        .ok_or_else(|| invalid_config("merged registry expected".to_owned()))?;

        assert_eq!(merged.pre_tool_len(), 1, "settings shell hook registered");
        assert_eq!(
            merged.stop_len(),
            1,
            "programmatic hooks must survive even when the Arc is shared",
        );
        let outcome = merged.run_stop("done").await;
        assert!(
            matches!(outcome, HookOutcome::Block { .. }),
            "the forwarded programmatic stop hook must still dispatch",
        );
        drop(outstanding_clone);
        Ok(())
    }

    /// Sole-owner programmatic registry merges by value: same observable
    /// result, no forwarding indirection needed.
    #[tokio::test]
    async fn assemble_merges_owned_programmatic_with_shell_hooks() -> Result<(), NornError> {
        let profile = Profile::default();
        let merged = assemble_hook_registry(
            Some(Arc::new(programmatic_with_stop())),
            &settings_with_one_pre_tool(),
            &profile,
            Path::new("/tmp"),
        )?
        .ok_or_else(|| invalid_config("merged registry expected".to_owned()))?;

        assert_eq!(merged.pre_tool_len(), 1);
        assert_eq!(merged.stop_len(), 1);
        Ok(())
    }

    /// No programmatic hooks and no settings entries → `None` (no
    /// allocation, no empty registry).
    #[test]
    fn assemble_returns_none_when_both_sources_empty() -> Result<(), NornError> {
        let profile = Profile::default();
        let result =
            assemble_hook_registry(None, &HookSettings::default(), &profile, Path::new("/tmp"))?;
        assert!(result.is_none());
        Ok(())
    }

    /// Settings empty → the programmatic Arc passes through untouched.
    #[test]
    fn assemble_passes_programmatic_through_when_settings_empty() -> Result<(), NornError> {
        let programmatic = Arc::new(programmatic_with_stop());
        let profile = Profile::default();
        let result = assemble_hook_registry(
            Some(Arc::clone(&programmatic)),
            &HookSettings::default(),
            &profile,
            Path::new("/tmp"),
        )?
        .ok_or_else(|| invalid_config("registry expected".to_owned()))?;
        assert!(Arc::ptr_eq(&result, &programmatic));
        Ok(())
    }
}
