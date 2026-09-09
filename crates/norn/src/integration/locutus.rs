//! Native Locutus read-aloud: one request, seat-scoped stop, no model or audio engine.

use std::path::PathBuf;
use std::time::Duration;

use locutus_contract::door::{DoorEvent, DoorIntent};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

// Waffles, 2026-09-09 14:35 Melbourne: a stop must release the UI after five
// seconds without a receipt. This covers blocked writes as well as reads;
// expiry is an unknown outcome, never a claim that the service stopped audio.
const STOP_RECEIPT_DEADLINE: Duration = Duration::from_secs(5);

/// One immutable speech admission, independent of later terminal focus changes.
#[derive(Clone, Debug)]
pub struct ReadAloudRequest {
    /// Client-generated correlation identity; replay always creates a new one.
    pub id: Uuid,
    /// Registered runtime seat that owns this request.
    pub seat: String,
    /// Explicit native control socket; no default service is guessed.
    pub socket: PathBuf,
    /// Completed answer selected by the operator.
    pub text: String,
    /// Seat voice override, if explicitly configured.
    pub voice: Option<String>,
}

/// Latest presentation state, not an audit stream or a playback measurement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadAloudProgress {
    /// Opening the explicitly selected service.
    Connecting,
    /// Submitted; waiting for the server's correlated admission.
    Submitted,
    /// Accepted for synthesis; hub admission may still hold playback.
    Accepted {
        /// Server-assigned speech chain.
        id: u64,
    },
    /// The server reports samples reaching its output callback.
    Playing {
        /// Server-assigned speech chain.
        id: u64,
    },
    /// Stop sent; awaiting a terminal receipt rather than claiming success.
    Stopping,
}

/// The server-confirmed outcome, with its actual playback identity and measurement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadAloudOutcome {
    /// Cancelled before speech was submitted; the service has no request to stop.
    NotSubmitted,
    /// The server reports that the entire say finished playing.
    Completed {
        /// Server process identity, paired with the chain id.
        session: String,
        /// Server-assigned speech chain.
        id: u64,
        /// Reported audio duration.
        duration_ms: u64,
        /// A stop was requested, but the server reported complete playback.
        stop_requested: bool,
    },
    /// Stop supervision expired; the server's playback outcome is unknown.
    StopUnconfirmed {
        /// Whether the complete request-tagged hush was written to the socket.
        stop_sent: bool,
        /// Actual wait since cancellation was observed by the supervisor.
        waited: Duration,
    },
    /// The server confirms a cut, including its measured playback uncertainty.
    Stopped {
        /// Server process identity, paired with the chain id.
        session: String,
        /// Server-assigned speech chain.
        id: u64,
        /// Output callback position at the cut.
        at_ms: u64,
        /// Device output latency; audible position may lag the callback by this much.
        latency_ms: u64,
    },
}

/// Native voice errors preserve the destination or request they describe.
#[derive(Debug, thiserror::Error)]
pub enum ReadAloudError {
    /// Configuration cannot identify valid speech.
    #[error("voice request {request} needs nonempty text and a registered seat")]
    InvalidRequest {
        /// Rejected client request identity.
        request: Uuid,
    },
    /// The process-wide descriptor budget could not admit the voice socket.
    #[error("Locutus socket {}: {source}", path.display())]
    Admission {
        /// Explicit service destination.
        path: PathBuf,
        /// Resource governor's refusal, including its observed budget.
        #[source]
        source: Box<crate::resource::DescriptorAdmissionError>,
    },
    /// A socket operation failed. A submitted request's outcome may be unknown.
    #[error("Locutus socket {}: {source}; submitted playback outcome may be unknown", path.display())]
    Io {
        /// Explicit service destination.
        path: PathBuf,
        /// Original I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// The control wire violated its published contract.
    #[error("Locutus request {request}: {reason}")]
    Protocol {
        /// Client request being supervised.
        request: Uuid,
        /// Exact refusal or protocol failure.
        reason: String,
    },
}

/// Read one answer through the registered native door, supervising it until terminal.
///
/// Cancellation sends a request-tagged hush on the same ordered connection. It
/// supervises speech through its receipt or the owner-declared stop deadline,
/// and never cancels an agent/provider token.
/// No reconnect or replay is automatic. The progress channel holds one latest
/// frame so a slow renderer cannot delay a stop behind presentation updates.
///
/// # Errors
/// Returns a named transport, protocol or service refusal. EOF after submission
/// is an unknown outcome, never a claim that audio stopped.
pub async fn read_aloud(
    request: ReadAloudRequest,
    cancel: CancellationToken,
    progress: watch::Sender<ReadAloudProgress>,
) -> Result<ReadAloudOutcome, ReadAloudError> {
    if request.text.trim().is_empty() || request.seat.trim().is_empty() {
        return Err(ReadAloudError::InvalidRequest {
            request: request.id,
        });
    }
    if cancel.is_cancelled() {
        return Ok(ReadAloudOutcome::NotSubmitted);
    }
    // A Unix stream owns exactly one descriptor, also after splitting its halves.
    let permit = crate::resource::DescriptorGovernor::global()
        .and_then(|governor| governor.try_acquire(1))
        .map_err(|source| ReadAloudError::Admission {
            path: request.socket.clone(),
            source: Box::new(source),
        })?;
    let outcome = supervise_read(request, cancel, progress).await;
    drop(permit);
    outcome
}

async fn supervise_read(
    request: ReadAloudRequest,
    cancel: CancellationToken,
    progress: watch::Sender<ReadAloudProgress>,
) -> Result<ReadAloudOutcome, ReadAloudError> {
    let operation = read_admitted(request, cancel.clone(), progress.clone());
    tokio::pin!(operation);
    tokio::select! {
        biased;
        outcome = &mut operation => outcome,
        () = cancel.cancelled() => {
            let started = tokio::time::Instant::now();
            match tokio::time::timeout(STOP_RECEIPT_DEADLINE, &mut operation).await {
                Ok(outcome) => outcome,
                Err(_) => Ok(ReadAloudOutcome::StopUnconfirmed {
                    stop_sent: matches!(*progress.borrow(), ReadAloudProgress::Stopping),
                    waited: started.elapsed(),
                }),
            }
        }
    }
}

async fn read_admitted(
    request: ReadAloudRequest,
    cancel: CancellationToken,
    progress: watch::Sender<ReadAloudProgress>,
) -> Result<ReadAloudOutcome, ReadAloudError> {
    let io_error = |source| ReadAloudError::Io {
        path: request.socket.clone(),
        source,
    };
    let stream = tokio::select! {
        biased;
        () = cancel.cancelled() => return Ok(ReadAloudOutcome::NotSubmitted),
        result = UnixStream::connect(&request.socket) => result.map_err(io_error)?,
    };
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    let Some(session) = server_session(&request, &mut lines, &cancel).await? else {
        return Ok(ReadAloudOutcome::NotSubmitted);
    };
    let registration = DoorIntent::Register {
        seat: request.seat.clone(),
        account: None,
        voice: request.voice.clone(),
    };
    writer
        .write_all(line(&registration).as_bytes())
        .await
        .map_err(io_error)?;
    if cancel.is_cancelled() {
        return Ok(ReadAloudOutcome::NotSubmitted);
    }
    let tag = request.id.to_string();
    let speech = DoorIntent::Say {
        text: request.text.clone(),
        voice: request.voice.clone(),
        id: None,
        request: Some(tag.clone()),
        more: false,
    };
    writer
        .write_all(line(&speech).as_bytes())
        .await
        .map_err(io_error)?;
    progress.send_replace(ReadAloudProgress::Submitted);
    let mut stop_sent = false;
    let mut accepted = None;
    loop {
        tokio::select! {
            biased;
            () = cancel.cancelled(), if !stop_sent => {
                let stop = DoorIntent::Hush { id: None, request: Some(tag.clone()) };
                writer.write_all(line(&stop).as_bytes()).await.map_err(io_error)?;
                stop_sent = true;
                progress.send_replace(ReadAloudProgress::Stopping);
            }
            next = lines.next_line() => {
                let Some(next) = next.map_err(io_error)? else {
                    return Err(protocol(&request, "door closed before a terminal receipt; playback outcome unknown"));
                };
                match decode(&request, &next)? {
                    DoorEvent::Say { id, request: Some(ref echoed), session: ref current, .. } if echoed == &tag => {
                        same_session(&request, &session, current)?;
                        if accepted.is_some_and(|previous| previous != id) {
                            return Err(protocol(&request, "one request was assigned two speech identities"));
                        }
                        accepted = Some(id);
                        if !stop_sent { progress.send_replace(ReadAloudProgress::Accepted { id }); }
                    }
                    DoorEvent::Playing { id, session: ref current, .. } if accepted == Some(id) => {
                        same_session(&request, &session, current)?;
                        if !stop_sent { progress.send_replace(ReadAloudProgress::Playing { id }); }
                    }
                    DoorEvent::Spoken { id, ms, request: Some(ref echoed), session: ref current, .. } if echoed == &tag => {
                        same_session(&request, &session, current)?;
                        terminal_identity(&request, accepted, id)?;
                        return Ok(ReadAloudOutcome::Completed { session, id, duration_ms: ms, stop_requested: stop_sent });
                    }
                    DoorEvent::Hushed { id, at_ms, latency_ms, request: Some(ref echoed), session: ref current, .. } if echoed == &tag => {
                        same_session(&request, &session, current)?;
                        terminal_identity(&request, accepted, id)?;
                        return Ok(ReadAloudOutcome::Stopped { session, id, at_ms, latency_ms });
                    }
                    DoorEvent::Error { message, request: echoed, session: ref current, .. } if echoed.as_ref().is_some_and(|echoed| echoed == &tag) || (echoed.is_none() && accepted.is_none()) => {
                        same_session(&request, &session, current)?;
                        return Err(protocol(&request, &message));
                    }
                    _ => {}
                }
            }
        }
    }
}

fn line(intent: &DoorIntent) -> String {
    format!("{}\n", intent.encode())
}

async fn server_session(
    request: &ReadAloudRequest,
    lines: &mut tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    cancel: &CancellationToken,
) -> Result<Option<String>, ReadAloudError> {
    let first = tokio::select! {
        biased;
        () = cancel.cancelled() => return Ok(None),
        line = lines.next_line() => line.map_err(|source| ReadAloudError::Io { path: request.socket.clone(), source })?,
    };
    let Some(first) = first else {
        return Err(protocol(request, "door closed before its state"));
    };
    match decode(request, &first)? {
        DoorEvent::State { session, .. } if !session.is_empty() => Ok(Some(session)),
        _ => Err(protocol(
            request,
            "first door event must be state with a nonempty session identity",
        )),
    }
}

fn protocol(request: &ReadAloudRequest, reason: &str) -> ReadAloudError {
    ReadAloudError::Protocol {
        request: request.id,
        reason: reason.to_owned(),
    }
}

fn decode(request: &ReadAloudRequest, line: &str) -> Result<DoorEvent, ReadAloudError> {
    DoorEvent::decode(line).map_err(|error| protocol(request, &error.to_string()))
}

fn same_session(
    request: &ReadAloudRequest,
    expected: &str,
    actual: &str,
) -> Result<(), ReadAloudError> {
    if actual == expected {
        Ok(())
    } else {
        Err(protocol(request, "door session changed during playback"))
    }
}

fn terminal_identity(
    request: &ReadAloudRequest,
    accepted: Option<u64>,
    actual: u64,
) -> Result<(), ReadAloudError> {
    if accepted.is_some_and(|id| id != actual) {
        Err(protocol(
            request,
            "terminal receipt names a different speech identity",
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[path = "locutus_tests.rs"]
mod tests;
