//! Retained detail policy, source mapping and semantic styling regressions.

use super::*;
use norn::session_view::{DisplayText, ToolView};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn tool() -> ToolView {
    ToolView {
        call_id: Some("presentation-call".to_owned()),
        stream_item_id: None,
        name: Some(DisplayText::new("read")),
        description: Some(DisplayText::new(
            "Inspect the word failed without treating it as an outcome",
        )),
        description_error: None,
        kind: None,
        arguments: None,
        result: None,
        invocation_event: None,
        invocation_attempt: None,
        result_event: None,
        result_parent: None,
        state: ToolState::Running,
        result_state: None,
        duration_ms: None,
        committed: None,
    }
}

#[test]
fn thinking_styles_preserve_exact_mapping_markdown_emphasis_and_code_colour() -> TestResult {
    let original = "Reason **carefully** about `let α = 1;`";
    let rendered = render_markdown(original, &crate::render::syntax::SyntaxHighlighter::new())?;
    let before = rendered.clone();
    let styled = with_base_style(rendered, body_style(&ViewItemKind::Thinking))?;
    assert_eq!(styled.styled.text(), before.styled.text());
    assert_eq!(styled.spans, before.spans);
    assert!(styled.styled.spans().iter().all(|span| {
        span.style.attributes.contains(TextAttribute::Dim)
            && span.style.attributes.contains(TextAttribute::Italic)
    }));
    for span in before.styled.spans() {
        let actual = styled
            .styled
            .spans()
            .iter()
            .find(|current| current.range == span.range)
            .ok_or("styled source span missing")?;
        assert_eq!(actual.style.foreground, span.style.foreground);
        if span.style.attributes.contains(TextAttribute::Bold) {
            assert!(actual.style.attributes.contains(TextAttribute::Bold));
        }
    }
    Ok(())
}

#[test]
fn tool_styles_use_typed_outcomes_and_generated_labels_keep_no_original_authority() -> TestResult {
    let mut tool = tool();
    assert_eq!(tool_colour(&tool), ACTIVE_BLUE);
    tool.result_state = Some(ToolState::Blocked);
    assert_eq!(tool_colour(&tool), WARNING_AMBER);
    tool.state = ToolState::Failed;
    assert_eq!(tool_colour(&tool), ERROR_RED);
    let label = crate::tools::summary::summarize(&tool, false).header();
    let rendered = header_text(&label, &ViewItemKind::Tool(Box::new(tool)))?;
    assert_eq!(rendered.styled.text(), label);
    assert!(
        rendered
            .spans
            .iter()
            .all(|span| span.source == SourceMapping::Generated)
    );
    let first = rendered
        .styled
        .spans()
        .first()
        .ok_or("tool name style absent")?;
    assert_eq!(first.range, 0.."read".len());
    assert!(first.style.attributes.contains(TextAttribute::Bold));
    assert!(
        rendered
            .styled
            .spans()
            .iter()
            .all(|span| span.style.foreground == Some(ERROR_RED))
    );
    Ok(())
}

#[test]
fn generated_separator_has_no_body_capability_or_mapped_bytes() -> TestResult {
    let group = local_group("", 80, None)?;
    assert!(group.reference.is_none());
    assert!(group.fixed_offset.is_none());
    assert!(group.text.spans.is_empty());
    assert_eq!(group.rows.len(), 1);
    assert!(group.rows[0].bytes().is_empty());
    Ok(())
}

#[test]
fn context_details_require_explicit_expansion_even_when_selected() -> TestResult {
    use crate::app::state::AppState;
    use norn::session::{EventStore, SessionBinding};
    let store = EventStore::new();
    let source = store.bind_view_source(
        &SessionBinding::ephemeral_root(),
        uuid::Uuid::new_v4(),
        None,
    )?;
    let mut state = AppState::new(
        crate::terminal::caps::TerminalCaps::baseline(),
        crate::input::history::InputHistory::in_memory(),
        norn::agent::registry::AgentRegistry::shared(),
        source,
        crate::render::fixed_panel::StatusBar::default(),
    );
    state.transcript.config.expanded_tools = true;
    let id = state.transcript.notice(
        ViewItemKind::Context,
        "Compacted context",
        Some("retained summary\nsecond line"),
    )?;
    let item = state
        .transcript
        .projection
        .item(&id)
        .ok_or("context item absent")?
        .clone();
    let reference = item.bodies.first().ok_or("context body absent")?;
    let demand = state
        .transcript
        .demand_body(&id, reference, false)?
        .ok_or("body demand absent")?;
    let loaded = state.transcript.read_local_body(&demand)?;
    state.transcript.accept_body(&demand, loaded)?;
    state
        .screen
        .viewport
        .select(id.clone(), &state.transcript.projection)?;
    let collapsed = item_groups(
        &state.transcript,
        &mut state.screen,
        &item,
        80,
        false,
        false,
    )?;
    assert_eq!(collapsed.len(), 1);
    assert!(collapsed[0].text.styled.text().starts_with("▸"));
    assert!(collapsed[0].reference.is_none());
    crate::app::render::prepare(&mut state, 80, 20)?;
    assert!(
        state
            .screen
            .hit_rows
            .iter()
            .filter(|hit| hit.anchor.item == id)
            .all(|hit| hit.body.is_none())
    );
    crate::app::view_actions::command("toggle", &mut state)?;
    let expanded = item_groups(
        &state.transcript,
        &mut state.screen,
        &item,
        80,
        false,
        false,
    )?;
    assert_eq!(expanded.len(), 2);
    assert_eq!(expanded[1].reference.as_ref(), Some(reference));
    assert_eq!(
        expanded[1].text.styled.text(),
        "retained summary\nsecond line"
    );
    state.screen.viewport.scroll_to(
        crate::app::viewport::ViewAnchor {
            item: id.clone(),
            position: crate::app::viewport::AnchorPosition::Header,
        },
        &state.transcript.projection,
    )?;
    crate::app::render::navigation::queue(&mut state, false, 1)?;
    crate::app::render::prepare(&mut state, 80, 20)?;
    assert!(matches!(
        state
            .screen
            .viewport
            .anchor()
            .map(|anchor| &anchor.position),
        Some(crate::app::viewport::AnchorPosition::Body {
            original_offset: 0,
            ..
        })
    ));
    assert!(
        state
            .screen
            .hit_rows
            .iter()
            .any(|hit| hit.anchor.item == id && hit.body.as_ref() == Some(reference))
    );
    crate::app::view_actions::command("toggle", &mut state)?;
    assert_eq!(
        item_groups(
            &state.transcript,
            &mut state.screen,
            &item,
            80,
            false,
            false
        )?
        .len(),
        1
    );
    Ok(())
}

#[test]
fn expanded_tool_background_preserves_original_source_spans() -> TestResult {
    let original = render_plain("command\noutput α")?;
    let mappings = original.spans.clone();
    let styled = with_base_style(original, body_style(&ViewItemKind::Tool(Box::new(tool()))))?;
    assert_eq!(styled.spans, mappings);
    assert!(
        styled
            .styled
            .spans()
            .iter()
            .all(|span| span.style.background == Some(TOOL_BACKGROUND))
    );
    Ok(())
}
