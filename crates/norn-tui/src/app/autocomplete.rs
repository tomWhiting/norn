//! Autocomplete lifecycle wiring for the event loop.
//!
//! The popup itself lives in [`crate::input::autocomplete`] — this module
//! owns the **policy** for keeping it in sync with [`AppState`] as the
//! user types. The single entry point is [`refresh_autocomplete`], which
//! the event loop calls after every input mutation:
//!
//! 1. Inspect the editor's current text and cursor character offset.
//! 2. Call [`detect_trigger`] to find the open `/` or `@` trigger.
//! 3. If no trigger is active, dismiss any open popup and zero out the
//!    fixed panel's popup row count.
//! 4. If a trigger is active and the existing popup's snapshot matches
//!    (same kind, same `trigger_start_byte`), narrow it against the
//!    typed prefix.
//! 5. Otherwise build a fresh popup — slash candidates (built-ins plus
//!    filesystem-discovered skills) or a walked-and-fuzzy-matched file
//!    snapshot — and seat it on `AppState`.
//!
//! Slash snapshot composition follows the brief: project skills shadow
//! user skills with the same name, and the built-in commands from the
//! TUI slash catalog are merged at the top of the list.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use termina::event::{KeyCode, KeyEvent, Modifiers};

use crate::error::TuiError;
use crate::input::composer_transactions::CompletionContext;

use crate::input::autocomplete::{
    AutocompletePopup, AutocompleteTrigger, SlashCandidate, SourceTag, TriggerKind, detect_trigger,
    walk_entries,
};

use super::render::sync_input_area;
use super::slash_catalog::tui_builtin_commands;
use super::state::AppState;

/// Outcome of routing a key press through the popup pre-intercept.
///
/// Returned by [`handle_popup_key`] so the event loop can decide whether
/// to short-circuit (consumed → redraw) or fall through to the normal
/// [`crate::input::keybindings::map_key_event`] pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopupKeyOutcome {
    /// The popup absorbed the key — the event loop should redraw the
    /// panel, popup, and input and skip [`crate::input::keybindings::map_key_event`].
    Consumed,
    /// The popup did not handle this key — the event loop should
    /// continue with its normal action pipeline.
    NotConsumed,
}

/// Pre-intercept popup-active special keys.
///
/// Caller must have already confirmed the popup is open and the event
/// is a [`termina::event::KeyEventKind::Press`]. The function mutates `state` directly
/// — selection navigation, splice on acceptance, dismiss on `Escape` —
/// and returns [`PopupKeyOutcome::Consumed`] when the redraw cycle
/// should run.
///
/// Only bare `Enter`/`Tab` accepts a completion. Modified `Enter` returns
/// to the configured host send/newline policy without accepting the popup.
pub fn handle_popup_key(
    key: KeyEvent,
    state: &mut AppState,
    cols: u16,
    terminal_rows: u16,
) -> Result<PopupKeyOutcome, TuiError> {
    if !single_caret(state) {
        dismiss(state);
        sync_editor_input_area(state, cols, terminal_rows)?;
        return Ok(PopupKeyOutcome::NotConsumed);
    }
    let bare = key.modifiers == Modifiers::NONE;
    let outcome = match key.code {
        KeyCode::Up => {
            if let Some(popup) = state.autocomplete.as_mut() {
                popup.select_up();
            }
            PopupKeyOutcome::Consumed
        }
        KeyCode::Down => {
            if let Some(popup) = state.autocomplete.as_mut() {
                popup.select_down();
            }
            PopupKeyOutcome::Consumed
        }
        KeyCode::Tab | KeyCode::Enter if bare => {
            accept(state, cols, terminal_rows)?;
            PopupKeyOutcome::Consumed
        }
        KeyCode::Escape => {
            dismiss(state);
            sync_editor_input_area(state, cols, terminal_rows)?;
            PopupKeyOutcome::Consumed
        }
        _ => PopupKeyOutcome::NotConsumed,
    };
    if outcome == PopupKeyOutcome::Consumed {
        state.screen.dirty = true;
    }
    Ok(outcome)
}

fn sync_editor_input_area(
    state: &mut AppState,
    cols: u16,
    terminal_rows: u16,
) -> Result<(), TuiError> {
    sync_input_area(state, cols, terminal_rows)?;
    Ok(())
}

/// Splice the popup's currently selected candidate into the editor and
/// dismiss the popup.
///
/// Idempotent in the absence of a popup. A popup whose `accept()`
/// returns `None` — possible only if the selection points outside the
/// candidate list — is dismissed without touching the editor.
fn accept(state: &mut AppState, cols: u16, terminal_rows: u16) -> Result<(), TuiError> {
    let acceptance = state.autocomplete.take().and_then(|popup| popup.accept());
    state.fixed_panel.set_autocomplete_popup(0);
    if let Some(acceptance) = acceptance {
        state.input_editor.apply_acceptance(&acceptance)?;
    }
    sync_editor_input_area(state, cols, terminal_rows)
}

/// Bring the popup state in line with the current editor contents.
///
/// `workspace_root` is the directory the `@` walker scans — typically
/// the current working directory the TUI was launched from. Passing it
/// in (rather than calling `std::env::current_dir` here) keeps the
/// helper deterministic and testable.
pub fn refresh_autocomplete(state: &mut AppState, workspace_root: &Path) -> Result<(), TuiError> {
    if !single_caret(state) {
        dismiss(state);
        return Ok(());
    }
    let text = state.input_editor.text();
    let cursor_char = state.input_editor.cursor_char_index()?;
    match detect_trigger(&text, cursor_char) {
        None => dismiss(state),
        Some(trigger) => {
            let context = state
                .input_editor
                .completion_context(trigger.trigger_start_byte)?;
            let needs_rebuild = state
                .autocomplete
                .as_ref()
                .is_none_or(|popup| !popup.matches_trigger(&trigger));
            if needs_rebuild {
                let popup = build_popup(&trigger, context, workspace_root)?;
                if popup.is_open() {
                    state.autocomplete = Some(popup);
                } else {
                    state.autocomplete = None;
                }
            } else if let Some(popup) = state.autocomplete.as_mut()
                && !popup.narrow(&trigger.prefix, context)
            {
                state.autocomplete = None;
            }
            sync_panel_height(state);
        }
    }
    Ok(())
}

fn single_caret(state: &AppState) -> bool {
    let cursor = &state.input_editor.kernel().state().cursor;
    cursor.cursor_count() == 1 && cursor.primary.is_collapsed()
}

/// Dismiss any open popup and zero out the panel's popup row count.
///
/// Idempotent: calling on a state with no popup is a no-op.
pub fn dismiss(state: &mut AppState) {
    if state.autocomplete.is_none() && state.fixed_panel.autocomplete_popup_rows() == 0 {
        return;
    }
    state.autocomplete = None;
    state.fixed_panel.set_autocomplete_popup(0);
}

/// Push the live popup row count into the fixed panel.
fn sync_panel_height(state: &mut AppState) {
    let rows = state
        .autocomplete
        .as_ref()
        .map_or(0, AutocompletePopup::height);
    state.fixed_panel.set_autocomplete_popup(rows);
}

/// Build a fresh popup for the supplied trigger.
fn build_popup(
    trigger: &AutocompleteTrigger,
    context: CompletionContext,
    workspace_root: &Path,
) -> Result<AutocompletePopup, TuiError> {
    Ok(match trigger.kind {
        TriggerKind::SlashCommand => {
            let snapshot = build_slash_snapshot(workspace_root)?;
            AutocompletePopup::new_slash(snapshot, &trigger.prefix, context)
        }
        TriggerKind::FilePath => {
            let paths = walk_entries(workspace_root);
            AutocompletePopup::new_file(paths, &trigger.prefix, context)
        }
    })
}

/// Compose the slash command snapshot: built-ins plus filesystem-
/// discovered skills, with project skills shadowing user skills.
///
/// The directory walk uses the [`profile_skills_dirs`] precedence — the
/// project-level `./.norn/skills/` is listed first so its names win the
/// `seen` shadow check before the user-level `~/.norn/skills/` directory
/// is scanned.
fn build_slash_snapshot(workspace_root: &Path) -> Result<Vec<SlashCandidate>, TuiError> {
    let mut snapshot: Vec<SlashCandidate> = tui_builtin_commands()
        .map(|command| SlashCandidate {
            name: command.name.to_owned(),
            source_tag: SourceTag::Builtin,
            description: command.autocomplete.to_owned(),
        })
        .collect();

    let mut seen: HashSet<String> = snapshot.iter().map(|c| c.name.clone()).collect();
    for dir in profile_skills_dirs(workspace_root) {
        discover_skills(&dir, &mut snapshot, &mut seen)?;
    }
    Ok(snapshot)
}

/// Directories searched for skill files, in shadow-priority order
/// (earlier entries take precedence on a name collision).
///
/// Mirrors the runtime's 6-tier search path from
/// `norn::runtime_init::base::build_skill_search_paths`:
/// project `.norn/skills/`, `.agents/skills/`, `.claude/skills/`,
/// user `~/.norn/skills/`, `~/.agents/skills/`, `~/.claude/skills/`.
/// (The legacy `.meridian/skills/` tier was removed — DECISIONS §0.6(a).)
fn profile_skills_dirs(workspace_root: &Path) -> Vec<PathBuf> {
    let mut out = vec![
        workspace_root.join(".norn").join("skills"),
        workspace_root.join(".agents").join("skills"),
        workspace_root.join(".claude").join("skills"),
    ];
    if let Some(home) = norn::config::paths::norn_dir() {
        out.push(home.join("skills"));
    }
    if let Some(home) = dirs::home_dir() {
        out.push(home.join(".agents").join("skills"));
        out.push(home.join(".claude").join("skills"));
    }
    out
}

/// Append every skill in `dir` to `snapshot`, skipping names already
/// present in `seen`.
///
/// Discovers both forms:
/// - **Flat**: `deploy.md` — the file stem is the skill name.
/// - **Dir**: `deploy/SKILL.md` — the directory name is the skill name.
///
/// A description is best-effort from YAML frontmatter. The parse is
/// intentionally minimal — full validation belongs in
/// [`norn::skill::catalog::SkillCatalog`].
fn discover_skills(
    dir: &Path,
    snapshot: &mut Vec<SlashCandidate>,
    seen: &mut HashSet<String>,
) -> Result<(), TuiError> {
    let descriptor_permit = norn::resource::acquire_filesystem_operation().map_err(|source| {
        TuiError::ViewInteraction {
            source: Box::new(source),
        }
    })?;
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(filesystem_error(dir, source)),
    };
    for entry in entries {
        let entry = entry.map_err(|source| filesystem_error(dir, source))?;
        let path = entry.path();
        let metadata =
            std::fs::metadata(&path).map_err(|source| filesystem_error(&path, source))?;
        let (name, description_path) = if metadata.is_dir() {
            let skill_md = path.join("SKILL.md");
            match std::fs::metadata(&skill_md) {
                Ok(metadata) if metadata.is_file() => {}
                Ok(_) => continue,
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => continue,
                Err(source) => return Err(filesystem_error(&skill_md, source)),
            }
            (path.file_name(), skill_md)
        } else if path.extension().is_some_and(|extension| extension == "md") {
            (path.file_stem(), path.clone())
        } else {
            continue;
        };
        let Some(name) = name.and_then(|value| value.to_str()) else {
            continue;
        };
        if seen.contains(name) {
            continue;
        }
        let description = read_skill_description(&description_path)?.unwrap_or_default();
        seen.insert(name.to_owned());
        snapshot.push(SlashCandidate {
            name: name.to_owned(),
            source_tag: SourceTag::Profile,
            description,
        });
    }
    drop(descriptor_permit);
    Ok(())
}

#[derive(Debug, thiserror::Error)]
#[error("autocomplete filesystem path {path:?}: {source}")]
struct AutocompleteFilesystemError {
    path: PathBuf,
    #[source]
    source: std::io::Error,
}

fn filesystem_error(path: &Path, source: std::io::Error) -> TuiError {
    TuiError::ViewInteraction {
        source: Box::new(AutocompleteFilesystemError {
            path: path.to_owned(),
            source,
        }),
    }
}

/// Read the `description:` field from a skill markdown file's YAML
/// frontmatter, if present.
///
/// The parser handles only the shape skills actually use: an opening
/// `---` fence on the first line, key-value lines, and a closing `---`
/// fence. Anything else returns `None` and the caller falls back to an
/// empty description.
fn read_skill_description(path: &Path) -> Result<Option<String>, TuiError> {
    let content = std::fs::read_to_string(path).map_err(|source| filesystem_error(path, source))?;
    let mut lines = content.lines();
    if lines.next().is_none_or(|line| line.trim() != "---") {
        return Ok(None);
    }
    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            return Ok(None);
        }
        if let Some(rest) = trimmed.strip_prefix("description:") {
            let value = rest.trim().trim_matches(|c| c == '"' || c == '\'');
            if value.is_empty() {
                return Ok(None);
            }
            return Ok(Some(value.to_owned()));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    use std::fs;
    use std::sync::Arc;

    use parking_lot::RwLock;

    use norn::agent::registry::AgentRegistry;

    use super::*;
    use crate::input::history::InputHistory;
    use crate::render::fixed_panel::StatusBar;
    use crate::terminal::caps::TerminalCaps;

    fn fresh_state() -> TestResult<AppState> {
        let registry: Arc<RwLock<AgentRegistry>> = AgentRegistry::shared();
        let guard = AgentRegistry::reserve(
            &registry,
            "/root".to_string(),
            "lead".to_string(),
            "claude".to_string(),
            None,
            norn::agent::child_policy::ChildPolicy {
                messaging: norn::agent::child_policy::MessagingScope::SiblingsAndParent,
                delegation: norn::agent::child_policy::DelegationBudget {
                    remaining_depth: 5,
                    max_concurrent_children: 32,
                },
                inbound_capacity: 32,
                loop_config: None,
            },
            None,
        )?;
        let root_id = guard.id();
        guard.confirm()?;
        Ok(AppState::new(
            TerminalCaps::baseline(),
            InputHistory::in_memory(),
            registry,
            crate::app::state::test_view_source(root_id),
            StatusBar::default(),
        ))
    }

    fn type_into(state: &mut AppState, text: &str) -> TestResult {
        let options = iridium_editor::editor::CellInputOptions {
            wrap: iridium_editor::cell_layout::CellWrapParameters::new(80, 4),
            visible_rows: 10,
        };
        for ch in text.chars() {
            assert_eq!(
                state.input_editor.handle_cell_key(
                    &iridium_editor::KeyEvent::simple(iridium_editor::KeyCode::Char(ch)),
                    options,
                )?,
                iridium_editor::EditorKeyResult::None
            );
        }
        Ok(())
    }

    #[test]
    fn builtin_slash_commands_appear_in_snapshot() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let snapshot = build_slash_snapshot(tmp.path())?;
        let names: Vec<&str> = snapshot.iter().map(|c| c.name.as_str()).collect();
        for name in [
            "compact",
            "clear",
            "effort",
            "exit",
            "fast",
            "help",
            "model",
            "new",
            "quit",
            "reasoning-effort",
            "service-tier",
            "tools",
        ] {
            assert!(
                names.contains(&name),
                "built-in `{name}` missing from snapshot: {names:?}",
            );
        }
        Ok(())
    }

    #[test]
    fn refresh_creates_slash_popup_after_slash_typed() -> TestResult {
        let mut state = fresh_state()?;
        let tmp = tempfile::tempdir()?;
        type_into(&mut state, "/")?;
        refresh_autocomplete(&mut state, tmp.path())?;
        assert!(
            state.autocomplete.is_some(),
            "popup must be created for `/` trigger",
        );
        assert!(state.fixed_panel.autocomplete_popup_rows() > 0);
        Ok(())
    }

    #[test]
    fn refresh_narrows_existing_slash_popup_as_user_types() -> TestResult {
        let mut state = fresh_state()?;
        let tmp = tempfile::tempdir()?;
        type_into(&mut state, "/")?;
        refresh_autocomplete(&mut state, tmp.path())?;
        let initial_count = state
            .autocomplete
            .as_ref()
            .ok_or("autocomplete popup is missing")?
            .candidates
            .len();
        type_into(&mut state, "he")?;
        refresh_autocomplete(&mut state, tmp.path())?;
        let narrowed = state
            .autocomplete
            .as_ref()
            .ok_or("autocomplete popup is missing")?
            .candidates
            .len();
        assert!(
            narrowed <= initial_count,
            "narrowing must not grow the list: {initial_count} → {narrowed}",
        );
        assert!(narrowed >= 1, "`help` must survive `/he` narrowing");
        Ok(())
    }

    #[test]
    fn refresh_dismisses_popup_when_trigger_disappears() -> TestResult {
        let mut state = fresh_state()?;
        let tmp = tempfile::tempdir()?;
        type_into(&mut state, "/he")?;
        refresh_autocomplete(&mut state, tmp.path())?;
        assert!(state.autocomplete.is_some());
        // Backspace removes the `/he`, no trigger remains.
        state.input_editor.run_cell_command(
            "edit.deleteBackward",
            iridium_editor::CommandArgs::NONE,
            iridium_editor::editor::CellInputOptions {
                wrap: iridium_editor::cell_layout::CellWrapParameters::new(80, 4),
                visible_rows: 10,
            },
        )?;
        state.input_editor.run_cell_command(
            "edit.deleteBackward",
            iridium_editor::CommandArgs::NONE,
            iridium_editor::editor::CellInputOptions {
                wrap: iridium_editor::cell_layout::CellWrapParameters::new(80, 4),
                visible_rows: 10,
            },
        )?;
        state.input_editor.run_cell_command(
            "edit.deleteBackward",
            iridium_editor::CommandArgs::NONE,
            iridium_editor::editor::CellInputOptions {
                wrap: iridium_editor::cell_layout::CellWrapParameters::new(80, 4),
                visible_rows: 10,
            },
        )?;
        refresh_autocomplete(&mut state, tmp.path())?;
        assert!(state.autocomplete.is_none());
        assert_eq!(state.fixed_panel.autocomplete_popup_rows(), 0);
        Ok(())
    }

    #[test]
    fn refresh_dismisses_popup_when_narrowing_eliminates_all_candidates() -> TestResult {
        let mut state = fresh_state()?;
        let tmp = tempfile::tempdir()?;
        type_into(&mut state, "/")?;
        refresh_autocomplete(&mut state, tmp.path())?;
        assert!(state.autocomplete.is_some());
        type_into(&mut state, "zzzzz")?;
        refresh_autocomplete(&mut state, tmp.path())?;
        assert!(
            state.autocomplete.is_none(),
            "no slash command starts with `zzzzz`",
        );
        Ok(())
    }

    #[test]
    fn refresh_creates_file_popup_for_at_trigger() -> TestResult {
        let tmp = tempfile::tempdir()?;
        fs::create_dir_all(tmp.path().join("src"))?;
        fs::write(tmp.path().join("src/main.rs"), "x")?;
        let mut state = fresh_state()?;
        type_into(&mut state, "@main")?;
        refresh_autocomplete(&mut state, tmp.path())?;
        assert!(
            state.autocomplete.is_some(),
            "file popup must be created for @main",
        );
        let popup = state
            .autocomplete
            .as_ref()
            .ok_or("autocomplete popup is missing")?;
        assert!(
            !popup.candidates.is_empty(),
            "@main must match at least one file in the temp tree",
        );
        Ok(())
    }

    #[test]
    fn refresh_rebuilds_when_trigger_kind_changes() -> TestResult {
        let tmp = tempfile::tempdir()?;
        // Seed at least one file so the @ snapshot is non-empty (an
        // empty snapshot yields a closed popup, which would dismiss
        // rather than rebuild — defeating the purpose of this test).
        fs::write(tmp.path().join("seed.txt"), "x")?;
        let mut state = fresh_state()?;
        type_into(&mut state, "/")?;
        refresh_autocomplete(&mut state, tmp.path())?;
        let slash_byte = state
            .autocomplete
            .as_ref()
            .ok_or("autocomplete popup is missing")?
            .trigger_start_byte();
        assert_eq!(slash_byte, 0);
        state.input_editor.clear()?;
        type_into(&mut state, "@")?;
        refresh_autocomplete(&mut state, tmp.path())?;
        assert!(state.autocomplete.is_some(), "@ trigger seeds file popup");
        Ok(())
    }

    #[test]
    fn dismiss_clears_popup_and_panel_height() -> TestResult {
        let mut state = fresh_state()?;
        let tmp = tempfile::tempdir()?;
        type_into(&mut state, "/")?;
        refresh_autocomplete(&mut state, tmp.path())?;
        assert!(state.autocomplete.is_some());
        dismiss(&mut state);
        assert!(state.autocomplete.is_none());
        assert_eq!(state.fixed_panel.autocomplete_popup_rows(), 0);
        Ok(())
    }

    #[test]
    fn refresh_after_initial_empty_input_is_noop() -> TestResult {
        let mut state = fresh_state()?;
        let tmp = tempfile::tempdir()?;
        refresh_autocomplete(&mut state, tmp.path())?;
        assert!(state.autocomplete.is_none());
        Ok(())
    }

    #[test]
    fn discover_skills_picks_up_md_files_with_descriptions() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let skill_path = tmp.path().join("my-skill.md");
        fs::write(
            &skill_path,
            "---\nname: my-skill\ndescription: Do a thing\n---\n\nbody\n",
        )?;
        let mut snapshot = Vec::new();
        let mut seen = HashSet::new();
        discover_skills(tmp.path(), &mut snapshot, &mut seen)?;
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].name, "my-skill");
        assert_eq!(snapshot[0].description, "Do a thing");
        assert_eq!(snapshot[0].source_tag, SourceTag::Profile);
        Ok(())
    }

    #[test]
    fn discover_skills_skips_already_seen_names() -> TestResult {
        let tmp = tempfile::tempdir()?;
        fs::write(tmp.path().join("help.md"), "---\ndescription: dup\n---\n")?;
        let mut snapshot = Vec::new();
        let mut seen: HashSet<String> = ["help".to_owned()].into_iter().collect();
        discover_skills(tmp.path(), &mut snapshot, &mut seen)?;
        assert!(
            snapshot.is_empty(),
            "shadowed name must not appear in snapshot: {snapshot:?}",
        );
        Ok(())
    }

    #[test]
    fn discover_skills_ignores_non_md_files() -> TestResult {
        let tmp = tempfile::tempdir()?;
        fs::write(tmp.path().join("notes.txt"), "ignored")?;
        let mut snapshot = Vec::new();
        let mut seen = HashSet::new();
        discover_skills(tmp.path(), &mut snapshot, &mut seen)?;
        assert!(snapshot.is_empty());
        Ok(())
    }

    #[test]
    fn read_skill_description_returns_none_for_file_without_frontmatter() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("plain.md");
        fs::write(&path, "no frontmatter here\n")?;
        assert!(read_skill_description(&path)?.is_none());
        Ok(())
    }

    #[test]
    fn read_skill_description_strips_quotes_around_value() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("quoted.md");
        fs::write(&path, "---\ndescription: \"hello world\"\n---\n")?;
        assert_eq!(
            read_skill_description(&path)?.as_deref(),
            Some("hello world"),
        );
        Ok(())
    }

    #[test]
    fn acceptance_uses_latest_narrowed_context_and_is_one_undo_gesture() -> TestResult {
        let mut state = fresh_state()?;
        let tmp = tempfile::tempdir()?;
        type_into(&mut state, "/")?;
        refresh_autocomplete(&mut state, tmp.path())?;
        type_into(&mut state, "hel")?;
        refresh_autocomplete(&mut state, tmp.path())?;
        state.screen.dirty = false;
        assert_eq!(
            handle_popup_key(
                KeyEvent::new(KeyCode::Tab, Modifiers::NONE),
                &mut state,
                80,
                24
            )?,
            PopupKeyOutcome::Consumed
        );
        assert_eq!(state.input_editor.text(), "/help");
        assert!(state.autocomplete.is_none());
        assert!(state.screen.dirty, "accepted completion must repaint");
        let options = state.composer_geometry.input_options();
        state.input_editor.run_cell_command(
            "history.undo",
            iridium_editor::CommandArgs::NONE,
            options,
        )?;
        assert_eq!(state.input_editor.text(), "/hel");
        Ok(())
    }

    #[test]
    fn stale_popup_acceptance_preserves_newer_document_cursor_and_history() -> TestResult {
        let mut state = fresh_state()?;
        let tmp = tempfile::tempdir()?;
        type_into(&mut state, "/he")?;
        refresh_autocomplete(&mut state, tmp.path())?;
        type_into(&mut state, "x")?;
        let snapshot = state.input_editor.snapshot()?;
        let history = serde_json::to_value(state.input_editor.kernel().history_snapshot())?;
        assert!(matches!(
            handle_popup_key(
                KeyEvent::new(KeyCode::Tab, Modifiers::NONE),
                &mut state,
                80,
                24
            ),
            Err(TuiError::Composer(
                crate::input::ComposerError::StaleSnapshot { .. }
            ))
        ));
        state.input_editor.validate_snapshot(&snapshot)?;
        assert_eq!(state.input_editor.text(), "/hex");
        assert_eq!(
            serde_json::to_value(state.input_editor.kernel().history_snapshot())?,
            history
        );
        assert_eq!(state.fixed_panel.autocomplete_popup_rows(), 0);
        Ok(())
    }

    #[test]
    fn active_and_multiple_selections_explicitly_dismiss_completion() -> TestResult {
        use iridium_editor::editor::CellReplacementCursor;
        use iridium_editor::{CommandArgs, CursorState, Position, Range, Selection};
        let mut state = fresh_state()?;
        let tmp = tempfile::tempdir()?;
        type_into(&mut state, "/he")?;
        refresh_autocomplete(&mut state, tmp.path())?;
        assert!(state.autocomplete.is_some());
        sync_input_area(&mut state, 80, 24)?;
        let options = state.composer_geometry.input_options();
        state
            .input_editor
            .run_cell_command("selection.selectAll", CommandArgs::NONE, options)?;
        refresh_autocomplete(&mut state, tmp.path())?;
        assert!(state.autocomplete.is_none());
        assert_eq!(state.fixed_panel.autocomplete_popup_rows(), 0);
        state.input_editor.run_cell_command(
            "selection.collapseToPrimary",
            CommandArgs::NONE,
            options,
        )?;
        refresh_autocomplete(&mut state, tmp.path())?;
        assert!(state.autocomplete.is_some());
        let mut cursor = CursorState::at(Position::new(0, 3));
        cursor.add_cursor(Selection::collapsed(Position::new(0, 1)));
        state.input_editor.replace_cells(
            Range::empty(Position::zero()),
            "",
            CellReplacementCursor::Exact(cursor),
        )?;
        assert_eq!(
            handle_popup_key(
                KeyEvent::new(KeyCode::Tab, Modifiers::NONE),
                &mut state,
                80,
                24
            )?,
            PopupKeyOutcome::NotConsumed
        );
        assert!(state.autocomplete.is_none());
        assert_eq!(state.fixed_panel.autocomplete_popup_rows(), 0);
        assert_eq!(state.input_editor.text(), "/he");
        assert_eq!(state.input_editor.kernel().state().cursor.cursor_count(), 2);
        Ok(())
    }

    #[test]
    fn file_acceptance_preserves_original_unicode_before_trigger() -> TestResult {
        let mut state = fresh_state()?;
        let tmp = tempfile::tempdir()?;
        fs::create_dir(tmp.path().join("src"))?;
        fs::write(tmp.path().join("src/main.rs"), "fixture")?;
        type_into(&mut state, "🙂 @main")?;
        refresh_autocomplete(&mut state, tmp.path())?;
        assert_eq!(
            handle_popup_key(
                KeyEvent::new(KeyCode::Enter, Modifiers::NONE),
                &mut state,
                80,
                24
            )?,
            PopupKeyOutcome::Consumed
        );
        assert_eq!(state.input_editor.text(), "🙂 src/main.rs");
        Ok(())
    }

    #[test]
    fn modified_enter_does_not_accept_visible_popup() -> TestResult {
        let mut state = fresh_state()?;
        let tmp = tempfile::tempdir()?;
        type_into(&mut state, "/he")?;
        refresh_autocomplete(&mut state, tmp.path())?;
        for modifiers in [Modifiers::ALT, Modifiers::SHIFT] {
            state.screen.dirty = false;
            assert_eq!(
                handle_popup_key(KeyEvent::new(KeyCode::Enter, modifiers), &mut state, 80, 24)?,
                PopupKeyOutcome::NotConsumed
            );
            assert_eq!(state.input_editor.text(), "/he");
            assert!(state.autocomplete.is_some());
            assert!(!state.screen.dirty);
        }
        Ok(())
    }

    #[test]
    fn popup_navigation_and_dismissal_repaint_without_editing_the_draft() -> TestResult {
        let mut state = fresh_state()?;
        let tmp = tempfile::tempdir()?;
        type_into(&mut state, "/")?;
        refresh_autocomplete(&mut state, tmp.path())?;
        let snapshot = state.input_editor.snapshot()?;
        for (code, selected) in [(KeyCode::Down, 1), (KeyCode::Up, 0)] {
            state.screen.dirty = false;
            assert_eq!(
                handle_popup_key(KeyEvent::new(code, Modifiers::NONE), &mut state, 80, 24)?,
                PopupKeyOutcome::Consumed
            );
            assert_eq!(
                state
                    .autocomplete
                    .as_ref()
                    .ok_or("navigated popup disappeared")?
                    .selected_index,
                selected
            );
            assert!(state.screen.dirty, "popup selection must repaint");
            state.input_editor.validate_snapshot(&snapshot)?;
        }
        state.screen.dirty = false;
        assert_eq!(
            handle_popup_key(
                KeyEvent::new(KeyCode::Escape, Modifiers::NONE),
                &mut state,
                80,
                24
            )?,
            PopupKeyOutcome::Consumed
        );
        assert!(state.autocomplete.is_none());
        assert_eq!(state.fixed_panel.autocomplete_popup_rows(), 0);
        assert!(state.screen.dirty, "popup dismissal must repaint");
        state.input_editor.validate_snapshot(&snapshot)?;
        Ok(())
    }

    #[test]
    fn absent_optional_skill_directory_is_empty_but_real_read_failures_surface() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let mut snapshot = Vec::new();
        let mut seen = HashSet::new();
        discover_skills(&tmp.path().join("absent"), &mut snapshot, &mut seen)?;
        assert!(snapshot.is_empty());
        let file = tmp.path().join("not-a-directory");
        fs::write(&file, "fixture")?;
        let error = discover_skills(&file, &mut snapshot, &mut seen)
            .err()
            .ok_or("non-directory skill root was accepted")?;
        assert!(error.to_string().contains("not-a-directory"));
        assert!(snapshot.is_empty());
        assert!(read_skill_description(&tmp.path().join("missing.md")).is_err());
        Ok(())
    }
}
