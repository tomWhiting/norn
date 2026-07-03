//! LD-012 fast-path that pulls language-server diagnostics from the
//! shared [`LspBridge`] before the LD-003 server-query or inline-adapter
//! cascade runs.
//!
//! The path is engaged only when:
//!
//! 1. [`DiagnosticInfra::lsp_bridge`] is `Some` (a language server is
//!    wired and the runtime donated its aggregator).
//! 2. At least one new-format rule in `CONVENTIONS.toml` matches the
//!    `(tool_name, relative_path)` pair AND configures `lsp.diagnostics`.
//!
//! When either condition fails the function returns
//! [`LspDiagnosticsOutcome::FellBack`] without touching the supplied
//! [`Findings`]; the caller continues down the existing
//! server-query / inline-adapter cascade (CO5 — graceful degradation).
//!
//! When the path is engaged, the function subscribes to the aggregator's
//! broadcast channel BEFORE asking the wired [`LspBackend`] to re-run its
//! flycheck (`clear_flycheck` followed by `run_flycheck`). It then waits
//! — bounded by the strongest configured `lsp.diagnostics.timeout` from
//! the matching rules — for a `publishDiagnostics` update that targets
//! the modified file. The wait is event-driven (no polling); mismatched
//! updates loop within the same deadline. On a successful match the
//! diagnostics are filtered through the shared [`PolicyRegistry`] so the
//! policy gating matches the inline-adapter path, then routed into
//! [`Findings::errors`] (Block) or [`Findings::advisories`] (Advise). On
//! timeout or broadcast-channel closure the function returns
//! [`LspDiagnosticsOutcome::FellBack`] so the LD-003 server-query and
//! inline-adapter cascade runs — CO11: stale reads must never silently
//! pass as current.

use std::path::{Path, PathBuf};
use std::time::Duration;

use diagnostics::conventions::{CompiledRule, ConventionsConfig, Handling};
use diagnostics::event::DiagnosticEvent;
use diagnostics::lsp_bridge::LspDiagnosticStream;
use diagnostics::policy::PolicyVerdict;
use diagnostics::registry::PolicyRegistry;

use crate::tool::lifecycle::{Advisory, AdvisorySeverity};
use crate::tools::lsp::LspBackend;

use super::adapters::format_verdict_message;
use super::findings::Findings;
use super::infra::DiagnosticInfra;
use super::lsp_test_exec::handling_for_rule;

/// Source string attributed to every advisory emitted by the LSP fast
/// path. Matches the diagnostics crate's
/// [`DEFAULT_SOURCE_TOOL`](diagnostics::lsp_bridge::map_lsp_diagnostic)
/// fallback so all LSP-derived findings carry a stable attribution.
const LSP_SOURCE: &str = "lsp";

/// Outcome of attempting to satisfy a post-check via the language
/// server's diagnostic aggregator.
///
/// `Used` means the LSP path matched at least one rule with
/// `lsp.diagnostics` configured and successfully received a
/// `publishDiagnostics` update for the modified file; findings have been
/// written and the caller MUST skip the server-query and inline-adapter
/// paths. `FellBack` means the LSP path did not apply (no bridge wired,
/// no rule opted in, the recheck timed out, or the broadcast channel
/// closed); nothing was written into [`Findings`] and the caller must
/// continue down the existing cascade.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LspDiagnosticsOutcome {
    /// LSP bridge produced a usable response; results merged into
    /// [`Findings`].
    Used,
    /// LSP path skipped — caller must run the server-query / inline
    /// fallback path.
    FellBack,
}

/// Walk all new-format rules in `conventions` and, when a rule matches
/// `(tool_name, relative_path)` and configures `lsp.diagnostics`,
/// subscribe to the bridge's broadcast channel, ask the wired
/// [`LspBackend`] to re-run its flycheck, and wait for the matching
/// `publishDiagnostics` update. Gates the events through the policy
/// registry and routes the surviving messages into `findings`.
///
/// Returns [`LspDiagnosticsOutcome::FellBack`] when no LSP bridge is
/// wired, no rule with `lsp.diagnostics` matches, the wait times out,
/// or the broadcast channel closes; in any of those cases `findings` is
/// left untouched and the caller must continue the cascade (CO5 +
/// CO11).
pub(super) async fn try_lsp_diagnostics_for_rules(
    file_path: &Path,
    relative_path: &Path,
    tool_name: &str,
    conventions: &ConventionsConfig,
    infra: &DiagnosticInfra,
    findings: &mut Findings<'_>,
) -> LspDiagnosticsOutcome {
    let Some(bridge) = infra.lsp_bridge.as_ref() else {
        return LspDiagnosticsOutcome::FellBack;
    };

    let matching_rules: Vec<&CompiledRule> = conventions
        .rules()
        .values()
        .filter(|compiled| {
            compiled
                .rule
                .lsp
                .as_ref()
                .and_then(|lsp| lsp.diagnostics.as_ref())
                .is_some()
                && compiled.rule.tools.iter().any(|tool| tool == tool_name)
                && compiled.matcher.is_match(relative_path)
        })
        .collect();

    if matching_rules.is_empty() {
        return LspDiagnosticsOutcome::FellBack;
    }

    let handling = strongest_handling(&matching_rules);
    let timeout = strongest_timeout(&matching_rules);

    // The aggregator keys its store — and therefore every broadcast
    // `DiagnosticUpdate::file_path` — by the *canonical* path (symlinks
    // resolved, macOS `/tmp` → `/private/tmp`). Canonicalize the modified
    // file once up front so wait-for-publish and the merge filter compare
    // like with like; otherwise a symlinked spelling of the same file
    // never matches and the path always times out into FellBack.
    let canonical_file = canonicalize_or_fallback(file_path).await;

    // R1 race-prevention: subscribe BEFORE triggering flycheck so the
    // broadcast send cannot race ahead of our receiver. Broadcasts have
    // no replay; a late subscriber misses the publish entirely.
    let mut stream = bridge.subscribe();

    // CO11: drain any in-flight publishes that arrived between subscribe
    // and now. A previous flycheck may have completed just as we
    // subscribed, putting a stale publishDiagnostics on the broadcast.
    // Consuming it here prevents wait_for_publish from returning stale
    // diagnostics from before the current edit.
    drain_pending(&mut stream).await;

    // R2: ask the backend to recompute diagnostics for `file_path`. When
    // no backend is wired we still subscribe-and-wait — a publish may
    // arrive from a server triggered elsewhere — though in practice the
    // wait will time out and fall back, which is correct (CO5).
    if let Some(backend) = infra.lsp_backend.as_ref() {
        trigger_flycheck(backend.as_ref(), file_path).await;
    }

    // R1 + R3: wait for a publishDiagnostics update matching the file,
    // bounded by the strongest configured timeout. On timeout / channel
    // closure the function falls back so the LD-003 cascade runs (CO11).
    let Some(events) = wait_for_publish(&mut stream, &canonical_file, timeout).await else {
        tracing::warn!(
            file = %file_path.display(),
            timeout_secs = timeout.as_secs(),
            "LSP recheck timed out, falling back to diagnostic server"
        );
        return LspDiagnosticsOutcome::FellBack;
    };

    merge_lsp_events(
        &events,
        &canonical_file,
        handling,
        &infra.policies,
        findings,
    );
    LspDiagnosticsOutcome::Used
}

/// Canonicalize a path, falling back to the input unchanged when
/// canonicalization fails (nonexistent path, permission error). Mirrors
/// the lsp crate's private `canonicalize_or_fallback` primitive that the
/// [`DiagnosticAggregator`](lsp::features::diagnostics::DiagnosticAggregator)
/// uses to derive its storage keys, so paths compared against broadcast
/// updates use the same spelling as the aggregator's keys.
async fn canonicalize_or_fallback(path: &Path) -> PathBuf {
    match tokio::fs::canonicalize(path).await {
        Ok(canonical) => canonical,
        Err(_) => path.to_path_buf(),
    }
}

/// Resolve the handling for an LSP event when multiple rules match the
/// same file. `Handling::Block` from any matching rule takes precedence,
/// otherwise the default is `Handling::Advise`. Mirrors the
/// `block_on`-takes-precedence semantics used by
/// [`super::server_query`].
fn strongest_handling(matching_rules: &[&CompiledRule]) -> Handling {
    if matching_rules
        .iter()
        .any(|rule| handling_for_rule(rule) == Handling::Block)
    {
        Handling::Block
    } else {
        Handling::Advise
    }
}

/// Pick the wait-for-publish budget. When multiple rules match, the
/// **largest** configured timeout wins — a generous rule must not be
/// clipped by a stricter sibling. Every rule that survives the filter
/// in [`try_lsp_diagnostics_for_rules`] carries a parsed
/// `lsp.diagnostics.timeout` (serde supplies the 30-second default
/// from [`diagnostics::conventions::rule`]), so the iterator yields at
/// least one value in practice; the type-level fallback mirrors that
/// same 30-second default to keep the wait bounded if a future caller
/// ever passes an empty slice.
fn strongest_timeout(matching_rules: &[&CompiledRule]) -> Duration {
    matching_rules
        .iter()
        .filter_map(|compiled| {
            compiled
                .rule
                .lsp
                .as_ref()
                .and_then(|lsp| lsp.diagnostics.as_ref())
                .map(|diagnostics| diagnostics.timeout)
        })
        .max()
        .unwrap_or_else(|| Duration::from_secs(30))
}

/// Ask the backend to re-run its flycheck for `file_path`. Calls
/// `clear_flycheck` first to cancel any in-progress check, then
/// `run_flycheck` to schedule a fresh recheck — the brief's strict
/// order. Both methods have no-op defaults on [`LspBackend`] so backends
/// without flycheck control degrade silently (CO5). Errors are logged
/// and swallowed: the wait-for-publish will time out on its own if the
/// recheck silently fails (CO5 — never abort the post-check on backend
/// failure).
async fn trigger_flycheck(backend: &dyn LspBackend, file_path: &Path) {
    if let Err(error) = backend.clear_flycheck().await {
        tracing::warn!(
            path = %file_path.display(),
            error = %error,
            "LSP clear_flycheck failed; continuing post-check"
        );
    }
    if let Err(error) = backend.run_flycheck(file_path).await {
        tracing::warn!(
            path = %file_path.display(),
            error = %error,
            "LSP run_flycheck failed; continuing post-check"
        );
    }
}

/// Non-blocking drain of any immediately-available events on `stream`.
/// Called between `subscribe()` and `trigger_flycheck()` to discard stale
/// publishes that were in-flight when we subscribed (CO11).
async fn drain_pending(stream: &mut LspDiagnosticStream) {
    while let Ok(Some(update)) = tokio::time::timeout(Duration::ZERO, stream.next()).await {
        tracing::debug!(
            file = %update.file_path.display(),
            events = update.events.len(),
            "drained stale publishDiagnostics from broadcast"
        );
    }
}

/// Drain `stream` until a [`LspFileUpdate`](diagnostics::lsp_bridge::LspFileUpdate)
/// targets `file_path` or the shared `deadline` elapses. Updates for
/// other files are skipped without consuming additional budget beyond
/// the single deadline (the brief's "loop back to `next()` — still within
/// the same outer timeout deadline" requirement).
///
/// Returns `Some(events)` on a matching update, or `None` when the
/// timeout expires or the broadcast channel closes.
async fn wait_for_publish(
    stream: &mut LspDiagnosticStream,
    file_path: &Path,
    timeout: Duration,
) -> Option<Vec<DiagnosticEvent>> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match tokio::time::timeout_at(deadline, stream.next()).await {
            Ok(Some(update)) => {
                if update.file_path == file_path {
                    return Some(update.events);
                }
                // Mismatched file — keep waiting within the same deadline.
            }
            Ok(None) => return None,
            Err(_elapsed) => return None,
        }
    }
}

/// Filter `events` to the modified file, evaluate each through the
/// policy registry, and route surviving messages into `findings`. Events
/// whose `file` differs from `modified_file` are dropped — the bridge
/// already keys diagnostics by path but the filter is kept as a
/// defensive guard (matches `merge_server_results` in the LD-003 path).
fn merge_lsp_events(
    events: &[DiagnosticEvent],
    modified_file: &Path,
    handling: Handling,
    policies: &PolicyRegistry,
    findings: &mut Findings<'_>,
) {
    for event in events {
        if event.file != modified_file {
            continue;
        }
        let verdict = policies.evaluate_all(event);
        if matches!(verdict, PolicyVerdict::Pass) {
            continue;
        }
        let Some(message) = format_verdict_message(event, &event.source_tool, &verdict) else {
            continue;
        };

        match handling {
            Handling::Block => findings.errors.push(message),
            Handling::Advise => findings.advisories.push(Advisory {
                severity: AdvisorySeverity::Warning,
                message,
                source: source_for_advisory(event),
            }),
        }
    }
}

/// Pick the advisory's source string: prefer the event's `source_tool`
/// (already prefixed by the bridge's mapping), falling back to the
/// stable `LSP_SOURCE` constant when the event omitted attribution.
fn source_for_advisory(event: &DiagnosticEvent) -> String {
    if event.source_tool.is_empty() {
        LSP_SOURCE.to_owned()
    } else {
        event.source_tool.clone()
    }
}
