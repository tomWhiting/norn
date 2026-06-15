//! Slash-command dispatch for CLI orchestrators (NC-006 R13).
//!
//! Wires the CLI's interception layer over libnorn's
//! [`preprocess_input`](norn::agent_loop::commands::preprocess_input).
//!
//! For inputs that match a CLI built-in name the closure is fired once
//! (so the stderr side effects happen) and the orchestrator is told the
//! command was [`DispatchOutcome::HandledLocally`] — no
//! [`run_agent_step`](norn::agent_loop::runner::run_agent_step) call follows.
//!
//! Anything else — non-slash input, unknown slash names, and
//! profile-registered slash commands — is returned as
//! [`DispatchOutcome::PassToAgent`] so the orchestrator can drive
//! `run_agent_step` with the original verbatim input. Profile slash
//! commands then fire through libnorn's
//! [`build_initial_messages`](norn::agent_loop::helpers::build_initial_messages)
//! call site, exactly as they do without the CLI layer.

use norn::agent_loop::commands::{PreprocessResult, SlashCommandRegistry, preprocess_input};
use norn::error::NornError;

use super::registry::CLI_BUILTIN_NAMES;

/// Outcome reported by [`dispatch_input`].
#[derive(Debug, Clone)]
pub enum DispatchOutcome {
    /// The input was a CLI builtin (e.g. `/help`); side effects have
    /// already happened and `run_agent_step` MUST NOT be invoked.
    HandledLocally,
    /// The input must be passed verbatim to `run_agent_step`. Profile
    /// slash commands fire from inside the agent loop on this path.
    PassToAgent(String),
}

/// Dispatch a single user input.
///
/// # Errors
///
/// Propagates any error from a CLI-builtin closure (currently none —
/// every builtin returns `Ok(_)`) or from
/// [`preprocess_input`](norn::agent_loop::commands::preprocess_input)
/// when a registered slash expansion fails.
pub fn dispatch_input(
    input: &str,
    registry: &SlashCommandRegistry,
) -> Result<DispatchOutcome, NornError> {
    let Some((name, _arg)) = split_command(input) else {
        return Ok(DispatchOutcome::PassToAgent(input.to_owned()));
    };

    if is_cli_builtin(name) && registry.get(name).is_some() {
        match preprocess_input(input, registry)? {
            PreprocessResult::Expanded { .. } => Ok(DispatchOutcome::HandledLocally),
            PreprocessResult::Passthrough(raw) => Ok(DispatchOutcome::PassToAgent(raw)),
        }
    } else {
        Ok(DispatchOutcome::PassToAgent(input.to_owned()))
    }
}

/// Mirror of libnorn's private `split_command` helper.
///
/// Strips the leading `/` and splits into `(name, argument)`. Returns
/// [`None`] when the input is not a slash command or is empty after
/// the slash.
fn split_command(input: &str) -> Option<(&str, &str)> {
    let after_slash = input.strip_prefix('/')?;
    let trimmed = after_slash.trim_start();
    if trimmed.is_empty() {
        return None;
    }
    let (name, rest) = match trimmed.find(char::is_whitespace) {
        Some(idx) => (&trimmed[..idx], trimmed[idx..].trim_start()),
        None => (trimmed, ""),
    };
    Some((name, rest))
}

fn is_cli_builtin(name: &str) -> bool {
    CLI_BUILTIN_NAMES.contains(&name)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use norn::agent_loop::commands::{SlashCommand, SlashCommandHandler};
    use norn::session::store::EventStore;

    use super::super::registry::build_slash_registry;
    use super::super::state::{SlashState, SlashStateSeed};
    use super::*;

    fn empty_seed() -> SlashStateSeed {
        SlashStateSeed {
            model: "gpt-x".to_owned(),
            service_tier: None,
            reasoning_effort: None,
            output_schema: None,
            session_name: None,
            session_id: None,
            data_dir: PathBuf::from("/tmp/norn-cli-dispatch-tests"),
            no_session: true,
            variable_pairs: Vec::new(),
            tools: Vec::new(),
            store: Arc::new(EventStore::new()),
        }
    }

    #[test]
    fn non_slash_input_passes_through() {
        let state = SlashState::new(empty_seed());
        let registry = build_slash_registry(&state, None);
        let outcome = dispatch_input("hello there", &registry).unwrap();
        match outcome {
            DispatchOutcome::PassToAgent(s) => assert_eq!(s, "hello there"),
            DispatchOutcome::HandledLocally => panic!("expected PassToAgent"),
        }
    }

    #[test]
    fn cli_builtin_help_is_intercepted() {
        let state = SlashState::new(empty_seed());
        let registry = build_slash_registry(&state, None);
        let outcome = dispatch_input("/help", &registry).unwrap();
        assert!(matches!(outcome, DispatchOutcome::HandledLocally));
    }

    #[test]
    fn cli_builtin_compact_sets_flag_and_handles_locally() {
        let state = SlashState::new(empty_seed());
        let registry = build_slash_registry(&state, None);
        let outcome = dispatch_input("/compact", &registry).unwrap();
        assert!(matches!(outcome, DispatchOutcome::HandledLocally));
        assert!(state.compact_requested.load(Ordering::Relaxed));
    }

    #[test]
    fn cli_builtin_exit_sets_flag_and_handles_locally() {
        let state = SlashState::new(empty_seed());
        let registry = build_slash_registry(&state, None);
        let outcome = dispatch_input("/exit", &registry).unwrap();
        assert!(matches!(outcome, DispatchOutcome::HandledLocally));
        assert!(state.exit_requested.load(Ordering::Relaxed));
    }

    #[test]
    fn cli_builtin_compact_absorbs_trailing_argument() {
        let state = SlashState::new(empty_seed());
        let registry = build_slash_registry(&state, None);
        let outcome = dispatch_input("/compact now please", &registry).unwrap();
        assert!(matches!(outcome, DispatchOutcome::HandledLocally));
        assert!(state.compact_requested.load(Ordering::Relaxed));
    }

    #[test]
    fn unknown_slash_passes_through_to_agent() {
        let state = SlashState::new(empty_seed());
        let registry = build_slash_registry(&state, None);
        let outcome = dispatch_input("/unknown foo bar", &registry).unwrap();
        match outcome {
            DispatchOutcome::PassToAgent(s) => assert_eq!(s, "/unknown foo bar"),
            DispatchOutcome::HandledLocally => panic!("unknown slash must pass to agent"),
        }
    }

    #[test]
    fn profile_command_passes_through_to_agent() {
        let mut profile = SlashCommandRegistry::new();
        profile.register(SlashCommand {
            name: "deploy".to_owned(),
            handler: SlashCommandHandler::Skill {
                skill_name: "deploy".to_owned(),
            },
        });
        let state = SlashState::new(empty_seed());
        let registry = build_slash_registry(&state, Some(&profile));
        let outcome = dispatch_input("/deploy staging", &registry).unwrap();
        match outcome {
            DispatchOutcome::PassToAgent(s) => assert_eq!(s, "/deploy staging"),
            DispatchOutcome::HandledLocally => panic!("profile command must pass to agent"),
        }
    }

    #[test]
    fn empty_slash_passes_through_to_agent() {
        let state = SlashState::new(empty_seed());
        let registry = build_slash_registry(&state, None);
        let outcome = dispatch_input("/   ", &registry).unwrap();
        match outcome {
            DispatchOutcome::PassToAgent(s) => assert_eq!(s, "/   "),
            DispatchOutcome::HandledLocally => panic!("empty slash must pass to agent"),
        }
    }
}
