//! Small event-forwarding helpers for the shared streaming executor.

use crate::error::ProviderError;
use crate::provider::debug::DebugDumper;
use crate::provider::events::ProviderEvent;
use crate::provider::openai::sse::SseEvent;

use super::SseEventMapper;

/// Dumps and maps one SSE frame, forwarding the mapped events.
pub(super) async fn emit_mapped<M: SseEventMapper>(
    mapper: &mut M,
    sse_event: &SseEvent,
    dumper: Option<&DebugDumper>,
    tx: &tokio::sync::mpsc::Sender<Result<ProviderEvent, ProviderError>>,
) -> bool {
    if let Some(dump) = dumper {
        dump.write_sse_event(mapper.dump_label(sse_event), &sse_event.data);
    }
    for provider_event in mapper.map_event(sse_event) {
        let is_terminal = matches!(
            provider_event,
            Ok(ProviderEvent::Done { .. } | ProviderEvent::Error { .. }) | Err(_)
        );
        if tx.send(provider_event).await.is_err() || is_terminal {
            return true;
        }
    }
    false
}

pub(super) fn log_complete(
    backend_label: &str,
    request_start: std::time::Instant,
    stream_start: std::time::Instant,
    counts: (u64, u64),
) {
    let (chunks, events) = counts;
    tracing::debug!(
        total_s = request_start.elapsed().as_secs_f64(),
        stream_s = stream_start.elapsed().as_secs_f64(),
        chunks,
        events,
        backend = backend_label,
        "provider request complete"
    );
}
