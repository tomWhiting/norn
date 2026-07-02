//! [`builder_from_cli`] — map a resolved CLI invocation onto a library
//! [`AgentBuilder`].
//!
//! Provider selection, model-alias / provider-profile resolution, and
//! profile resolution + CLI overrides have already happened in the caller
//! (they are CLI config surface, not assembly, and provider construction
//! sits between model resolution and this call). This function only
//! translates the resolved state — the resolved [`Profile`], the merged
//! [`NornSettings`], the [`AppliedOverrides`] side-channel, and the raw
//! [`Cli`] flags — into [`AgentBuilder`] setter calls.
//!
//! It is the single library-owned assembler's CLI front door, coexisting
//! with [`build_runtime`](crate::runtime::build_runtime) until the print,
//! driven, and TUI drivers migrate onto it. Driver-specific concerns that
//! the resolved config surface cannot carry — the agent-coordination
//! registry (with `.register_root` and `.terminal_reclamation`), the LSP
//! handles, the execution mode, and the event/inbound channel capacities —
//! are chained by each driver onto the returned builder, not decided here.

use std::sync::Arc;

use norn::agent::AgentBuilder;
use norn::agent::SessionSpec;
use norn::config::NornSettings;
use norn::profile::Profile;
use norn::provider::traits::Provider;
use norn::session::SessionManager;
use norn::session::store::{DurabilityPolicy, EventStore};
use norn::tool::context::SharedWorkingDir;

use crate::cli::{BuildError, Cli};
use crate::config::{
    AppliedOverrides, ConfigOverrides, apply_config_overrides_to_loop, apply_loop_config_overrides,
    apply_settings_to_agent_config, build_variable_store, default_agent_loop_config,
    merge_event_schemas, parse_inline_or_file, session_data_dir,
};

/// Map a resolved CLI invocation onto an [`AgentBuilder`].
///
/// `profile` already carries the CLI model / tool / reasoning overrides
/// (the caller ran `apply_cli_profile_overrides`, which produced
/// `applied`); the allow-list therefore rides on `profile.tools` and is
/// not re-applied here. `settings` is the merged settings the caller
/// loaded. The returned builder has `.load_runtime_base()` set, so
/// `build()` re-derives the settings-backed agent-loop config, rules,
/// hooks, task store, skill catalog, and permission policy and overlays
/// the explicit config assembled here on top — the conformance test
/// (`assembly_conformance.rs`) is the fence that this overlay reproduces
/// [`build_runtime`](crate::runtime::build_runtime)'s result.
///
/// # Errors
///
/// [`BuildError::Argument`] when the agent-loop `-c` / flag overrides,
/// the merged event schemas, the `--variables` store, or the
/// `--output-schema` value fail to parse.
pub fn builder_from_cli(
    cli: &Cli,
    provider: Arc<dyn Provider>,
    profile: Profile,
    settings: &NornSettings,
    applied: &AppliedOverrides,
) -> Result<AgentBuilder, BuildError> {
    // `apply_working_dir(cli)` already ran in the caller (it mutates the
    // process CWD), exactly as `build_runtime` does today, so the resolved
    // working directory is the process CWD here.
    let cwd = std::env::current_dir()?;
    let shared_wd = SharedWorkingDir::new(cwd.clone());

    // Merge event schemas while the resolved profile is still owned here;
    // the profile is then moved into the builder.
    let event_schemas = merge_event_schemas(&profile, &cli.event_schema)?;

    let mut builder = AgentBuilder::new(provider)
        .profile(profile)
        .working_dir(cwd)
        .load_runtime_base();

    // The task-store group slug is derived from `--session-name` (replacing
    // `build_shared_task_store`'s slug derivation); unset defers to the
    // runtime base's own `"default"` slug.
    if let Some(name) = cli.session_name.as_deref() {
        builder = builder.task_group_slug(name.to_owned());
    }

    // Deny-wins tool gating. The allow-list already rides on `profile.tools`
    // (set by the caller's `apply_cli_profile_overrides`), so only the
    // `--disallowed-tools` deny-list is layered here.
    if !applied.disallowed_tools.is_empty() {
        let disallowed: Vec<&str> = applied
            .disallowed_tools
            .iter()
            .map(String::as_str)
            .collect();
        builder = builder.disallowed_tools(&disallowed);
    }

    // Agent-loop config: settings supply defaults; `-c key=value` overlays
    // them; explicit CLI flags (`--max-turns`, `--timeout`, …) win last.
    // This mirrors `build_runtime`'s layering exactly.
    let mut agent_config = default_agent_loop_config();
    apply_settings_to_agent_config(settings, &mut agent_config)?;
    let config_overrides = ConfigOverrides::parse(&cli.config)?;
    apply_config_overrides_to_loop(&config_overrides, &mut agent_config);
    apply_loop_config_overrides(cli, &mut agent_config)?;
    // On the unified path the output schema rides on the agent-loop config
    // (serialized, introspectable) rather than being threaded separately
    // into the runner call as `build_runtime` does today.
    if let Some(raw) = cli.output_schema.as_deref() {
        let schema = parse_inline_or_file(raw)
            .map_err(|err| BuildError::Argument(format!("--output-schema: {err}")))?;
        agent_config.output_schema = Some(schema);
    }
    builder = builder.agent_config(agent_config);

    if let Some(schemas) = event_schemas {
        builder = builder.event_schemas(schemas);
    }
    if let Some(variables) = build_variable_store(&cli.variables, shared_wd)? {
        builder = builder.variables(variables);
    }

    // Session front door (D4): `--no-session` maps to a fresh in-memory
    // store; every other flag combination opens a managed session through
    // the one library `open_session` front door with an explicit
    // `Flush` durability.
    if cli.no_session {
        builder = builder.session(EventStore::new());
    } else if let Some(spec) = session_spec_from_cli(cli) {
        let manager = SessionManager::new(session_data_dir());
        builder = builder.open_session(&manager, spec, DurabilityPolicy::Flush);
    }

    Ok(builder)
}

/// Translate the CLI session flags into the matching [`SessionSpec`].
///
/// Returns `None` only for `--no-session` (handled by the caller as a
/// fresh in-memory store — there is no `SessionSpec` for "no session").
///
/// An empty `--resume` / `--fork` value is the "most recently updated
/// session" sentinel, mapped to the empty-string [`SessionSpec::Resume`] /
/// [`SessionSpec::Fork`] source. (The library resolves that globally; the
/// legacy CLI path resolved it scoped to the working directory — a
/// difference the step-3 migration reconciles.)
#[must_use]
fn session_spec_from_cli(cli: &Cli) -> Option<SessionSpec> {
    if cli.no_session {
        return None;
    }
    if let Some(source) = cli.resume.as_deref() {
        return Some(SessionSpec::Resume {
            id_or_name: source.trim().to_owned(),
        });
    }
    if let Some(source) = cli.fork.as_deref() {
        return Some(SessionSpec::Fork {
            source: source.trim().to_owned(),
            name: None,
        });
    }
    if let Some(id) = cli.session_id.as_deref() {
        return Some(if cli.resume_if_exists {
            SessionSpec::OpenOrResume { id: id.to_owned() }
        } else {
            SessionSpec::CreateWithId {
                id: id.to_owned(),
                name: cli.session_name.clone(),
            }
        });
    }
    Some(SessionSpec::Create {
        name: cli.session_name.clone(),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use clap::Parser;

    use super::*;

    fn cli_with(mutate: impl FnOnce(&mut Cli)) -> Cli {
        let mut cli = Cli::parse_from(["norn"]);
        mutate(&mut cli);
        cli
    }

    #[test]
    fn no_session_maps_to_no_spec() {
        let cli = cli_with(|c| c.no_session = true);
        assert!(session_spec_from_cli(&cli).is_none());
    }

    #[test]
    fn default_maps_to_create_with_session_name() {
        let cli = cli_with(|c| c.session_name = Some("my-work".to_owned()));
        match session_spec_from_cli(&cli) {
            Some(SessionSpec::Create { name }) => assert_eq!(name.as_deref(), Some("my-work")),
            other => panic!("expected Create, got {other:?}"),
        }
    }

    #[test]
    fn resume_maps_to_resume_spec() {
        let cli = cli_with(|c| c.resume = Some("abc123".to_owned()));
        match session_spec_from_cli(&cli) {
            Some(SessionSpec::Resume { id_or_name }) => assert_eq!(id_or_name, "abc123"),
            other => panic!("expected Resume, got {other:?}"),
        }
    }

    #[test]
    fn empty_resume_maps_to_latest_sentinel() {
        let cli = cli_with(|c| c.resume = Some(String::new()));
        match session_spec_from_cli(&cli) {
            Some(SessionSpec::Resume { id_or_name }) => assert!(id_or_name.is_empty()),
            other => panic!("expected Resume, got {other:?}"),
        }
    }

    #[test]
    fn fork_maps_to_fork_spec() {
        let cli = cli_with(|c| c.fork = Some("src-sess".to_owned()));
        match session_spec_from_cli(&cli) {
            Some(SessionSpec::Fork { source, name }) => {
                assert_eq!(source, "src-sess");
                assert!(name.is_none());
            }
            other => panic!("expected Fork, got {other:?}"),
        }
    }

    #[test]
    fn session_id_without_resume_if_exists_maps_to_create_with_id() {
        let cli = cli_with(|c| c.session_id = Some("fixed-id".to_owned()));
        match session_spec_from_cli(&cli) {
            Some(SessionSpec::CreateWithId { id, .. }) => assert_eq!(id, "fixed-id"),
            other => panic!("expected CreateWithId, got {other:?}"),
        }
    }

    #[test]
    fn session_id_with_resume_if_exists_maps_to_open_or_resume() {
        let cli = cli_with(|c| {
            c.session_id = Some("fixed-id".to_owned());
            c.resume_if_exists = true;
        });
        match session_spec_from_cli(&cli) {
            Some(SessionSpec::OpenOrResume { id }) => assert_eq!(id, "fixed-id"),
            other => panic!("expected OpenOrResume, got {other:?}"),
        }
    }
}
