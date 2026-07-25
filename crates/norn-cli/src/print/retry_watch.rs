//! The human retry surfaces for headless runs (retry-forever DESIGN D8,
//! commit C6).
//!
//! The engine announces every inter-attempt wait as a typed
//! [`AgentStreamRetry`] marker on the agent event channel, emitted BEFORE
//! the wait it describes. `-f stream-json` and driven mode already carry
//! that marker on their own wires; the human formats carried nothing, so
//! a run retrying a transient provider failure — indefinitely, by default
//! — was indistinguishable from a hang.
//!
//! This module is the subscriber that fixes that for the two formats with
//! no event wire of their own:
//!
//! - **`-f text`**: one concise line per notice on **stderr**. Never
//!   stdout: stdout carries the model's final output and nothing else, in
//!   every mode (D9). Suppressed by `--quiet`, which is documented as
//!   suppressing progress on stderr, and a retry wait is progress.
//! - **`-f json`**: nothing live (the envelope is written once, at the
//!   end), but the activity is accumulated into a [`RetryRollup`] the
//!   orchestrator folds into the envelope's `diagnostics` array — attempt
//!   count, total backoff waited, and the classes seen. DESIGN D8 pins
//!   this as a diagnostics rollup precisely so the envelope schema does
//!   not change in v1.
//!
//! `error_class` is the engine's taxonomy label
//! ([`AgentStreamRetry::error_class`]) and is rendered verbatim on both
//! surfaces. Provider free text never reaches them: the marker does not
//! carry any, and reasons belong to the loud terminal error.
//!
//! The task is shut down explicitly, exactly like the stream renderer:
//! the registry's shared `ToolContext` holds a `Sender` clone for
//! subagent event forwarding, so waiting for channel closure alone would
//! hang forever.

use std::io::Write;

use norn::integration::{DiagnosticSeverity, NornDiagnostic};
use norn::provider::AgentEvent;
use norn::provider::agent_event::{AgentEventKind, AgentStreamRetry};
use tokio::sync::broadcast::error::{RecvError, TryRecvError};

use super::error::PrintError;
use crate::cli::{Cli, OutputFormat};

/// Machine-stable diagnostic code of the retry rollup. Consumers branch
/// on this, never on the message wording.
pub(super) const RETRY_ROLLUP_CODE: &str = "provider-retry";

/// Render a backoff in the shortest form that stays exact: whole
/// milliseconds below a second, then seconds with at most one decimal.
fn format_backoff(ms: u64) -> String {
    if ms < 1_000 {
        return format!("{ms}ms");
    }
    let whole = ms / 1_000;
    let tenth = (ms % 1_000) / 100;
    if tenth == 0 {
        format!("{whole}s")
    } else {
        format!("{whole}.{tenth}s")
    }
}

/// The attempt-budget phrase. An unbounded policy — the default — says so
/// in words; it is never rendered as a sentinel number.
fn budget_phrase(attempt: u32, max_attempts: Option<u32>) -> String {
    match max_attempts {
        Some(max) => format!("attempt {attempt} of {max}"),
        None => format!("attempt {attempt}, unbounded"),
    }
}

/// The one stderr line a retry notice produces in text mode.
///
/// Carries the `norn: ` operator prefix every other headless operator
/// line uses, so it is distinguishable from the engine's `tracing`
/// output on the same stream.
#[must_use]
pub(super) fn retry_notice_line(retry: &AgentStreamRetry) -> String {
    format!(
        "norn: provider call failed ({class}); retrying in {wait} ({budget})",
        class = retry.error_class,
        wait = format_backoff(retry.delay_ms),
        budget = budget_phrase(retry.attempt, retry.max_attempts),
    )
}

/// What a run's retry activity added up to.
///
/// Accumulated live by the watch task and reported once, at the end, into
/// the `-f json` envelope's diagnostics array.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct RetryRollup {
    /// Number of retry notices observed — one per inter-attempt wait, so
    /// also the number of replayed provider attempts.
    notices: u64,
    /// Sum of the announced waits, in milliseconds.
    total_backoff_ms: u64,
    /// Taxonomy classes seen, in first-seen order, without duplicates.
    error_classes: Vec<String>,
    /// Events the broadcast receiver missed because it lagged. Non-zero
    /// makes every count above a LOWER BOUND, and the rollup says so
    /// rather than reporting a short count as if it were exact.
    missed_events: u64,
}

impl RetryRollup {
    /// Fold one observed notice in.
    fn observe(&mut self, retry: &AgentStreamRetry) {
        self.notices = self.notices.saturating_add(1);
        self.total_backoff_ms = self.total_backoff_ms.saturating_add(retry.delay_ms);
        if !self
            .error_classes
            .iter()
            .any(|class| class == &retry.error_class)
        {
            self.error_classes.push(retry.error_class.clone());
        }
    }

    /// Record that the receiver lagged and lost `missed` events.
    fn observe_lag(&mut self, missed: u64) {
        self.missed_events = self.missed_events.saturating_add(missed);
    }

    /// Whether this run retried at all.
    #[must_use]
    pub(super) const fn is_empty(&self) -> bool {
        self.notices == 0
    }

    /// The envelope diagnostic for this rollup, or [`None`] when nothing
    /// was retried — the envelope reports what happened, not what did
    /// not.
    ///
    /// Severity is `Info`: every notice here describes a failure the loop
    /// RECOVERED from by replaying the request. A failure that ended the
    /// retrying is reported loudly on its own path (the typed stop /
    /// error envelope), never softened into this rollup.
    #[must_use]
    pub(super) fn diagnostic(&self) -> Option<NornDiagnostic> {
        if self.is_empty() {
            return None;
        }
        let plural = if self.notices == 1 { "time" } else { "times" };
        let classes = if self.error_classes.is_empty() {
            "none recorded".to_owned()
        } else {
            self.error_classes.join(", ")
        };
        let lower_bound = if self.missed_events == 0 {
            String::new()
        } else {
            format!(
                "; lower bound only — {missed} live events were missed by a lagging consumer",
                missed = self.missed_events,
            )
        };
        let message = format!(
            "provider call retried {notices} {plural} (total backoff {backoff}; error classes: \
             {classes}){lower_bound}",
            notices = self.notices,
            backoff = format_backoff(self.total_backoff_ms),
        );
        Some(NornDiagnostic {
            severity: DiagnosticSeverity::Info,
            code: RETRY_ROLLUP_CODE.to_owned(),
            message,
            source_tool: None,
            file_path: None,
            suggestion: None,
        })
    }
}

/// Handle to the background retry watch spawned by
/// [`spawn_retry_watch`].
///
/// Like the stream renderer, the task cannot rely on channel closure to
/// stop — the registry's shared `ToolContext` keeps a `Sender` clone
/// alive for the runtime's lifetime — so [`Self::finish`] signals it
/// explicitly and it drains whatever is already buffered before exiting.
pub(super) struct RetryWatchHandle {
    /// Explicit shutdown trigger consumed by [`Self::finish`].
    shutdown: tokio::sync::oneshot::Sender<()>,
    /// The watch task, carrying the accumulated rollup.
    task: tokio::task::JoinHandle<RetryRollup>,
}

impl RetryWatchHandle {
    /// Drain and stop the watch, returning what it accumulated.
    ///
    /// Call only after the step's own senders have been dropped.
    ///
    /// # Errors
    ///
    /// [`PrintError::Agent`] when the task panicked or was cancelled. The
    /// rollup is then unknowable, and a visibility surface that silently
    /// reported "no retries" for a run that retried would be worse than
    /// no surface at all — so the failure is surfaced to the caller
    /// rather than swallowed.
    pub(super) async fn finish(self) -> Result<RetryRollup, PrintError> {
        // A failed send means the task already exited on its own; the
        // join below still completes.
        let _ = self.shutdown.send(());
        self.task.await.map_err(|err| {
            PrintError::Agent(format!(
                "retry visibility task failed ({kind}): {err}; the run's retry activity could \
                 not be reported",
                kind = if err.is_panic() { "panic" } else { "cancelled" },
            ))
        })
    }
}

/// Where the per-notice retry lines go for this invocation.
///
/// Text mode writes them to **stderr** — stdout carries the model's final
/// output and nothing else (D9), in the human format as much as in the
/// machine ones. `--quiet` is documented as suppressing progress on
/// stderr and a retry wait is progress, so it silences them. `-f json`
/// gets [`None`]: its surface is the end-of-run rollup in the envelope,
/// not a live line.
#[must_use]
pub(super) fn retry_line_writer(cli: &Cli, format: OutputFormat) -> Option<Box<dyn Write + Send>> {
    match format {
        OutputFormat::Text if !cli.quiet => Some(Box::new(std::io::stderr())),
        OutputFormat::Text | OutputFormat::Json | OutputFormat::StreamJson => None,
    }
}

/// Watch this invocation's retries, or [`None`] for the surfaces that
/// already carry the typed marker on their own wire (`-f stream-json`,
/// driven mode) and therefore need no subscriber.
#[must_use]
pub(super) fn watch_for_invocation(
    tx: &tokio::sync::broadcast::Sender<AgentEvent>,
    cli: &Cli,
    format: OutputFormat,
    is_driven: bool,
) -> Option<RetryWatchHandle> {
    (!is_driven && matches!(format, OutputFormat::Text | OutputFormat::Json))
        .then(|| spawn_retry_watch(tx, retry_line_writer(cli, format)))
}

/// Stop `watch` and split its outcome into the rollup to report and the
/// failure to fold into the run's background-failure chain.
///
/// A watch that panicked cannot report the run's retry activity, and
/// silently rendering "no retries" for a run that retried would be worse
/// than no surface at all — so the failure travels with the run instead
/// of being dropped here.
pub(super) async fn finish_watch(
    watch: Option<RetryWatchHandle>,
) -> (RetryRollup, Option<PrintError>) {
    match watch {
        Some(handle) => match handle.finish().await {
            Ok(rollup) => (rollup, None),
            Err(error) => (RetryRollup::default(), Some(error)),
        },
        None => (RetryRollup::default(), None),
    }
}

/// Append the end-of-run rollup to the envelope diagnostics, for the one
/// format whose surface it is.
///
/// `-f json` writes its envelope once, at the end, so the run's retry
/// activity rides the diagnostics array — the shape DESIGN D8 pins
/// precisely so the envelope schema stays at v1. Every other format
/// already reported the retries as they happened.
pub(super) fn push_rollup_for_format(
    diagnostics: &mut Vec<NornDiagnostic>,
    format: OutputFormat,
    rollup: &RetryRollup,
) {
    if matches!(format, OutputFormat::Json)
        && let Some(diagnostic) = rollup.diagnostic()
    {
        diagnostics.push(diagnostic);
    }
}

/// Subscribe to the agent event channel and surface every retry notice.
///
/// `report` is the destination of the per-notice human lines — process
/// stderr in text mode, [`None`] when the format has no live human line
/// to write (`-f json`, which reports the rollup at the end) or when
/// `--quiet` asked for silence. The rollup is accumulated either way.
#[must_use]
pub(super) fn spawn_retry_watch(
    tx: &tokio::sync::broadcast::Sender<AgentEvent>,
    report: Option<Box<dyn Write + Send>>,
) -> RetryWatchHandle {
    let rx = tx.subscribe();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(watch_retries(rx, shutdown_rx, report));
    RetryWatchHandle {
        shutdown: shutdown_tx,
        task,
    }
}

/// The watch task body: report and accumulate every retry notice until
/// the channel closes or the shutdown signal fires.
async fn watch_retries(
    mut rx: tokio::sync::broadcast::Receiver<AgentEvent>,
    mut shutdown_rx: tokio::sync::oneshot::Receiver<()>,
    report: Option<Box<dyn Write + Send>>,
) -> RetryRollup {
    let mut reporter = LineReporter::new(report);
    let mut rollup = RetryRollup::default();
    loop {
        tokio::select! {
            // Biased: drain what is already ready before observing
            // shutdown, so a notice broadcast just ahead of the signal
            // still reaches the operator.
            biased;
            received = rx.recv() => match received {
                Ok(event) => observe(&mut rollup, &mut reporter, &event),
                Err(RecvError::Closed) => return rollup,
                Err(RecvError::Lagged(missed)) => {
                    tracing::warn!(
                        missed,
                        "retry visibility lagged — {missed} events dropped; the retry rollup is \
                         a lower bound",
                    );
                    rollup.observe_lag(missed);
                }
            },
            // Resolves on the explicit signal AND when the handle is
            // dropped without finishing — the watch must never outlive
            // its run.
            _ = &mut shutdown_rx => {
                drain_buffered(&mut rollup, &mut reporter, &mut rx);
                return rollup;
            }
        }
    }
}

/// Drain the events already buffered after the shutdown signal.
/// `try_recv` never blocks, so this terminates even while the shared
/// context's sender clone keeps the channel open.
fn drain_buffered(
    rollup: &mut RetryRollup,
    reporter: &mut LineReporter,
    rx: &mut tokio::sync::broadcast::Receiver<AgentEvent>,
) {
    loop {
        match rx.try_recv() {
            Ok(event) => observe(rollup, reporter, &event),
            Err(TryRecvError::Empty | TryRecvError::Closed) => return,
            Err(TryRecvError::Lagged(missed)) => {
                tracing::warn!(
                    missed,
                    "retry visibility lagged while draining — {missed} events dropped; the \
                     retry rollup is a lower bound",
                );
                rollup.observe_lag(missed);
            }
        }
    }
}

/// Route one event: retry markers are reported and counted, everything
/// else is another surface's business.
fn observe(rollup: &mut RetryRollup, reporter: &mut LineReporter, event: &AgentEvent) {
    if let AgentEventKind::StreamRetry(retry) = &event.event {
        rollup.observe(retry);
        reporter.report(retry);
    }
}

/// The per-notice line writer.
///
/// A stderr write failure is real but not fatal: stdout — the surface
/// with the contract — is untouched, and failing a run because its
/// progress line could not be printed would be a worse outcome than
/// losing the line. It is reported once through `tracing` (never
/// swallowed) and the reporter then stops writing, so a broken stream
/// cannot produce one warning per attempt for the rest of an unbounded
/// retry streak.
struct LineReporter {
    writer: Option<Box<dyn Write + Send>>,
}

impl LineReporter {
    const fn new(writer: Option<Box<dyn Write + Send>>) -> Self {
        Self { writer }
    }

    fn report(&mut self, retry: &AgentStreamRetry) {
        let Some(writer) = self.writer.as_mut() else {
            return;
        };
        let line = retry_notice_line(retry);
        let written = writeln!(writer, "{line}").and_then(|()| writer.flush());
        if let Err(err) = written {
            tracing::warn!(
                error = %err,
                "could not write the retry progress line to stderr; further retry lines for \
                 this run are suppressed",
            );
            self.writer = None;
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// A capturing stand-in for process stderr.
    #[derive(Clone)]
    struct SharedBuffer(Arc<Mutex<Vec<u8>>>);

    impl SharedBuffer {
        fn new() -> Self {
            Self(Arc::new(Mutex::new(Vec::new())))
        }

        fn text(&self) -> String {
            String::from_utf8(self.0.lock().expect("buffer lock").clone()).expect("utf-8")
        }
    }

    impl Write for SharedBuffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("buffer lock").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A writer that fails every write, standing in for a closed stderr.
    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "simulated stderr failure",
            ))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn retry(attempt: u32, max_attempts: Option<u32>, delay_ms: u64, class: &str) -> AgentEvent {
        AgentEvent {
            agent_id: uuid::Uuid::nil(),
            agent_role: Arc::from("root"),
            event: AgentEventKind::StreamRetry(AgentStreamRetry {
                attempt,
                max_attempts,
                delay_ms,
                error_class: class.to_owned(),
            }),
        }
    }

    fn text_event() -> AgentEvent {
        AgentEvent {
            agent_id: uuid::Uuid::nil(),
            agent_role: Arc::from("root"),
            event: AgentEventKind::Provider(norn::provider::events::ProviderEvent::TextComplete {
                text: "hello".to_owned(),
            }),
        }
    }

    fn cli_from(args: &[&str]) -> Cli {
        use clap::Parser as _;
        let mut full = vec!["norn"];
        full.extend_from_slice(args);
        Cli::try_parse_from(full).unwrap()
    }

    /// D8 / C6: the live retry line is a text-mode stderr surface. `-q`
    /// silences it (it is progress), and the machine formats never get
    /// it — json reports the end-of-run rollup instead, stream-json
    /// already carries the typed event.
    #[test]
    fn retry_lines_are_text_mode_stderr_and_quiet_silences_them() {
        let cli = cli_from(&["-p", "hi"]);
        assert!(
            retry_line_writer(&cli, OutputFormat::Text).is_some(),
            "text mode reports each wait as it happens"
        );
        assert!(retry_line_writer(&cli, OutputFormat::Json).is_none());
        assert!(retry_line_writer(&cli, OutputFormat::StreamJson).is_none());

        let quiet = cli_from(&["-p", "-q", "hi"]);
        assert!(
            retry_line_writer(&quiet, OutputFormat::Text).is_none(),
            "--quiet suppresses progress on stderr"
        );
    }

    /// The watch runs for the two formats with no event wire of their
    /// own, and for nothing else: stream-json carries the typed marker,
    /// and driven mode emits it as an `event/progress` notification.
    #[tokio::test]
    async fn the_watch_runs_only_for_the_formats_that_need_it() {
        let cli = cli_from(&["-p", "hi"]);
        let (tx, _rx) = tokio::sync::broadcast::channel::<AgentEvent>(4);
        for format in [OutputFormat::Text, OutputFormat::Json] {
            let watch = watch_for_invocation(&tx, &cli, format, false);
            assert!(watch.is_some(), "{format:?} has no other retry surface");
            let (rollup, error) = finish_watch(watch).await;
            assert!(error.is_none(), "{format:?}: {error:?}");
            assert!(rollup.is_empty(), "{format:?}: nothing was retried");
        }
        assert!(
            watch_for_invocation(&tx, &cli, OutputFormat::StreamJson, false).is_none(),
            "stream-json already carries the typed stream_retry event"
        );
        assert!(
            watch_for_invocation(&tx, &cli, OutputFormat::Json, true).is_none(),
            "driven mode emits the marker as an event/progress notification"
        );
    }

    /// The rollup rides the `-f json` envelope and no other format's.
    #[test]
    fn the_rollup_is_pushed_for_json_only() {
        let mut rollup = RetryRollup::default();
        rollup.observe(&AgentStreamRetry {
            attempt: 2,
            max_attempts: None,
            delay_ms: 1_000,
            error_class: "timeout".to_owned(),
        });

        let mut json = Vec::new();
        push_rollup_for_format(&mut json, OutputFormat::Json, &rollup);
        assert_eq!(json.len(), 1);
        assert_eq!(json[0].code, RETRY_ROLLUP_CODE);

        for format in [OutputFormat::Text, OutputFormat::StreamJson] {
            let mut other = Vec::new();
            push_rollup_for_format(&mut other, format, &rollup);
            assert!(
                other.is_empty(),
                "{format:?} reports retries as they happen"
            );
        }

        let mut clean = Vec::new();
        push_rollup_for_format(&mut clean, OutputFormat::Json, &RetryRollup::default());
        assert!(clean.is_empty(), "a run with no retries adds nothing");
    }

    /// A finished watch with no handle is the no-retry, no-failure case —
    /// the formats that never spawned one still get a usable rollup.
    #[tokio::test]
    async fn finishing_an_absent_watch_yields_an_empty_rollup() {
        let (rollup, error) = finish_watch(None).await;
        assert!(rollup.is_empty());
        assert!(error.is_none());
    }

    #[test]
    fn the_notice_line_names_class_wait_and_bounded_budget() {
        let AgentEventKind::StreamRetry(marker) = retry(2, Some(3), 200, "server_error").event
        else {
            panic!("constructed a retry event");
        };
        assert_eq!(
            retry_notice_line(&marker),
            "norn: provider call failed (server_error); retrying in 200ms (attempt 2 of 3)",
        );
    }

    #[test]
    fn the_notice_line_spells_out_an_unbounded_budget() {
        let AgentEventKind::StreamRetry(marker) = retry(9, None, 60_000, "rate_limited").event
        else {
            panic!("constructed a retry event");
        };
        let line = retry_notice_line(&marker);
        assert_eq!(
            line,
            "norn: provider call failed (rate_limited); retrying in 60s (attempt 9, unbounded)",
        );
        assert!(
            !line.contains("of 0") && !line.contains("4294967295"),
            "an unbounded budget is never a sentinel number: {line}",
        );
    }

    /// Backoffs are reported exactly: sub-second waits keep their
    /// milliseconds, longer ones read in seconds.
    #[test]
    fn backoffs_are_reported_without_rounding_to_zero() {
        assert_eq!(format_backoff(0), "0ms");
        assert_eq!(format_backoff(1), "1ms");
        assert_eq!(format_backoff(999), "999ms");
        assert_eq!(format_backoff(1_000), "1s");
        assert_eq!(format_backoff(1_500), "1.5s");
        assert_eq!(format_backoff(60_000), "60s");
    }

    /// Text mode: one line per notice on the reporting writer, and the
    /// rollup counts every one of them.
    #[tokio::test]
    async fn every_notice_produces_one_line_and_one_rollup_entry() {
        let buffer = SharedBuffer::new();
        let (tx, _rx) = tokio::sync::broadcast::channel::<AgentEvent>(16);
        let handle = spawn_retry_watch(&tx, Some(Box::new(buffer.clone())));

        tx.send(retry(2, Some(3), 200, "server_error")).unwrap();
        tx.send(text_event()).unwrap();
        tx.send(retry(3, Some(3), 400, "timeout")).unwrap();

        let rollup = handle.finish().await.expect("clean shutdown");
        let written = buffer.text();
        let lines: Vec<&str> = written.lines().collect();
        assert_eq!(lines.len(), 2, "one line per notice: {lines:?}");
        assert!(lines[0].contains("attempt 2 of 3"), "{lines:?}");
        assert!(lines[1].contains("timeout"), "{lines:?}");

        let diagnostic = rollup.diagnostic().expect("a retried run has a rollup");
        assert_eq!(diagnostic.code, RETRY_ROLLUP_CODE);
        assert_eq!(diagnostic.severity, DiagnosticSeverity::Info);
        assert_eq!(
            diagnostic.message,
            "provider call retried 2 times (total backoff 600ms; error classes: server_error, \
             timeout)",
        );
    }

    /// `-f json` (and `--quiet`) pass no writer: nothing is printed, but
    /// the rollup is still accumulated for the envelope.
    #[tokio::test]
    async fn without_a_reporter_the_rollup_is_still_accumulated() {
        let (tx, _rx) = tokio::sync::broadcast::channel::<AgentEvent>(16);
        let handle = spawn_retry_watch(&tx, None);
        tx.send(retry(2, None, 1_000, "connection_reset")).unwrap();

        let rollup = handle.finish().await.expect("clean shutdown");
        assert!(!rollup.is_empty());
        let diagnostic = rollup.diagnostic().expect("a rollup");
        assert!(
            diagnostic.message.contains("connection_reset"),
            "{}",
            diagnostic.message
        );
    }

    /// A run that never retried adds nothing to the envelope.
    #[tokio::test]
    async fn a_clean_run_produces_no_rollup() {
        let (tx, _rx) = tokio::sync::broadcast::channel::<AgentEvent>(16);
        let handle = spawn_retry_watch(&tx, None);
        tx.send(text_event()).unwrap();

        let rollup = handle.finish().await.expect("clean shutdown");
        assert!(rollup.is_empty());
        assert!(rollup.diagnostic().is_none());
    }

    /// Duplicate classes are recorded once, in first-seen order, and the
    /// backoff total is exact.
    #[test]
    fn the_rollup_dedupes_classes_and_totals_the_backoff() {
        let mut rollup = RetryRollup::default();
        for (attempt, delay, class) in [
            (2, 1_000, "server_error"),
            (3, 2_000, "server_error"),
            (4, 4_000, "timeout"),
        ] {
            rollup.observe(&AgentStreamRetry {
                attempt,
                max_attempts: None,
                delay_ms: delay,
                error_class: class.to_owned(),
            });
        }
        assert_eq!(rollup.error_classes, ["server_error", "timeout"]);
        assert_eq!(rollup.total_backoff_ms, 7_000);
        assert_eq!(rollup.notices, 3);
        assert_eq!(
            rollup.diagnostic().unwrap().message,
            "provider call retried 3 times (total backoff 7s; error classes: server_error, \
             timeout)",
        );
    }

    /// A lagging receiver makes the counts a lower bound, and the rollup
    /// says so instead of presenting a short count as exact.
    #[test]
    fn a_lagging_receiver_marks_the_rollup_as_a_lower_bound() {
        let mut rollup = RetryRollup::default();
        rollup.observe(&AgentStreamRetry {
            attempt: 2,
            max_attempts: None,
            delay_ms: 500,
            error_class: "timeout".to_owned(),
        });
        rollup.observe_lag(4);
        let message = rollup.diagnostic().unwrap().message;
        assert!(message.contains("lower bound"), "{message}");
        assert!(message.contains('4'), "{message}");
    }

    /// A stderr that cannot be written to costs the LINES, never the run:
    /// the rollup is unaffected and the watch keeps going.
    #[tokio::test]
    async fn a_failing_stderr_does_not_stop_the_watch_or_the_rollup() {
        let (tx, _rx) = tokio::sync::broadcast::channel::<AgentEvent>(16);
        let handle = spawn_retry_watch(&tx, Some(Box::new(FailingWriter)));
        tx.send(retry(2, None, 100, "timeout")).unwrap();
        tx.send(retry(3, None, 200, "timeout")).unwrap();

        let rollup = handle
            .finish()
            .await
            .expect("a broken stderr is not a run failure");
        assert_eq!(rollup.notices, 2);
        assert_eq!(rollup.total_backoff_ms, 300);
    }

    /// The watch must terminate on the explicit signal even while an
    /// outstanding sender clone (the registry's shared context, in
    /// production) keeps the channel open.
    #[tokio::test]
    async fn the_watch_finishes_despite_an_outstanding_sender_clone() {
        let (tx, _rx) = tokio::sync::broadcast::channel::<AgentEvent>(16);
        let registry_clone = tx.clone();
        let handle = spawn_retry_watch(&tx, None);
        drop(tx);

        tokio::time::timeout(std::time::Duration::from_secs(10), handle.finish())
            .await
            .expect("the watch must exit via the explicit shutdown")
            .expect("the watch must not fail");
        drop(registry_clone);
    }

    /// A notice broadcast just before shutdown is still drained and
    /// reported — the biased select plus the post-signal drain.
    #[tokio::test]
    async fn notices_broadcast_before_shutdown_are_drained() {
        let buffer = SharedBuffer::new();
        let (tx, _rx) = tokio::sync::broadcast::channel::<AgentEvent>(16);
        let handle = spawn_retry_watch(&tx, Some(Box::new(buffer.clone())));
        tx.send(retry(2, None, 100, "timeout")).unwrap();
        let rollup = handle.finish().await.expect("clean shutdown");

        assert_eq!(rollup.notices, 1);
        assert_eq!(buffer.text().lines().count(), 1, "{:?}", buffer.text());
    }

    /// A panicked watch is surfaced, never reported as "no retries".
    #[tokio::test]
    async fn a_panicked_watch_is_surfaced() {
        let (shutdown, shutdown_receiver) = tokio::sync::oneshot::channel::<()>();
        drop(shutdown_receiver);
        let task = tokio::spawn(async { panic!("simulated watch panic") });
        let handle = RetryWatchHandle { shutdown, task };
        let error = handle.finish().await.expect_err("a panic must surface");
        assert!(matches!(error, PrintError::Agent(_)), "{error:?}");
        assert!(error.to_string().contains("retry visibility"), "{error}");
    }
}
