//! CLI slash-command registry construction (NC-006 R1, R2–R12).
//!
//! [`build_slash_registry`] merges a (currently empty by design)
//! profile-supplied [`SlashCommandRegistry`] with the CLI
//! built-in commands from libnorn's shared slash catalog. Profile commands
//! are registered first; CLI builtins overwrite same-named entries
//! second so the CLI surface always wins on collision. After both
//! sources have been applied the function populates the
//! [`SlashState::command_descriptions`](super::state::SlashState::command_descriptions)
//! snapshot used by `/help`.
//!
//! Every closure returns `Ok(Vec::new())` — slash commands handled
//! locally must not emit a user message to the model. The dispatcher
//! ([`super::dispatch::dispatch_input`]) intercepts each CLI builtin by
//! name and never reaches
//! [`run_agent_step`](norn::agent_loop::runner::run_agent_step) for it, so
//! the empty-expansion behaviour is purely a defensive invariant.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use norn::agent_loop::{
    BuiltinSlashKind, CustomSlashHandler, EffortCommand, ServiceTierCommand, SlashCommand,
    SlashCommandHandler, SlashCommandRegistry, SlashSurface, builtin_slash_commands, effort_label,
    parse_effort_command, parse_service_tier_command, service_tier_supported_for_model,
    unsupported_service_tier_message,
};
use norn::provider::request::ServiceTier;

use crate::config::parse_inline_or_file;
use crate::session::SessionManager;

use super::state::SlashState;

/// CLI built-in command names. The CLI surface registers each entry as
/// a [`SlashCommandHandler::Custom`] so the slash dispatcher can
/// intercept by name before invoking the agent.
#[must_use]
pub fn cli_builtin_names() -> Vec<&'static str> {
    builtin_slash_commands(SlashSurface::Cli)
        .map(|command| command.name)
        .collect()
}

/// Static `(name, description)` rows used by `/help` and by the
/// initial population of
/// [`SlashState::command_descriptions`](super::state::SlashState::command_descriptions).
///
/// `/exit` and `/quit` are deliberately listed with the same description
/// so users see both aliases in `/help` output.
#[must_use]
pub fn builtin_descriptions() -> Vec<(&'static str, &'static str)> {
    builtin_slash_commands(SlashSurface::Cli)
        .map(|command| (command.name, command.cli_description))
        .collect()
}

/// Placeholder description used for profile-registered slash commands.
/// libnorn's [`SlashCommand`] struct does not carry a description field,
/// so the CLI surfaces a fixed tag instead of attempting to infer one
/// from the handler shape.
pub const PROFILE_DESCRIPTION_PLACEHOLDER: &str = "(profile)";

/// Build the merged slash registry: profile commands first, CLI
/// builtins second.
///
/// Profile commands appear in `/help` and in tab completion. CLI
/// builtins always overwrite same-named profile entries so a profile
/// can never displace `/help`, `/exit`, or any other built-in.
///
/// The returned [`SlashCommandRegistry`] is installed onto
/// [`LoopContext::slash_commands`](norn::agent_loop::loop_context::LoopContext::slash_commands)
/// so that profile commands continue to fire inside
/// [`run_agent_step`](norn::agent_loop::runner::run_agent_step) when the
/// dispatcher hands an unrecognised slash through to the agent.
///
/// As a side effect the merged command roster is recorded inside
/// `state.command_descriptions` so `/help` can iterate the full set.
#[must_use]
pub fn build_slash_registry(
    state: &SlashState,
    profile_registry: Option<&SlashCommandRegistry>,
) -> SlashCommandRegistry {
    let mut registry = SlashCommandRegistry::new();

    if let Some(profile) = profile_registry {
        for name in profile.names() {
            if let Some(command) = profile.get(name) {
                registry.register(command.clone());
            }
        }
    }

    register_cli_builtins(&mut registry, state);
    refresh_command_snapshot(&registry, state);

    registry
}

/// Refresh the `command_descriptions` snapshot inside `state` from the
/// merged registry. CLI builtins use their fixed
/// shared built-in descriptions; every other name is tagged
/// [`PROFILE_DESCRIPTION_PLACEHOLDER`].
///
/// Public so tests can exercise the snapshot in isolation when the
/// registry is mutated outside [`build_slash_registry`].
pub fn refresh_command_snapshot(registry: &SlashCommandRegistry, state: &SlashState) {
    let builtin = builtin_description_map();
    let mut rows: Vec<(String, String)> = registry
        .names()
        .map(|name| {
            let description = builtin
                .get(name)
                .copied()
                .unwrap_or(PROFILE_DESCRIPTION_PLACEHOLDER);
            (name.to_owned(), description.to_owned())
        })
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    *state.command_descriptions.lock() = rows;
}

fn builtin_description_map() -> std::collections::HashMap<&'static str, &'static str> {
    builtin_descriptions().into_iter().collect()
}

fn register_cli_builtins(registry: &mut SlashCommandRegistry, state: &SlashState) {
    for command in builtin_slash_commands(SlashSurface::Cli) {
        let handler = match command.kind {
            BuiltinSlashKind::Help => help_handler(state),
            BuiltinSlashKind::Tools => tools_handler(state),
            BuiltinSlashKind::Model => model_handler(state),
            BuiltinSlashKind::Effort => effort_handler(state),
            BuiltinSlashKind::ServiceTier => service_tier_handler(state),
            BuiltinSlashKind::Fast => fast_handler(state),
            BuiltinSlashKind::Schema => schema_handler(state),
            BuiltinSlashKind::Compact => compact_handler(state),
            BuiltinSlashKind::Clear => clear_handler(state),
            BuiltinSlashKind::Session => session_handler(state),
            BuiltinSlashKind::Name => name_handler(state),
            BuiltinSlashKind::Variables => variables_handler(state),
            BuiltinSlashKind::Exit | BuiltinSlashKind::Quit => exit_handler(state),
            BuiltinSlashKind::New => continue,
        };
        register_custom(registry, command.name, handler);
    }
}

fn register_custom(registry: &mut SlashCommandRegistry, name: &str, handler: CustomSlashHandler) {
    registry.register(SlashCommand {
        name: name.to_owned(),
        handler: SlashCommandHandler::Custom { handler },
    });
}

// -- /help ------------------------------------------------------------------

fn help_handler(state: &SlashState) -> CustomSlashHandler {
    let descriptions = Arc::clone(&state.command_descriptions);
    Arc::new(move |_arg| {
        let rows = descriptions.lock().clone();
        eprintln!("Available commands:");
        for (name, description) in rows {
            eprintln!("  /{name:<10}  {description}");
        }
        Ok(Vec::new())
    })
}

// -- /tools -----------------------------------------------------------------

fn tools_handler(state: &SlashState) -> CustomSlashHandler {
    let tools = Arc::clone(&state.tools_snapshot);
    Arc::new(move |_arg| {
        if tools.is_empty() {
            eprintln!("No tools registered.");
            return Ok(Vec::new());
        }
        eprintln!("Available tools:");
        for (name, description) in tools.iter() {
            eprintln!("  {name:<20}  {description}");
        }
        Ok(Vec::new())
    })
}

// -- /model -----------------------------------------------------------------

fn model_handler(state: &SlashState) -> CustomSlashHandler {
    let model = Arc::clone(&state.model);
    let service_tier = Arc::clone(&state.service_tier);
    Arc::new(move |arg| {
        let trimmed = arg.trim();
        if trimmed.is_empty() {
            let current = model.lock().clone();
            eprintln!("{current}");
        } else {
            trimmed.clone_into(&mut model.lock());
            let cleared_tier = clear_unsupported_service_tier(trimmed, &service_tier);
            if let Some(tier) = cleared_tier {
                eprintln!(
                    "Switched to model: {trimmed}; cleared service tier '{}' because it is unsupported",
                    tier.as_str(),
                );
            } else {
                eprintln!("Switched to model: {trimmed}");
            }
        }
        Ok(Vec::new())
    })
}

// -- /effort / /reasoning-effort -------------------------------------------

fn effort_handler(state: &SlashState) -> CustomSlashHandler {
    let reasoning_effort = Arc::clone(&state.reasoning_effort);
    Arc::new(move |arg| {
        let trimmed = arg.trim();
        if trimmed.is_empty() {
            let current = (*reasoning_effort.lock()).map_or("default", effort_label);
            eprintln!("{current}");
            return Ok(Vec::new());
        }
        match parse_effort_command(trimmed) {
            Some(EffortCommand::Set(effort)) => {
                *reasoning_effort.lock() = Some(effort);
                eprintln!("Reasoning effort: {}", effort_label(effort));
            }
            Some(EffortCommand::Clear) => {
                *reasoning_effort.lock() = None;
                eprintln!("Reasoning effort cleared.");
            }
            None => {
                eprintln!(
                    "norn: invalid reasoning effort '{trimmed}'; expected none, low, medium, high, x-high, or default"
                );
            }
        }
        Ok(Vec::new())
    })
}

// -- /service-tier / /fast --------------------------------------------------

fn service_tier_handler(state: &SlashState) -> CustomSlashHandler {
    let service_tier = Arc::clone(&state.service_tier);
    let model = Arc::clone(&state.model);
    Arc::new(move |arg| {
        let trimmed = arg.trim().to_ascii_lowercase();
        if trimmed.is_empty() {
            let current = match *service_tier.lock() {
                Some(tier) => tier.as_str().to_owned(),
                None => "none".to_owned(),
            };
            eprintln!("{current}");
            return Ok(Vec::new());
        }
        match parse_service_tier_command(&trimmed) {
            Some(ServiceTierCommand::Fast) => {
                let model_name = model.lock().clone();
                if service_tier_supported_for_model(&model_name, ServiceTier::Fast) {
                    *service_tier.lock() = Some(ServiceTier::Fast);
                    eprintln!("Service tier: fast");
                } else {
                    eprintln!("{}", unsupported_service_tier_message(&model_name, "fast"));
                }
            }
            Some(ServiceTierCommand::Clear) => {
                *service_tier.lock() = None;
                eprintln!("Service tier cleared.");
            }
            None => {
                eprintln!("norn: invalid service tier '{trimmed}'; expected fast or none");
            }
        }
        Ok(Vec::new())
    })
}

fn fast_handler(state: &SlashState) -> CustomSlashHandler {
    let service_tier = Arc::clone(&state.service_tier);
    let model = Arc::clone(&state.model);
    Arc::new(move |_arg| {
        let model_name = model.lock().clone();
        if service_tier_supported_for_model(&model_name, ServiceTier::Fast) {
            *service_tier.lock() = Some(ServiceTier::Fast);
            eprintln!("Service tier: fast");
        } else {
            eprintln!("{}", unsupported_service_tier_message(&model_name, "fast"));
        }
        Ok(Vec::new())
    })
}

fn clear_unsupported_service_tier(
    model: &str,
    service_tier: &Arc<parking_lot::Mutex<Option<ServiceTier>>>,
) -> Option<ServiceTier> {
    let mut guard = service_tier.lock();
    let tier = (*guard)?;
    if service_tier_supported_for_model(model, tier) {
        return None;
    }
    *guard = None;
    Some(tier)
}

// -- /schema ----------------------------------------------------------------

fn schema_handler(state: &SlashState) -> CustomSlashHandler {
    let schema = Arc::clone(&state.output_schema);
    Arc::new(move |arg| {
        let trimmed = arg.trim();
        if trimmed.is_empty() {
            let current = schema.lock().clone();
            match current {
                Some(value) => {
                    let pretty =
                        serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
                    eprintln!("{pretty}");
                }
                None => eprintln!("No output schema set."),
            }
            return Ok(Vec::new());
        }
        match parse_inline_or_file(trimmed) {
            Ok(value) => {
                *schema.lock() = Some(value);
                eprintln!("Output schema set.");
            }
            Err(err) => {
                eprintln!("norn: invalid schema: {err}");
            }
        }
        Ok(Vec::new())
    })
}

// -- /compact ---------------------------------------------------------------

fn compact_handler(state: &SlashState) -> CustomSlashHandler {
    let flag = Arc::clone(&state.compact_requested);
    Arc::new(move |_arg| {
        flag.store(true, Ordering::Relaxed);
        Ok(Vec::new())
    })
}

// -- /clear -----------------------------------------------------------------

fn clear_handler(state: &SlashState) -> CustomSlashHandler {
    let flag = Arc::clone(&state.clear_requested);
    Arc::new(move |_arg| {
        flag.store(true, Ordering::Relaxed);
        eprintln!("Conversation cleared.");
        Ok(Vec::new())
    })
}

// -- /session ---------------------------------------------------------------

fn session_handler(state: &SlashState) -> CustomSlashHandler {
    let store_cell = Arc::clone(&state.store);
    let session_id = state.session_id.clone();
    let session_name = Arc::clone(&state.session_name);
    let cumulative = Arc::clone(&state.cumulative_usage);
    Arc::new(move |_arg| {
        let id = session_id.as_deref().unwrap_or("<no-session>");
        let name_snapshot = session_name.lock().clone();
        let name_str = name_snapshot.as_deref().unwrap_or("unnamed");
        let store_arc = Arc::clone(&store_cell.lock());
        let events = store_arc.events();
        let turns = events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    norn::session::events::SessionEvent::UserMessage { .. }
                )
            })
            .count();
        let usage = cumulative.lock().clone();
        eprintln!("Session:");
        eprintln!("  id:     {id}");
        eprintln!("  name:   {name_str}");
        eprintln!("  turns:  {turns}");
        eprintln!("  usage:");
        eprintln!("    input:        {}", usage.input_tokens);
        eprintln!("    output:       {}", usage.output_tokens);
        eprintln!("    cache read:   {}", usage.cache_read_tokens);
        eprintln!("    cache write:  {}", usage.cache_write_tokens);
        Ok(Vec::new())
    })
}

// -- /name ------------------------------------------------------------------

fn name_handler(state: &SlashState) -> CustomSlashHandler {
    let session_name = Arc::clone(&state.session_name);
    let session_id = state.session_id.clone();
    let data_dir = state.data_dir.clone();
    let no_session = state.no_session;
    Arc::new(move |arg| {
        let trimmed = arg.trim();
        if trimmed.is_empty() {
            eprintln!("Usage: /name <text>");
            return Ok(Vec::new());
        }
        *session_name.lock() = Some(trimmed.to_owned());
        eprintln!("Session named: {trimmed}");
        if !no_session && let Some(id) = session_id.as_deref() {
            persist_session_name(&data_dir, id, trimmed);
        }
        Ok(Vec::new())
    })
}

fn persist_session_name(data_dir: &Path, session_id: &str, name: &str) {
    if let Err(err) = SessionManager::new(data_dir).rename(session_id, Some(name.to_owned())) {
        eprintln!("norn: warning: failed to update session index: {err}");
    }
}

// -- /variables -------------------------------------------------------------

fn variables_handler(state: &SlashState) -> CustomSlashHandler {
    let pairs = state.variable_pairs.clone();
    Arc::new(move |_arg| {
        if pairs.is_empty() {
            eprintln!("No session variables set.");
            return Ok(Vec::new());
        }
        eprintln!("Session variables:");
        for (name, value) in &pairs {
            eprintln!("  {name}={value}");
        }
        Ok(Vec::new())
    })
}

// -- /exit / /quit ----------------------------------------------------------

fn exit_handler(state: &SlashState) -> CustomSlashHandler {
    let flag = Arc::clone(&state.exit_requested);
    Arc::new(move |_arg| {
        flag.store(true, Ordering::Relaxed);
        Ok(Vec::new())
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::Ordering;

    use norn::error::NornError;
    use norn::provider::request::{Message, ReasoningEffort, ServiceTier};
    use norn::session::store::EventStore;

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
            data_dir: PathBuf::from("/tmp/norn-cli-slash-registry"),
            no_session: true,
            variable_pairs: Vec::new(),
            tools: Vec::new(),
            store: Arc::new(EventStore::new()),
        }
    }

    fn fire(registry: &SlashCommandRegistry, name: &str, arg: &str) {
        let command = registry
            .get(name)
            .unwrap_or_else(|| panic!("/{name} should be registered"));
        match &command.handler {
            SlashCommandHandler::Custom { handler } => {
                let messages = handler(arg).expect("handler ran");
                assert!(
                    messages.is_empty(),
                    "CLI builtin /{name} must not emit user messages",
                );
            }
            other => panic!("/{name} handler is not Custom: {other:?}"),
        }
    }

    #[test]
    fn registry_contains_all_builtins() {
        let state = SlashState::new(empty_seed());
        let registry = build_slash_registry(&state, None);
        let names = cli_builtin_names();
        for name in &names {
            assert!(registry.get(name).is_some(), "missing CLI builtin: /{name}");
        }
        assert_eq!(registry.len(), names.len());
    }

    #[test]
    fn command_descriptions_populated_for_builtins() {
        let state = SlashState::new(empty_seed());
        let _registry = build_slash_registry(&state, None);
        let rows = state.command_descriptions.lock().clone();
        let names = cli_builtin_names();
        assert_eq!(rows.len(), names.len());
        for name in &names {
            assert!(
                rows.iter().any(|(n, _)| n == name),
                "/help snapshot missing /{name}",
            );
        }
    }

    #[test]
    fn profile_command_does_not_override_cli_builtin() {
        let mut profile = SlashCommandRegistry::new();
        profile.register(SlashCommand {
            name: "help".to_owned(),
            handler: SlashCommandHandler::Skill {
                skill_name: "rogue-help".to_owned(),
            },
        });
        let state = SlashState::new(empty_seed());
        let registry = build_slash_registry(&state, Some(&profile));
        let help = registry.get("help").expect("help present");
        assert!(
            matches!(help.handler, SlashCommandHandler::Custom { .. }),
            "CLI builtin /help must win over profile /help",
        );
    }

    #[test]
    fn profile_command_appears_in_merged_registry_and_help_snapshot() {
        let mut profile = SlashCommandRegistry::new();
        profile.register(SlashCommand {
            name: "deploy".to_owned(),
            handler: SlashCommandHandler::Skill {
                skill_name: "deploy".to_owned(),
            },
        });
        let state = SlashState::new(empty_seed());
        let registry = build_slash_registry(&state, Some(&profile));
        assert!(registry.get("deploy").is_some());
        let rows = state.command_descriptions.lock().clone();
        let deploy_row = rows.iter().find(|(n, _)| n == "deploy");
        assert_eq!(
            deploy_row.map(|(_, d)| d.as_str()),
            Some(PROFILE_DESCRIPTION_PLACEHOLDER),
        );
    }

    #[test]
    fn help_handler_runs_without_emitting_messages() {
        let state = SlashState::new(empty_seed());
        let registry = build_slash_registry(&state, None);
        fire(&registry, "help", "");
    }

    #[test]
    fn tools_handler_handles_empty_snapshot() {
        let state = SlashState::new(empty_seed());
        let registry = build_slash_registry(&state, None);
        fire(&registry, "tools", "");
    }

    #[test]
    fn tools_handler_renders_snapshot() {
        let mut seed = empty_seed();
        seed.tools = vec![
            ("read".to_owned(), "Read a file".to_owned()),
            ("write".to_owned(), "Write a file".to_owned()),
        ];
        let state = SlashState::new(seed);
        let registry = build_slash_registry(&state, None);
        fire(&registry, "tools", "");
    }

    #[test]
    fn model_no_arg_prints_current_model() {
        let state = SlashState::new(empty_seed());
        let registry = build_slash_registry(&state, None);
        fire(&registry, "model", "");
        assert_eq!(state.model_snapshot(), "gpt-x");
    }

    #[test]
    fn model_with_arg_switches_model() {
        let state = SlashState::new(empty_seed());
        let registry = build_slash_registry(&state, None);
        fire(&registry, "model", "gpt-5.5");
        assert_eq!(state.model_snapshot(), "gpt-5.5");
    }

    #[test]
    fn service_tier_commands_update_runtime_state() {
        let mut seed = empty_seed();
        seed.model = "gpt-5.5".to_owned();
        let state = SlashState::new(seed);
        let registry = build_slash_registry(&state, None);

        fire(&registry, "service-tier", "fast");
        assert_eq!(state.service_tier_snapshot(), Some(ServiceTier::Fast));

        fire(&registry, "service-tier", "none");
        assert_eq!(state.service_tier_snapshot(), None);

        fire(&registry, "fast", "");
        assert_eq!(state.service_tier_snapshot(), Some(ServiceTier::Fast));
    }

    #[test]
    fn service_tier_fast_rejects_unsupported_model() {
        let mut seed = empty_seed();
        seed.model = "gpt-5.4-mini".to_owned();
        let state = SlashState::new(seed);
        let registry = build_slash_registry(&state, None);

        fire(&registry, "service-tier", "fast");
        assert_eq!(state.service_tier_snapshot(), None);

        fire(&registry, "fast", "");
        assert_eq!(state.service_tier_snapshot(), None);
    }

    #[test]
    fn model_switch_clears_unsupported_service_tier() {
        let mut seed = empty_seed();
        seed.model = "gpt-5.5".to_owned();
        seed.service_tier = Some(ServiceTier::Fast);
        let state = SlashState::new(seed);
        let registry = build_slash_registry(&state, None);

        fire(&registry, "model", "gpt-5.4-mini");

        assert_eq!(state.model_snapshot(), "gpt-5.4-mini");
        assert_eq!(state.service_tier_snapshot(), None);
    }

    #[test]
    fn effort_commands_update_runtime_state() {
        let state = SlashState::new(empty_seed());
        let registry = build_slash_registry(&state, None);

        fire(&registry, "effort", "high");
        assert_eq!(
            state.reasoning_effort_snapshot(),
            Some(ReasoningEffort::High),
        );

        fire(&registry, "reasoning-effort", "x-high");
        assert_eq!(
            state.reasoning_effort_snapshot(),
            Some(ReasoningEffort::XHigh),
        );

        fire(&registry, "effort", "default");
        assert_eq!(state.reasoning_effort_snapshot(), None);
    }

    #[test]
    fn schema_no_arg_with_none_prints_placeholder() {
        let state = SlashState::new(empty_seed());
        let registry = build_slash_registry(&state, None);
        fire(&registry, "schema", "");
        assert!(state.output_schema_snapshot().is_none());
    }

    #[test]
    fn schema_inline_json_sets_active_schema() {
        let state = SlashState::new(empty_seed());
        let registry = build_slash_registry(&state, None);
        fire(&registry, "schema", r#"{"type":"object"}"#);
        assert_eq!(
            state.output_schema_snapshot(),
            Some(serde_json::json!({"type": "object"})),
        );
    }

    #[test]
    fn schema_invalid_input_leaves_state_unchanged() {
        let mut seed = empty_seed();
        seed.output_schema = Some(serde_json::json!({"type": "string"}));
        let state = SlashState::new(seed);
        let registry = build_slash_registry(&state, None);
        fire(&registry, "schema", "/no/such/file.json");
        assert_eq!(
            state.output_schema_snapshot(),
            Some(serde_json::json!({"type": "string"})),
        );
    }

    #[test]
    fn compact_handler_sets_flag() {
        let state = SlashState::new(empty_seed());
        let registry = build_slash_registry(&state, None);
        fire(&registry, "compact", "");
        assert!(state.compact_requested.load(Ordering::Relaxed));
    }

    #[test]
    fn clear_handler_sets_flag_and_keeps_state() {
        let state = SlashState::new(empty_seed());
        let registry = build_slash_registry(&state, None);
        fire(&registry, "clear", "");
        assert!(state.clear_requested.load(Ordering::Relaxed));
    }

    #[test]
    fn session_handler_handles_no_persistence() {
        let state = SlashState::new(empty_seed());
        let registry = build_slash_registry(&state, None);
        fire(&registry, "session", "");
    }

    #[test]
    fn name_handler_with_arg_updates_session_name() {
        let state = SlashState::new(empty_seed());
        let registry = build_slash_registry(&state, None);
        fire(&registry, "name", "refactor-auth");
        assert_eq!(
            state.session_name_snapshot().as_deref(),
            Some("refactor-auth")
        );
    }

    #[test]
    fn name_handler_no_arg_leaves_session_name_unchanged() {
        let state = SlashState::new(empty_seed());
        let registry = build_slash_registry(&state, None);
        fire(&registry, "name", "");
        assert!(state.session_name_snapshot().is_none());
    }

    #[test]
    fn variables_handler_handles_empty_pairs() {
        let state = SlashState::new(empty_seed());
        let registry = build_slash_registry(&state, None);
        fire(&registry, "variables", "");
    }

    #[test]
    fn variables_handler_with_pairs_renders() {
        let mut seed = empty_seed();
        seed.variable_pairs = vec![("project".to_owned(), "yggdrasil".to_owned())];
        let state = SlashState::new(seed);
        let registry = build_slash_registry(&state, None);
        fire(&registry, "variables", "");
    }

    #[test]
    fn exit_and_quit_both_flip_exit_flag() {
        let state = SlashState::new(empty_seed());
        let registry = build_slash_registry(&state, None);
        fire(&registry, "exit", "");
        assert!(state.exit_requested.load(Ordering::Relaxed));

        let state2 = SlashState::new(empty_seed());
        let registry2 = build_slash_registry(&state2, None);
        fire(&registry2, "quit", "");
        assert!(state2.exit_requested.load(Ordering::Relaxed));
    }

    #[test]
    fn closure_signature_satisfies_norn_error() {
        // Smoke test ensuring the handler type alias is the right shape.
        fn assert_send_sync<T: Send + Sync>(_: &T) {}
        let state = SlashState::new(empty_seed());
        let registry = build_slash_registry(&state, None);
        let help = registry.get("help").unwrap();
        match &help.handler {
            SlashCommandHandler::Custom { handler } => {
                assert_send_sync(handler);
                let result: Result<Vec<Message>, NornError> = handler("");
                assert!(result.unwrap().is_empty());
            }
            other => panic!("expected Custom, got {other:?}"),
        }
    }
}
