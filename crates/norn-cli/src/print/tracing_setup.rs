//! Diagnostics routing for the print path (retry-forever DESIGN D9 / M1).
//!
//! stdout is the machine-output channel in `-f json` / `-f stream-json`
//! (DESIGN CO5): every diagnostic — norn's and its dependencies' —
//! belongs on stderr. `tracing_subscriber`'s own default writer is
//! STDOUT, so a print-mode embedder that installed no subscriber would
//! otherwise get engine diagnostics interleaved into the JSON it is
//! parsing.

/// Install the stderr-routed tracing subscriber unless a global
/// subscriber is already installed, reporting whether this call won the
/// install.
///
/// Returns `false` when a global subscriber was already installed: the
/// global subscriber can be set only once, so the pre-existing one keeps
/// ownership and the caller must honour the embedder contract documented
/// on [`run_async`](super::orchestrator::run_async). The binary turns a
/// lost install into an operator-visible stderr warning; it is never
/// discarded silently.
///
/// The filter is `RUST_LOG` when set, else `warn` — the same
/// pre-existing default the binary has always used.
pub fn ensure_stderr_tracing() -> bool {
    use tracing_subscriber::util::SubscriberInitExt as _;
    stderr_tracing_subscriber(std::io::stderr)
        .try_init()
        .is_ok()
}

/// Build the stderr-routed subscriber over `writer`. Split out so the
/// routing can be proven against a capturing writer in tests.
fn stderr_tracing_subscriber<W>(writer: W) -> impl tracing::Subscriber + Send + Sync + 'static
where
    W: for<'writer> tracing_subscriber::fmt::MakeWriter<'writer> + Send + Sync + 'static,
{
    tracing_subscriber::fmt()
        .with_writer(writer)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .finish()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// M1: the subscriber the print path installs writes every event to
    /// the writer it was built with — stderr in production — and never to
    /// stdout, which carries the machine formats.
    ///
    /// Proven against a capturing writer rather than the global
    /// subscriber, which can be installed only once per process.
    /// `error!` is used so the assertion holds under any `RUST_LOG` a
    /// developer's shell may set (short of `off`).
    #[test]
    fn stderr_tracing_subscriber_routes_events_to_its_writer() {
        #[derive(Clone)]
        struct CapturingWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

        impl std::io::Write for CapturingWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().expect("capture lock").extend_from_slice(buf);
                Ok(buf.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let writer = CapturingWriter(std::sync::Arc::clone(&captured));
        let subscriber = stderr_tracing_subscriber(move || writer.clone());
        tracing::subscriber::with_default(subscriber, || {
            tracing::error!("stdout-purity probe");
        });

        let text = String::from_utf8(captured.lock().expect("capture lock").clone())
            .expect("utf-8 diagnostics");
        assert!(
            text.contains("stdout-purity probe"),
            "the subscriber must write to its own writer, not to stdout: {text:?}"
        );
    }

    /// The global subscriber can be installed only once: after the print
    /// path (or a host) has installed one, a second attempt must REPORT
    /// the loss rather than pretend it won — the binary turns that into
    /// an operator-visible stderr warning.
    #[test]
    fn ensure_stderr_tracing_reports_a_lost_install() {
        let first = ensure_stderr_tracing();
        let second = ensure_stderr_tracing();
        assert!(
            !second,
            "a second install cannot win the global subscriber (first attempt won: {first})"
        );
    }
}
