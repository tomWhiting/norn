//! Selection regressions for exact revisions, original graphemes, paging and reflow.

use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::sync::Arc;

use norn::model_selection::ModelRuntime;
use norn::provider::{AgentEvent, AgentEventKind, ProviderEvent};
use norn::session_view::{
    AcceptedModel, BodyRef, BodyRepresentation, SessionIdentity, SessionProjection, ViewItemKind,
    ViewSource,
};
use uuid::Uuid;

use super::{MappedBody, OriginalBody, Selection, SelectionError};
use crate::render::retained_markdown::{BoundaryAffinity, render_markdown, render_plain};
use crate::render::retained_text::TextLayout;
use crate::render::syntax::SyntaxHighlighter;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn source() -> ViewSource {
    ViewSource {
        session: SessionIdentity::Ephemeral(Uuid::new_v4()),
        agent_id: Uuid::new_v4(),
        parent_agent_id: None,
        store_generation: Uuid::new_v4(),
    }
}

fn local_body(text: &str) -> Result<(ViewSource, BodyRef), Box<dyn std::error::Error>> {
    let owner = source();
    let mut projection = SessionProjection::new(owner.clone());
    let item = projection.record_local_body(
        ViewItemKind::Notice,
        "selection fixture",
        text,
        BodyRepresentation::Text,
    )?;
    let reference = projection
        .item(&item)
        .and_then(|item| item.bodies.first())
        .ok_or("fixture has no owner-minted body")?
        .clone();
    Ok((owner, reference))
}

#[test]
fn resize_preserves_original_graphemes_and_hard_newlines() -> TestResult {
    let text = "e\u{301} 👩‍💻 alpha\nbeta 界 tail";
    let (owner, reference) = local_body(text)?;
    let rendered = render_plain(text)?;
    let original = OriginalBody::new(&reference, text, true);
    let mapped = MappedBody::new(&reference, &rendered);
    let mut selection = Selection::start(&owner, original, mapped, 0, BoundaryAffinity::After)?;
    selection.extend(
        &owner,
        original,
        mapped,
        text.len(),
        BoundaryAffinity::Before,
    )?;
    let retained = selection.clone();
    let tab_width = NonZeroUsize::new(4).ok_or("zero fixture tab width")?;
    let mut rows_per_width = Vec::new();
    for columns in [3, 8, 80] {
        let TextLayout::Rows(rows) = rendered.styled.layout(columns, tab_width)? else {
            return Err("nonzero fixture width produced no layout".into());
        };
        rows_per_width.push(rows.len());
        assert_eq!(selection, retained);
        assert_eq!(selection.read(&owner, Some(original))?, text);
        assert_eq!(selection.range(), 0..text.len());
    }
    assert!(rows_per_width[0] > rows_per_width[2]);
    assert_eq!(
        selection
            .read(&owner, Some(original))?
            .matches('\n')
            .count(),
        1
    );
    Ok(())
}

#[test]
fn reverse_drag_and_soft_wrap_hits_select_original_bytes() -> TestResult {
    let text = "alpha beta gamma delta";
    let (owner, reference) = local_body(text)?;
    let rendered = render_plain(text)?;
    let original = OriginalBody::new(&reference, text, true);
    let mapped = MappedBody::new(&reference, &rendered);
    let width = NonZeroUsize::new(4).ok_or("zero fixture tab width")?;
    let TextLayout::Rows(rows) = rendered.styled.layout(6, width)? else {
        return Err("wrapped fixture has no rows".into());
    };
    let last = rows.last().ok_or("wrapped fixture is empty")?;
    let last_edge = last.bytes().end;
    let mut selection = Selection::start(
        &owner,
        original,
        mapped,
        last_edge,
        BoundaryAffinity::Before,
    )?;
    let first = rows.first().ok_or("wrapped fixture is empty")?;
    selection.extend(
        &owner,
        original,
        mapped,
        first.hit(0),
        BoundaryAffinity::After,
    )?;
    assert_eq!(selection.read(&owner, Some(original))?, text);
    assert!(!selection.read(&owner, Some(original))?.contains('\n'));
    Ok(())
}

#[test]
fn transformed_spans_select_whole_original_intervals_and_chrome_is_refused() -> TestResult {
    let text = "- [x] done &amp; ready";
    let (owner, reference) = local_body(text)?;
    let rendered = render_markdown(text, &SyntaxHighlighter::new())?;
    let original = OriginalBody::new(&reference, text, true);
    let mapped = MappedBody::new(&reference, &rendered);
    let check = Selection::start(&owner, original, mapped, 0, BoundaryAffinity::After)?;
    assert_eq!(check.range(), 2..5);
    assert_eq!(check.read(&owner, Some(original))?, "[x]");
    let entity = rendered
        .styled
        .text()
        .find('&')
        .ok_or("rendered entity absent")?;
    let amp = Selection::start(&owner, original, mapped, entity, BoundaryAffinity::After)?;
    assert_eq!(amp.read(&owner, Some(original))?, "&amp;");
    let mut extended = check;
    extended.extend(&owner, original, mapped, entity, BoundaryAffinity::After)?;
    assert_eq!(extended.read(&owner, Some(original))?, "[x] done &amp;");

    let list = "- item";
    let (list_owner, list_reference) = local_body(list)?;
    let list_render = render_markdown(list, &SyntaxHighlighter::new())?;
    assert!(matches!(
        Selection::start(
            &list_owner,
            OriginalBody::new(&list_reference, list, true),
            MappedBody::new(&list_reference, &list_render),
            0,
            BoundaryAffinity::After,
        ),
        Err(SelectionError::Generated { offset: 0, .. })
    ));
    let previous = extended.clone();
    assert!(matches!(
        extended.extend(
            &owner,
            original,
            mapped,
            rendered.styled.text().len(),
            BoundaryAffinity::After
        ),
        Err(SelectionError::Generated { .. })
    ));
    assert_eq!(extended, previous);
    Ok(())
}

#[test]
fn both_display_and_original_boundaries_must_be_complete_graphemes() -> TestResult {
    let text = "e\u{301} 👩‍💻";
    let (owner, reference) = local_body(text)?;
    let rendered = render_plain(text)?;
    let original = OriginalBody::new(&reference, text, true);
    for offset in [1, 5, text.len() + 1] {
        assert!(matches!(
            Selection::start(
                &owner,
                original,
                MappedBody::new(&reference, &rendered),
                offset,
                BoundaryAffinity::After
            ),
            Err(SelectionError::Mapping { .. })
        ));
    }
    // A cached render of a shorter prefix does not prove that its last visible
    // letter is an original grapheme boundary after the next page arrives.
    let prefix_render = render_plain("e")?;
    assert!(matches!(
        Selection::start(
            &owner,
            original,
            MappedBody::new(&reference, &prefix_render),
            1,
            BoundaryAffinity::Before
        ),
        Err(SelectionError::OriginalBoundary { offset: 1, .. })
    ));
    Ok(())
}

#[test]
fn paging_reports_missing_context_and_retains_selection_when_bytes_are_evicted() -> TestResult {
    let text = "alpha beta";
    let (owner, reference) = local_body(text)?;
    let rendered = render_plain(text)?;
    let mapped = MappedBody::new(&reference, &rendered);
    let full = OriginalBody::new(&reference, text, true);
    let prefix = OriginalBody::new(&reference, "alpha", false);
    let mut selection = Selection::start(&owner, full, mapped, 0, BoundaryAffinity::After)?;
    let empty = selection.clone();
    assert!(matches!(
        selection.extend(&owner, prefix, mapped, 5, BoundaryAffinity::Before),
        Err(SelectionError::IncompleteBoundary { offset: 5, .. })
    ));
    assert_eq!(selection, empty);
    selection.extend(&owner, full, mapped, text.len(), BoundaryAffinity::Before)?;
    let selected = selection.clone();
    assert!(matches!(
        selection.read(&owner, Some(prefix)),
        Err(SelectionError::OutsideLoaded {
            offset: 10,
            loaded: 5,
            ..
        })
    ));
    assert!(matches!(
        selection.read(&owner, None),
        Err(SelectionError::Unavailable { .. })
    ));
    assert!(matches!(
        selection.extend(&owner, prefix, mapped, 1, BoundaryAffinity::After),
        Err(SelectionError::OutsideLoaded { .. })
    ));
    assert_eq!(selection, selected);
    assert_eq!(selection.read(&owner, Some(full))?, text);
    Ok(())
}

struct RevisionFixture {
    owner: ViewSource,
    previous: BodyRef,
    current: BodyRef,
}

fn same_text_revisions(text: &str) -> Result<RevisionFixture, Box<dyn std::error::Error>> {
    let owner = source();
    let mut projection = SessionProjection::new(owner.clone());
    let model = ModelRuntime::new(None, "fixture", Some(4096), None, None, BTreeMap::new())?;
    projection.begin_execution(Uuid::new_v4(), AcceptedModel::capture(&model, 0))?;
    let mut bodies = Vec::new();
    for event in [
        ProviderEvent::TextDelta {
            text: text.to_owned(),
        },
        ProviderEvent::TextComplete {
            text: text.to_owned(),
        },
    ] {
        projection.apply_live(&AgentEvent {
            agent_id: owner.agent_id,
            agent_role: Arc::from("root"),
            event: AgentEventKind::Provider(event),
        })?;
        bodies.push(
            projection
                .items()
                .find_map(|item| item.bodies.first())
                .ok_or("live fixture body absent")?
                .clone(),
        );
    }
    let current = bodies.pop().ok_or("current body absent")?;
    let previous = bodies.pop().ok_or("previous body absent")?;
    Ok(RevisionFixture {
        owner,
        previous,
        current,
    })
}

#[test]
fn equal_text_never_authorizes_new_revision_or_stale_render_mapping() -> TestResult {
    let text = "same bytes";
    let RevisionFixture {
        owner,
        previous,
        current,
    } = same_text_revisions(text)?;
    assert_ne!(previous, current);
    let rendered = render_plain(text)?;
    let original = OriginalBody::new(&previous, text, true);
    let mapped = MappedBody::new(&previous, &rendered);
    let mut selection = Selection::start(&owner, original, mapped, 0, BoundaryAffinity::After)?;
    selection.extend(
        &owner,
        original,
        mapped,
        text.len(),
        BoundaryAffinity::Before,
    )?;
    let retained = selection.clone();
    let replacement = OriginalBody::new(&current, text, true);
    assert!(matches!(
        selection.read(&owner, Some(replacement)),
        Err(SelectionError::BodyChanged { .. })
    ));
    assert!(matches!(
        selection.extend(
            &owner,
            replacement,
            MappedBody::new(&current, &rendered),
            1,
            BoundaryAffinity::After
        ),
        Err(SelectionError::BodyChanged { .. })
    ));
    assert!(matches!(
        Selection::start(&owner, replacement, mapped, 0, BoundaryAffinity::After),
        Err(SelectionError::MappingChanged { .. })
    ));
    assert_eq!(selection, retained);
    assert_eq!(selection.reference(), &previous);
    Ok(())
}

#[test]
fn source_replacement_never_rebinds_selection_and_errors_do_not_quote_body() -> TestResult {
    let text = "private-body-marker\u{1b}[31m";
    let (owner, reference) = local_body(text)?;
    let rendered = render_plain(text)?;
    let original = OriginalBody::new(&reference, text, true);
    let mapped = MappedBody::new(&reference, &rendered);
    let mut selection = Selection::start(&owner, original, mapped, 0, BoundaryAffinity::After)?;
    selection.extend(
        &owner,
        original,
        mapped,
        rendered.styled.text().len(),
        BoundaryAffinity::Before,
    )?;
    let mut reopened = owner.clone();
    reopened.store_generation = Uuid::new_v4();
    let failure = selection
        .read(&reopened, Some(original))
        .err()
        .ok_or("source replacement accepted")?;
    assert!(matches!(failure, SelectionError::SourceChanged { .. }));
    assert!(!failure.to_string().contains("private-body-marker"));
    assert!(!format!("{failure:?}").contains("private-body-marker"));
    // Pure selection returns approved original bytes, including controls as
    // data; it does not silently pretend to have sanitized or copied them.
    assert_eq!(selection.read(&owner, Some(original))?, text);
    Ok(())
}

#[test]
fn explicit_original_selection_keeps_markdown_and_refuses_unknown_prefix_end() -> TestResult {
    let text = "**bold** e\u{301} 👩‍💻\nnext";
    let (owner, reference) = local_body(text)?;
    let original = OriginalBody::new(&reference, text, true);
    let selection = Selection::from_original(&owner, original, 0..text.len())?;
    assert_eq!(selection.read(&owner, Some(original))?, text);
    assert!(Selection::from_original(&owner, original, 10..11).is_err());
    assert!(
        Selection::from_original(
            &owner,
            OriginalBody::new(&reference, text, false),
            0..text.len()
        )
        .is_err()
    );
    let first = Selection::from_original(&owner, OriginalBody::new(&reference, text, false), 0..8)?;
    assert_eq!(first.read(&owner, Some(original))?, "**bold**");
    Ok(())
}
