//! Constant-space, non-disclosing diagnostics for MCP server stderr.

use std::fmt;
use std::io::ErrorKind;
use std::sync::{Arc, Mutex, MutexGuard};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::ChildStderr;

use crate::resource::DescriptorPermit;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Completion {
    #[default]
    Active,
    Eof,
    Interrupted,
    ReadError(ErrorKind),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContentShape {
    NoneObserved,
    OneLine,
    MultipleLines,
}

#[derive(Debug, Default)]
struct ObservationState {
    completed_lines: u8,
    trailing_content: bool,
    completion: Completion,
}

#[derive(Debug, Default)]
pub(super) struct StderrObservation {
    state: Mutex<ObservationState>,
}

impl StderrObservation {
    pub(super) fn observe(&self, chunk: &[u8]) {
        let mut state = self.state();
        if state.completion != Completion::Active {
            return;
        }
        for byte in chunk {
            if *byte == b'\n' {
                state.completed_lines = state.completed_lines.saturating_add(1).min(2);
                state.trailing_content = false;
            } else {
                state.trailing_content = true;
            }
        }
    }

    pub(super) fn finish_eof(&self) {
        self.finish(Completion::Eof);
    }

    pub(super) fn finish_read_error(&self, kind: ErrorKind) {
        self.finish(Completion::ReadError(kind));
    }

    pub(super) fn interrupt(&self) {
        self.finish(Completion::Interrupted);
    }

    pub(super) fn snapshot(&self) -> StderrSummary {
        let state = self.state();
        let observed_lines = state.completed_lines + u8::from(state.trailing_content);
        let content = match observed_lines {
            0 => ContentShape::NoneObserved,
            1 => ContentShape::OneLine,
            _ => ContentShape::MultipleLines,
        };
        StderrSummary {
            content,
            completion: state.completion,
        }
    }

    fn finish(&self, completion: Completion) {
        let mut state = self.state();
        if state.completion == Completion::Active {
            state.completion = completion;
        }
    }

    fn state(&self) -> MutexGuard<'_, ObservationState> {
        match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct StderrSummary {
    content: ContentShape,
    completion: Completion,
}

impl fmt::Display for StderrSummary {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let content = match self.content {
            ContentShape::NoneObserved => "no content observed",
            ContentShape::OneLine => "one withheld line",
            ContentShape::MultipleLines => "multiple withheld lines",
        };
        write!(formatter, "MCP server stderr: {content}; ")?;
        match self.completion {
            Completion::Active => formatter.write_str("diagnostic drain active (truncated)"),
            Completion::Eof => formatter.write_str("diagnostic drain completed"),
            Completion::Interrupted => {
                formatter.write_str("diagnostic drain interrupted (truncated)")
            }
            Completion::ReadError(kind) => {
                write!(formatter, "diagnostic drain failed ({kind:?}, truncated)")
            }
        }
    }
}

struct DrainGuard {
    observation: Arc<StderrObservation>,
}

impl Drop for DrainGuard {
    fn drop(&mut self) {
        self.observation.interrupt();
    }
}

pub(super) async fn drain_stderr(
    mut stderr: BufReader<ChildStderr>,
    permit: DescriptorPermit,
    child_id: Option<u32>,
    observation: Arc<StderrObservation>,
) {
    let guard = DrainGuard {
        observation: Arc::clone(&observation),
    };
    loop {
        let available = match stderr.fill_buf().await {
            Ok([]) => {
                observation.finish_eof();
                tracing::debug!(?child_id, summary = %observation.snapshot(), "MCP stderr closed");
                break;
            }
            Ok(available) => available,
            Err(error) => {
                observation.finish_read_error(error.kind());
                tracing::warn!(
                    ?child_id,
                    summary = %observation.snapshot(),
                    "MCP stderr read failed"
                );
                break;
            }
        };
        observation.observe(available);
        let consumed = available.len();
        stderr.consume(consumed);
    }
    drop(permit);
    drop(guard);
}

#[cfg(test)]
mod tests {
    use super::{StderrObservation, StderrSummary};

    #[test]
    fn line_shape_survives_chunk_boundaries_without_retaining_content() {
        let observation = StderrObservation::default();
        observation.observe(b"secret-frag");
        observation.observe(b"ment\nsecond");
        observation.finish_eof();

        let rendered = observation.snapshot().to_string();

        assert_eq!(
            rendered,
            "MCP server stderr: multiple withheld lines; diagnostic drain completed"
        );
        assert!(!rendered.contains("secret"));
        assert!(!rendered.contains("fragment"));
    }

    #[test]
    fn terminal_state_is_first_writer_wins() {
        let observation = StderrObservation::default();
        observation.observe(b"one line");
        observation.interrupt();
        let interrupted = observation.snapshot();

        observation.finish_eof();
        observation.observe(b"\nsecond line");

        assert_eq!(observation.snapshot(), interrupted);
        assert_eq!(
            interrupted.to_string(),
            "MCP server stderr: one withheld line; diagnostic drain interrupted (truncated)"
        );
    }

    #[test]
    fn empty_completed_stderr_has_a_fixed_summary() {
        let observation = StderrObservation::default();
        observation.finish_eof();
        let completed = StderrSummary {
            content: super::ContentShape::NoneObserved,
            completion: super::Completion::Eof,
        };

        assert_eq!(observation.snapshot(), completed);
        observation.interrupt();
        assert_eq!(observation.snapshot(), completed);
    }
}
