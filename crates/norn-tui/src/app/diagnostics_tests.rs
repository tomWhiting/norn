//! UI-owner delivery uses real retained notices and acknowledges only after publication.

use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

#[tokio::test]
async fn diagnostics_are_local_expandable_notices_and_closure_disables_wait()
-> Result<(), Box<dyn std::error::Error>> {
    let store = norn::session::EventStore::new();
    let source = store.bind_view_source(
        &norn::session::SessionBinding::ephemeral_root(),
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
    let (sender, receiver) = tokio::sync::broadcast::channel(1);
    let acknowledged = Arc::new(AtomicU64::new(0));
    state.diagnostics = Some(DiagnosticReceiver::new(receiver, Arc::clone(&acknowledged)));
    sender.send(Diagnostic {
        sequence: 1,
        level: tracing::Level::ERROR,
        target: "norn::retry".to_owned(),
        text: Arc::from("actual error detail\n"),
    })?;
    let result = wait(&mut state.diagnostics).await;
    assert_eq!(acknowledged.load(Ordering::Acquire), 0);
    finish(&mut state, result)?;
    assert_eq!(acknowledged.load(Ordering::Acquire), 1);
    assert!(state.screen.dirty);
    assert!(store.is_empty());
    let item = state
        .transcript
        .projection
        .items()
        .next()
        .ok_or("notice absent")?
        .clone();
    assert!(matches!(item.kind, norn::session_view::ViewItemKind::Error));
    assert_eq!(item.label.as_str(), "ERROR · norn::retry");
    let reference = item.bodies.first().ok_or("expandable detail absent")?;
    let demand = state
        .transcript
        .demand_body(&item.id, reference, false)?
        .ok_or("detail demand absent")?;
    assert_eq!(
        state.transcript.read_local_body(&demand)?.text,
        "actual error detail\n"
    );
    finish(&mut state, Err(RecvError::Lagged(3)))?;
    drop(sender);
    let result = wait(&mut state.diagnostics).await;
    finish(&mut state, result)?;
    assert!(state.diagnostics.is_none());
    Ok(())
}
