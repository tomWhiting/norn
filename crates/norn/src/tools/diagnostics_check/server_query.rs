//! LD-003 fast-path that queries the running diagnostic server over its
//! UNIX socket before falling back to inline adapter dispatch.
//!
//! [`try_server_query_for_tool`] short-circuits when the server is unavailable
//! (missing socket, stale socket, IO failure, request/response timeout,
//! or `DiagnosticStatus::Error` response) so the inline rule diagnostic path
//! can continue. On a successful `Fresh` / `Stale` response, results for the
//! requested activated tool are routed into the shared [`Findings`] accumulator
//! using the same formatter as the inline path (see
//! [`super::adapters::format_verdict_message`]) so both paths surface identical
//! text to the model.
//!
//! [LD-003b R2] [`QUERY_RESPONSE_TIMEOUT`] bounds the per-IO wait on the
//! server connection. A server that accepts a connection but stalls
//! mid-protocol (e.g. a deadlocked adapter run) must not be allowed to
//! hang the post-check indefinitely; on timeout the fast path returns
//! [`ServerQueryOutcome::FellBack`] so the existing inline adapter
//! dispatch path runs instead.

use std::path::Path;
#[cfg(unix)]
use std::time::Duration;

use diagnostics::conventions::Handling;

use super::findings::Findings;
use super::infra::DiagnosticInfra;

/// Per-IO budget [`try_server_query`] grants the diagnostic server for
/// each of the `write_frame` request and `read_frame` response when it
/// has accepted a connection. Sized large enough not to clip warm-cache
/// `Fresh` responses (which return well under 100ms — see DESIGN.md D11)
/// or the worst case of an in-flight `InvalidateAndCheck` adapter run
/// that the server is already executing, while still being tight enough
/// that a deadlocked server cannot stall the tool lifecycle beyond a
/// few seconds. Chosen to mirror the existing
/// [`diagnostics::server::TRY_CONNECT_TIMEOUT`] pattern of a named
/// per-stage transport bound.
#[cfg(unix)]
const QUERY_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);

/// Outcome of attempting to satisfy a post-check via the diagnostic
/// server.
///
/// `Used` means the server returned a usable response and its results
/// have been written into the supplied [`Findings`]; callers MUST skip
/// the inline adapter dispatch loop. `FellBack` means the post-check
/// must continue down the existing inline path; nothing was written
/// into [`Findings`] by the fast path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ServerQueryOutcome {
    /// Server replied with `Fresh` or `Stale`; results merged into
    /// the supplied [`Findings`].
    Used,
    /// Server unavailable or replied with `Error`; caller must run
    /// the existing inline adapter dispatch path.
    FellBack,
}

pub(super) async fn try_server_query_for_tool(
    file_path: &Path,
    relative_path: &Path,
    tool_name: &str,
    handling: Handling,
    infra: &DiagnosticInfra,
    findings: &mut Findings<'_>,
) -> ServerQueryOutcome {
    platform::try_server_query_for_tool(
        file_path,
        relative_path,
        tool_name,
        handling,
        infra,
        findings,
    )
    .await
}

#[cfg(unix)]
mod platform {
    use std::path::Path;

    use diagnostics::conventions::Handling;
    use diagnostics::policy::PolicyVerdict;
    use diagnostics::server::protocol::{
        DiagnosticQuery, DiagnosticResponse, DiagnosticResult, DiagnosticStatus, QueryMode,
        read_frame, write_frame,
    };
    use diagnostics::server::{server_available, try_connect};

    use crate::tool::lifecycle::{Advisory, AdvisorySeverity};
    use crate::tools::diagnostics_check::adapters::format_verdict_message;
    use crate::tools::diagnostics_check::findings::Findings;
    use crate::tools::diagnostics_check::infra::DiagnosticInfra;

    use super::{QUERY_RESPONSE_TIMEOUT, ServerQueryOutcome};

    pub(super) async fn try_server_query_for_tool(
        file_path: &Path,
        relative_path: &Path,
        tool_name: &str,
        handling: Handling,
        infra: &DiagnosticInfra,
        findings: &mut Findings<'_>,
    ) -> ServerQueryOutcome {
        if !server_available(&infra.socket_path) {
            return ServerQueryOutcome::FellBack;
        }
        let Some(mut stream) = try_connect(&infra.socket_path).await else {
            return ServerQueryOutcome::FellBack;
        };

        let query = DiagnosticQuery {
            file_path: relative_path.to_path_buf(),
            mode: QueryMode::InvalidateAndCheck,
        };
        match tokio::time::timeout(QUERY_RESPONSE_TIMEOUT, write_frame(&mut stream, &query)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::debug!(
                    error = %error,
                    "diagnostic server query write failed; falling back to inline adapter"
                );
                return ServerQueryOutcome::FellBack;
            }
            Err(_elapsed) => {
                tracing::debug!(
                    timeout_secs = QUERY_RESPONSE_TIMEOUT.as_secs(),
                    "diagnostic server query write timed out; falling back to inline adapter"
                );
                return ServerQueryOutcome::FellBack;
            }
        }

        let response: DiagnosticResponse =
            match tokio::time::timeout(QUERY_RESPONSE_TIMEOUT, read_frame(&mut stream)).await {
                Ok(Ok(response)) => response,
                Ok(Err(error)) => {
                    tracing::debug!(
                        error = %error,
                        "diagnostic server response read failed; falling back to inline adapter"
                    );
                    return ServerQueryOutcome::FellBack;
                }
                Err(_elapsed) => {
                    tracing::debug!(
                        timeout_secs = QUERY_RESPONSE_TIMEOUT.as_secs(),
                        "diagnostic server response read timed out; falling back to inline adapter"
                    );
                    return ServerQueryOutcome::FellBack;
                }
            };

        match response.status {
            DiagnosticStatus::Fresh | DiagnosticStatus::Stale => {
                let matched_count = merge_server_results_for_tool(
                    file_path,
                    tool_name,
                    handling,
                    &response.results,
                    findings,
                );
                if matched_count == 0 {
                    tracing::debug!(
                        tool = tool_name,
                        total_results = response.results.len(),
                        "no results for requested tool; falling back to subprocess"
                    );
                    ServerQueryOutcome::FellBack
                } else {
                    ServerQueryOutcome::Used
                }
            }
            DiagnosticStatus::Error => ServerQueryOutcome::FellBack,
        }
    }

    fn merge_server_results_for_tool(
        modified_file: &Path,
        tool_name: &str,
        handling: Handling,
        results: &[DiagnosticResult],
        findings: &mut Findings<'_>,
    ) -> usize {
        let mut matched_count = 0;
        for result in results {
            if result.event.file != modified_file || result.event.source_tool != tool_name {
                continue;
            }
            matched_count += 1;
            if matches!(result.verdict, PolicyVerdict::Pass) {
                continue;
            }
            let Some(message) =
                format_verdict_message(&result.event, &result.event.source_tool, &result.verdict)
            else {
                continue;
            };

            match handling {
                Handling::Block => findings.errors.push(message),
                Handling::Advise => findings.advisories.push(Advisory {
                    severity: AdvisorySeverity::Warning,
                    message,
                    source: result.event.source_tool.clone(),
                }),
            }
        }
        matched_count
    }
}

#[cfg(not(unix))]
mod platform {
    use std::path::Path;

    use diagnostics::conventions::Handling;

    use crate::tools::diagnostics_check::findings::Findings;
    use crate::tools::diagnostics_check::infra::DiagnosticInfra;

    use super::ServerQueryOutcome;

    pub(super) async fn try_server_query_for_tool(
        file_path: &Path,
        relative_path: &Path,
        tool_name: &str,
        handling: Handling,
        infra: &DiagnosticInfra,
        findings: &mut Findings<'_>,
    ) -> ServerQueryOutcome {
        let _ = (
            file_path,
            relative_path,
            tool_name,
            handling,
            infra,
            findings,
        );
        ServerQueryOutcome::FellBack
    }
}
