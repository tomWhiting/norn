//! User-message colour remains uniform across original lines and terminal wrapping.

use super::*;
use crate::render::layout::Layout;
use crate::terminal::caps::TerminalCaps;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn every_user_line_keeps_its_colour_and_original_source_mapping() -> TestResult {
    for content in [
        "first\nsecond\nthird",
        "first\r\nsecond",
        "\nsecond\n",
        "🙂 e\u{301}\n宽\t界",
        "one\n**literal user text**\nthree",
        "first\n\u{1b}]52;c;untrusted\u{7}",
        "",
    ] {
        let rendered = input_text(content)?;
        let plain = crate::render::retained_markdown::render_plain(content)?;
        assert_eq!(rendered.styled.text(), plain.styled.text());
        assert_eq!(rendered.spans, plain.spans);
        for (byte, character) in rendered.styled.text().char_indices() {
            if character.is_whitespace() {
                continue;
            }
            let span = rendered
                .styled
                .spans()
                .iter()
                .find(|span| span.range.contains(&byte))
                .ok_or("a user-message character lost its style")?;
            assert_eq!(span.style.foreground, Some([80, 160, 220]));
        }
    }
    Ok(())
}

#[test]
fn encoded_user_rows_keep_the_same_colour_after_newlines_and_wrapping() -> TestResult {
    for (content, width, expected_rows) in
        [("first\nsecond\nthird", 12, 3), ("abcdef\nghijkl", 3, 4)]
    {
        let text = input_text(content)?;
        let rows = super::super::layout_rows(&text.styled, width)?;
        assert_eq!(rows.len(), expected_rows);
        let area = Rect {
            column: 0,
            row: 0,
            width,
            height: u16::try_from(rows.len())?,
        };
        let mut frame = Frame {
            layout: Layout::ResizeRequired { area },
            rows: Vec::new(),
            composer: None,
            cursor: None,
        };
        for (row, geometry) in rows.into_iter().enumerate() {
            frame.rows.push(PaintRow {
                area,
                row: u16::try_from(row)?,
                text: Arc::clone(&text),
                geometry,
                selected: false,
                selection: Vec::new(),
                composer: false,
            });
        }
        let mut caps = TerminalCaps::baseline();
        caps.true_colour = true;
        let encoded = frame.encode(&caps)?;
        let mut observed = UserColours::default();
        vte::Parser::new().advance(&mut observed, &encoded);
        assert_eq!(
            observed.printed.len(),
            content.chars().filter(|c| !c.is_whitespace()).count()
        );
        assert!(
            observed
                .printed
                .iter()
                .all(|colour| *colour == Some([80, 160, 220])),
            "all visible user characters need the same colour, regardless of escape batching"
        );
    }
    Ok(())
}

#[derive(Default)]
struct UserColours {
    foreground: Option<[u8; 3]>,
    printed: Vec<Option<[u8; 3]>>,
}

impl vte::Perform for UserColours {
    fn print(&mut self, character: char) {
        if !character.is_whitespace() {
            self.printed.push(self.foreground);
        }
    }

    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        intermediates: &[u8],
        ignore: bool,
        action: char,
    ) {
        if ignore || !intermediates.is_empty() || action != 'm' {
            return;
        }
        let codes: Vec<u16> = params
            .iter()
            .flat_map(|values| values.iter().copied())
            .collect();
        let mut remaining = codes.as_slice();
        while let Some((code, rest)) = remaining.split_first() {
            match (*code, rest) {
                (0 | 39, _) => self.foreground = None,
                (38, [2, red, green, blue, following @ ..]) => {
                    self.foreground = match (
                        u8::try_from(*red),
                        u8::try_from(*green),
                        u8::try_from(*blue),
                    ) {
                        (Ok(red), Ok(green), Ok(blue)) => Some([red, green, blue]),
                        _ => None,
                    };
                    remaining = following;
                    continue;
                }
                _ => {}
            }
            remaining = rest;
        }
    }
}

#[test]
fn live_composer_keeps_full_width_default_colours_and_three_existing_chrome_rows() -> TestResult {
    for (columns, terminal_rows, expected_chrome) in
        [(80, 24, 3), (16, 10, 3), (8, 5, 0), (1, 2, 0)]
    {
        let mut state = AppState::new(
            TerminalCaps::baseline(),
            crate::input::history::InputHistory::in_memory(),
            norn::agent::registry::AgentRegistry::shared(),
            crate::app::state::test_view_source(uuid::Uuid::new_v4()),
            crate::render::fixed_panel::StatusBar::default(),
        );
        state.input_editor.paste_cells("a")?;
        let frame = super::super::prepare(&mut state, columns, terminal_rows)?;
        let Layout::Ready { composer, .. } = frame.layout else {
            return Err("ready layout expected".into());
        };
        let layer = frame
            .composer
            .as_ref()
            .ok_or("typed composer layer missing")?;
        assert_eq!(layer.area.column, 0);
        assert_eq!(layer.area.width, columns);
        assert_eq!(composer.height - layer.area.height, expected_chrome);
        assert_eq!(
            layer.area,
            crate::render::layout::composer_input_area(composer)
        );
        assert!(
            frame.rows.iter().all(|row| !row.composer),
            "draft must not be rewrapped as a PaintRow"
        );
        let first = layer.cells.get(0, 0).ok_or("first composer cell missing")?;
        assert_eq!(first.style().foreground, iridium_tui::cell::Color::Default);
        assert_eq!(first.style().background, iridium_tui::cell::Color::Default);
        assert_eq!(state.input_editor.text(), "a");
        assert!(frame.cursor.is_some());
        frame.prepare(&TerminalCaps::baseline())?;
    }
    Ok(())
}

#[test]
fn send_key_control_reuses_the_last_hint_row_and_never_claims_a_clipped_hit_area() -> TestResult {
    let mut state = AppState::new(
        TerminalCaps::baseline(),
        crate::input::history::InputHistory::in_memory(),
        norn::agent::registry::AgentRegistry::shared(),
        crate::app::state::test_view_source(uuid::Uuid::new_v4()),
        crate::render::fixed_panel::StatusBar::default(),
    );
    for (policy, kitty, label) in [
        (
            crate::frontend_preferences::ComposerSendKey::Enter,
            false,
            "[Enter sends]",
        ),
        (
            crate::frontend_preferences::ComposerSendKey::AltEnter,
            false,
            "[Alt+Enter sends]",
        ),
        (
            crate::frontend_preferences::ComposerSendKey::ShiftEnter,
            false,
            "[Shift+Enter unconfirmed]",
        ),
        (
            crate::frontend_preferences::ComposerSendKey::ShiftEnter,
            true,
            "[Shift+Enter sends]",
        ),
    ] {
        state.terminal_caps.kitty_keyboard = kitty;
        state.composer_send_key = policy;
        let frame = super::super::prepare(&mut state, 80, 24)?;
        let button = state
            .screen
            .composer_send_key_area
            .ok_or("send control missing")?;
        assert_eq!(button.row, 23);
        assert_eq!(button.height, 1);
        assert_eq!(usize::from(button.width), label.len());
        let hint = frame
            .rows
            .iter()
            .find(|row| row.area.row == button.row)
            .ok_or("hint row missing")?;
        assert!(hint.text.styled.text().starts_with(label));
        assert!(hint.text.styled.text().contains("Option+s / F10 send key"));
        if policy == crate::frontend_preferences::ComposerSendKey::ShiftEnter {
            assert!(hint.text.styled.text().contains("Enter newline"));
        }
        let Layout::Ready { composer, .. } = frame.layout else {
            return Err("composer missing".into());
        };
        assert_eq!(composer.height, 4);
        super::super::prepare(&mut state, u16::try_from(label.len())?, 24)?;
        assert!(
            state.screen.composer_send_key_area.is_none(),
            "ellipsis clips the final button cell"
        );
        super::super::prepare(&mut state, u16::try_from(label.len() + 1)?, 24)?;
        assert!(state.screen.composer_send_key_area.is_some());
    }
    super::super::prepare(&mut state, 3, 24)?;
    assert!(state.screen.composer_send_key_area.is_none());
    super::super::prepare(&mut state, 80, 5)?;
    assert!(state.screen.composer_send_key_area.is_none());
    super::super::prepare(&mut state, 0, 0)?;
    assert!(state.screen.composer_send_key_area.is_none());
    Ok(())
}

#[test]
fn latest_uses_existing_hint_row_without_overlap_or_clipped_click_area() -> TestResult {
    let mut state = AppState::new(
        TerminalCaps::baseline(),
        crate::input::history::InputHistory::in_memory(),
        norn::agent::registry::AgentRegistry::shared(),
        crate::app::state::test_view_source(uuid::Uuid::new_v4()),
        crate::render::fixed_panel::StatusBar::default(),
    );
    state.input_editor.paste_cells("draft")?;
    let following = super::super::prepare(&mut state, 80, 24)?;
    let Layout::Ready {
        composer: original, ..
    } = following.layout
    else {
        return Err("following composer absent".into());
    };
    assert!(state.screen.prepared_latest.is_none());
    state.screen.viewport.pin();
    for columns in [80, 24, 8, 7, 1] {
        let frame = super::super::prepare(&mut state, columns, 24)?;
        let Layout::Ready { composer, .. } = frame.layout else {
            return Err("pinned composer absent".into());
        };
        if columns == 80 {
            assert_eq!(
                composer, original,
                "Latest must allocate no additional chrome row"
            );
        }
        if let Some(area) = state.screen.prepared_latest {
            assert_eq!(area.row, composer.row + composer.height - 1);
            assert_eq!(area.column + area.width, columns);
            assert_eq!(area.width, 8);
            let painted = frame
                .rows
                .iter()
                .find(|row| row.area == area)
                .ok_or("Latest hit has no painted label")?;
            assert_eq!(
                painted.text.styled.text(),
                crate::app::view_actions::latest::LABEL
            );
            if let Some(send) = state.screen.composer_send_key_area {
                assert!(send.column + send.width < area.column);
            }
        } else {
            assert!(
                columns < 8 || crate::render::layout::composer_input_area(composer) == composer
            );
        }
        assert!(
            state.screen.latest_hit.is_none(),
            "prepared geometry is not published click authority"
        );
        assert_eq!(state.input_editor.text(), "draft");
    }
    state.screen.viewport.follow_tail();
    state.transcript.request_latest();
    super::super::prepare(&mut state, 80, 24)?;
    assert!(
        state.screen.prepared_latest.is_some(),
        "pending coverage keeps Latest visible"
    );
    state.transcript.cancel_latest();
    super::super::prepare(&mut state, 80, 24)?;
    assert!(state.screen.prepared_latest.is_none());
    Ok(())
}

#[test]
fn send_key_hint_uses_the_current_cached_bindings_and_shows_explicit_unbinding() -> TestResult {
    let mut state = AppState::new(
        TerminalCaps::baseline(),
        crate::input::history::InputHistory::in_memory(),
        norn::agent::registry::AgentRegistry::shared(),
        crate::app::state::test_view_source(uuid::Uuid::new_v4()),
        crate::render::fixed_panel::StatusBar::default(),
    );
    state.view_shortcuts = std::sync::Arc::new(
        state
            .view_shortcuts
            .replacement("send_key_cycle", &["alt+q"])?,
    );
    let frame = super::super::prepare(&mut state, 100, 24)?;
    assert!(
        frame
            .rows
            .iter()
            .any(|row| row.text.styled.text().contains("Option+q send key"))
    );
    assert!(
        !frame
            .rows
            .iter()
            .any(|row| row.text.styled.text().contains("F10 send key"))
    );
    state.view_shortcuts =
        std::sync::Arc::new(state.view_shortcuts.replacement("send_key_cycle", &[])?);
    let frame = super::super::prepare(&mut state, 100, 24)?;
    assert!(
        frame
            .rows
            .iter()
            .any(|row| row.text.styled.text().contains("unbound send key"))
    );
    assert!(
        state.screen.composer_send_key_area.is_some(),
        "click recovery remains available"
    );
    Ok(())
}
