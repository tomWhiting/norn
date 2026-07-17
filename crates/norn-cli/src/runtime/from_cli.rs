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
//! It is the single library-owned assembler's CLI front door: the print,
//! driven, and TUI drivers all assemble through it. Driver-specific concerns
//! that
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
use norn::session::store::{DurabilityPolicy, EventStore};
use norn::session::{ResumePolicy, SessionManager};
use norn::tool::context::SharedWorkingDir;

use crate::cli::{BuildError, Cli};
use crate::config::{
    AppliedOverrides, ConfigOverrides, apply_config_overrides_to_loop, apply_loop_config_overrides,
    apply_settings_to_agent_config, default_agent_loop_config, load_rule_engine,
    merge_event_schemas, parse_inline_or_file, parse_kv, resolve_index_lock_deadline,
};
use crate::runtime::build_write_tool;

/// Map a resolved CLI invocation onto an [`AgentBuilder`].
///
/// `profile` already carries the CLI model / tool / reasoning overrides
/// (the caller ran `apply_cli_profile_overrides`, which produced
/// `applied`); the allow-list therefore rides on `profile.tools` and is
/// not re-applied here. `settings` is the merged settings the caller
/// loaded. The returned builder has `.load_runtime_base()` set, so
/// `build()` re-derives the settings-backed agent-loop config, rules,
/// hooks, task store, skill catalog, and permission policy and overlays
/// the explicit config assembled here on top — the golden-snapshot fence
/// (`assembly_conformance.rs`) pins this overlay's assembled result.
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
    // `apply_working_dir(cli)` already ran in the caller (`resolve_invocation`
    // mutates the process CWD), so the resolved working directory is the
    // process CWD here.
    let cwd = std::env::current_dir()?;

    // Merge event schemas and build the length-limited write tool while the
    // resolved profile is still owned here; the profile is then moved into
    // the builder.
    let event_schemas = merge_event_schemas(&profile, &cli.event_schema)?;
    // `register_standard_tools` (run inside `load_runtime_base`) registers a
    // default-limit `WriteTool`; the CLI's profile `[tool_config.write]`
    // section and `-c write.max_code_lines=N` override must overlay it, so
    // the configured tool is pushed as an extra tool (registered after the
    // standard set, keying on the same name — deny-of-drift by construction).
    let config_overrides = ConfigOverrides::parse(&cli.config)?;
    let write_tool = build_write_tool(&profile, &config_overrides)?;

    let mut builder = AgentBuilder::new(provider)
        .profile(profile)
        .working_dir(cwd.clone())
        .load_runtime_base()
        .tool(Box::new(write_tool));

    // `--workspace-root` confines the file tools to the given root.
    // `AgentBuilder::build` validates it through the single shared
    // `validate_workspace_root` (canonicalize; reject a nonexistent /
    // non-directory root loudly), so a bad root fails assembly instead of
    // silently confining nothing.
    if let Some(root) = cli.workspace_root.as_ref() {
        builder = builder.workspace_root(root.clone());
    }

    // `--rules <file>` loads an explicit guardrail/injection rule engine.
    // It is merged ONTO the runtime base's auto-discovered rules by
    // `AgentBuilder::build` (both sets enforced); wiring the working dir
    // here covers the case where the base discovered no rules and the
    // explicit engine stands alone. An unreadable / malformed file is a
    // hard argument error.
    if let Some(path) = cli.rules.as_deref() {
        let shared_wd = SharedWorkingDir::new(cwd.clone());
        let engine = load_rule_engine(path)?.with_working_dir(shared_wd);
        builder = builder.rules(engine);
    }

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
    let mut agent_config = default_agent_loop_config();
    apply_settings_to_agent_config(settings, &mut agent_config)?;
    apply_config_overrides_to_loop(&config_overrides, &mut agent_config);
    apply_loop_config_overrides(cli, &mut agent_config)?;
    // On the unified path the output schema rides on the agent-loop config
    // (serialized, introspectable) rather than being threaded separately
    // into the runner call.
    if let Some(raw) = cli.output_schema.as_deref() {
        let schema = parse_inline_or_file(raw)
            .map_err(|err| BuildError::Argument(format!("--output-schema: {err}")))?;
        agent_config.output_schema = Some(schema);
    }
    builder = builder.agent_config(agent_config);

    if let Some(schemas) = event_schemas {
        builder = builder.event_schemas(schemas);
    }
    // `--variables KEY=VALUE` pairs are handed to the builder as raw
    // name/value pairs (not a pre-built store): `build` applies them to the
    // store it mints with the RESOLVED session id, so a persisted-session
    // run never aborts on a store carrying an independently-minted id.
    if !cli.variables.is_empty() {
        let mut pairs = Vec::with_capacity(cli.variables.len());
        for raw in &cli.variables {
            pairs.push(parse_kv(raw)?);
        }
        builder = builder.variable_pairs(pairs);
    }

    // Session front door (D4): `--no-session` maps to a fresh in-memory
    // store; every other flag combination opens a managed session through
    // the one library `open_session` front door with an explicit
    // `Flush` durability.
    //
    // The index-lock deadline is resolved unconditionally (invalid config
    // errors loudly even under `--no-session`) and bounds every index
    // mutation the manager performs: without it, `file.lock()` blocks
    // forever behind a wedged sibling process and every new norn on the
    // machine silently hangs before a session file even exists. On expiry
    // the typed `SessionPersistError::IndexLockTimeout` propagates through
    // the normal build/open error surface, naming the lock file and the
    // deadline.
    let index_lock_deadline = resolve_index_lock_deadline(settings, &config_overrides)?;
    if cli.no_session {
        builder = builder.session(std::sync::Arc::new(EventStore::new()));
    } else if let Some(spec) = session_spec_from_cli(cli, &cwd) {
        let manager =
            SessionManager::standard()?.with_index_lock_deadline(Some(index_lock_deadline));
        builder = builder.open_session(&manager, spec, DurabilityPolicy::Flush);
    }

    Ok(builder)
}

/// Translate the CLI session flags into the matching [`SessionSpec`].
///
/// Returns `None` only for `--no-session` (handled by the caller as a
/// fresh in-memory store — there is no `SessionSpec` for "no session").
///
/// An empty `--resume` / `--fork` value is the "latest session for THIS
/// project" sentinel: it maps to
/// [`SessionSpec::ResumeLatestInWorkingDir`] /
/// [`SessionSpec::ForkLatestInWorkingDir`] carrying `working_dir`, so the
/// library resolves it scoped to the current working directory — never the
/// globally most-recently-updated session in an unrelated directory. A
/// non-empty value keeps the exact-id [`SessionSpec::Resume`] /
/// [`SessionSpec::Fork`] resolution.
#[must_use]
fn session_spec_from_cli(cli: &Cli, working_dir: &std::path::Path) -> Option<SessionSpec> {
    if cli.no_session {
        return None;
    }
    let working_dir_string = || working_dir.display().to_string();
    let resume_policy = if cli.allow_degraded_session {
        ResumePolicy::ApproveFreshEpochProjection
    } else {
        ResumePolicy::RequireCanonical
    };
    if let Some(source) = cli.resume.as_deref() {
        let trimmed = source.trim();
        return Some(if trimmed.is_empty() {
            SessionSpec::resume_latest_with_policy(working_dir_string(), resume_policy)
        } else {
            SessionSpec::resume_with_policy(trimmed, resume_policy)
        });
    }
    if let Some(source) = cli.fork.as_deref() {
        let trimmed = source.trim();
        return Some(if trimmed.is_empty() {
            SessionSpec::fork_latest_with_policy(working_dir_string(), resume_policy)
        } else {
            SessionSpec::fork_with_policy(trimmed, None, resume_policy)
        });
    }
    if let Some(id) = cli.session_id.as_deref() {
        return Some(if cli.resume_if_exists {
            SessionSpec::open_or_resume_with_policy(id, resume_policy)
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

    fn spec_for(cli: &Cli) -> Option<SessionSpec> {
        session_spec_from_cli(cli, std::path::Path::new("/repo/current"))
    }

    #[test]
    fn no_session_maps_to_no_spec() {
        let cli = cli_with(|c| c.no_session = true);
        assert!(spec_for(&cli).is_none());
    }

    #[test]
    fn default_maps_to_create_with_session_name() {
        let cli = cli_with(|c| c.session_name = Some("my-work".to_owned()));
        match spec_for(&cli) {
            Some(SessionSpec::Create { name }) => assert_eq!(name.as_deref(), Some("my-work")),
            other => panic!("expected Create, got {other:?}"),
        }
    }

    #[test]
    fn resume_maps_to_resume_spec() {
        let cli = cli_with(|c| c.resume = Some("abc123".to_owned()));
        match spec_for(&cli) {
            Some(SessionSpec::Resume { id_or_name, policy }) => {
                assert_eq!(id_or_name, "abc123");
                assert_eq!(policy, ResumePolicy::RequireCanonical);
            }
            other => panic!("expected Resume, got {other:?}"),
        }
    }

    #[test]
    fn degraded_session_approval_reaches_resume_policy() {
        let cli = cli_with(|c| {
            c.resume = Some("abc123".to_owned());
            c.allow_degraded_session = true;
        });
        match spec_for(&cli) {
            Some(SessionSpec::Resume { id_or_name, policy }) => {
                assert_eq!(id_or_name, "abc123");
                assert_eq!(policy, ResumePolicy::ApproveFreshEpochProjection);
            }
            other => panic!("expected Resume, got {other:?}"),
        }
    }

    /// Regression (F1): an empty `--resume` maps to the working-dir-scoped
    /// sentinel carrying the resolved working directory, not a global
    /// [`SessionSpec::Resume`] with an empty id (which the library would
    /// resolve to the globally newest session, cross-contaminating
    /// unrelated projects).
    #[test]
    fn empty_resume_maps_to_working_dir_scoped_sentinel() {
        let cli = cli_with(|c| c.resume = Some(String::new()));
        match spec_for(&cli) {
            Some(SessionSpec::ResumeLatestInWorkingDir {
                working_dir,
                policy,
            }) => {
                assert_eq!(working_dir, "/repo/current");
                assert_eq!(policy, ResumePolicy::RequireCanonical);
            }
            other => panic!("expected ResumeLatestInWorkingDir, got {other:?}"),
        }
    }

    #[test]
    fn fork_maps_to_fork_spec() {
        let cli = cli_with(|c| c.fork = Some("src-sess".to_owned()));
        match spec_for(&cli) {
            Some(SessionSpec::Fork {
                source,
                name,
                policy,
            }) => {
                assert_eq!(source, "src-sess");
                assert!(name.is_none());
                assert_eq!(policy, ResumePolicy::RequireCanonical);
            }
            other => panic!("expected Fork, got {other:?}"),
        }
    }

    /// Regression (F1): an empty `--fork` maps to the working-dir-scoped
    /// fork sentinel carrying the resolved working directory.
    #[test]
    fn empty_fork_maps_to_working_dir_scoped_sentinel() {
        let cli = cli_with(|c| c.fork = Some("   ".to_owned()));
        match spec_for(&cli) {
            Some(SessionSpec::ForkLatestInWorkingDir {
                working_dir,
                policy,
            }) => {
                assert_eq!(working_dir, "/repo/current");
                assert_eq!(policy, ResumePolicy::RequireCanonical);
            }
            other => panic!("expected ForkLatestInWorkingDir, got {other:?}"),
        }
    }

    #[test]
    fn session_id_without_resume_if_exists_maps_to_create_with_id() {
        let cli = cli_with(|c| c.session_id = Some("fixed-id".to_owned()));
        match spec_for(&cli) {
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
        match spec_for(&cli) {
            Some(SessionSpec::OpenOrResume { id, policy }) => {
                assert_eq!(id, "fixed-id");
                assert_eq!(policy, ResumePolicy::RequireCanonical);
            }
            other => panic!("expected OpenOrResume, got {other:?}"),
        }
    }
}
