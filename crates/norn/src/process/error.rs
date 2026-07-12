//! Typed failures while creating managed background processes and spools.

use std::io;
use std::path::Path;

/// Managed-process construction or I/O failure.
#[derive(Clone, Debug, thiserror::Error)]
pub enum ProcessError {
    /// The process or system descriptor pool was exhausted.
    #[error(transparent)]
    DescriptorExhausted(Box<crate::resource::DescriptorExhaustion>),
    /// A non-descriptor I/O failure.
    #[error("{operation}: {reason}")]
    Io {
        /// Locally authored operation label.
        operation: String,
        /// Original I/O diagnostic.
        reason: String,
    },
}

impl ProcessError {
    pub(crate) fn from_io(
        operation: impl Into<String>,
        path: Option<&Path>,
        error: &io::Error,
    ) -> Self {
        let operation = operation.into();
        match crate::resource::classify_descriptor_error(error, operation.clone(), path) {
            Some(exhaustion) => Self::DescriptorExhausted(Box::new(exhaustion)),
            None => Self::Io {
                operation,
                reason: error.to_string(),
            },
        }
    }
}

impl From<ProcessError> for crate::error::ToolError {
    fn from(error: ProcessError) -> Self {
        match error {
            ProcessError::DescriptorExhausted(source) => Self::DescriptorExhausted(source),
            other @ ProcessError::Io { .. } => Self::ExecutionFailed {
                reason: other.to_string(),
            },
        }
    }
}
