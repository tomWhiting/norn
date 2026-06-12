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
//! user skills with the same name, and the built-in commands the TUI
//! always accepts (`/compact`, `/clear`, `/exit`, `/help`, `/model`) are
//! merged at the top of the list.

use std::collections::HashSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use termina::Terminal as _;
use termina::event::{KeyCode, KeyEvent, Modifiers};

use crate::TuiError;
use crate::input::autocomplete::{
    AutocompletePopup, AutocompleteTrigger, SlashCandidate, SourceTag, TriggerKind, detect_trigger,
    walk_entries,
};
use crate::terminal::setup::TerminalGuard;

use super::render::sync_input_area;
use super::state::AppState;

/// Built-in slash commands surfaced in the `/` popup.
///
/// Pairs of `(name, description)`. Execution wiring lives outside the
/// TUI — these entries make the commands discoverable in the popup so
/// the user can complete them without remembering the spelling.
const BUILTIN_SLASH_COMMANDS: &[(&str, &str)] = &[
    ("clear", "Clear the input buffer"),
    ("compact", "Compact conversation history"),
    ("exit", "Exit the TUI"),
    ("help", "Show help"),
    ("model", "Switch model"),
];

/// Outcome of routing a key press through the popup pre-intercept.
///
/// Returned by [`handle_popup_key`] so the event loop can decide whether
/// to short-circuit (consumed → redraw) or fall through to the normal
/// [`crate::input::keybindings::map_key_event`] pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopupKeyOutcome {
    /// The popup absorbed the key — the event loop should redraw the
    /// panel, popup, and input and skip [`map_key_event`].
    Consumed,
    /// The popup did not handle this key — the event loop should
    /// continue with its normal action pipeline.
    NotConsumed,
}

/// Pre-intercept popup-active special keys.
///
/// Caller must have already confirmed the popup is open and the event
/// is a [`KeyEventKind::Press`]. The function mutates `state` directly
/// — selection navigation, splice on acceptance, dismiss on `Escape` —
/// and returns [`PopupKeyOutcome::Consumed`] when the redraw cycle
/// should run.
///
/// `Enter` is consumed only when it carries no modifiers: `Alt+Enter`
/// and `Shift+Enter` still insert a newline through `map_key_event`
/// (matching `map_enter`'s capability fallback), so the user can break
/// the line while a popup is open without first dismissing it.
pub fn handle_popup_key(
    key: KeyEvent,
    state: &mut AppState,
    cols: u16,
    terminal_rows: u16,
) -> PopupKeyOutcome {
    let bare = key.modifiers == Modifiers::NONE;
    match key.code {
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
            accept(state, cols, terminal_rows);
            PopupKeyOutcome::Consumed
        }
        KeyCode::Escape => {
            dismiss(state);
            sync_editor_input_area(state, cols, terminal_rows);
            PopupKeyOutcome::Consumed
        }
        _ => PopupKeyOutcome::NotConsumed,
    }
}

fn sync_editor_input_area(state: &mut AppState, cols: u16, terminal_rows: u16) {
    let rows = sync_input_area(&mut state.input_editor, cols, terminal_rows);
    state.fixed_panel.set_input_area(rows);
}

/// Splice the popup's currently selected candidate into the editor and
/// dismiss the popup.
///
/// Idempotent in the absence of a popup. A popup whose `accept()`
/// returns `None` — possible only if the selection points outside the
/// candidate list — is dismissed without touching the editor.
fn accept(state: &mut AppState, cols: u16, terminal_rows: u16) {
    if let Some(popup) = state.autocomplete.take()
        && let Some(acceptance) = popup.accept()
    {
        state.input_editor.apply_acceptance(&acceptance);
    }
    state.fixed_panel.set_autocomplete_popup(0);
    sync_editor_input_area(state, cols, terminal_rows);
}

/// Paint the autocomplete popup over the fixed panel's popup
/// placeholder rows.
///
/// Called after the fixed-panel redraw, which has cleared the popup
/// row range as part of its frame draw. When `state.autocomplete` is
/// `None` this is a no-op — the panel's placeholder rows already show
/// nothing. The popup write is cursor-addressed within fixed-panel
/// territory only (CO8); no scroll-region row is touched (CO7).
///
/// # Errors
///
/// Returns [`TuiError::Io`] when the popup render or the terminal
/// flush fails.
pub fn render_popup(state: &AppState, guard: &mut TerminalGuard) -> Result<(), TuiError> {
    let Some(popup) = state.autocomplete.as_ref() else {
        return Ok(());
    };
    let rows = guard.terminal_rows();
    let cols = guard.terminal_mut().get_dimensions().map_or(80, |d| d.cols);
    let top = state.fixed_panel.autocomplete_popup_top(rows);
    let caps = state.terminal_caps.clone();
    popup.render(top, cols, guard.terminal_mut(), &caps)?;
    guard.terminal_mut().flush()?;
    Ok(())
}

/// Bring the popup state in line with the current editor contents.
///
/// `workspace_root` is the directory the `@` walker scans — typically
/// the current working directory the TUI was launched from. Passing it
/// in (rather than calling `std::env::current_dir` here) keeps the
/// helper deterministic and testable.
pub fn refresh_autocomplete(state: &mut AppState, workspace_root: &Path) {
    let text = state.input_editor.text();
    let cursor_char = state.input_editor.cursor_char_index();
    match detect_trigger(&text, cursor_char) {
        None => dismiss(state),
        Some(trigger) => {
            let needs_rebuild = state
                .autocomplete
                .as_ref()
                .is_none_or(|popup| !popup.matches_trigger(&trigger));
            if needs_rebuild {
                let popup = build_popup(&trigger, workspace_root);
                if popup.is_open() {
                    state.autocomplete = Some(popup);
                } else {
                    state.autocomplete = None;
                }
            } else if let Some(popup) = state.autocomplete.as_mut()
                && !popup.narrow(&trigger.prefix)
            {
                state.autocomplete = None;
            }
            sync_panel_height(state);
        }
    }
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
fn build_popup(trigger: &AutocompleteTrigger, workspace_root: &Path) -> AutocompletePopup {
    match trigger.kind {
        TriggerKind::SlashCommand => {
            let snapshot = build_slash_snapshot();
            AutocompletePopup::new_slash(snapshot, &trigger.prefix, trigger.trigger_start_byte)
        }
        TriggerKind::FilePath => {
            let paths = walk_entries(workspace_root);
            AutocompletePopup::new_file(paths, &trigger.prefix, trigger.trigger_start_byte)
        }
    }
}

/// Compose the slash command snapshot: built-ins plus filesystem-
/// discovered skills, with project skills shadowing user skills.
///
/// The directory walk uses the [`profile_skills_dirs`] precedence — the
/// project-level `./.norn/skills/` is listed first so its names win the
/// `seen` shadow check before the user-level `~/.norn/skills/` directory
/// is scanned.
fn build_slash_snapshot() -> Vec<SlashCandidate> {
    let mut snapshot: Vec<SlashCandidate> = BUILTIN_SLASH_COMMANDS
        .iter()
        .map(|(name, desc)| SlashCandidate {
            name: (*name).to_owned(),
            source_tag: SourceTag::Builtin,
            description: (*desc).to_owned(),
        })
        .collect();

    let mut seen: HashSet<String> = snapshot.iter().map(|c| c.name.clone()).collect();
    for dir in profile_skills_dirs() {
        discover_skills(&dir, &mut snapshot, &mut seen);
    }
    snapshot
}

/// Directories searched for skill files, in shadow-priority order
/// (earlier entries take precedence on a name collision).
///
/// Mirrors the runtime's 7-tier search path from
/// `norn-cli::runtime::wiring::build_skill_search_paths`:
/// project `.norn/skills/`, `.agents/skills/`, `.claude/skills/`,
/// user `~/.norn/skills/`, `~/.agents/skills/`, `~/.claude/skills/`,
/// project `.meridian/skills/`.
fn profile_skills_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        out.push(cwd.join(".norn").join("skills"));
        out.push(cwd.join(".agents").join("skills"));
        out.push(cwd.join(".claude").join("skills"));
    }
    if let Some(home) = norn::config::paths::norn_dir() {
        out.push(home.join("skills"));
    }
    if let Some(home) = dirs::home_dir() {
        out.push(home.join(".agents").join("skills"));
        out.push(home.join(".claude").join("skills"));
    }
    if let Ok(cwd) = std::env::current_dir() {
        out.push(cwd.join(".meridian").join("skills"));
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
fn discover_skills(dir: &Path, snapshot: &mut Vec<SlashCandidate>, seen: &mut HashSet<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let skill_md = path.join("SKILL.md");
            if skill_md.is_file() {
                let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                    continue;
                };
                if seen.contains(name) {
                    continue;
                }
                seen.insert(name.to_owned());
                let description = read_skill_description(&skill_md).unwrap_or_default();
                snapshot.push(SlashCandidate {
                    name: name.to_owned(),
                    source_tag: SourceTag::Profile,
                    description,
                });
            }
            continue;
        }
        let Some(ext) = path.extension() else {
            continue;
        };
        if ext != "md" {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if seen.contains(stem) {
            continue;
        }
        seen.insert(stem.to_owned());
        let description = read_skill_description(&path).unwrap_or_default();
        snapshot.push(SlashCandidate {
            name: stem.to_owned(),
            source_tag: SourceTag::Profile,
            description,
        });
    }
}

/// Read the `description:` field from a skill markdown file's YAML
/// frontmatter, if present.
///
/// The parser handles only the shape skills actually use: an opening
/// `---` fence on the first line, key-value lines, and a closing `---`
/// fence. Anything else returns `None` and the caller falls back to an
/// empty description.
fn read_skill_description(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut lines = content.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            return None;
        }
        if let Some(rest) = trimmed.strip_prefix("description:") {
            let value = rest.trim().trim_matches(|c| c == '"' || c == '\'');
            if value.is_empty() {
                return None;
            }
            return Some(value.to_owned());
        }
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::fs;
    use std::sync::Arc;

    use parking_lot::RwLock;

    use norn::agent::registry::AgentRegistry;

    use super::*;
    use crate::input::history::InputHistory;
    use crate::render::fixed_panel::StatusBar;
    use crate::terminal::caps::TerminalCaps;

    fn fresh_state() -> AppState {
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
            },
            None,
        )
        .unwrap();
        let root_id = guard.id();
        guard.confirm().unwrap();
        AppState::new(
            TerminalCaps::baseline(),
            InputHistory::in_memory(),
            registry,
            root_id,
            StatusBar::default(),
        )
    }

    fn type_into(state: &mut AppState, text: &str) {
        for ch in text.chars() {
            state.input_editor.insert_char(ch);
        }
    }

    #[test]
    fn builtin_slash_commands_appear_in_snapshot() {
        let snapshot = build_slash_snapshot();
        let names: Vec<&str> = snapshot.iter().map(|c| c.name.as_str()).collect();
        for name in ["compact", "clear", "exit", "help", "model"] {
            assert!(
                names.contains(&name),
                "built-in `{name}` missing from snapshot: {names:?}",
            );
        }
    }

    #[test]
    fn refresh_creates_slash_popup_after_slash_typed() {
        let mut state = fresh_state();
        let tmp = tempfile::tempdir().unwrap();
        type_into(&mut state, "/");
        refresh_autocomplete(&mut state, tmp.path());
        assert!(
            state.autocomplete.is_some(),
            "popup must be created for `/` trigger",
        );
        assert!(state.fixed_panel.autocomplete_popup_rows() > 0);
    }

    #[test]
    fn refresh_narrows_existing_slash_popup_as_user_types() {
        let mut state = fresh_state();
        let tmp = tempfile::tempdir().unwrap();
        type_into(&mut state, "/");
        refresh_autocomplete(&mut state, tmp.path());
        let initial_count = state.autocomplete.as_ref().unwrap().candidates.len();
        type_into(&mut state, "he");
        refresh_autocomplete(&mut state, tmp.path());
        let narrowed = state.autocomplete.as_ref().unwrap().candidates.len();
        assert!(
            narrowed <= initial_count,
            "narrowing must not grow the list: {initial_count} → {narrowed}",
        );
        assert!(narrowed >= 1, "`help` must survive `/he` narrowing");
    }

    #[test]
    fn refresh_dismisses_popup_when_trigger_disappears() {
        let mut state = fresh_state();
        let tmp = tempfile::tempdir().unwrap();
        type_into(&mut state, "/he");
        refresh_autocomplete(&mut state, tmp.path());
        assert!(state.autocomplete.is_some());
        // Backspace removes the `/he`, no trigger remains.
        state.input_editor.backspace();
        state.input_editor.backspace();
        state.input_editor.backspace();
        refresh_autocomplete(&mut state, tmp.path());
        assert!(state.autocomplete.is_none());
        assert_eq!(state.fixed_panel.autocomplete_popup_rows(), 0);
    }

    #[test]
    fn refresh_dismisses_popup_when_narrowing_eliminates_all_candidates() {
        let mut state = fresh_state();
        let tmp = tempfile::tempdir().unwrap();
        type_into(&mut state, "/");
        refresh_autocomplete(&mut state, tmp.path());
        assert!(state.autocomplete.is_some());
        type_into(&mut state, "zzzzz");
        refresh_autocomplete(&mut state, tmp.path());
        assert!(
            state.autocomplete.is_none(),
            "no slash command starts with `zzzzz`",
        );
    }

    #[test]
    fn refresh_creates_file_popup_for_at_trigger() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("src")).unwrap();
        fs::write(tmp.path().join("src/main.rs"), "x").unwrap();
        let mut state = fresh_state();
        type_into(&mut state, "@main");
        refresh_autocomplete(&mut state, tmp.path());
        assert!(
            state.autocomplete.is_some(),
            "file popup must be created for @main",
        );
        let popup = state.autocomplete.as_ref().unwrap();
        assert!(
            !popup.candidates.is_empty(),
            "@main must match at least one file in the temp tree",
        );
    }

    #[test]
    fn refresh_rebuilds_when_trigger_kind_changes() {
        let tmp = tempfile::tempdir().unwrap();
        // Seed at least one file so the @ snapshot is non-empty (an
        // empty snapshot yields a closed popup, which would dismiss
        // rather than rebuild — defeating the purpose of this test).
        fs::write(tmp.path().join("seed.txt"), "x").unwrap();
        let mut state = fresh_state();
        type_into(&mut state, "/");
        refresh_autocomplete(&mut state, tmp.path());
        let slash_byte = state.autocomplete.as_ref().unwrap().trigger_start_byte();
        assert_eq!(slash_byte, 0);
        state.input_editor.clear();
        type_into(&mut state, "@");
        refresh_autocomplete(&mut state, tmp.path());
        assert!(state.autocomplete.is_some(), "@ trigger seeds file popup");
    }

    #[test]
    fn dismiss_clears_popup_and_panel_height() {
        let mut state = fresh_state();
        let tmp = tempfile::tempdir().unwrap();
        type_into(&mut state, "/");
        refresh_autocomplete(&mut state, tmp.path());
        assert!(state.autocomplete.is_some());
        dismiss(&mut state);
        assert!(state.autocomplete.is_none());
        assert_eq!(state.fixed_panel.autocomplete_popup_rows(), 0);
    }

    #[test]
    fn refresh_after_initial_empty_input_is_noop() {
        let mut state = fresh_state();
        let tmp = tempfile::tempdir().unwrap();
        refresh_autocomplete(&mut state, tmp.path());
        assert!(state.autocomplete.is_none());
    }

    #[test]
    fn discover_skills_picks_up_md_files_with_descriptions() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_path = tmp.path().join("my-skill.md");
        fs::write(
            &skill_path,
            "---\nname: my-skill\ndescription: Do a thing\n---\n\nbody\n",
        )
        .unwrap();
        let mut snapshot = Vec::new();
        let mut seen = HashSet::new();
        discover_skills(tmp.path(), &mut snapshot, &mut seen);
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].name, "my-skill");
        assert_eq!(snapshot[0].description, "Do a thing");
        assert_eq!(snapshot[0].source_tag, SourceTag::Profile);
    }

    #[test]
    fn discover_skills_skips_already_seen_names() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("help.md"), "---\ndescription: dup\n---\n").unwrap();
        let mut snapshot = Vec::new();
        let mut seen: HashSet<String> = ["help".to_owned()].into_iter().collect();
        discover_skills(tmp.path(), &mut snapshot, &mut seen);
        assert!(
            snapshot.is_empty(),
            "shadowed name must not appear in snapshot: {snapshot:?}",
        );
    }

    #[test]
    fn discover_skills_ignores_non_md_files() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("notes.txt"), "ignored").unwrap();
        let mut snapshot = Vec::new();
        let mut seen = HashSet::new();
        discover_skills(tmp.path(), &mut snapshot, &mut seen);
        assert!(snapshot.is_empty());
    }

    #[test]
    fn read_skill_description_returns_none_for_file_without_frontmatter() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("plain.md");
        fs::write(&path, "no frontmatter here\n").unwrap();
        assert!(read_skill_description(&path).is_none());
    }

    #[test]
    fn read_skill_description_strips_quotes_around_value() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("quoted.md");
        fs::write(&path, "---\ndescription: \"hello world\"\n---\n").unwrap();
        assert_eq!(
            read_skill_description(&path).as_deref(),
            Some("hello world"),
        );
    }
}
