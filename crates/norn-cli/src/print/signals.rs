//! Headless signal handling for `norn --print` and driven mode
//! (retry-forever DESIGN D5).
//!
//! Before this existed, a headless run had **zero** signal handling:
//! SIGINT was instant process death — no `Cancelled` envelope, no
//! tool-result repair, no checkpoint, orphaned tool children. That was
//! already wrong; with the loop's retry policy now unbounded by default it
//! is also the only way an operator can stop a run that is retrying a
//! transient provider failure. Ctrl-C/SIGTERM **is** the user cancel.
//!
//! The ladder is two rungs and no more:
//!
//! 1. The **first** SIGINT or SIGTERM trips the run's root cancellation
//!    token — the same token the step already runs under and the same one
//!    published to every spawned descendant as `AgentCancellation`. That
//!    hands the run to the existing graceful path: `AgentStepResult::Cancelled`,
//!    tool-result repair, session checkpoint, and the `Cancelled` envelope
//!    in whatever output format was requested.
//! 2. A **second** signal, arriving while that wind-down is still in
//!    flight, means the operator is no longer asking. The process leaves
//!    immediately with `128 + signal number` — 130 for SIGINT, 143 for
//!    SIGTERM — the shell convention for a signal-terminated process.
//!
//! Platform split: unix watches SIGINT and SIGTERM; everywhere else only
//! `ctrl_c` exists to watch.
//!
//! # Why installation failure is fatal
//!
//! [`SignalWatch::install`] returns a typed error rather than logging and
//! continuing. A headless run that cannot be signalled is a run the
//! operator cannot stop — precisely the hazard the unbounded retry policy
//! creates. Degrading silently to a no-signal run would hide that.

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// A signal the headless run reacts to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RunSignal {
    /// SIGINT on unix; `Ctrl+C` (or the console close events tokio maps
    /// onto `ctrl_c`) elsewhere.
    Interrupt,
    /// SIGTERM — the signal an orchestrator, container runtime, or
    /// `systemd` sends to ask a process to stop.
    #[cfg(unix)]
    Terminate,
}

impl RunSignal {
    /// Operator-facing name, used in the stderr notice and in install
    /// failures so the reported signal is never ambiguous.
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Interrupt => {
                if cfg!(unix) {
                    "SIGINT"
                } else {
                    "Ctrl+C"
                }
            }
            #[cfg(unix)]
            Self::Terminate => "SIGTERM",
        }
    }

    /// The process exit code for an immediate, second-signal exit.
    ///
    /// `128 + signal number` is the shell convention for a
    /// signal-terminated process. The number is taken from the platform's
    /// own [`SignalKind`](tokio::signal::unix::SignalKind) rather than
    /// written out here, so it is the operating system's value and not a
    /// transcribed one.
    #[cfg(unix)]
    pub(super) fn immediate_exit_code(self) -> i32 {
        128 + self.kind().as_raw_value()
    }

    /// The process exit code for an immediate, second-signal exit.
    ///
    /// Non-unix platforms have no signal number to add to 128; `ctrl_c` is
    /// the interrupt, and 130 (128 + SIGINT) is the pinned convention
    /// (DESIGN D5).
    #[cfg(not(unix))]
    pub(super) const fn immediate_exit_code(self) -> i32 {
        match self {
            Self::Interrupt => 130,
        }
    }

    /// The tokio signal kind this variant listens on.
    #[cfg(unix)]
    fn kind(self) -> tokio::signal::unix::SignalKind {
        match self {
            Self::Interrupt => tokio::signal::unix::SignalKind::interrupt(),
            Self::Terminate => tokio::signal::unix::SignalKind::terminate(),
        }
    }
}

/// What the run must do in response to an arriving signal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SignalAction {
    /// Trip the run's cancellation token and let the graceful path run:
    /// `Cancelled` result, tool-result repair, checkpoint, envelope.
    CancelRun,
    /// The graceful path already had its chance. Leave now with this exit
    /// code (`128 + signal number`).
    ExitImmediately(i32),
}

/// The signal escalation ladder: cancel once, then leave.
///
/// Split out from the watcher task so the rung ordering is testable
/// without raising real signals at a test process.
#[derive(Debug, Default)]
pub(super) struct SignalEscalation {
    /// Whether a signal has already tripped the run's cancel token.
    cancel_requested: bool,
}

impl SignalEscalation {
    /// Record an arriving signal and decide what it means.
    pub(super) fn observe(&mut self, signal: RunSignal) -> SignalAction {
        if self.cancel_requested {
            SignalAction::ExitImmediately(signal.immediate_exit_code())
        } else {
            self.cancel_requested = true;
            SignalAction::CancelRun
        }
    }
}

/// The signal handler could not be installed, so the run would not be
/// cancellable. Fatal by design — see the module docs.
#[derive(Debug, thiserror::Error)]
#[error(
    "could not install the {signal} handler; a run that cannot be cancelled must not start: {source}"
)]
pub(super) struct SignalInstallError {
    /// The signal whose registration failed.
    signal: &'static str,
    /// The operating system's refusal.
    #[source]
    source: std::io::Error,
}

/// A live signal watcher, scoped to one run.
///
/// Dropping it aborts the watcher task, so the task can never outlive the
/// run it was installed for. Hold it across the WHOLE run — the step, the
/// checkpoint, the output write — not just the step: tokio's signal
/// registration is process-global and is never removed, so after the
/// watcher stops, a signal is delivered to a handler nobody is listening
/// behind. Keeping the watcher alive for the run's full length keeps the
/// window in which a signal has no visible effect down to the process's
/// own return path.
pub(super) struct SignalWatch {
    task: JoinHandle<()>,
}

impl SignalWatch {
    /// Install the handlers and start watching, tripping `cancel` on the
    /// first signal.
    ///
    /// Registration happens synchronously, before the watcher task is
    /// spawned, so a signal delivered from this moment on is captured even
    /// if the task has not been polled yet.
    ///
    /// # Panics
    ///
    /// Tokio's signal registration requires a runtime built with the
    /// signal driver enabled (`enable_all` / `enable_io`) and panics
    /// without one. Every runtime that reaches this — the binary's, and
    /// any embedder calling `print::run_async` — must be built that way;
    /// it is a runtime-construction contract, not a runtime condition this
    /// function can report on.
    ///
    /// # Errors
    ///
    /// [`SignalInstallError`] when the operating system refuses to
    /// register a handler. The caller must fail the run: see the module
    /// docs.
    #[cfg(unix)]
    pub(super) fn install(cancel: CancellationToken) -> Result<Self, SignalInstallError> {
        let mut interrupt = stream_for(RunSignal::Interrupt)?;
        let mut terminate = stream_for(RunSignal::Terminate)?;
        let task = tokio::spawn(async move {
            let mut escalation = SignalEscalation::default();
            loop {
                let received = tokio::select! {
                    signal = interrupt.recv() => match signal {
                        Some(()) => RunSignal::Interrupt,
                        None => break report_closed(RunSignal::Interrupt),
                    },
                    signal = terminate.recv() => match signal {
                        Some(()) => RunSignal::Terminate,
                        None => break report_closed(RunSignal::Terminate),
                    },
                };
                apply(escalation.observe(received), received, &cancel);
            }
        });
        Ok(Self { task })
    }

    /// Install the Ctrl+C handler and start watching, tripping `cancel`
    /// on the first one.
    ///
    /// Windows has no SIGTERM equivalent to watch, so `ctrl_c` is the
    /// whole surface (DESIGN D5). Registration is synchronous here for the
    /// same reason it is on unix.
    ///
    /// # Errors
    ///
    /// [`SignalInstallError`] when the platform refuses to register the
    /// handler.
    #[cfg(windows)]
    pub(super) fn install(cancel: CancellationToken) -> Result<Self, SignalInstallError> {
        let mut interrupt =
            tokio::signal::windows::ctrl_c().map_err(|source| SignalInstallError {
                signal: RunSignal::Interrupt.label(),
                source,
            })?;
        let task = tokio::spawn(async move {
            let mut escalation = SignalEscalation::default();
            while interrupt.recv().await.is_some() {
                apply(
                    escalation.observe(RunSignal::Interrupt),
                    RunSignal::Interrupt,
                    &cancel,
                );
            }
            report_closed(RunSignal::Interrupt);
        });
        Ok(Self { task })
    }

    /// Refuse to start on a platform that exposes no interrupt to watch.
    ///
    /// A headless run that cannot be signalled cannot be stopped, and the
    /// retry policy is unbounded — so this fails loudly instead of
    /// silently running uninterruptibly.
    ///
    /// # Errors
    ///
    /// Always: the platform has no supported signal surface.
    #[cfg(all(not(unix), not(windows)))]
    pub(super) fn install(_cancel: CancellationToken) -> Result<Self, SignalInstallError> {
        Err(SignalInstallError {
            signal: RunSignal::Interrupt.label(),
            source: std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "this platform exposes no interrupt signal for a headless run to watch",
            ),
        })
    }
}

impl Drop for SignalWatch {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Register one unix signal stream, naming the signal on failure.
#[cfg(unix)]
fn stream_for(signal: RunSignal) -> Result<tokio::signal::unix::Signal, SignalInstallError> {
    tokio::signal::unix::signal(signal.kind()).map_err(|source| SignalInstallError {
        signal: signal.label(),
        source,
    })
}

/// A signal stream ended, which only happens if its registration was torn
/// down underneath us. The run keeps going — it is mid-flight and killing
/// it here would be worse — but the operator is told, loudly, that this
/// signal no longer stops it.
#[cfg(any(unix, windows))]
fn report_closed(signal: RunSignal) {
    tracing::error!(
        signal = signal.label(),
        "the signal stream closed; this run can no longer be stopped by that signal",
    );
}

/// Carry out an escalation decision.
fn apply(action: SignalAction, signal: RunSignal, cancel: &CancellationToken) {
    match action {
        SignalAction::CancelRun => {
            // stderr, never stdout: the machine formats own stdout (D9).
            eprintln!(
                "norn: {} received; cancelling the run — signal again to exit immediately",
                signal.label(),
            );
            cancel.cancel();
        }
        SignalAction::ExitImmediately(code) => {
            eprintln!(
                "norn: {} received again while cancelling; exiting immediately",
                signal.label(),
            );
            std::process::exit(code);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// Rung 1: the first signal asks for the graceful path, never an
    /// immediate exit.
    #[test]
    fn the_first_signal_asks_for_a_graceful_cancel() {
        let mut escalation = SignalEscalation::default();
        assert_eq!(
            escalation.observe(RunSignal::Interrupt),
            SignalAction::CancelRun,
        );
    }

    /// Rung 2: a signal arriving while the first cancel is still winding
    /// down exits with `128 + signal number` — 130 for SIGINT.
    #[test]
    fn a_second_interrupt_exits_with_130() {
        let mut escalation = SignalEscalation::default();
        assert_eq!(
            escalation.observe(RunSignal::Interrupt),
            SignalAction::CancelRun,
        );
        assert_eq!(
            escalation.observe(RunSignal::Interrupt),
            SignalAction::ExitImmediately(130),
        );
    }

    /// The exit code follows the signal actually received, not the one
    /// that started the wind-down: SIGINT then SIGTERM exits 143.
    #[cfg(unix)]
    #[test]
    fn a_second_terminate_exits_with_143() {
        let mut escalation = SignalEscalation::default();
        assert_eq!(
            escalation.observe(RunSignal::Interrupt),
            SignalAction::CancelRun,
        );
        assert_eq!(
            escalation.observe(RunSignal::Terminate),
            SignalAction::ExitImmediately(143),
        );
    }

    /// SIGTERM is a first-class first rung: an orchestrator's stop request
    /// gets the same graceful `Cancelled` path as an operator's Ctrl-C.
    #[cfg(unix)]
    #[test]
    fn a_first_terminate_also_cancels_gracefully() {
        let mut escalation = SignalEscalation::default();
        assert_eq!(
            escalation.observe(RunSignal::Terminate),
            SignalAction::CancelRun,
        );
    }

    /// The ladder has no third rung: every signal after the first keeps
    /// meaning "leave now", so a wedged wind-down can always be escaped.
    #[cfg(unix)]
    #[test]
    fn every_further_signal_keeps_exiting() {
        let mut escalation = SignalEscalation::default();
        let _ = escalation.observe(RunSignal::Interrupt);
        for _ in 0..3 {
            assert_eq!(
                escalation.observe(RunSignal::Interrupt),
                SignalAction::ExitImmediately(130),
            );
        }
    }

    /// The exit codes are the operating system's signal numbers plus 128,
    /// not transcribed constants.
    #[cfg(unix)]
    #[test]
    fn exit_codes_are_128_plus_the_platform_signal_number() {
        use tokio::signal::unix::SignalKind;
        assert_eq!(
            RunSignal::Interrupt.immediate_exit_code(),
            128 + SignalKind::interrupt().as_raw_value(),
        );
        assert_eq!(
            RunSignal::Terminate.immediate_exit_code(),
            128 + SignalKind::terminate().as_raw_value(),
        );
    }

    /// End-to-end at the task level: a REAL signal delivered to this
    /// process trips the run's cancellation token. Pinned in-process
    /// because the escalation ladder above proves only the decisions, not
    /// that the handler is wired to the token the step runs under.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_real_signal_trips_the_run_cancel_token() -> Result<(), Box<dyn std::error::Error>> {
        let cancel = CancellationToken::new();
        // Held for the whole test: dropping the watch aborts the task, and
        // tokio's process-global registration would then swallow the
        // signal instead of letting it kill the test binary.
        let _watch = SignalWatch::install(cancel.clone())?;

        let status = std::process::Command::new("kill")
            .arg("-TERM")
            .arg(std::process::id().to_string())
            .status()?;
        assert!(status.success(), "signalling this test process failed");

        tokio::time::timeout(std::time::Duration::from_secs(30), cancel.cancelled()).await?;
        Ok(())
    }
}
