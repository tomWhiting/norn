//! TUI-local slash command dispatch.
//!
//! Handles the most-used builtins (`/new`, `/clear`, `/compact`, `/exit`,
//! `/quit`, `/help`, `/model`, `/effort`, `/service-tier`, `/fast`, `/tools`)
//! directly in the TUI without depending on `norn-cli`'s slash machinery. The dependency direction
//! `norn-cli → norn-tui` rules out calling
//! `norn_cli::commands::slash::dispatch_input` from here; Phase 2 will
//! lift that machinery into a shared layer (most likely `libnorn`) and
//! plumb a unified registry through
//! [`crate::app::event_loop::TuiInputs`].
//!
//! Unknown slashes and profile-registered slash commands return [`None`]
//! from [`try_dispatch_slash`] so the event loop's `Submit` arm falls
//! through to `run_turn`. Inside the agent loop, `libnorn`'s
//! `preprocess_input` handles profile commands; unknown slashes reach
//! the model as user messages (matching REPL behaviour).

use std::fmt::Write as _;
use std::io::Write as IoWrite;
use std::sync::Arc;

use norn::provider::request::ReasoningEffort;
use norn::session::context_edit::ContextEdits;
use norn::session::events::SessionEvent;
use norn::session::{
    CreateSessionOptions, DurabilityPolicy, EventStore, SessionManager, SessionPersistError,
};

use crate::TuiError;
use crate::render::scroll_region::write_to_scroll;
use crate::terminal::setup::TerminalGuard;

use super::dispatch::write_error_line;
use super::event_loop::RuntimeRefs;
use super::state::AppState;

/// Outcome of a recognised slash command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SlashOutcome {
    /// Slash handled — the outer loop should redraw and continue.
    Continue,
    /// Slash handled — the TUI should exit cleanly.
    Exit,
}

/// Static help table — one row per supported builtin.
const HELP_ENTRIES: &[(&str, &str)] = &[
    ("/new", "Start a new session (rotates the JSONL file)"),
    ("/clear", "Alias for /new"),
    (
        "/compact",
        "Compact older context using the auto-compact threshold",
    ),
    ("/exit", "Exit the TUI"),
    ("/quit", "Exit the TUI"),
    ("/help", "Show this help"),
    ("/model <name>", "Switch the active model for the next turn"),
    (
        "/effort <none|low|medium|high|x-high|default>",
        "Show, set, or clear reasoning effort",
    ),
    (
        "/reasoning-effort <none|low|medium|high|x-high|default>",
        "Alias for /effort",
    ),
    (
        "/service-tier <fast|none>",
        "Show, set, or clear the service tier",
    ),
    ("/fast", "Use the fast service tier for the next turn"),
    ("/tools", "List tools available to the model"),
];

/// Classification result for [`classify_slash`].
///
/// Separates the parse-and-recognise step from the do-the-work step so
/// the matching logic can be unit-tested without constructing a
/// [`TerminalGuard`] or a [`RuntimeRefs`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SlashClass<'a> {
    /// Text is not a slash command at all (no leading `/`, ignoring
    /// surrounding whitespace).
    NotSlash,
    /// Text is `/` followed by whitespace or nothing — fall through to
    /// the agent like REPL behaviour.
    Empty,
    /// Recognised Phase 1 command name and its trimmed argument tail.
    Recognised {
        /// Command name as typed (case preserved). Lowercasing happens
        /// at the dispatch site in [`try_dispatch_slash`] so the
        /// classifier stays zero-allocation — the borrowed `&str`
        /// points back into the original input.
        cmd: &'a str,
        /// Trimmed argument tail (may be empty).
        arg: &'a str,
    },
}

/// Parse `text` against the Phase 1 grammar.
pub(super) fn classify_slash(text: &str) -> SlashClass<'_> {
    let trimmed = text.trim();
    let Some(rest) = trimmed.strip_prefix('/') else {
        return SlashClass::NotSlash;
    };
    let (cmd, arg) = split_first_word(rest);
    if cmd.is_empty() {
        return SlashClass::Empty;
    }
    SlashClass::Recognised { cmd, arg }
}

/// Builtin command names currently recognised by
/// [`try_dispatch_slash`].
///
/// Kept as a single source of truth so [`is_tui_builtin`] and the test
/// that asserts help-table coverage cannot drift from the match arms in
/// [`try_dispatch_slash`].
#[cfg(test)]
const TUI_BUILTINS: &[&str] = &[
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
];

/// Whether `cmd` (without leading slash) is a TUI builtin.
///
/// Test-only helper today — the production dispatch path lists each
/// command explicitly in [`try_dispatch_slash`]'s match arms so the
/// compiler exhaustiveness check catches additions. Phase 2's unified
/// registry replaces both surfaces.
#[cfg(test)]
fn is_tui_builtin(cmd: &str) -> bool {
    TUI_BUILTINS.contains(&cmd)
}

/// Try to dispatch `text` as a slash command.
///
/// Returns `Ok(Some(_))` when `text` is a recognised Phase 1 builtin
/// (in which case the command has already taken effect — scroll-region
/// writes, state mutations, the lot). Returns `Ok(None)` when the
/// input is not a slash, is an empty slash, is `/<unknown>`, or is a
/// profile command — the caller's `Submit` arm then runs its normal
/// `write_user_message + run_turn` pipeline so the agent loop's
/// `preprocess_input` can intercept profile commands as usual.
pub(super) fn try_dispatch_slash(
    text: &str,
    state: &mut AppState,
    runtime: &mut RuntimeRefs,
    guard: &mut TerminalGuard,
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
    match lower.as_str() {
        "new" | "clear" => {
            handle_new(state, runtime, guard)?;
            Ok(Some(SlashOutcome::Continue))
        }
        "compact" => {
            handle_compact(state, runtime, guard)?;
            Ok(Some(SlashOutcome::Continue))
        }
        "exit" | "quit" => Ok(Some(SlashOutcome::Exit)),
        "help" => {
            handle_help(guard)?;
            Ok(Some(SlashOutcome::Continue))
        }
        "model" => {
            handle_model(state, runtime, guard, arg)?;
            Ok(Some(SlashOutcome::Continue))
        }
        "effort" | "reasoning-effort" => {
            handle_reasoning_effort(runtime, guard, arg)?;
            Ok(Some(SlashOutcome::Continue))
        }
        "service-tier" => {
            handle_service_tier(runtime, guard, arg)?;
            Ok(Some(SlashOutcome::Continue))
        }
        "fast" => {
            runtime.loop_context.service_tier = Some(norn::provider::request::ServiceTier::Fast);
            write_dim_line("Service tier: fast", guard)?;
            Ok(Some(SlashOutcome::Continue))
        }
        "tools" => {
            handle_tools(runtime, guard)?;
            Ok(Some(SlashOutcome::Continue))
        }
        _ => Ok(None),
    }
}

/// Split `s` on the first whitespace run, returning
/// `(first_word, trimmed_rest)`.
fn split_first_word(s: &str) -> (&str, &str) {
    let trimmed = s.trim_start();
    match trimmed.find(char::is_whitespace) {
        Some(idx) => (&trimmed[..idx], trimmed[idx..].trim()),
        None => (trimmed, ""),
    }
}

/// Write `message` to the scroll region wrapped in dim SGR, terminated
/// with a newline so subsequent writes start on a fresh row.
fn write_dim_line(message: &str, guard: &mut TerminalGuard) -> Result<(), TuiError> {
    let line = format!("\x1b[2m{message}\x1b[22m\n");
    write_to_scroll(&line, guard.terminal_mut())?;
    guard.note_scroll_newlines(&line)?;
    guard.terminal_mut().flush()?;
    Ok(())
}

/// Create a new persistent session through [`SessionManager::create`]:
/// the session is registered in the index (so it is listable and
/// resumable) and the returned store carries an index-registered JSONL
/// sink using [`DurabilityPolicy::Flush`] — the same durability every
/// other interactive open in the workspace uses.
///
/// Returns the new session id and the sink-equipped store. Pure
/// store-stack work with no terminal I/O, so both the success and the
/// failure path are unit-testable without a [`TerminalGuard`].
fn create_new_session_store(
    data_dir: &std::path::Path,
    model: &str,
) -> Result<(String, EventStore), SessionPersistError> {
    // Same derivation as the CLI driver's startup path, but propagated
    // instead of defaulted: if the cwd is unreadable the user sees the
    // error and keeps the current session rather than silently
    // indexing a session with an empty working directory.
    let working_dir = std::env::current_dir()?.to_string_lossy().into_owned();
    let opened = SessionManager::new(data_dir).create(
        CreateSessionOptions {
            model: model.to_owned(),
            working_dir,
            name: None,
        },
        DurabilityPolicy::Flush,
    )?;
    Ok((opened.entry.id, opened.store))
}

/// `/new` (also `/clear`) — rotate to a new session, drop conversation
/// context, clear the viewport, and reset session-cumulative tokens.
///
/// When persistence is enabled (`runtime.data_dir` and
/// `runtime.session_id` are `Some`), the new session is created via
/// [`create_new_session_store`] — indexed, listable, resumable, and
/// sink-registered. If that fails, the error is written to the scroll
/// region in the standard error style and the current session is left
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
/// Terminal scrollback retains the previous conversation — the user
/// can still scroll up. The model's view is what gets reset.
fn handle_new(
    state: &mut AppState,
    runtime: &mut RuntimeRefs,
    guard: &mut TerminalGuard,
) -> Result<(), TuiError> {
    // Phase 1 — all fallible work, touching no app state. A failure
    // here leaves the current session running exactly as it was.
    let (new_id, new_store) = if let (Some(data_dir), Some(_old_id)) =
        (runtime.data_dir.as_ref(), runtime.session_id.as_ref())
    {
        match create_new_session_store(data_dir, &runtime.model) {
            Ok((new_id, store)) => (Some(new_id), store),
            Err(err) => {
                tracing::error!(
                    "/new: failed to create session in {}: {err}",
                    data_dir.display(),
                );
                let message = format!("/new failed: {err} — keeping the current session");
                return write_error_line(state, guard, &message);
            }
        }
    } else {
        (None, EventStore::new())
    };

    // Phase 2 — infallible commit: checkpoint the old store's pending
    // index delta, repoint the action log and agent-tool infra at the
    // new store, swap `runtime.store`, then update the session
    // identity everywhere it is displayed or sent.
    super::rotation::rotate_store_dependents(
        runtime.executor.shared_context(),
        &mut runtime.store,
        &mut runtime.loop_context,
        Arc::new(new_store),
    );
    if let Some(new_id) = new_id {
        runtime.session_id = Some(new_id.clone());
        runtime.agent_config.cache_key = Some(new_id.clone());
        if let Some(env) = runtime.loop_context.environment.as_mut() {
            env.session_id = Some(new_id.clone());
        }
        state.fixed_panel.status_bar_mut().session_name = new_id;
    }

    if runtime.loop_context.context_edits.is_some() {
        runtime.loop_context.context_edits = Some(ContextEdits::new());
    }

    let root_id = state.tab_state.root_id();
    state.agent_panel.reset_tokens(root_id);
    let status = state.fixed_panel.status_bar_mut();
    status.input_tokens = 0;
    status.output_tokens = 0;

    {
        let writer = guard.terminal_mut();
        write!(writer, "\x1b[2J\x1b[H")?;
        writer.flush()?;
    }
    // The clear+home placed the hardware cursor at (1, 1); resync the
    // software tracker so the next save_scroll_cursor captures the
    // correct row.
    guard.reset_scroll_cursor(1);

    write_dim_line("[new session]", guard)?;

    guard.save_scroll_cursor()?;
    guard.terminal_mut().flush()?;
    Ok(())
}

/// `/compact` — supersede older assistant turns by calling libnorn's
/// [`ContextEdits::auto_compact_keeping_recent_turns`] against the
/// current event store.
///
/// The TUI's [`RuntimeRefs::loop_context`] carries the same
/// `context_edits` field the CLI uses; calling it here means Phase 1
/// does not need the norn-cli `apply_compact_request` helper (which
/// would be a cross-crate dependency violation). Phase 2 will lift
/// both into a shared layer and remove this duplication.
fn handle_compact(
    state: &AppState,
    runtime: &mut RuntimeRefs,
    guard: &mut TerminalGuard,
) -> Result<(), TuiError> {
    let keep = runtime.agent_config.auto_compact_keep_recent_turns;

    // Count assistant turns. If we don't have more than `keep`,
    // there is nothing to do — surface a dim line and return.
    let events = runtime.store.events();
    let assistant_count = events
        .iter()
        .filter(|e| matches!(e, SessionEvent::AssistantMessage { .. }))
        .count();
    if assistant_count <= keep {
        return write_dim_line("Nothing to compact.", guard);
    }
    drop(events);

    // Estimate freed tokens by summing the byte counts of every event
    // up to and including the cut. Mirrors norn-cli's
    // `apply_compact_request::estimate_freed` but pared down to the
    // fields the TUI's `RuntimeRefs` exposes. Phase 2 deletes this
    // duplicate when the helper is lifted.
    let token_estimate_freed = estimate_freed_tokens(runtime, keep);

    let Some(edits) = runtime.loop_context.context_edits.as_mut() else {
        return write_dim_line(
            "norn: warning: context edits unavailable; cannot compact.",
            guard,
        );
    };

    match edits.auto_compact_keeping_recent_turns(&runtime.store, keep, token_estimate_freed) {
        Ok(Some(_)) => {
            let line = format!(
                "Compacted older turns, freed ~{token_estimate_freed} tokens (keeping {keep} most recent)."
            );
            write_dim_line(&line, guard)?;
            // The compaction appended a Compaction event through the
            // sink; flush the sink's pending index delta now so the
            // session index reflects it even if the TUI aborts before
            // the next turn-boundary checkpoint. Failure is surfaced
            // in the error-line style but never undoes the compaction.
            if let Some(message) = super::helpers::checkpoint_session(&runtime.store) {
                write_error_line(state, guard, &message)?;
            }
            Ok(())
        }
        Ok(None) => write_dim_line("Nothing to compact.", guard),
        Err(err) => {
            let line = format!("Compact failed: {err}");
            write_dim_line(&line, guard)
        }
    }
}

/// Estimate the bytes freed by compacting everything up to the cut
/// index that retains `keep` most-recent assistant turns.
///
/// Mirrors `crates/norn-cli/src/commands/slash/actions.rs::estimate_freed`
/// pared down to the event variants and the token-estimator field the
/// TUI's runtime makes available. Returns zero when the estimator is
/// absent — the compact still proceeds but its freed-token figure
/// shows as `~0`.
fn estimate_freed_tokens(runtime: &RuntimeRefs, keep: usize) -> usize {
    let Some(estimator) = runtime.loop_context.token_estimator.as_ref() else {
        return 0;
    };
    let events = runtime.store.events();
    let assistant_positions: Vec<usize> = events
        .iter()
        .enumerate()
        .filter_map(|(idx, e)| matches!(e, SessionEvent::AssistantMessage { .. }).then_some(idx))
        .collect();
    if assistant_positions.len() <= keep {
        return 0;
    }
    let cut_idx = assistant_positions[assistant_positions.len() - keep - 1];
    let mut total: usize = 0;
    for event in &events[..=cut_idx] {
        let bytes = match event {
            SessionEvent::UserMessage { content, .. } => estimator.estimate(content),
            SessionEvent::AssistantMessage { content, .. } => {
                if content.is_empty() {
                    0
                } else {
                    estimator.estimate(content)
                }
            }
            SessionEvent::ToolResult { output, .. } => estimator.estimate(&output.to_string()),
            SessionEvent::SpokenResponse { content, .. } => {
                estimator.estimate(&content.to_string())
            }
            SessionEvent::Compaction { summary, .. } => estimator.estimate(summary),
            SessionEvent::ModelChange { .. }
            | SessionEvent::Fork { .. }
            | SessionEvent::ForkComplete { .. }
            | SessionEvent::Label { .. }
            | SessionEvent::Custom { .. } => 0,
        };
        total = total.saturating_add(bytes);
    }
    total
}

/// `/help` — write a static help block to the scroll region.
///
/// The transient-overlay alternative would require cursor-addressed
/// rendering inside the scroll region, which CO7 forbids ("scroll
/// region content is immutable once written"). Sandra's call: write
/// via [`write_to_scroll`] so the block lands in scrollback and the
/// user can scroll back to find it.
fn handle_help(guard: &mut TerminalGuard) -> Result<(), TuiError> {
    let mut block = String::from("\x1b[2mSlash commands:\x1b[22m\n");
    for (name, desc) in HELP_ENTRIES {
        // `write!` into a String via `std::fmt::Write` — clippy rejects
        // `block.push_str(&format!(...))` because the intermediate
        // allocation is avoidable.
        let _ = writeln!(block, "\x1b[2m  {name:<16}  {desc}\x1b[22m");
    }
    write_to_scroll(&block, guard.terminal_mut())?;
    guard.note_scroll_newlines(&block)?;
    guard.terminal_mut().flush()?;
    Ok(())
}

/// `/model <name>` — validate the name, mutate
/// [`RuntimeRefs::model`] and the status bar's model display, then
/// surface a confirmation crumb.
///
/// Per-turn `run_turn` reads `runtime.model.clone()` at the top of
/// the function, so the new model takes effect on the next submission
/// without further plumbing. The current turn (if mid-flight, which
/// it cannot be because we hold `&mut runtime`) is unaffected.
fn handle_model(
    state: &mut AppState,
    runtime: &mut RuntimeRefs,
    guard: &mut TerminalGuard,
    arg: &str,
) -> Result<(), TuiError> {
    let name = arg.trim();
    if name.is_empty() {
        return write_dim_line("usage: /model <name>", guard);
    }
    runtime.model = name.to_string();
    state.fixed_panel.status_bar_mut().model_name = name.to_string();
    let line = format!("Switched model to {name}");
    write_dim_line(&line, guard)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EffortCommand {
    Set(ReasoningEffort),
    Clear,
}

fn parse_effort_command(value: &str) -> Option<EffortCommand> {
    match value.trim().to_ascii_lowercase().as_str() {
        "none" => Some(EffortCommand::Set(ReasoningEffort::None)),
        "low" => Some(EffortCommand::Set(ReasoningEffort::Low)),
        "medium" => Some(EffortCommand::Set(ReasoningEffort::Medium)),
        "high" => Some(EffortCommand::Set(ReasoningEffort::High)),
        "x-high" | "xhigh" => Some(EffortCommand::Set(ReasoningEffort::XHigh)),
        "default" | "off" | "clear" => Some(EffortCommand::Clear),
        _ => None,
    }
}

fn effort_label(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::None => "none",
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
        ReasoningEffort::XHigh => "x-high",
    }
}

/// `/effort <none|low|medium|high|x-high|default>` — mutate the reasoning
/// effort read by the next `run_turn` provider request.
fn handle_reasoning_effort(
    runtime: &mut RuntimeRefs,
    guard: &mut TerminalGuard,
    arg: &str,
) -> Result<(), TuiError> {
    let value = arg.trim();
    if value.is_empty() {
        let current = runtime
            .loop_context
            .reasoning_effort
            .map(effort_label)
            .unwrap_or("default");
        return write_dim_line(current, guard);
    }

    match parse_effort_command(value) {
        Some(EffortCommand::Set(effort)) => {
            runtime.loop_context.reasoning_effort = Some(effort);
            write_dim_line(
                &format!("Reasoning effort: {}", effort_label(effort)),
                guard,
            )
        }
        Some(EffortCommand::Clear) => {
            runtime.loop_context.reasoning_effort = None;
            write_dim_line("Reasoning effort cleared.", guard)
        }
        None => write_dim_line(
            &format!(
                "norn: invalid reasoning effort '{value}'; expected none, low, medium, high, x-high, or default"
            ),
            guard,
        ),
    }
}

/// `/service-tier <fast|none>` — mutate the service tier read by the
/// next `run_turn` provider request.
fn handle_service_tier(
    runtime: &mut RuntimeRefs,
    guard: &mut TerminalGuard,
    arg: &str,
) -> Result<(), TuiError> {
    let value = arg.trim().to_ascii_lowercase();
    if value.is_empty() {
        let current = match runtime.loop_context.service_tier {
            Some(tier) => tier.as_str(),
            None => "none",
        };
        return write_dim_line(current, guard);
    }
    match value.as_str() {
        "fast" => {
            runtime.loop_context.service_tier = Some(norn::provider::request::ServiceTier::Fast);
            write_dim_line("Service tier: fast", guard)
        }
        "none" | "off" | "default" => {
            runtime.loop_context.service_tier = None;
            write_dim_line("Service tier cleared.", guard)
        }
        other => write_dim_line(
            &format!("norn: invalid service tier '{other}'; expected fast or none"),
            guard,
        ),
    }
}

/// `/tools` — list every [`ToolDefinition`] currently advertised to
/// the provider, with its description, as a dim block in the scroll
/// region.
///
/// Pure read against [`RuntimeRefs::tools`]. The Phase 2 closure
/// refactor in `norn-cli` would let this share the CLI's `/tools`
/// surface; for now the TUI renders its own static table from the
/// same data the provider sees.
fn handle_tools(runtime: &RuntimeRefs, guard: &mut TerminalGuard) -> Result<(), TuiError> {
    let block = format_tools_block(&runtime.tools);
    write_to_scroll(&block, guard.terminal_mut())?;
    guard.note_scroll_newlines(&block)?;
    guard.terminal_mut().flush()?;
    Ok(())
}

/// Compose the dim-styled tool-list block written by [`handle_tools`].
///
/// Lifted out of the handler so tests can assert the rendering shape
/// without a live [`TerminalGuard`]. Each tool name is padded to a
/// fixed-width column with the description trailing; the trailing
/// newline survives the [`write_to_scroll`] CR/LF translation.
fn format_tools_block(tools: &[norn::provider::request::ToolDefinition]) -> String {
    if tools.is_empty() {
        return String::from("\x1b[2mNo tools available.\x1b[22m\n");
    }
    let mut block = String::from("\x1b[2mTools available to the model:\x1b[22m\n");
    let name_width = tools
        .iter()
        .map(|t| t.name.chars().count())
        .max()
        .unwrap_or(0)
        .max(8);
    for tool in tools {
        let first_line = tool.description.lines().next().unwrap_or("").trim();
        let _ = writeln!(
            block,
            "\x1b[2m  {name:<width$}  {desc}\x1b[22m",
            name = tool.name,
            width = name_width,
            desc = first_line,
        );
    }
    block
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    use norn::session::events::EventBase;
    use norn::session::{read_index, read_session_events};

    #[test]
    fn create_new_session_store_registers_session_in_index() {
        // Regression for the H20 bug: `/new` previously opened a raw
        // JsonlSink, so the rotated session never appeared in the
        // index — unlistable and unresumable. The full stack must
        // index it.
        let tmp = tempfile::tempdir().unwrap();
        let (id, _store) = create_new_session_store(tmp.path(), "test-model").unwrap();
        let index = read_index(tmp.path()).unwrap();
        assert!(
            index.iter().any(|e| e.id == id),
            "session {id} missing from index: {index:?}",
        );
        let entry = index.iter().find(|e| e.id == id).unwrap();
        assert_eq!(entry.model, "test-model");
    }

    #[test]
    fn create_new_session_store_attaches_registered_sink() {
        // Events appended after rotation must reach disk through the
        // registered sink, and the index entry must track them — the
        // raw-sink path bypassed index maintenance entirely.
        let tmp = tempfile::tempdir().unwrap();
        let (id, store) = create_new_session_store(tmp.path(), "test-model").unwrap();
        store
            .append(SessionEvent::UserMessage {
                base: EventBase::new(None),
                content: "hello after rotation".to_owned(),
            })
            .unwrap();
        let read = read_session_events(tmp.path(), &id).unwrap();
        assert_eq!(read.events.len(), 1, "appended event must be on disk");
        assert!(matches!(
            &read.events[0],
            SessionEvent::UserMessage { content, .. } if content == "hello after rotation",
        ));
        // Drop before the index assertion so any deferred index
        // maintenance in the sink has flushed.
        drop(store);
        let index = read_index(tmp.path()).unwrap();
        let entry = index.iter().find(|e| e.id == id).unwrap();
        assert_eq!(
            entry.event_count, 1,
            "registered sink must keep the index event count current",
        );
    }

    #[test]
    fn create_new_session_store_session_is_resumable() {
        let tmp = tempfile::tempdir().unwrap();
        let (id, store) = create_new_session_store(tmp.path(), "test-model").unwrap();
        store
            .append(SessionEvent::UserMessage {
                base: EventBase::new(None),
                content: "persist me".to_owned(),
            })
            .unwrap();
        drop(store);
        let resumed = SessionManager::new(tmp.path())
            .resume(&id, DurabilityPolicy::Flush)
            .unwrap();
        assert_eq!(resumed.entry.id, id);
        assert_eq!(
            resumed.replay.replayed_events, 1,
            "resume must replay the appended event"
        );
    }

    #[test]
    fn create_new_session_store_propagates_failure() {
        // The failure path must surface an Err — never silently hand
        // back an in-memory store. A regular file in place of the data
        // directory makes every filesystem step fail.
        let tmp = tempfile::tempdir().unwrap();
        let bogus_dir = tmp.path().join("not-a-dir");
        std::fs::write(&bogus_dir, b"occupied").unwrap();
        let result = create_new_session_store(&bogus_dir, "test-model");
        assert!(
            result.is_err(),
            "creating a session under a file path must fail loudly",
        );
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
    fn help_entries_cover_all_tui_builtins() {
        // Defensive: the help block must list every command
        // try_dispatch_slash recognises. If we add a new branch above
        // and forget the help entry, this test fails.
        let names: Vec<&str> = HELP_ENTRIES.iter().map(|(n, _)| *n).collect();
        for needle in [
            "/new",
            "/clear",
            "/compact",
            "/exit",
            "/quit",
            "/help",
            "/model <name>",
            "/effort <none|low|medium|high|x-high|default>",
            "/reasoning-effort <none|low|medium|high|x-high|default>",
            "/service-tier <fast|none>",
            "/fast",
            "/tools",
        ] {
            assert!(
                names.contains(&needle),
                "help table missing `{needle}`: {names:?}",
            );
        }
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
            parse_effort_command("x-high"),
            Some(EffortCommand::Set(ReasoningEffort::XHigh)),
        );
        assert_eq!(
            parse_effort_command("xhigh"),
            Some(EffortCommand::Set(ReasoningEffort::XHigh)),
        );
        assert_eq!(parse_effort_command("default"), Some(EffortCommand::Clear));
        assert_eq!(parse_effort_command("off"), Some(EffortCommand::Clear));
        assert_eq!(parse_effort_command("clear"), Some(EffortCommand::Clear));
        assert_eq!(parse_effort_command("maximum"), None);
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
        // are NOT routed by try_dispatch_slash (the match arm falls to
        // `_ => Ok(None)`). The classifier only parses; the dispatcher
        // decides what to do with the name.
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
            "new", "clear", "compact", "exit", "quit", "help", "model", "tools",
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
    fn format_tools_block_empty_returns_no_tools_line() {
        let block = format_tools_block(&[]);
        assert!(
            block.contains("No tools available."),
            "empty-tools sentinel must surface: {block:?}",
        );
        // Even the empty form must be dim-wrapped — the indicator line
        // recedes behind the conversation content.
        assert!(block.contains("\x1b[2m"));
        assert!(block.contains("\x1b[22m"));
    }

    #[test]
    fn format_tools_block_lists_each_tool_name_and_first_description_line() {
        let tools = vec![
            tool_def("read", "Read file contents from disk"),
            tool_def("bash", "Execute a shell command"),
        ];
        let block = format_tools_block(&tools);
        assert!(block.contains("read"));
        assert!(block.contains("bash"));
        assert!(block.contains("Read file contents from disk"));
        assert!(block.contains("Execute a shell command"));
        assert!(
            block.starts_with("\x1b[2m"),
            "block must open with dim SGR: {block:?}",
        );
    }

    #[test]
    fn format_tools_block_uses_first_description_line_for_multiline_descriptions() {
        // Tool descriptions often have multiple lines (long-form
        // guidance for the model). The /tools view is a one-liner per
        // tool — assert only the first line ends up in the block.
        let tools = vec![tool_def("apply_patch", "Apply a patch\nDetails follow…")];
        let block = format_tools_block(&tools);
        assert!(block.contains("Apply a patch"));
        assert!(
            !block.contains("Details follow"),
            "second description line must be elided: {block:?}",
        );
    }

    #[test]
    fn format_tools_block_pads_names_to_aligned_column() {
        // Aligned column makes the descriptions readable when names
        // vary in length. Specifically, every padded name + 2 spaces
        // gap should appear in front of its description, and the
        // column width should be at least max(tool name length, 8).
        let tools = vec![
            tool_def("read", "Read it"),
            tool_def("apply_patch", "Patch it"),
        ];
        let block = format_tools_block(&tools);
        // "apply_patch" is 11 chars, so "read       " is padded to 11
        // chars too. Two spaces follow the padded column.
        assert!(
            block.contains("read         Read it"),
            "read must be padded to align with apply_patch: {block:?}",
        );
    }
}
