//! Lightweight timing trace for TUI startup.

use std::time::{Duration, Instant};

const TARGET: &str = "norn_cli::tui::startup";

/// Records cumulative and per-stage TUI startup timing.
#[derive(Debug)]
pub(super) struct StartupTrace {
    started_at: Instant,
    last_mark: Instant,
}

impl StartupTrace {
    /// Start a new startup timing trace.
    pub(super) fn start() -> Self {
        let now = Instant::now();
        tracing::info!(
            target: TARGET,
            stage = "start",
            elapsed_ms = 0_u128,
            delta_ms = 0_u128,
            "tui startup milestone",
        );
        Self {
            started_at: now,
            last_mark: now,
        }
    }

    /// Record a plain milestone.
    pub(super) fn mark(&mut self, stage: &'static str) {
        let timing = self.advance();
        tracing::info!(
            target: TARGET,
            stage,
            elapsed_ms = timing.elapsed_ms,
            delta_ms = timing.delta_ms,
            "tui startup milestone",
        );
    }

    /// Record a milestone with a count, such as events or tools loaded.
    pub(super) fn mark_count(
        &mut self,
        stage: &'static str,
        count_label: &'static str,
        count: usize,
    ) {
        let timing = self.advance();
        tracing::info!(
            target: TARGET,
            stage,
            elapsed_ms = timing.elapsed_ms,
            delta_ms = timing.delta_ms,
            count_label,
            count,
            "tui startup milestone",
        );
    }

    /// Record the opened session and its replayed event count.
    pub(super) fn mark_session(
        &mut self,
        stage: &'static str,
        session_id: &str,
        event_count: usize,
        persisted: bool,
    ) {
        let timing = self.advance();
        tracing::info!(
            target: TARGET,
            stage,
            elapsed_ms = timing.elapsed_ms,
            delta_ms = timing.delta_ms,
            session_id,
            event_count,
            persisted,
            "tui startup milestone",
        );
    }

    fn advance(&mut self) -> StartupTiming {
        let now = Instant::now();
        let timing = StartupTiming {
            elapsed_ms: millis_since(now.duration_since(self.started_at)),
            delta_ms: millis_since(now.duration_since(self.last_mark)),
        };
        self.last_mark = now;
        timing
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StartupTiming {
    elapsed_ms: u128,
    delta_ms: u128,
}

fn millis_since(duration: Duration) -> u128 {
    duration.as_millis()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::millis_since;

    #[test]
    fn millis_since_reports_whole_milliseconds() {
        assert_eq!(millis_since(Duration::from_micros(1_999)), 1);
    }
}
