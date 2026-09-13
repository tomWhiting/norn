//! Actual asynchronous display reads retain source identity without runtime store authority.

use super::*;
use crate::app::read_tasks::ReadTasks;
use norn::session::SessionBinding;
use norn::session::events::{EventBase, SessionEvent};
use std::num::NonZeroUsize;
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn fixture(label: &str) -> TestResult<(Arc<EventStore>, Transcript)> {
    let store = Arc::new(EventStore::new());
    let source = store.bind_view_source(&SessionBinding::ephemeral_root(), Uuid::new_v4(), None)?;
    for index in 0..5 {
        store.append(SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: format!("{label} {index}"),
        })?;
    }
    let mut view = Transcript::new(source);
    view.config
        .set_history_demand(NonZeroUsize::new(2).ok_or("zero fixture page")?);
    view.accept_history(&store.history_page(&view.initial_history()?)?)?;
    Ok((store, view))
}

async fn finish_history(view: &mut Transcript, jobs: &mut ReadTasks) -> TestResult {
    let result = jobs
        .history
        .join_next()
        .await
        .ok_or("missing history task")?;
    assert!(view.finish_history(result)?);
    Ok(())
}

#[tokio::test]
async fn missing_and_foreign_readers_do_not_poison_pending_demands() -> TestResult {
    let mut jobs = ReadTasks::default();
    let (store, mut view) = fixture("selected")?;
    let (other, foreign) = fixture("foreign")?;
    let revision = view.projection.revision();
    assert!(view.load_older(&mut jobs).is_err());
    assert!(jobs.history.is_empty());
    view.request_latest();
    assert!(view.load_latest(&mut jobs).is_err());
    assert!(view.latest_pending());
    assert!(
        view.attach_history_reader(open_history_reader(other).await?)
            .is_err()
    );
    assert_eq!(view.projection.revision(), revision);
    assert_ne!(view.projection.source(), foreign.projection.source());
    assert!(view.history_reader().is_err());
    view.attach_history_reader(open_history_reader(store).await?)?;
    assert!(view.load_latest(&mut jobs)?);
    assert!(!view.load_latest(&mut jobs)?);
    finish_history(&mut view, &mut jobs).await?;
    assert!(!view.latest_pending());
    assert!(view.load_older(&mut jobs)?);
    assert!(!view.load_older(&mut jobs)?);
    finish_history(&mut view, &mut jobs).await?;
    assert_eq!(view.projection.items().count(), 4);
    Ok(())
}

#[tokio::test]
async fn missing_body_access_can_retry_after_attaching_the_actual_reader() -> TestResult {
    let mut jobs = ReadTasks::default();
    let (store, mut view) = fixture("selected original")?;
    let item = view
        .projection
        .items()
        .next()
        .ok_or("missing fixture item")?;
    let id = item.id.clone();
    let reference = item.bodies.first().ok_or("missing body reference")?.clone();
    view.config
        .set_body_demand(NonZeroUsize::new(8).ok_or("zero fixture bytes")?);
    assert!(view.load_body(&mut jobs, &id, &reference, false).is_err());
    assert!(jobs.bodies.is_empty());
    view.attach_history_reader(open_history_reader(store).await?)?;
    view.load_body(&mut jobs, &id, &reference, false)?;
    view.load_body(&mut jobs, &id, &reference, false)?;
    assert_eq!(jobs.bodies.len(), 1);
    let result = jobs.bodies.join_next().await.ok_or("missing body task")?;
    view.finish_body(result)?;
    let body = view.body(&reference).ok_or("missing accepted body")?;
    assert_eq!(body.original, "selected");
    assert_eq!(body.next_offset, Some(8));
    view.load_body(&mut jobs, &id, &reference, true)?;
    let result = jobs
        .bodies
        .join_next()
        .await
        .ok_or("missing continuation")?;
    view.finish_body(result)?;
    assert!(
        view.body(&reference)
            .ok_or("body vanished")?
            .original
            .starts_with("selected origina")
    );
    Ok(())
}

#[tokio::test]
async fn independent_views_keep_their_readers_through_new_appends_and_foreign_refusal() -> TestResult
{
    let mut jobs = ReadTasks::default();
    let (root, mut root_view) = fixture("root")?;
    let (child, mut child_view) = fixture("child")?;
    root_view.attach_history_reader(open_history_reader(Arc::clone(&root)).await?)?;
    child_view.attach_history_reader(open_history_reader(Arc::clone(&child)).await?)?;
    let root_revision = root_view.projection.revision();
    child.append(SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: "child future append".to_owned(),
    })?;
    let reader = child_view.history_reader()?;
    assert!(
        read_history(reader.clone(), root_view.initial_history()?)
            .await
            .is_err()
    );
    let page = read_history(reader, child_view.newer_history()?).await?;
    assert!(child_view.accept_history(&page)?);
    assert_eq!(child_view.observed_events, 6);
    assert_eq!(root_view.observed_events, 5);
    assert_eq!(root_view.projection.revision(), root_revision);
    assert!(root_view.load_older(&mut jobs)?);
    assert!(child_view.load_older(&mut jobs)?);
    // The selected view may change while either task is running: each queue has one owner.
    while let Some(result) = jobs.history.join_next().await {
        let (request, page) = result?;
        let target = if &request.source == root_view.projection.source() {
            &mut root_view
        } else if &request.source == child_view.projection.source() {
            &mut child_view
        } else {
            return Err("unknown source in shared read supervisor".into());
        };
        assert!(target.finish_history(Ok((request, page)))?);
    }
    assert_eq!(root_view.projection.items().count(), 4);
    assert_eq!(child_view.projection.items().count(), 5);
    assert_eq!(root.len(), 5);
    assert_eq!(child.len(), 6);
    Ok(())
}
