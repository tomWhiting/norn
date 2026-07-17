//! Off-executor persistence for one provider attempt's response audio.

use crate::error::{NornError, ProviderError, SessionError};
use crate::provider::openai::response_stream_event::ResponseStreamEvent;
use crate::provider::response_audio::ResponseAudioEvent;
use crate::session::response_audio::ResponseAudioWriter;
use crate::session::{ResponseAudioArtifactRef, ResponseAudioStore, SessionPersistError};

pub(super) struct ResponseAudioCapture<'store> {
    store: Option<&'store ResponseAudioStore>,
    attempt: u32,
    writer: Option<ResponseAudioWriter>,
}

impl<'store> ResponseAudioCapture<'store> {
    pub(super) const fn new(store: Option<&'store ResponseAudioStore>, attempt: u32) -> Self {
        Self {
            store,
            attempt,
            writer: None,
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
        off_executor(|| writer.seal(response_id))
            .map(Some)
            .map_err(local_error)
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
