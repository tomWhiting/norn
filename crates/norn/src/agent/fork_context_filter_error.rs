use thiserror::Error;

use crate::session::ResponseAudioReferenceError;

/// A parent history could not be filtered without weakening its typed records.
#[derive(Debug, Error)]
pub enum ContextFilterError {
    /// A reserved response-audio artifact link was malformed.
    #[error(transparent)]
    ResponseAudio(#[from] ResponseAudioReferenceError),
}
