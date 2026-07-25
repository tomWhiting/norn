//! Off-executor persistence for one provider attempt's response audio.

use crate::error::{NornError, ProviderError, SessionError};
use crate::provider::openai::response_stream_event::ResponseStreamEvent;
use crate::provider::response_audio::ResponseAudioEvent;
use crate::session::response_audio::ResponseAudioWriter;
use crate::session::{ResponseAudioArtifactRef, ResponseAudioStore, SessionPersistError};

/// Reference to the response-audio sidecar the current provider attempt
/// opened, if any.
///
/// The retry loop reads this slot at the top of every replay to discard
/// the abandoned attempt's unsealed sidecar. Without it, unbounded retry
/// across a long outage would leave one unsealed artifact per attempt on
/// disk forever (design D10: the retry whisper must be flat in memory
/// *and* on disk). The slot holds at most one reference, so at most one
/// unsealed sidecar exists per in-flight provider call.
#[derive(Default)]
pub(super) struct AttemptArtifactSlot {
    reference: parking_lot::Mutex<Option<ResponseAudioArtifactRef>>,
}

impl AttemptArtifactSlot {
    fn record(&self, reference: ResponseAudioArtifactRef) {
        *self.reference.lock() = Some(reference);
    }

    fn take(&self) -> Option<ResponseAudioArtifactRef> {
        self.reference.lock().take()
    }
}

/// Discard the sidecar an abandoned provider attempt left unsealed.
///
/// Called at the top of every replay, so the reference being dropped
/// belongs to an attempt the loop has already decided to retry — never to
/// the in-flight attempt, and never to a sealed artifact (sealing clears
/// the slot).
///
/// The live reference is cleared from the shared in-flight capture
/// *before* the file is removed, so no observer can ever hold a reference
/// naming a deleted sidecar. A removal failure is loud (a warning naming
/// the artifact) and non-fatal: a stale sidecar is a bounded disk cost,
/// while failing the turn over it would trade a whisper for a death.
pub(super) fn discard_abandoned_attempt_artifact(
    slot: &AttemptArtifactSlot,
    store: Option<&ResponseAudioStore>,
    partial_capture: Option<&crate::r#loop::compaction::SharedTimeoutState>,
) {
    let Some(reference) = slot.take() else {
        return;
    };
    if let Some(state) = partial_capture
        && let Some(partial) = state.lock().in_flight_partial.as_mut()
    {
        partial.response_audio = None;
    }
    let Some(store) = store else {
        // Unreachable by construction: a recorded reference implies the
        // writer that minted it, which implies a store. Warn rather than
        // silently forget the artifact if that ever stops holding.
        tracing::warn!(
            artifact = %reference,
            "abandoned response-audio sidecar has no store to discard it from",
        );
        return;
    };
    if let Err(error) = off_executor(|| store.discard(reference)) {
        tracing::warn!(
            artifact = %reference,
            %error,
            "failed to discard an abandoned response-audio sidecar before replay",
        );
    }
}

pub(super) struct ResponseAudioCapture<'store> {
    store: Option<&'store ResponseAudioStore>,
    attempt: u32,
    writer: Option<ResponseAudioWriter>,
    slot: Option<&'store AttemptArtifactSlot>,
}

impl<'store> ResponseAudioCapture<'store> {
    pub(super) const fn new(
        store: Option<&'store ResponseAudioStore>,
        attempt: u32,
        slot: Option<&'store AttemptArtifactSlot>,
    ) -> Self {
        Self {
            store,
            attempt,
            writer: None,
            slot,
        }
    }

    pub(super) fn append(
        &mut self,
        stream_event: &ResponseStreamEvent,
        event: &ResponseAudioEvent,
    ) -> Result<(), NornError> {
        if self.writer.is_none() {
            let store = self
                .store
                .ok_or(NornError::Provider(ProviderError::UnsupportedResponseMedia))?;
            let writer = off_executor(|| store.begin(self.attempt)).map_err(local_error)?;
            if let Some(slot) = self.slot {
                slot.record(writer.reference());
            }
            self.writer = Some(writer);
        }
        let writer = self.writer.as_mut().ok_or_else(|| {
            NornError::Session(SessionError::StorageError {
                reason: "response-audio writer disappeared before append".to_owned(),
            })
        })?;
        off_executor(|| writer.append(stream_event, event)).map_err(local_error)
    }

    pub(super) fn reference(&self) -> Option<ResponseAudioArtifactRef> {
        self.writer.as_ref().map(ResponseAudioWriter::reference)
    }

    pub(super) fn seal(
        mut self,
        response_id: Option<&str>,
    ) -> Result<Option<ResponseAudioArtifactRef>, NornError> {
        let Some(writer) = self.writer.take() else {
            return Ok(None);
        };
        let sealed = off_executor(|| writer.seal(response_id)).map_err(local_error)?;
        // A sealed sidecar is durable evidence of a completed attempt and
        // must never be reachable by the abandoned-attempt discard.
        if let Some(slot) = self.slot {
            slot.take();
        }
        Ok(Some(sealed))
    }
}

fn off_executor<T>(operation: impl FnOnce() -> T) -> T {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(operation)
        }
        _ => operation(),
    }
}

fn local_error(error: SessionPersistError) -> NornError {
    NornError::Session(SessionError::from(error))
}
