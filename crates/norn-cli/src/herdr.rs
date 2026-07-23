//! `HerdR` pane integration for active `Norn` sessions.
//!
//! `HerdR` injects `HERDR_ENV`, `HERDR_PANE_ID`, and `HERDR_SOCKET_PATH` into
//! processes started in one of its terminal panes. While those variables are
//! present, `Norn` reports the active root session through `HerdR`'s documented
//! `pane.report_agent` socket API. Dropping the claim sends
//! `pane.release_agent`, allowing the pane's previous detector/reporter to
//! become authoritative again (important when `Norn` was launched by another
//! agent in the same pane).

#[cfg(unix)]
mod imp {
    use std::env;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use serde::Serialize;
    use serde_json::Value;

    const AGENT: &str = "norn";
    const SOURCE: &str = "herdr:norn";
    // HerdR's own installed hooks use a 500 ms local-socket timeout.
    const SOCKET_TIMEOUT: Duration = Duration::from_millis(500);
    static LAST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    /// An active `Norn` claim on the surrounding `HerdR` pane.
    ///
    /// Construction is a no-op outside `HerdR`. Inside `HerdR`, a successful
    /// claim is released automatically on every normal return path.
    pub(crate) struct PaneClaim {
        endpoint: Endpoint,
    }

    #[derive(Clone)]
    struct Endpoint {
        pane_id: String,
        socket_path: PathBuf,
    }

    #[derive(Serialize)]
    struct Request<'a, T> {
        id: String,
        method: &'a str,
        params: T,
    }

    #[derive(Serialize)]
    struct ReportParams<'a> {
        pane_id: &'a str,
        source: &'static str,
        agent: &'static str,
        state: &'static str,
        seq: u64,
        agent_session_id: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        agent_session_path: Option<&'a str>,
    }

    #[derive(Serialize)]
    struct ReleaseParams<'a> {
        pane_id: &'a str,
        source: &'static str,
        agent: &'static str,
        seq: u64,
    }

    impl PaneClaim {
        /// Claim the surrounding `HerdR` pane as a working `Norn` session.
        ///
        /// Returns `None` when `Norn` is not running in a `HerdR`-managed pane
        /// or when `HerdR` rejects/unavailable for the initial report.
        /// Integration failures are observational and never fail the `Norn` run itself.
        pub(crate) fn claim(session_id: &str, session_path: Option<&Path>) -> Option<Self> {
            let endpoint = match Endpoint::from_env() {
                Ok(Some(endpoint)) => endpoint,
                Ok(None) => return None,
                Err(error) => {
                    tracing::warn!(%error, "invalid HerdR integration environment");
                    return None;
                }
            };
            let path = session_path.and_then(Path::to_str);
            if session_path.is_some() && path.is_none() {
                tracing::warn!("Norn session path is not UTF-8; omitting it from the HerdR report");
            }
            if let Err(error) = endpoint.report(session_id, path) {
                tracing::warn!(%error, "failed to report Norn session to HerdR");
                return None;
            }
            Some(Self { endpoint })
        }
    }

    impl Drop for PaneClaim {
        fn drop(&mut self) {
            if let Err(error) = self.endpoint.release() {
                tracing::warn!(%error, "failed to release Norn session from HerdR");
            }
        }
    }

    impl Endpoint {
        fn from_env() -> Result<Option<Self>, String> {
            if env::var_os("HERDR_ENV").as_deref() != Some(std::ffi::OsStr::new("1")) {
                return Ok(None);
            }
            let pane_id = env::var("HERDR_PANE_ID")
                .map_err(|error| format!("HERDR_PANE_ID is missing or is not UTF-8: {error}"))?;
            if pane_id.is_empty() {
                return Err("HERDR_PANE_ID is empty".to_owned());
            }
            let socket_path = env::var_os("HERDR_SOCKET_PATH")
                .filter(|path| !path.is_empty())
                .map(PathBuf::from)
                .ok_or_else(|| "HERDR_SOCKET_PATH is missing or empty".to_owned())?;
            Ok(Some(Self {
                pane_id,
                socket_path,
            }))
        }

        fn report(&self, session_id: &str, session_path: Option<&str>) -> Result<(), String> {
            let seq = next_sequence();
            self.send(&Request {
                id: format!("{SOURCE}:{seq}"),
                method: "pane.report_agent",
                params: ReportParams {
                    pane_id: &self.pane_id,
                    source: SOURCE,
                    agent: AGENT,
                    state: "working",
                    seq,
                    agent_session_id: session_id,
                    agent_session_path: session_path,
                },
            })
        }

        fn release(&self) -> Result<(), String> {
            let seq = next_sequence();
            self.send(&Request {
                id: format!("{SOURCE}:{seq}"),
                method: "pane.release_agent",
                params: ReleaseParams {
                    pane_id: &self.pane_id,
                    source: SOURCE,
                    agent: AGENT,
                    seq,
                },
            })
        }

        fn send<T: Serialize>(&self, request: &Request<'_, T>) -> Result<(), String> {
            let mut stream = UnixStream::connect(&self.socket_path)
                .map_err(|error| format!("connect {}: {error}", self.socket_path.display()))?;
            stream
                .set_write_timeout(Some(SOCKET_TIMEOUT))
                .map_err(|error| format!("set HerdR write timeout: {error}"))?;
            stream
                .set_read_timeout(Some(SOCKET_TIMEOUT))
                .map_err(|error| format!("set HerdR read timeout: {error}"))?;
            serde_json::to_writer(&mut stream, request)
                .map_err(|error| format!("encode HerdR request: {error}"))?;
            stream
                .write_all(b"\n")
                .map_err(|error| format!("write HerdR request: {error}"))?;

            let mut response = String::new();
            BufReader::new(stream)
                .read_line(&mut response)
                .map_err(|error| format!("read HerdR response: {error}"))?;
            if response.is_empty() {
                return Err("HerdR closed the socket without a response".to_owned());
            }
            let response: Value = serde_json::from_str(&response)
                .map_err(|error| format!("decode HerdR response: {error}"))?;
            if let Some(error) = response.get("error") {
                return Err(format!("HerdR rejected the request: {error}"));
            }
            Ok(())
        }
    }

    fn next_sequence() -> u64 {
        let wall_clock = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .try_into()
            .unwrap_or(u64::MAX);
        let mut previous = LAST_SEQUENCE.load(Ordering::Relaxed);
        loop {
            let next = wall_clock.max(previous.saturating_add(1));
            match LAST_SEQUENCE.compare_exchange_weak(
                previous,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return next,
                Err(actual) => previous = actual,
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use std::fmt::Display;
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        use std::thread;

        use serde_json::json;
        use tempfile::tempdir;

        use super::*;

        fn must<T, E: Display>(result: Result<T, E>, context: &str) -> T {
            match result {
                Ok(value) => value,
                Err(error) => {
                    eprintln!("{context}: {error}");
                    std::process::abort();
                }
            }
        }

        fn capture_request(endpoint: &Endpoint, report: bool) -> Value {
            let temp = must(tempdir(), "create temp dir");
            let socket = temp.path().join("herdr.sock");
            let listener = must(UnixListener::bind(&socket), "bind socket");
            let server = thread::spawn(move || {
                let (mut stream, _) = must(listener.accept(), "accept connection");
                let mut line = String::new();
                let cloned = must(stream.try_clone(), "clone stream");
                must(BufReader::new(cloned).read_line(&mut line), "read request");
                must(
                    stream.write_all(b"{\"id\":\"ok\",\"result\":{\"type\":\"ok\"}}\n"),
                    "write response",
                );
                must(serde_json::from_str(&line), "decode JSON request")
            });
            let local = Endpoint {
                pane_id: endpoint.pane_id.clone(),
                socket_path: socket,
            };
            if report {
                must(
                    local.report("session-42", Some("/tmp/session-42.jsonl")),
                    "report succeeds",
                );
            } else {
                must(local.release(), "release succeeds");
            }
            match server.join() {
                Ok(request) => request,
                Err(payload) => std::panic::resume_unwind(payload),
            }
        }

        #[test]
        fn report_uses_herdr_custom_agent_contract() {
            let endpoint = Endpoint {
                pane_id: "w1:p2".to_owned(),
                socket_path: PathBuf::new(),
            };
            let request = capture_request(&endpoint, true);
            assert_eq!(request["method"], "pane.report_agent");
            assert!(request["params"]["seq"].as_u64().is_some_and(|seq| seq > 0));
            assert_eq!(
                request["params"],
                json!({
                    "pane_id": "w1:p2",
                    "source": "herdr:norn",
                    "agent": "norn",
                    "state": "working",
                    "seq": request["params"]["seq"],
                    "agent_session_id": "session-42",
                    "agent_session_path": "/tmp/session-42.jsonl"
                })
            );
        }

        #[test]
        fn release_uses_same_source_and_a_newer_sequence() {
            let endpoint = Endpoint {
                pane_id: "w1:p2".to_owned(),
                socket_path: PathBuf::new(),
            };
            let report = capture_request(&endpoint, true);
            let release = capture_request(&endpoint, false);
            assert_eq!(release["method"], "pane.release_agent");
            assert_eq!(release["params"]["source"], "herdr:norn");
            assert_eq!(release["params"]["agent"], "norn");
            assert_eq!(release["params"]["pane_id"], "w1:p2");
            assert!(release["params"]["seq"].as_u64() > report["params"]["seq"].as_u64());
        }

        #[test]
        fn server_errors_are_not_treated_as_success() {
            let temp = must(tempdir(), "create temp dir");
            let socket = temp.path().join("herdr.sock");
            let listener = must(UnixListener::bind(&socket), "bind socket");
            let server = thread::spawn(move || {
                let (mut stream, _) = must(listener.accept(), "accept connection");
                let mut line = String::new();
                let cloned = must(stream.try_clone(), "clone stream");
                must(BufReader::new(cloned).read_line(&mut line), "read request");
                must(
                    stream.write_all(b"{\"id\":\"x\",\"error\":{\"message\":\"rejected\"}}\n"),
                    "write response",
                );
            });
            let endpoint = Endpoint {
                pane_id: "w1:p2".to_owned(),
                socket_path: socket,
            };
            let result = endpoint.release();
            assert!(result.is_err(), "server error must surface");
            let error = result.err().unwrap_or_default();
            if let Err(payload) = server.join() {
                std::panic::resume_unwind(payload);
            }
            assert!(error.contains("rejected"));
        }
    }
}

#[cfg(unix)]
pub(crate) use imp::PaneClaim;

#[cfg(not(unix))]
pub(crate) struct PaneClaim;

#[cfg(not(unix))]
impl PaneClaim {
    pub(crate) fn claim(_: &str, _: Option<&std::path::Path>) -> Option<Self> {
        None
    }
}
