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

/// Constructor mapping a [`ShellCommandHook`] into its [`Hook`] variant —
/// one entry per event slot in [`assemble_hook_registry`]'s table.
type WrapFn = fn(ShellCommandHook) -> Hook;

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
/// settings hook entry with an empty command.
pub fn assemble_hook_registry(
    programmatic: Option<Arc<HookRegistry>>,
    settings: &HookSettings,
    profile: &Profile,
    cwd: &Path,
) -> Result<Option<Arc<HookRegistry>>, NornError> {
    // Exhaustive destructuring: adding a HookSettings event slot without
    // wiring it here is a compile error, never a silently dropped hook
    // class (the H13 failure mode via schema evolution).
    let HookSettings {
        pre_tool,
        post_tool,
        post_tool_failure,
        pre_llm,
        post_llm,
        session_event,
        user_prompt,
        stop,
        subagent_start,
        subagent_stop,
        session_start,
        session_end,
        pre_compaction,
    } = settings;

    let slot_len = |slot: &Option<Vec<crate::config::HookEntry>>| slot.as_ref().map_or(0, Vec::len);
    let shell_total = slot_len(pre_tool)
        + slot_len(post_tool)
        + slot_len(post_tool_failure)
        + slot_len(pre_llm)
        + slot_len(post_llm)
        + slot_len(session_event)
        + slot_len(user_prompt)
        + slot_len(stop)
        + slot_len(subagent_start)
        + slot_len(subagent_stop)
        + slot_len(session_start)
        + slot_len(session_end)
        + slot_len(pre_compaction);
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

    // One (slot, event type, wrapper) row per destructured field: the
    // compiler enforces every slot appears above, and this table is the
    // single place a slot's registration is defined.
    let slots: &[(
        &Option<Vec<crate::config::HookEntry>>,
        HookEventType,
        WrapFn,
    )] = &[
        (pre_tool, HookEventType::PreTool, |h| {
            Hook::PreTool(Box::new(h))
        }),
        (post_tool, HookEventType::PostTool, |h| {
            Hook::PostTool(Box::new(h))
        }),
        (post_tool_failure, HookEventType::PostToolFailure, |h| {
            Hook::PostToolFailure(Box::new(h))
        }),
        (pre_llm, HookEventType::PreLlm, |h| {
            Hook::PreLlm(Box::new(h))
        }),
        (post_llm, HookEventType::PostLlm, |h| {
            Hook::PostLlm(Box::new(h))
        }),
        (session_event, HookEventType::SessionEvent, |h| {
            Hook::SessionEvent(Box::new(h))
        }),
        (user_prompt, HookEventType::UserPrompt, |h| {
            Hook::UserPrompt(Box::new(h))
        }),
        (stop, HookEventType::Stop, |h| Hook::Stop(Box::new(h))),
        (subagent_start, HookEventType::SubagentStart, |h| {
            Hook::Subagent(Box::new(h))
        }),
        (subagent_stop, HookEventType::SubagentStop, |h| {
            Hook::Subagent(Box::new(h))
        }),
        (session_start, HookEventType::SessionStart, |h| {
            Hook::SessionLifecycle(Box::new(h))
        }),
        (session_end, HookEventType::SessionEnd, |h| {
            Hook::SessionLifecycle(Box::new(h))
        }),
        (pre_compaction, HookEventType::PreCompaction, |h| {
            Hook::Compaction(Box::new(h))
        }),
    ];
    for (entries, event_type, wrap) in slots {
        register_shell_hooks(&mut registry, entries.as_ref(), *event_type, &context, wrap)?;
    }

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
        // Backstop for programmatically-built HookSettings that bypassed
        // config validation: a commandless hook has no behaviour and must
        // not be silently registered.
        if entry.command.trim().is_empty() {
            return Err(invalid_config(format!(
                "hook {event_type:?} has an empty command; a hook without a command has no \
                 behaviour",
            )));
        }
        let timeout = Duration::from_millis(entry.timeout);
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
                timeout: 1_000,
            }]),
            ..HookSettings::default()
        }
    }

    /// Backstop regression: a programmatically-built [`HookSettings`] entry
    /// with an empty command must be a typed error, not a silently
    /// registered no-op hook.
    #[test]
    fn assemble_rejects_empty_command_entries() -> Result<(), String> {
        let profile = Profile::default();
        let settings = HookSettings {
            stop: Some(vec![HookEntry {
                matcher: None,
                command: "   ".to_owned(),
                timeout: 5,
            }]),
            ..HookSettings::default()
        };
        match assemble_hook_registry(None, &settings, &profile, Path::new("/tmp")) {
            Err(err) if err.to_string().contains("empty command") => Ok(()),
            Err(err) => Err(format!("error must name the defect, got: {err}")),
            Ok(_) => Err("empty command must be rejected".to_owned()),
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
