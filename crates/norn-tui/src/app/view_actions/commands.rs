//! Frontend command palette parsing and explicit reading requests; never agent input.

use super::{browse, ensure_selected, expand, focus, pin_visible, select_original};
use crate::TuiError;
use crate::app::focus::Focus;
use crate::app::render::interaction;
use crate::app::slash::LocalCommandOutcome;
use crate::app::state::AppState;
use crate::render::layout::SplitPreference;
use std::num::{NonZeroU16, NonZeroUsize};

const HELP: &str = "View controls\n/view focus composer|conversation|changes|divider · F6 / Shift+F6 cycles visible regions\n/pane [diff|agents] · toggle the side pane or select its content\n/view pane open|close|toggle|diff|agents · F2 switches upper pane\n/view split <conversation-weight> <changes-weight> · arrows resize focused divider\n/view up|down · PgUp/PgDn browse; Up/Down select rows outside composer\n/view expand|collapse|toggle|reset · Enter toggles selected tool\n/view compact|detailed · Ctrl+O toggles global tool detail\n/view follow|pin · return to live tail or keep current position\n/view older · demand one older history page\n/view more · demand next bytes of selected item's bodies\n/view history <events> · /view body <bytes> · positive demand preferences\n/view select <body-index> [<start-byte> <end-byte>] · select a whole loaded original body or explicit grapheme range\n/view selection [clear] · inspect/reset selection; mouse drag selects text\n/view copy · F4 · /view clipboard unspecified|disabled|osc52\n/view search [loaded|selected|older] <literal> · F3 · older requests one page and configured body prefixes; unavailable suffixes stay explicit\n/view next|previous · select a retained search hit; stale/unloaded revisions are refused\n/view export [--replace] <path> · F5 · original selection, create-new by default; spaces belong to the path\n/view status · current model, session, effort, tier, usage and local reading settings\n/view composer send-key enter|alt-enter · physical send key, independent of steer/queue\n/view preferences status|run|user|local|save · remembered or temporary frontend choices\n/view help · frontend actions never enter steer/queue";

/// Whether this exact input belongs to the shared TUI-only view or pane commands.
pub(in crate::app) fn is_frontend_command(text: &str) -> bool {
    matches!(crate::app::slash_catalog::classify_slash(text), crate::app::slash_catalog::SlashClass::Recognised { cmd, .. } if cmd.eq_ignore_ascii_case("view") || cmd.eq_ignore_ascii_case("pane"))
}

/// Preserve the named command's semantics on both idle and active submission paths.
pub(in crate::app) fn command_named(
    name: &str,
    arguments: &str,
    state: &mut AppState,
) -> Result<LocalCommandOutcome, TuiError> {
    if name.eq_ignore_ascii_case("view") {
        return command(arguments, state);
    }
    if name.eq_ignore_ascii_case("pane") {
        if !matches!(arguments.trim(), "" | "diff" | "agents") {
            command_error(state, "Use /pane [diff|agents]")?;
            state.screen.dirty = true;
            state.screen.allow_body_load = true;
            return Ok(LocalCommandOutcome::Rejected);
        }
        return command(
            &format!(
                "pane {}",
                if arguments.trim().is_empty() {
                    "toggle"
                } else {
                    arguments
                }
            ),
            state,
        );
    }
    Err(interaction(std::io::Error::other(
        "frontend command handler received an unrecognized command name",
    )))
}

/// Execute a locally submitted command without admitting it to the agent.
pub(in crate::app) fn command(
    text: &str,
    state: &mut AppState,
) -> Result<LocalCommandOutcome, TuiError> {
    let mut outcome = if text == "preferences" || text.starts_with("preferences ") {
        crate::app::frontend_preferences::command(
            text.strip_prefix("preferences").unwrap_or(text),
            state,
        )?
    } else {
        match execute(text, state) {
            Ok(()) => LocalCommandOutcome::Accepted,
            Err(message) => {
                command_error(state, &message)?;
                LocalCommandOutcome::Rejected
            }
        }
    };
    if matches!(outcome, LocalCommandOutcome::Accepted)
        && let Err(error) = crate::app::frontend_preferences::edited(state)
    {
        // The local change already happened. A save-start failure must remain
        // visible without turning the submitted command into a rejected draft.
        let reporting = command_error(state, &error.to_string());
        outcome = LocalCommandOutcome::after_reported_failure(error, reporting);
    }
    state.screen.dirty = true;
    state.screen.allow_body_load = true;
    Ok(outcome)
}

fn command_error(state: &mut AppState, message: &str) -> Result<(), TuiError> {
    let item = crate::app::notices::error(state, "View command", message)?;
    state
        .screen
        .viewport
        .scroll_to(
            crate::app::viewport::ViewAnchor {
                item,
                position: crate::app::viewport::AnchorPosition::Header,
            },
            &state.transcript.projection,
        )
        .map_err(interaction)
}

fn execute(text: &str, state: &mut AppState) -> Result<(), String> {
    if let Some(arguments) = text.strip_prefix("search ") {
        let (scope, query) = arguments.split_once(' ').unwrap_or(("loaded", arguments));
        let (scope, query) = match scope {
            "loaded" => (crate::app::search::SearchScope::LoadedTranscript, query),
            "selected" => (crate::app::search::SearchScope::SelectedBody, query),
            "older" => (
                crate::app::search::SearchScope::RequestedOlderHistory,
                query,
            ),
            _ => (crate::app::search::SearchScope::LoadedTranscript, arguments),
        };
        return super::reading::search(state, scope, query).map_err(|error| error.to_string());
    }
    if let Some(path) = text.strip_prefix("export ") {
        let (mode, path) = match path.strip_prefix("--replace ") {
            Some(path) => (crate::app::export::ExportMode::ReplaceExplicit, path),
            None => (crate::app::export::ExportMode::CreateNew, path),
        };
        if path.is_empty() {
            return Err("Export requires an explicit destination path".to_owned());
        }
        return super::reading::export(state, path, mode).map_err(|error| error.to_string());
    }
    let words: Vec<_> = text.split_whitespace().collect();
    match words.as_slice() {
        ["composer", "send-key", key] => {
            use crate::frontend_preferences::ComposerSendKey;
            state.composer_send_key = match *key {
                "enter" => ComposerSendKey::Enter,
                "alt-enter" => ComposerSendKey::AltEnter,
                _ => return Err("Use /view composer send-key enter|alt-enter".to_owned()),
            };
            state.screen.feedback = Some(format!("Composer send key: {key}"));
            Ok(())
        }
        ["next"] => super::reading::next_hit(state, false).map_err(|error| error.to_string()),
        ["previous"] => super::reading::next_hit(state, true).map_err(|error| error.to_string()),
        ["status"] => status(state),
        ["copy"] => {
            state.screen.request_copy = true;
            Ok(())
        }
        ["clipboard", capability] => {
            use crate::terminal::clipboard::ClipboardCapability;
            state.transcript.config.clipboard = match *capability {
                "unspecified" => ClipboardCapability::Unspecified,
                "disabled" => ClipboardCapability::Disabled,
                "osc52" => ClipboardCapability::Osc52,
                _ => return Err("Use /view clipboard unspecified|disabled|osc52".to_owned()),
            };
            state.screen.feedback = Some(format!("Clipboard capability: {capability}"));
            Ok(())
        }
        ["select", index] => select_original(
            state,
            index
                .parse()
                .map_err(|error| format!("Invalid body index: {error}"))?,
            None,
        )
        .map_err(|error| error.to_string()),
        ["select", index, start, end] => select_original(
            state,
            index
                .parse()
                .map_err(|error| format!("Invalid body index: {error}"))?,
            Some(
                start
                    .parse()
                    .map_err(|error| format!("Invalid start byte: {error}"))?
                    ..end
                        .parse()
                        .map_err(|error| format!("Invalid end byte: {error}"))?,
            ),
        )
        .map_err(|error| error.to_string()),
        ["selection", "clear"] => {
            state.screen.selection = None;
            state.screen.selection_item = None;
            Ok(())
        }
        ["selection"] => {
            state.screen.feedback = Some(match &state.screen.selection {
                Some(selection) => format!(
                    "Original selection {:?} in {:?}",
                    selection.range(),
                    selection.reference()
                ),
                None => "No original text selection".to_owned(),
            });
            Ok(())
        }

        [] | ["help"] => {
            let item = crate::app::notices::notice(state, "View", Some(HELP))
                .map_err(|error| error.to_string())?;
            state
                .screen
                .viewport
                .scroll_to(
                    crate::app::viewport::ViewAnchor {
                        item,
                        position: crate::app::viewport::AnchorPosition::Header,
                    },
                    &state.transcript.projection,
                )
                .map_err(|error| error.to_string())
        }
        ["focus", target] => focus(state, parse_focus(target)?).map_err(|error| error.to_string()),
        ["pane", action] => {
            use crate::app::render::AuxiliaryPane;
            if let Some(content) = match *action {
                "diff" => Some(AuxiliaryPane::Diff),
                "agents" => Some(AuxiliaryPane::Agents),
                _ => None,
            } {
                state.screen.auxiliary = content;
                state.screen.changes_open = true;
                state.screen.upper = crate::render::layout::UpperPane::Changes;
                return Ok(());
            }
            state.screen.changes_open = match *action {
                "open" => true,
                "close" => false,
                "toggle" => !state.screen.changes_open,
                _ => {
                    return Err(
                        "Use /pane [diff|agents] or /view pane open|close|toggle".to_owned()
                    );
                }
            };
            Ok(())
        }
        ["split", left, right] => {
            let left = left
                .parse::<NonZeroU16>()
                .map_err(|error| format!("Invalid conversation split weight: {error}"))?;
            let right = right
                .parse::<NonZeroU16>()
                .map_err(|error| format!("Invalid Changes split weight: {error}"))?;
            state.screen.split = SplitPreference::new(left, right);
            Ok(())
        }
        ["up"] => browse(state, true).map_err(|error| error.to_string()),
        ["down"] => browse(state, false).map_err(|error| error.to_string()),
        ["follow"] => {
            state.screen.viewport.follow_tail();
            Ok(())
        }
        ["pin"] => pin_visible(state).map_err(|error| error.to_string()),
        ["older"] => {
            state.screen.request_older = true;
            pin_visible(state).map_err(|error| error.to_string())
        }
        ["more"] => {
            ensure_selected(state).map_err(|error| error.to_string())?;
            state.screen.request_more = true;
            Ok(())
        }
        ["compact"] => {
            state.transcript.config.expanded_tools = false;
            Ok(())
        }
        ["detailed"] => {
            state.transcript.config.expanded_tools = true;
            Ok(())
        }
        ["reset"] => {
            state.screen.tool_overrides.clear();
            Ok(())
        }
        ["expand"] => expand(state, Some(true)).map_err(|error| error.to_string()),
        ["collapse"] => expand(state, Some(false)).map_err(|error| error.to_string()),
        ["toggle"] => expand(state, None).map_err(|error| error.to_string()),
        ["history", value] => {
            let demand = positive(value, "history")?;
            state.transcript.config.set_history_demand(demand);
            Ok(())
        }
        ["body", value] => {
            let demand = positive(value, "body")?;
            state.transcript.config.set_body_demand(demand);
            Ok(())
        }
        _ => Err("Unknown view action or arguments; use /view help".to_owned()),
    }
}

fn positive(value: &str, name: &str) -> Result<NonZeroUsize, String> {
    value
        .parse()
        .map_err(|error| format!("Invalid positive {name} demand: {error}"))
}

fn parse_focus(value: &str) -> Result<Focus, String> {
    match value {
        "composer" => Ok(Focus::Composer),
        "conversation" => Ok(Focus::Conversation),
        "changes" => Ok(Focus::Changes),
        "divider" => Ok(Focus::Divider),
        _ => Err("Focus must name composer, conversation, changes or divider".to_owned()),
    }
}

fn status(state: &mut AppState) -> Result<(), String> {
    let status = state.fixed_panel.status_bar();
    let text = format!(
        "Model: {}\nSession: {}\nReasoning effort: {}\nService tier: {}\nTurn input tokens: {} ({})\nTurn output tokens: {} ({})\nView source: {:?}\nHistory demand: {} events\nBody demand: {} original bytes\nTool details by default: {}\nClipboard: {:?}\nThinking visible: {}\nSecondary fields visible: {}\nComposer send key: {}\n",
        if status.model_name.is_empty() {
            "unavailable"
        } else {
            &status.model_name
        },
        if status.session_name.is_empty() {
            "unnamed; exact identity is in View source"
        } else {
            &status.session_name
        },
        status.reasoning_effort.as_deref().unwrap_or("unset"),
        status.service_tier.as_deref().unwrap_or("unset"),
        status.input_tokens,
        if status.input_tokens_estimated {
            "estimated"
        } else {
            "reported"
        },
        status.output_tokens,
        if status.output_tokens_estimated {
            "estimated"
        } else {
            "reported"
        },
        state.transcript.projection.source(),
        state
            .transcript
            .config
            .history_demand()
            .map_err(|error| error.to_string())?,
        state
            .transcript
            .config
            .body_demand()
            .map_err(|error| error.to_string())?,
        state.transcript.config.expanded_tools,
        state.transcript.config.clipboard,
        state.display_toggles.thinking_visible,
        state.display_toggles.secondary_fields_visible,
        state.composer_send_key.label(),
    );
    let item = crate::app::notices::notice(state, "Current view and runtime status", Some(&text))
        .map_err(|error| error.to_string())?;
    state
        .screen
        .viewport
        .scroll_to(
            crate::app::viewport::ViewAnchor {
                item,
                position: crate::app::viewport::AnchorPosition::Header,
            },
            &state.transcript.projection,
        )
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    use crate::frontend_preferences::ComposerSendKey;
    use crate::input::history::InputHistory;
    use crate::render::fixed_panel::StatusBar;
    use crate::terminal::caps::TerminalCaps;

    fn state() -> AppState {
        AppState::new(
            TerminalCaps::baseline(),
            InputHistory::in_memory(),
            norn::agent::registry::AgentRegistry::shared(),
            crate::app::state::test_view_source(uuid::Uuid::new_v4()),
            StatusBar::default(),
        )
    }

    #[test]
    fn frontend_command_recognition_is_exact_and_case_insensitive() {
        for text in [
            "/pane",
            " /PANE agents ",
            "/pane invalid",
            "/view",
            "/VIEW pane diff",
        ] {
            assert!(is_frontend_command(text), "{text}");
        }
        for text in ["/panels", "//pane", "/viewer", "pane", "explain /pane", "/"] {
            assert!(!is_frontend_command(text), "{text}");
        }
    }

    #[test]
    fn pane_toggle_preserves_content_and_geometry_focus() -> Result<(), Box<dyn std::error::Error>>
    {
        use crate::app::render::AuxiliaryPane;
        use crate::render::layout::UpperPane;

        let mut state = state();
        state.screen.auxiliary = AuxiliaryPane::Agents;
        state.screen.upper = UpperPane::Conversation;
        state.screen.changes_open = false;
        for expected_open in [true, false, true] {
            assert!(matches!(
                command_named("pane", "", &mut state)?,
                LocalCommandOutcome::Accepted
            ));
            assert_eq!(state.screen.changes_open, expected_open);
            assert_eq!(state.screen.auxiliary, AuxiliaryPane::Agents);
            assert_eq!(state.screen.upper, UpperPane::Conversation);
        }
        Ok(())
    }

    #[test]
    fn named_pane_and_view_alias_select_visible_content() -> Result<(), Box<dyn std::error::Error>>
    {
        use crate::app::render::AuxiliaryPane;
        use crate::render::layout::UpperPane;

        let mut state = state();
        for (name, argument, expected) in [
            ("pane", "agents", AuxiliaryPane::Agents),
            ("pane", "diff", AuxiliaryPane::Diff),
            ("view", "pane agents", AuxiliaryPane::Agents),
            ("view", "pane diff", AuxiliaryPane::Diff),
        ] {
            state.screen.changes_open = false;
            state.screen.upper = UpperPane::Conversation;
            assert!(matches!(
                command_named(name, argument, &mut state)?,
                LocalCommandOutcome::Accepted
            ));
            assert!(state.screen.changes_open);
            assert_eq!(state.screen.auxiliary, expected);
            assert_eq!(state.screen.upper, UpperPane::Changes);
        }
        for (argument, expected_open) in [
            ("pane close", false),
            ("pane open", true),
            ("pane toggle", false),
        ] {
            assert!(matches!(
                command_named("view", argument, &mut state)?,
                LocalCommandOutcome::Accepted
            ));
            assert_eq!(state.screen.changes_open, expected_open);
            assert_eq!(state.screen.auxiliary, AuxiliaryPane::Diff);
        }
        Ok(())
    }

    #[test]
    fn rejected_pane_arguments_do_not_change_state_or_draft()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::app::render::AuxiliaryPane;
        use crate::render::layout::UpperPane;

        let mut state = state();
        state.input_editor.paste_cells("/pane agents extra")?;
        state.screen.auxiliary = AuxiliaryPane::Diff;
        state.screen.changes_open = false;
        state.screen.upper = UpperPane::Conversation;
        for arguments in ["agents extra", "unknown", "open", "toggle"] {
            assert!(matches!(
                command_named("pane", arguments, &mut state)?,
                LocalCommandOutcome::Rejected
            ));
            assert!(!state.screen.changes_open);
            assert_eq!(state.screen.auxiliary, AuxiliaryPane::Diff);
            assert_eq!(state.screen.upper, UpperPane::Conversation);
            assert_eq!(state.input_editor.text(), "/pane agents extra");
        }
        Ok(())
    }

    #[test]
    fn accepted_pane_survives_missing_save_authority() -> Result<(), Box<dyn std::error::Error>> {
        use crate::app::render::AuxiliaryPane;

        let mut state = state();
        assert!(matches!(
            command_named("view", "preferences local", &mut state)?,
            LocalCommandOutcome::Accepted
        ));
        let previous_items = state.transcript.projection.items().len();
        assert!(matches!(
            command_named("pane", "agents", &mut state)?,
            LocalCommandOutcome::Accepted
        ));
        assert!(state.screen.changes_open);
        assert_eq!(state.screen.auxiliary, AuxiliaryPane::Agents);
        assert_eq!(
            state.transcript.projection.items().len(),
            previous_items + 1
        );
        let notice = state
            .transcript
            .projection
            .items()
            .next_back()
            .ok_or("missing save failure")?;
        assert!(matches!(
            notice.kind,
            norn::session_view::ViewItemKind::Error
        ));
        Ok(())
    }

    #[test]
    fn invalid_command_retains_setting_and_reports_typed_rejection()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut state = state();
        assert!(matches!(
            command("composer send-key alt-enter", &mut state)?,
            LocalCommandOutcome::Accepted
        ));
        assert_eq!(state.composer_send_key, ComposerSendKey::AltEnter);
        assert!(matches!(
            command("composer send-key invalid", &mut state)?,
            LocalCommandOutcome::Rejected
        ));
        assert_eq!(state.composer_send_key, ComposerSendKey::AltEnter);
        let refusal = state
            .transcript
            .projection
            .items()
            .next_back()
            .ok_or("missing command refusal")?;
        assert!(matches!(
            refusal.kind,
            norn::session_view::ViewItemKind::Error
        ));
        assert_eq!(refusal.label.as_str(), "View command");
        assert!(matches!(
            command("status", &mut state)?,
            LocalCommandOutcome::Accepted
        ));
        Ok(())
    }

    #[test]
    fn accepted_setting_survives_save_start_failure() -> Result<(), Box<dyn std::error::Error>> {
        let mut state = state();
        // The run-only fixture has no local save authority. Choosing a local
        // scope and then changing a setting still commits those local choices.
        assert!(matches!(
            command("preferences local", &mut state)?,
            LocalCommandOutcome::Accepted
        ));
        let notices = state.transcript.projection.items().len();
        assert!(matches!(
            command("composer send-key alt-enter", &mut state)?,
            LocalCommandOutcome::Accepted
        ));
        assert_eq!(state.composer_send_key, ComposerSendKey::AltEnter);
        assert_eq!(state.transcript.projection.items().len(), notices + 1);
        let failure = state
            .transcript
            .projection
            .items()
            .next_back()
            .ok_or("missing save-start error")?;
        assert!(matches!(
            failure.kind,
            norn::session_view::ViewItemKind::Error
        ));
        assert!(matches!(
            command("preferences save", &mut state)?,
            LocalCommandOutcome::Rejected
        ));
        assert_eq!(state.composer_send_key, ComposerSendKey::AltEnter);
        Ok(())
    }

    #[test]
    fn preference_validation_is_not_lost_at_view_boundary() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut state = state();
        for arguments in ["preferences invalid", "preferences save"] {
            assert!(
                matches!(
                    command(arguments, &mut state)?,
                    LocalCommandOutcome::Rejected
                ),
                "{arguments}"
            );
        }
        for arguments in ["preferences", "preferences status", "preferences run"] {
            assert!(
                matches!(
                    command(arguments, &mut state)?,
                    LocalCommandOutcome::Accepted
                ),
                "{arguments}"
            );
        }
        Ok(())
    }
}
