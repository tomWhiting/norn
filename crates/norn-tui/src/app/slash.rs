//! TUI-local slash command dispatch.
//!
//! Handles builtins directly in the TUI while sharing semantic parsers with
//! libnorn where commands also exist on the CLI surface.
//!
//! Unknown slashes and profile-registered slash commands return [`None`]
//! from [`try_dispatch_slash`] so the event loop's `Submit` arm falls
//! through to `run_turn`. Inside the agent loop, `libnorn`'s
//! `preprocess_input` handles profile commands; unknown slashes reach
//! the model as user messages (matching REPL behaviour).

use std::fmt::Write as _;
use std::sync::Arc;

use norn::session::context_edit::ContextEdits;
use norn::session::{
    CreateSessionOptions, DurabilityPolicy, EventStore, SessionBinding, SessionBrancher,
    SessionManager, SessionPersistError,
};

use crate::TuiError;

use super::dispatch::write_error_line;
use super::event_loop::RuntimeRefs;
use super::mcp_slash::{handle_mcp, mcp_exit_is_blocked, render_pending_mcp_exit};
use super::model_selection::{
    handle_model, handle_reasoning_effort, handle_service_tier, set_fast_service_tier,
};
use super::notices;
use super::slash_catalog::{
    SlashClass, TuiBuiltinKind, classify_slash, find_tui_builtin_command, tui_builtin_commands,
};
use super::state::AppState;

#[cfg(test)]
use super::slash_catalog::{EffortCommand, is_tui_builtin, parse_effort_command, split_first_word};

/// Outcome of a recognised slash command.
#[derive(Debug)]
pub(super) enum SlashOutcome {
    /// Slash handled — the outer loop should redraw and continue.
    Continue,
    /// The local effect committed, but its result could not be reported.
    AcceptedWithError(TuiError),
    /// Local validation refused the command; retain the submitted draft.
    Rejected,
    /// Slash handled — the TUI should exit cleanly.
    Exit,
}

/// Whether a local operation was admitted, independently of its displayed notices.
#[derive(Debug)]
pub(super) enum LocalCommandOutcome {
    /// The query, setting change or asynchronous operation was admitted.
    Accepted,
    /// The effect committed before this reporting failure occurred.
    AcceptedWithError(TuiError),
    /// The command was refused before its requested operation was admitted.
    Rejected,
}

impl LocalCommandOutcome {
    /// Reporting failure never revokes an already committed local effect.
    pub(super) fn after_acceptance(reporting: Result<(), TuiError>) -> Self {
        match reporting {
            Ok(()) => Self::Accepted,
            Err(error) => Self::AcceptedWithError(error),
        }
    }

    /// Retain both an accepted operation's failure and any failure to report it.
    pub(super) fn after_reported_failure(
        original: TuiError,
        reporting: Result<(), TuiError>,
    ) -> Self {
        Self::after_acceptance(
            reporting.map_err(|secondary| combine_local_errors(original, secondary)),
        )
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{primary}; local command reporting also failed: {secondary}")]
struct LocalCommandErrors {
    #[source]
    primary: TuiError,
    secondary: TuiError,
}

fn combine_local_errors(primary: TuiError, secondary: TuiError) -> TuiError {
    super::render::interaction(LocalCommandErrors { primary, secondary })
}

/// Try to dispatch `text` as a slash command.
///
/// Returns `Ok(Some(_))` when `text` is a recognised Phase 1 builtin
/// (in which case its state changes and semantic notices are retained).
/// Returns `Ok(None)` when the
/// input is not a slash, is an empty slash, is `/<unknown>`, or is a
/// profile command — the caller's `Submit` arm then runs its normal
/// retained-input and `run_turn` pipeline so the agent loop's
/// `preprocess_input` can intercept profile commands as usual.
pub(super) async fn try_dispatch_slash(
    text: &str,
    state: &mut AppState,
    runtime: &mut RuntimeRefs,
) -> Result<Option<SlashOutcome>, TuiError> {
    let SlashClass::Recognised { cmd, arg } = classify_slash(text) else {
        return Ok(None);
    };
    // Slash commands match case-insensitively — `/CLEAR`, `/Clear`,
    // and `/clear` all dispatch to the same handler. The classifier
    // stays zero-allocation by returning the borrowed slice as-is;
    // the lowercase allocation happens here, at the only site that
    // needs it.
    let lower = cmd.to_ascii_lowercase();
    let Some(command) = find_tui_builtin_command(&lower) else {
        return Ok(None);
    };
    let outcome = match command.kind {
        TuiBuiltinKind::New | TuiBuiltinKind::Clear => handle_new(state, runtime).await?,
        TuiBuiltinKind::Compact => handle_compact(state, runtime).await?,
        TuiBuiltinKind::Exit | TuiBuiltinKind::Quit => {
            if mcp_exit_is_blocked(runtime.mcp_command.as_ref()) {
                render_pending_mcp_exit(state)?;
                return Ok(Some(SlashOutcome::Rejected));
            }
            return Ok(Some(SlashOutcome::Exit));
        }
        TuiBuiltinKind::View => super::view_actions::command(arg, state)?,
        TuiBuiltinKind::Help => {
            handle_help(state)?;
            LocalCommandOutcome::Accepted
        }
        TuiBuiltinKind::Model => handle_model(state, runtime, arg)?,
        TuiBuiltinKind::Effort => handle_reasoning_effort(state, runtime, arg)?,
        TuiBuiltinKind::ServiceTier => handle_service_tier(state, runtime, arg)?,
        TuiBuiltinKind::Fast => set_fast_service_tier(state, runtime)?,
        TuiBuiltinKind::Tools => {
            handle_tools(runtime, state)?;
            LocalCommandOutcome::Accepted
        }
        TuiBuiltinKind::Mcp => handle_mcp(
            arg,
            runtime.mcp_control.as_ref(),
            &mut runtime.mcp_command,
            state,
        )?,
        TuiBuiltinKind::Schema
        | TuiBuiltinKind::Session
        | TuiBuiltinKind::Name
        | TuiBuiltinKind::Variables => return Ok(None),
    };
    Ok(Some(match outcome {
        LocalCommandOutcome::Accepted => SlashOutcome::Continue,
        LocalCommandOutcome::AcceptedWithError(error) => SlashOutcome::AcceptedWithError(error),
        LocalCommandOutcome::Rejected => SlashOutcome::Rejected,
    }))
}

/// Retain a compact status notice for the single frame owner to paint.
pub(super) fn write_dim_line(message: &str, state: &mut AppState) -> Result<(), TuiError> {
    notices::notice(state, message, None)?;
    Ok(())
}

/// Create a new persistent session through [`SessionManager::create`]:
/// the session is registered in the index (so it is listable and
/// resumable) and the returned store carries an index-registered JSONL
/// sink using [`DurabilityPolicy::Flush`] — the same durability every
/// other interactive open in the workspace uses.
///
/// Returns the new session id, the sink-equipped store, and the new
/// session's branching authority ([`SessionBinding::persistent_root`])
/// so post-rotation spawn/fork children mint under the NEW session —
/// never the rotated-out one. Pure store-stack work with no terminal
/// I/O, so both the success and the failure path are unit-testable
/// without a terminal.
///
/// `index_lock_deadline` bounds the inter-process index-lock wait the
/// create (and the sink it registers) performs: without it a wedged
/// sibling process would freeze the running TUI inside the `/new`
/// handler forever. On expiry the typed
/// [`SessionPersistError::IndexLockTimeout`] propagates to `handle_new`'s
/// error path, which keeps the current session fully intact. The same
/// deadline rides on the returned binding's manager, bounding every
/// child-mint index insert too.
fn create_new_session_store(
    data_dir: &std::path::Path,
    index_lock_deadline: std::time::Duration,
    model: &str,
) -> Result<(String, EventStore, Arc<SessionBinding>), SessionPersistError> {
    // Same derivation as the CLI driver's startup path, but propagated
    // instead of defaulted: if the cwd is unreadable the user sees the
    // error and keeps the current session rather than silently
    // indexing a session with an empty working directory.
    let working_dir = std::env::current_dir()?.to_string_lossy().into_owned();
    let manager = SessionManager::new(data_dir).with_index_lock_deadline(Some(index_lock_deadline));
    let opened = manager.create(
        CreateSessionOptions {
            model: model.to_owned(),
            working_dir,
            name: None,
        },
        DurabilityPolicy::Flush,
    )?;
    let binding = Arc::new(SessionBinding::persistent_root(
        Arc::new(SessionBrancher::new(
            manager,
            opened.entry.id.clone(),
            DurabilityPolicy::Flush,
        )),
        &opened.entry,
        &[],
    ));
    Ok((opened.entry.id, opened.store, binding))
}

/// `/new` (also `/clear`) — rotate to a new session, drop conversation
/// context, clear the viewport, and reset visible token counters.
///
/// When persistence is enabled (`runtime.data_dir` and
/// `runtime.session_id` are `Some`), the new session is created via
/// [`create_new_session_store`] — indexed, listable, resumable, and
/// sink-registered. If that fails, a semantic error is retained and the
/// current session is left
/// fully intact — no app state has been mutated yet, so no
/// partially-rotated state is reachable: the TUI never silently
/// degrades a persistent session to an in-memory one. In ephemeral
/// mode, the store is replaced with a plain in-memory store (no disk
/// I/O).
///
/// Once the fallible session-stack work succeeds, the rotation commits
/// through [`super::rotation::rotate_store_dependents`], which
/// checkpoints the old store's final index delta and repoints every
/// component that captured the old store at driver startup — the
/// `LoopContext` / tool-context [`norn::session::action_log::ActionLog`]
/// and the agent tools' `AgentToolInfra` event store — before swapping
/// `runtime.store`.
///
/// The new view carries the current frontend preferences while resetting
/// source-bound rows, cursors and body demands. Prior persisted history remains
/// in its original session store.
async fn handle_new(
    state: &mut AppState,
    runtime: &mut RuntimeRefs,
) -> Result<LocalCommandOutcome, TuiError> {
    // Phase 1 — all fallible work, touching no app state. A failure
    // here leaves the current session running exactly as it was.
    let (new_id, new_store, new_binding) = if let (Some(data_dir), Some(_)) =
        (runtime.data_dir.as_ref(), runtime.session_id.as_ref())
    {
        match create_new_session_store(data_dir, runtime.index_lock_deadline, &runtime.model) {
            Ok((new_id, store, binding)) => (Some(new_id), store, binding),
            Err(err) => {
                tracing::error!(
                    "/new: failed to create session in {}: {err}",
                    data_dir.display(),
                );
                let message = format!("/new failed: {err} — keeping the current session");
                write_error_line(state, &message)?;
                return Ok(LocalCommandOutcome::Rejected);
            }
        }
    } else {
        // Ephemeral mode: the rotated-in conversation stays memory-only,
        // and so do any children it spawns — the honest propagation.
        (
            None,
            EventStore::new(),
            Arc::new(SessionBinding::ephemeral_root()),
        )
    };

    let new_source = new_store.bind_view_source(&new_binding, state.tab_state.root_id(), None)?;

    // Phase 2 — infallible commit: reset the context-edit ledger for
    // the new conversation FIRST (rotation replays the incoming store's
    // compaction marks into it — a no-op for a fresh store, but the
    // order keeps any replayed marks from being wiped), then checkpoint
    // the old store's pending index delta, repoint the action log and
    // agent-tool infra at the new store, swap `runtime.store`, and
    // update the session identity everywhere it is displayed or sent.
    if runtime.loop_context.context_edits.is_some() {
        runtime.loop_context.context_edits = Some(ContextEdits::new());
    }
    super::rotation::rotate_store_dependents(
        runtime.executor.shared_context(),
        &mut runtime.store,
        &mut runtime.loop_context,
        Arc::new(new_store),
        Arc::clone(&new_binding),
    )
    .await;
    runtime.session_binding = new_binding;
    let config = state.transcript.config.clone();
    state.transcript = super::transcript::Transcript::new(new_source);
    state.transcript.config = config;
    state
        .screen
        .replace_source(state.transcript.projection.source());
    state.screen.allow_body_load = true;
    if let Some(new_id) = new_id {
        runtime.session_id = Some(new_id.clone());
        runtime.agent_config.cache_key = Some(new_id.clone());
        if let Some(env) = runtime.loop_context.environment.as_mut() {
            env.session_id = Some(new_id.clone());
        }
        state.fixed_panel.status_bar_mut().session_name = new_id;
    }

    state.clear_usage_totals();

    Ok(LocalCommandOutcome::after_acceptance(write_dim_line(
        "[new session]",
        state,
    )))
}

/// `/compact` — supersede older assistant turns by calling libnorn's
/// [`ContextEdits::auto_compact_keeping_recent_turns`] against the
/// current event store.
///
/// The TUI retains its own semantic notice for the command result, but
/// shares the mechanical compaction estimate with CLI mode through
/// [`norn::agent_loop::estimate_manual_compaction`].
async fn handle_compact(
    state: &mut AppState,
    runtime: &mut RuntimeRefs,
) -> Result<LocalCommandOutcome, TuiError> {
    let keep = runtime.agent_config.auto_compact_keep_recent_turns;

    let Some(estimate) = norn::agent_loop::estimate_manual_compaction(
        &runtime.store,
        keep,
        runtime.loop_context.token_estimator.as_deref(),
    ) else {
        write_dim_line("Nothing to compact.", state)?;
        return Ok(LocalCommandOutcome::Accepted);
    };

    let Some(edits) = runtime.loop_context.context_edits.as_mut() else {
        write_dim_line(
            "norn: warning: context edits unavailable; cannot compact.",
            state,
        )?;
        return Ok(LocalCommandOutcome::Rejected);
    };

    match edits.auto_compact_keeping_recent_turns(
        &runtime.store,
        keep,
        estimate.token_estimate_freed,
    ) {
        Ok(Some(_)) => {
            let line = format!(
                "Compacted older turns, freed ~{} tokens (keeping {keep} most recent).",
                estimate.token_estimate_freed,
            );
            let mut reporting = write_dim_line(&line, state);
            // The compaction appended a Compaction event through the
            // sink; flush the sink's pending index delta now so the
            // session index reflects it even if the TUI aborts before
            // the next turn-boundary checkpoint. Failure is surfaced
            // in the error-line style but never undoes the compaction.
            if let Some(message) = super::helpers::checkpoint_session(&runtime.store).await {
                reporting = match (reporting, write_error_line(state, &message)) {
                    (Ok(()), result) | (result, Ok(())) => result,
                    (Err(primary), Err(secondary)) => Err(combine_local_errors(primary, secondary)),
                };
            }
            Ok(LocalCommandOutcome::after_acceptance(reporting))
        }
        Ok(None) => {
            write_dim_line("Nothing to compact.", state)?;
            Ok(LocalCommandOutcome::Accepted)
        }
        Err(err) => {
            let line = format!("Compact failed: {err}");
            write_error_line(state, &line)?;
            Ok(LocalCommandOutcome::Rejected)
        }
    }
}

/// `/help` retains the complete command catalog as one demanded body.
fn handle_help(state: &mut AppState) -> Result<(), TuiError> {
    let mut block = String::new();
    let commands: Vec<_> = tui_builtin_commands().collect();
    let usage_width = commands
        .iter()
        .map(|command| command.usage.chars().count())
        .max()
        .unwrap_or(0);
    for command in commands {
        writeln!(
            block,
            "  {usage:<width$}  {help}",
            usage = command.usage,
            width = usage_width,
            help = command.help,
        )
        .map_err(std::io::Error::other)?;
    }
    notices::notice(state, "Slash commands", Some(&block))?;
    Ok(())
}

/// `/tools` retains the definitions from the live executor generation, or the
/// actual startup definitions when the executor is static.
fn handle_tools(runtime: &RuntimeRefs, state: &mut AppState) -> Result<(), TuiError> {
    let live = runtime.executor.execution_snapshot();
    let tools = live.as_ref().map_or(runtime.tools.as_slice(), |snapshot| {
        snapshot.definitions.as_ref()
    });
    let block = format_tools_block(tools)?;
    notices::notice(state, "Tools available to the model", Some(&block))?;
    Ok(())
}

/// Compose plain tool-list content; display styling belongs to the frame owner.
fn format_tools_block(
    tools: &[norn::provider::request::ToolDefinition],
) -> Result<String, TuiError> {
    if tools.is_empty() {
        return Ok(String::from("No tools available.\n"));
    }
    let mut block = String::new();
    let name_width = tools
        .iter()
        .map(|t| t.name.chars().count())
        .max()
        .unwrap_or(0)
        .max(8);
    for tool in tools {
        let first_line = tool.description.lines().next().unwrap_or("").trim();
        writeln!(
            block,
            "  {name:<width$}  {desc}",
            name = tool.name,
            width = name_width,
            desc = first_line,
        )
        .map_err(std::io::Error::other)?;
    }
    Ok(block)
}

#[cfg(test)]
mod tests {
    use super::*;

    use norn::provider::request::ReasoningEffort;
    use norn::session::events::{EventBase, SessionEvent};
    use norn::session::{read_index, read_session_events_for_entry};

    /// Index-lock deadline for the store fixtures — generous test
    /// configuration; no test here contends the lock.
    const TEST_LOCK_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

    #[test]
    fn accepted_effect_keeps_original_typed_reporting_error()
    -> Result<(), Box<dyn std::error::Error>> {
        let outcome = LocalCommandOutcome::after_acceptance(Err(TuiError::Io(
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "confirmation write"),
        )));
        let LocalCommandOutcome::AcceptedWithError(TuiError::Io(error)) = outcome else {
            return Err("accepted effect lost its typed reporting failure".into());
        };
        assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
        assert_eq!(error.to_string(), "confirmation write");
        Ok(())
    }

    #[test]
    fn accepted_save_and_notice_failures_are_both_retained()
    -> Result<(), Box<dyn std::error::Error>> {
        let outcome = LocalCommandOutcome::after_reported_failure(
            TuiError::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "settings publication",
            )),
            Err(TuiError::FrameBounds),
        );
        let LocalCommandOutcome::AcceptedWithError(TuiError::ViewInteraction { source }) = outcome
        else {
            return Err("accepted effect lost its combined reporting failures".into());
        };
        let errors = source
            .downcast_ref::<LocalCommandErrors>()
            .ok_or("missing typed local command errors")?;
        let TuiError::Io(original) = &errors.primary else {
            return Err("missing original settings I/O error".into());
        };
        assert_eq!(original.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(original.to_string(), "settings publication");
        assert!(matches!(errors.secondary, TuiError::FrameBounds));
        Ok(())
    }

    #[test]
    fn create_new_session_store_registers_session_in_index()
    -> Result<(), Box<dyn std::error::Error>> {
        // Regression for the H20 bug: `/new` previously opened a raw
        // JsonlSink, so the rotated session never appeared in the
        // index — unlistable and unresumable. The full stack must
        // index it.
        let tmp = tempfile::tempdir()?;
        let (id, _, _) = create_new_session_store(tmp.path(), TEST_LOCK_DEADLINE, "test-model")?;
        let index = read_index(tmp.path())?;
        assert!(
            index.iter().any(|e| e.id == id),
            "session {id} missing from index: {index:?}",
        );
        let entry = index
            .iter()
            .find(|e| e.id == id)
            .ok_or("created session missing from index")?;
        assert_eq!(entry.model, "test-model");
        Ok(())
    }

    #[test]
    fn create_new_session_store_attaches_registered_sink() -> Result<(), Box<dyn std::error::Error>>
    {
        // Events appended after rotation must reach disk through the
        // registered sink, and the index entry must track them — the
        // raw-sink path bypassed index maintenance entirely.
        let tmp = tempfile::tempdir()?;
        let (id, store, _) =
            create_new_session_store(tmp.path(), TEST_LOCK_DEADLINE, "test-model")?;
        store.append(SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: "hello after rotation".to_owned(),
        })?;
        let registered = read_index(tmp.path())?;
        let entry = registered
            .iter()
            .find(|entry| entry.id == id)
            .ok_or_else(|| std::io::Error::other("created session is absent from the index"))?;
        let read = read_session_events_for_entry(tmp.path(), entry)?;
        assert_eq!(read.events.len(), 1, "appended event must be on disk");
        assert!(matches!(
            &read.events[0],
            SessionEvent::UserMessage { content, .. } if content == "hello after rotation",
        ));
        // Drop before the index assertion so any deferred index
        // maintenance in the sink has flushed.
        drop(store);
        let index = read_index(tmp.path())?;
        let entry = index
            .iter()
            .find(|entry| entry.id == id)
            .ok_or_else(|| std::io::Error::other("appended session is absent from the index"))?;
        assert_eq!(
            entry.event_count, 1,
            "registered sink must keep the index event count current",
        );
        Ok(())
    }

    #[test]
    fn create_new_session_store_session_is_resumable() -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let (id, store, _) =
            create_new_session_store(tmp.path(), TEST_LOCK_DEADLINE, "test-model")?;
        store.append(SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: "persist me".to_owned(),
        })?;
        drop(store);
        let resumed = SessionManager::new(tmp.path()).resume(&id, DurabilityPolicy::Flush)?;
        assert_eq!(resumed.entry.id, id);
        assert_eq!(
            resumed.replay.replayed_events, 1,
            "resume must replay the appended event"
        );
        Ok(())
    }

    #[test]
    fn create_new_session_store_propagates_failure() -> Result<(), Box<dyn std::error::Error>> {
        // The failure path must surface an Err — never silently hand
        // back an in-memory store. A regular file in place of the data
        // directory makes every filesystem step fail.
        let tmp = tempfile::tempdir()?;
        let bogus_dir = tmp.path().join("not-a-dir");
        std::fs::write(&bogus_dir, b"occupied")?;
        let result = create_new_session_store(&bogus_dir, TEST_LOCK_DEADLINE, "test-model");
        assert!(
            result.is_err(),
            "creating a session under a file path must fail loudly",
        );
        Ok(())
    }

    #[test]
    fn split_first_word_returns_command_and_arg() {
        assert_eq!(split_first_word("clear"), ("clear", ""));
        assert_eq!(split_first_word("model gpt-x"), ("model", "gpt-x"));
        assert_eq!(
            split_first_word("model   gpt-x   "),
            ("model", "gpt-x"),
            "trailing whitespace must be trimmed",
        );
        assert_eq!(split_first_word("   clear   "), ("clear", ""));
        assert_eq!(split_first_word(""), ("", ""));
    }

    #[test]
    fn command_catalog_covers_all_tui_builtins() {
        // The catalog feeds `/help`, autocomplete, and dispatch. This
        // exact shape prevents aliases from silently drifting to a
        // wrong handler kind.
        let catalog: Vec<(&str, TuiBuiltinKind)> = tui_builtin_commands()
            .map(|command| (command.name, command.kind))
            .collect();
        assert_eq!(
            catalog,
            vec![
                ("new", TuiBuiltinKind::New),
                ("clear", TuiBuiltinKind::Clear),
                ("compact", TuiBuiltinKind::Compact),
                ("exit", TuiBuiltinKind::Exit),
                ("quit", TuiBuiltinKind::Quit),
                ("help", TuiBuiltinKind::Help),
                ("view", TuiBuiltinKind::View),
                ("model", TuiBuiltinKind::Model),
                ("effort", TuiBuiltinKind::Effort),
                ("reasoning-effort", TuiBuiltinKind::Effort),
                ("service-tier", TuiBuiltinKind::ServiceTier),
                ("fast", TuiBuiltinKind::Fast),
                ("tools", TuiBuiltinKind::Tools),
                ("mcp", TuiBuiltinKind::Mcp),
            ],
        );
    }

    #[test]
    fn classify_non_slash_returns_not_slash() {
        assert_eq!(classify_slash("hello world"), SlashClass::NotSlash);
        assert_eq!(classify_slash(""), SlashClass::NotSlash);
        assert_eq!(classify_slash("   "), SlashClass::NotSlash);
    }

    #[test]
    fn classify_lone_slash_returns_empty() {
        // `/` followed by nothing or only whitespace must fall through
        // to the agent (REPL parity — slash-then-prose is meaningful).
        assert_eq!(classify_slash("/"), SlashClass::Empty);
        assert_eq!(classify_slash("/   "), SlashClass::Empty);
    }

    #[test]
    fn parse_effort_command_accepts_supported_values_and_clear_aliases() {
        assert_eq!(
            parse_effort_command("none"),
            Some(EffortCommand::Set(ReasoningEffort::None)),
        );
        assert_eq!(
            parse_effort_command("low"),
            Some(EffortCommand::Set(ReasoningEffort::Low)),
        );
        assert_eq!(
            parse_effort_command("medium"),
            Some(EffortCommand::Set(ReasoningEffort::Medium)),
        );
        assert_eq!(
            parse_effort_command("high"),
            Some(EffortCommand::Set(ReasoningEffort::High)),
        );
        assert_eq!(
            parse_effort_command("xhigh"),
            Some(EffortCommand::Set(ReasoningEffort::XHigh)),
        );
        assert_eq!(
            parse_effort_command("max"),
            Some(EffortCommand::Set(ReasoningEffort::Max)),
        );
        assert_eq!(parse_effort_command("default"), Some(EffortCommand::Clear));
        assert_eq!(parse_effort_command("off"), Some(EffortCommand::Clear));
        assert_eq!(parse_effort_command("clear"), Some(EffortCommand::Clear));
        assert_eq!(parse_effort_command("x-high"), None);
        assert_eq!(parse_effort_command("maximum"), None);
    }

    #[test]
    fn effort_help_uses_canonical_xhigh_and_max_spellings() {
        let spelling_checks = find_tui_builtin_command("effort").map(|effort| {
            (
                effort.usage.contains("xhigh"),
                effort.usage.contains("max"),
                effort.usage.contains("x-high"),
            )
        });
        assert_eq!(spelling_checks, Some((true, true, false)));
    }

    #[test]
    fn classify_recognised_extracts_cmd_and_arg() {
        assert_eq!(
            classify_slash("/clear"),
            SlashClass::Recognised {
                cmd: "clear",
                arg: ""
            }
        );
        assert_eq!(
            classify_slash("/model gpt-x"),
            SlashClass::Recognised {
                cmd: "model",
                arg: "gpt-x"
            }
        );
        assert_eq!(
            classify_slash("  /model   gpt-x   "),
            SlashClass::Recognised {
                cmd: "model",
                arg: "gpt-x"
            }
        );
    }

    #[test]
    fn classify_passes_through_unknown_command_name() {
        // Unknown slashes are *recognised* as having a command name but
        // are NOT routed by try_dispatch_slash because catalog lookup
        // fails. The classifier only parses; the dispatcher decides
        // what to do with the name.
        assert!(matches!(
            classify_slash("/deploy staging"),
            SlashClass::Recognised {
                cmd: "deploy",
                arg: "staging"
            }
        ));
        assert!(!is_tui_builtin("deploy"));
    }

    #[test]
    fn tui_builtins_are_recognised() {
        for name in [
            "new",
            "clear",
            "compact",
            "exit",
            "quit",
            "help",
            "model",
            "effort",
            "reasoning-effort",
            "service-tier",
            "fast",
            "tools",
        ] {
            assert!(is_tui_builtin(name), "`{name}` must be a TUI builtin");
        }
        assert!(!is_tui_builtin("deploy"));
        assert!(!is_tui_builtin("variables")); // not yet wired
        assert!(!is_tui_builtin("session")); // not yet wired
        assert!(!is_tui_builtin("name")); // not yet wired
        assert!(!is_tui_builtin("schema")); // not yet wired
        assert!(!is_tui_builtin(""));
    }

    #[test]
    fn classify_preserves_case_in_command_name() {
        // The classifier itself does NOT lowercase — that allocation
        // happens at the dispatch site (Sandra fix 1, option B). The
        // borrowed `&str` returned here points back into the original
        // input. The test pins this so a refactor that changes
        // classify_slash to allocate doesn't slip past review.
        assert!(matches!(
            classify_slash("/CLEAR"),
            SlashClass::Recognised {
                cmd: "CLEAR",
                arg: ""
            }
        ));
        assert!(matches!(
            classify_slash("/Model GPT-x"),
            SlashClass::Recognised {
                cmd: "Model",
                arg: "GPT-x"
            }
        ));
    }

    #[test]
    fn try_dispatch_slash_recognises_case_insensitive_names() {
        for raw in ["NEW", "New", "nEw"] {
            let input = format!("/{raw}");
            let class = classify_slash(&input);
            let lower = match class {
                SlashClass::Recognised { cmd, .. } => cmd.to_ascii_lowercase(),
                _ => String::new(),
            };
            assert_eq!(
                lower, "new",
                "case-insensitive match must collapse `{raw}` to `new`",
            );
        }
    }

    fn tool_def(name: &str, description: &str) -> norn::provider::request::ToolDefinition {
        norn::provider::request::ToolDefinition {
            name: name.to_string(),
            description: description.to_string(),
            parameters: serde_json::json!({}),
        }
    }

    #[test]
    fn format_tools_block_empty_returns_no_tools_line() -> Result<(), TuiError> {
        let block = format_tools_block(&[])?;
        assert!(
            block.contains("No tools available."),
            "empty-tools sentinel must surface: {block:?}",
        );
        assert!(!block.contains('\u{1b}'));
        Ok(())
    }

    #[test]
    fn format_tools_block_lists_each_tool_name_and_first_description_line() -> Result<(), TuiError>
    {
        let tools = vec![
            tool_def("read", "Read file contents from disk"),
            tool_def("bash", "Execute a shell command"),
        ];
        let block = format_tools_block(&tools)?;
        assert!(block.contains("read"));
        assert!(block.contains("bash"));
        assert!(block.contains("Read file contents from disk"));
        assert!(block.contains("Execute a shell command"));
        assert!(!block.contains('\u{1b}'));
        Ok(())
    }

    #[test]
    fn format_tools_block_uses_first_description_line_for_multiline_descriptions()
    -> Result<(), TuiError> {
        // Tool descriptions often have multiple lines (long-form
        // guidance for the model). The /tools view is a one-liner per
        // tool — assert only the first line ends up in the block.
        let tools = vec![tool_def("apply_patch", "Apply a patch\nDetails follow…")];
        let block = format_tools_block(&tools)?;
        assert!(block.contains("Apply a patch"));
        assert!(
            !block.contains("Details follow"),
            "second description line must be elided: {block:?}",
        );
        Ok(())
    }

    #[test]
    fn format_tools_block_pads_names_to_aligned_column() -> Result<(), TuiError> {
        // Aligned column makes the descriptions readable when names
        // vary in length. Specifically, every padded name + 2 spaces
        // gap should appear in front of its description, and the
        // column width should be at least max(tool name length, 8).
        let tools = vec![
            tool_def("read", "Read it"),
            tool_def("apply_patch", "Patch it"),
        ];
        let block = format_tools_block(&tools)?;
        // "apply_patch" is 11 chars, so "read       " is padded to 11
        // chars too. Two spaces follow the padded column.
        assert!(
            block.contains("read         Read it"),
            "read must be padded to align with apply_patch: {block:?}",
        );
        Ok(())
    }
}
