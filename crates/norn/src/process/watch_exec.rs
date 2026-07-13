//! Incremental, deterministic filter execution for a single watch (NP-002 R2).
//!
//! One [`run_watch`] task runs per attached watch. It reacts **only** to spool
//! append notifications (the committed-length watch from the NP-001 R8 seam) —
//! there are no cadence timers, debounce windows, poll intervals, or model
//! calls anywhere in this file (the zero-model-cost guarantee is structural:
//! there are no provider/model imports here). Each notification hands the watch
//! the region appended since its cursor; the filter runs via `sh -c` with that
//! region on stdin, in the agent's working directory with its `ProcessEnv`,
//! exactly one execution at a time. Exit 0 is a match (stdout is the excerpt);
//! any clean non-zero exit is no match; anything else (spawn error, signal
//! kill, stdin write failure) is a watch-error alert that never silently
//! disables the watch (NP-002 R4).
//!
//! The cursor advances **only** over regions a filter run actually consumed:
//! [`SpoolReader::read_new`] advances the cursor by exactly the bytes it
//! returns, so appends arriving while a run is in flight coalesce into the next
//! run's region (no gap, no overlap) and a failed region is not re-examined.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use chrono::Utc;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::tool::context::ProcessEnv;

use super::manager::ProcessNotifier;
use super::spool::SpoolReader;
use super::watch::{Watch, WatchAlert, WatchAlertKind};

/// Everything one watch executor task owns for its lifetime.
pub(super) struct WatchExec {
    /// The watch record (ids, brief, filter).
    pub(super) watch: Watch,
    /// The watch's independent incremental reader over the spool.
    pub(super) reader: SpoolReader,
    /// The committed-length append notification receiver (NP-001 R8 seam).
    pub(super) len_rx: watch::Receiver<u64>,
    /// Cancelled on unwatch: stop after the in-flight run, no final region.
    pub(super) detach: CancellationToken,
    /// Cancelled on process exit: filter the remaining region, then end.
    pub(super) finalize: CancellationToken,
    /// The agent's working directory for the filter's `sh -c`.
    pub(super) cwd: PathBuf,
    /// The agent's process environment for the filter.
    pub(super) env: Option<ProcessEnv>,
    /// The delivery sink for alerts. `None` on a bare manager (no owning
    /// agent); alerts are then logged rather than injected.
    pub(super) notifier: Option<Arc<dyn ProcessNotifier>>,
}

/// The verdict of one filter execution over a region.
#[derive(Debug)]
pub(super) enum FilterOutcome {
    /// Exit 0: a match, with the filter's stdout as the excerpt.
    Match(String),
    /// A clean non-zero exit: no match, no alert, no log noise above debug.
    NoMatch,
    /// The filter could not be run to a clean verdict (spawn error, signal
    /// kill, stdin write failure). Surfaced as a watch-error alert (R4).
    Error(String),
}

/// The watch executor loop (NP-002 R2/R4/R5).
///
/// Runs an initial catch-up over any output already committed at attach time
/// (a late-attached watch observes existing state per the seam's `send_replace`
/// contract), then reacts to each append notification until the watch detaches
/// (unwatch) or is finalized (process exit).
pub(super) async fn run_watch(mut exec: WatchExec) {
    // Initial catch-up: examine everything committed before this watch
    // attached. The reader starts at cursor 0, so this filters the region that
    // already existed — a watch attached mid-run does not miss prior output.
    process_new_region(&mut exec).await;

    loop {
        tokio::select! {
            biased;
            // Unwatch wins over a pending append: nothing after detach is
            // examined (any in-flight run already completed before this poll).
            () = exec.detach.cancelled() => return,
            // Process exit: the manager has joined the drains, so the remaining
            // committed region is complete. Filter it once, then end.
            () = exec.finalize.cancelled() => {
                process_new_region(&mut exec).await;
                return;
            }
            changed = exec.len_rx.changed() => {
                // A `changed` error means every spool sender is gone (the
                // process's Spool was dropped). Either way, filter the new
                // region; on the closed case, end after this final drain.
                let closed = changed.is_err();
                process_new_region(&mut exec).await;
                if closed {
                    return;
                }
            }
        }
    }
}

/// Read the region appended since the cursor, run the filter over it, and
/// deliver an alert on a match or a filter failure. A no-match or an empty
/// region produces no alert.
async fn process_new_region(exec: &mut WatchExec) {
    let start = exec.reader.cursor();
    let region = match exec.reader.read_new().await {
        Ok(region) => region,
        Err(error) => {
            // A local I/O failure reading the append-only spool is a system
            // fault, not a filter verdict. Log it loudly (never swallowed); the
            // cursor did not advance, so the next notification retries.
            tracing::error!(
                watch = %exec.watch.watch_id,
                process = %exec.watch.process_id,
                %error,
                "failed to read the spool region for a watch; will retry on the next append",
            );
            return;
        }
    };
    if region.is_empty() {
        return;
    }
    let end = exec.reader.cursor();
    let outcome = run_filter(&exec.watch.filter, &region, &exec.cwd, exec.env.as_ref()).await;
    match outcome {
        FilterOutcome::Match(excerpt) => {
            emit(
                exec,
                start,
                end,
                WatchAlertKind::Match {
                    excerpt,
                    matched_at: Utc::now(),
                },
            );
        }
        FilterOutcome::NoMatch => {
            tracing::debug!(
                watch = %exec.watch.watch_id,
                process = %exec.watch.process_id,
                "watch filter did not match this region",
            );
        }
        FilterOutcome::Error(error) => {
            // R4: a filter failure is surfaced to the agent AND logged — never
            // swallowed, never silently disabling the watch. The cursor already
            // advanced past the failed region, so the watch fires normally on a
            // later matching append.
            tracing::warn!(
                watch = %exec.watch.watch_id,
                process = %exec.watch.process_id,
                %error,
                "watch filter execution failed; alerting the agent and keeping the watch attached",
            );
            emit(exec, start, end, WatchAlertKind::Error { error });
        }
    }
}

/// Build a [`WatchAlert`] and deliver it through the notifier, or log it when
/// no notifier is wired (a bare manager with no owning agent).
fn emit(exec: &WatchExec, spool_start: u64, spool_end: u64, kind: WatchAlertKind) {
    let alert = WatchAlert {
        watch_id: exec.watch.watch_id.clone(),
        process_id: exec.watch.process_id.clone(),
        brief: exec.watch.brief.clone(),
        spool_start,
        spool_end,
        kind,
    };
    if let Some(notifier) = exec.notifier.as_ref() {
        notifier.deliver_watch_alert(alert);
    } else {
        tracing::debug!(
            watch = %alert.watch_id,
            process = %alert.process_id,
            "no watch-alert sink wired; alert is not injected (bare manager with no owning agent)",
        );
    }
}

/// Run `filter` via `sh -c` with `region` on stdin, in `cwd` with `env`. Exit 0
/// is a match (stdout is the excerpt); a clean non-zero exit is no match;
/// anything else is an [`FilterOutcome::Error`]. Reads nothing from the spool
/// file — the region is handed in, so a filter cannot desynchronize cursors.
pub(super) async fn run_filter(
    filter: &str,
    region: &[u8],
    cwd: &Path,
    env: Option<&ProcessEnv>,
) -> FilterOutcome {
    let governor = match crate::resource::DescriptorGovernor::global() {
        Ok(governor) => governor,
        Err(error) => {
            return FilterOutcome::Error(format!(
                "watch filter descriptor admission unavailable: {error}"
            ));
        }
    };
    let _permit = match governor.try_acquire(crate::resource::THREE_PIPE_SPAWN_PEAK) {
        Ok(permit) => permit,
        Err(error) => {
            return FilterOutcome::Error(format!(
                "watch filter descriptor admission failed: {error}"
            ));
        }
    };
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(filter)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // An aborted watch task (unwatch, shutdown) drops this `Child` at its
        // await point; `kill_on_drop` guarantees the filter process is reaped
        // rather than orphaned to outlive norn.
        .kill_on_drop(true)
        .current_dir(cwd);
    if let Some(env) = env {
        for (key, value) in env.iter() {
            command.env(key, value);
        }
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return FilterOutcome::Error(format!("failed to spawn filter `sh -c`: {error}"));
        }
    };
    // Drive the stdin write CONCURRENTLY with output collection. A region and a
    // filter stdout both larger than the pipe buffer (~64KB) would otherwise
    // wedge permanently: writing the whole region before reading any output
    // blocks the executor once the filter's unread stdout fills its pipe, while
    // the filter blocks writing that stdout because nothing is draining it. The
    // `join!` reads stdout/stderr as the region is fed in, so neither side
    // stalls (NP-002 R2 byte-equality holds at any region size).
    let stdin = child.stdin.take();
    let write = async move {
        let Some(mut stdin) = stdin else {
            return Ok(());
        };
        match stdin.write_all(region).await {
            // Drop stdin so the filter observes EOF once the region is fed.
            Ok(()) => {
                drop(stdin);
                Ok(())
            }
            // The filter may legitimately close stdin early (e.g. `head -1`);
            // a broken pipe is not a failure (already ruled not-a-failure).
            Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => {
                drop(stdin);
                Ok(())
            }
            // Any other write error is a genuine filter-execution failure.
            Err(error) => Err(format!("failed to write region to filter stdin: {error}")),
        }
    };
    let (output, write_result) = tokio::join!(child.wait_with_output(), write);
    // A genuine stdin write failure (not an early close) is a watch-error, even
    // if the filter still produced an exit status — the region was not fully
    // delivered, so no verdict over it is trustworthy.
    if let Err(error) = write_result {
        return FilterOutcome::Error(error);
    }
    match output {
        Ok(output) if output.status.success() => {
            FilterOutcome::Match(String::from_utf8_lossy(&output.stdout).into_owned())
        }
        Ok(output) => match output.status.code() {
            // `sh` reserves 127 for "command not found" and 126 for "found but
            // not executable" — the filter itself cannot run (NP-002 R4: a
            // filter referencing a nonexistent binary is a watch-error, not a
            // no-match). A runnable filter deciding no-match exits with any
            // other non-zero code (grep returns 1), which R2 rules as no match.
            Some(127) => FilterOutcome::Error(
                "filter is not runnable: `sh -c` reported command not found (exit 127) — the \
                 filter references a binary that does not exist or is not on PATH"
                    .to_owned(),
            ),
            Some(126) => FilterOutcome::Error(
                "filter is not runnable: `sh -c` reported the command is not executable (exit 126)"
                    .to_owned(),
            ),
            Some(_) => FilterOutcome::NoMatch,
            None => FilterOutcome::Error(signal_description(output.status)),
        },
        Err(error) => FilterOutcome::Error(format!("failed to await filter process: {error}")),
    }
}

/// Describe a filter process that terminated without an exit code (a signal
/// kill), distinctly from a clean no-match.
fn signal_description(status: std::process::ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return format!("filter process was killed by signal {signal}");
        }
    }
    format!("filter process terminated without an exit code ({status})")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn cwd() -> PathBuf {
        std::env::current_dir().unwrap()
    }

    #[tokio::test]
    async fn exit_zero_is_a_match_carrying_stdout() {
        let region = b"info: ok\nERROR: boom\ninfo: fine\n";
        let outcome = run_filter("grep ERROR", region, &cwd(), None).await;
        match outcome {
            FilterOutcome::Match(excerpt) => assert_eq!(excerpt, "ERROR: boom\n"),
            other => panic!("expected match, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn clean_non_zero_exit_is_no_match() {
        let region = b"all good here\n";
        let outcome = run_filter("grep ERROR", region, &cwd(), None).await;
        assert!(matches!(outcome, FilterOutcome::NoMatch), "got {outcome:?}");
    }

    #[tokio::test]
    async fn a_filter_matching_twice_returns_its_full_stdout() {
        let region = b"ERROR one\nok\nERROR two\n";
        let outcome = run_filter("grep ERROR", region, &cwd(), None).await;
        match outcome {
            FilterOutcome::Match(excerpt) => {
                assert!(excerpt.contains("ERROR one"), "{excerpt}");
                assert!(excerpt.contains("ERROR two"), "{excerpt}");
            }
            other => panic!("expected match, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_nonexistent_binary_is_a_watch_error_not_a_no_match() {
        // R4: a filter referencing a nonexistent binary is a watch-error (sh
        // reports 127, command not found), distinct from a runnable filter's
        // clean non-zero no-match.
        let region = b"anything\n";
        let outcome = run_filter("this-binary-does-not-exist-xyz", region, &cwd(), None).await;
        match outcome {
            FilterOutcome::Error(error) => assert!(
                error.contains("127") || error.contains("not runnable"),
                "the spawn/not-found failure must be named: {error}"
            ),
            other => panic!("expected a watch-error for a nonexistent binary, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_filter_killed_by_signal_is_an_error_distinct_from_no_match() {
        // The filter kills itself with SIGKILL: no exit code, so an Error.
        let region = b"payload\n";
        let outcome = run_filter("kill -9 $$", region, &cwd(), None).await;
        match outcome {
            FilterOutcome::Error(error) => assert!(
                error.contains("signal"),
                "signal kill should be named: {error}"
            ),
            other => panic!("expected a signal error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn env_is_visible_to_the_filter() {
        let region = b"x\n";
        let env = ProcessEnv::new([("WATCH_TEST_TOKEN", "sentinel")]);
        // Match only if the env var is visible with the expected value.
        let outcome = run_filter(
            "test \"$WATCH_TEST_TOKEN\" = sentinel && echo matched",
            region,
            &cwd(),
            Some(&env),
        )
        .await;
        match outcome {
            FilterOutcome::Match(excerpt) => assert_eq!(excerpt.trim(), "matched"),
            other => panic!("env not visible: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_one_megabyte_region_through_cat_completes_byte_equal() {
        // Regression for the filter I/O deadlock: a region far larger than the
        // pipe buffer (~64KB) fed through a filter whose stdout is equally
        // large (`cat` echoes every byte). Before driving the stdin write
        // concurrently with output collection this wedged forever — the
        // executor blocked writing stdin while the filter blocked writing its
        // unread stdout. It must now complete with a byte-equal excerpt (the R2
        // byte-equality property at scale). A distinctive repeating pattern
        // (not a single byte) guards against any silent truncation/reordering.
        let mut region = Vec::with_capacity(1_000_000);
        for i in 0..1_000_000_u32 {
            region.push(b"ABCDEFGH"[(i % 8) as usize]);
        }
        let outcome = run_filter("cat", &region, &cwd(), None).await;
        match outcome {
            FilterOutcome::Match(excerpt) => {
                assert_eq!(
                    excerpt.as_bytes(),
                    region.as_slice(),
                    "the excerpt must be byte-equal to the 1MB region",
                );
            }
            other => panic!("a 1MB region through `cat` should match byte-equal: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_filter_closing_stdin_early_is_not_a_write_failure() {
        // `head -c1` reads one byte then exits, closing stdin while the region
        // is large — the resulting broken pipe must not be reported as an error.
        let region = vec![b'a'; 1_000_000];
        let outcome = run_filter("head -c1 >/dev/null; echo done", &region, &cwd(), None).await;
        match outcome {
            FilterOutcome::Match(excerpt) => assert_eq!(excerpt.trim(), "done"),
            other => panic!("early stdin close should still match: {other:?}"),
        }
    }
}
