//! Root context occupancy and actual compaction status, separate from accumulated billing.

use std::time::Instant;

use norn::provider::{AgentCompactionProgress, CompactionPhase};
use uuid::Uuid;

#[derive(Default)]
pub(super) struct ContextStatus {
    window: Option<u64>,
    input_estimate: Option<u64>,
    operation: Option<(Uuid, CompactionPhase, Instant)>,
    uncertain: bool,
}

impl ContextStatus {
    /// Use only the actual runtime limit at accepted model/session boundaries.
    pub(super) fn set_window(&mut self, window: Option<u64>) {
        self.window = window.filter(|value| *value > 0);
        self.invalidate_estimate();
    }

    pub(super) fn invalidate_estimate(&mut self) {
        self.input_estimate = None;
    }

    pub(super) fn estimate(&mut self, input_tokens: u64) {
        self.input_estimate = Some(input_tokens);
    }

    pub(super) fn context_label(&self) -> String {
        match (self.input_estimate, self.window) {
            (Some(input), Some(window)) => {
                let percent = u128::from(input) * 100 / u128::from(window);
                format!("~{percent}% context")
            }
            _ => "context unavailable".into(),
        }
    }

    pub(super) fn record(&mut self, progress: &AgentCompactionProgress, now: Instant) {
        if let Some((operation, phase, _)) = &self.operation {
            if *operation != progress.operation_id
                && !matches!(progress.phase, CompactionPhase::Started)
            {
                return;
            }
            // Replayed observations cannot reopen a completed operation.
            if *operation == progress.operation_id && !matches!(phase, CompactionPhase::Started) {
                return;
            }
        }
        let started = self
            .operation
            .as_ref()
            .map_or(now, |(operation, _, started)| {
                if *operation == progress.operation_id {
                    *started
                } else {
                    now
                }
            });
        self.operation = Some((progress.operation_id, progress.phase.clone(), started));
        self.uncertain = false;
        if matches!(progress.phase, CompactionPhase::Finished { .. }) {
            self.invalidate_estimate();
        }
    }

    /// Missing terminal observation means uncertainty, never fabricated success.
    pub(super) fn observation_lost(&mut self) {
        if self
            .operation
            .as_ref()
            .is_some_and(|(_, phase, _)| matches!(phase, CompactionPhase::Started))
        {
            self.uncertain = true;
        }
    }

    pub(super) fn clear_activity(&mut self) {
        self.operation = None;
        self.uncertain = false;
    }

    /// Normal provider work supersedes terminal status, but not an active operation.
    pub(super) fn normal_phase(&mut self) {
        if self.uncertain
            || self
                .operation
                .as_ref()
                .is_some_and(|(_, phase, _)| !matches!(phase, CompactionPhase::Started))
        {
            self.clear_activity();
        }
    }

    pub(super) fn activity_label(&self, now: Instant) -> Option<String> {
        if self.uncertain {
            return Some("compaction status unavailable".into());
        }
        let (_, phase, started) = self.operation.as_ref()?;
        Some(match phase {
            CompactionPhase::Started => {
                let seconds = now.saturating_duration_since(*started).as_secs();
                let glyph = match seconds % 4 {
                    0 => "◐",
                    1 => "◓",
                    2 => "◑",
                    _ => "◒",
                };
                format!("{glyph} compacting {seconds}s")
            }
            CompactionPhase::Finished {
                mechanical_fallback: true,
                ..
            } => "compacted (mechanical fallback)".into(),
            CompactionPhase::Finished {
                mechanical_fallback: false,
                ..
            } => "compacted".into(),
            CompactionPhase::Failed => "compaction failed".into(),
            CompactionPhase::Cancelled => "compaction cancelled".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use norn::session::events::EventId;
    use std::time::Duration;

    #[test]
    fn context_requires_current_numerator_and_actual_nonzero_window() {
        let mut status = ContextStatus::default();
        status.estimate(250);
        assert_eq!(status.context_label(), "context unavailable");
        status.set_window(Some(1_000));
        assert_eq!(status.context_label(), "context unavailable");
        status.estimate(250);
        assert_eq!(status.context_label(), "~25% context");
        status.estimate(1_250);
        assert_eq!(status.context_label(), "~125% context");
        status.set_window(Some(2_000));
        assert_eq!(status.context_label(), "context unavailable");
        status.estimate(250);
        assert_eq!(status.context_label(), "~12% context");
        status.invalidate_estimate();
        assert_eq!(status.context_label(), "context unavailable");
        status.set_window(Some(0));
        status.estimate(1);
        assert_eq!(status.context_label(), "context unavailable");
    }

    #[test]
    fn lifecycle_uses_exact_operation_and_uncertainty_never_means_finished() {
        let mut status = ContextStatus::default();
        let now = Instant::now();
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        status.record(
            &AgentCompactionProgress {
                operation_id: first,
                phase: CompactionPhase::Started,
            },
            now,
        );
        assert_eq!(
            status.activity_label(now).as_deref(),
            Some("◐ compacting 0s")
        );
        assert_eq!(
            status
                .activity_label(now + Duration::from_secs(1))
                .as_deref(),
            Some("◓ compacting 1s")
        );
        status.record(
            &AgentCompactionProgress {
                operation_id: second,
                phase: CompactionPhase::Started,
            },
            now,
        );
        status.record(
            &AgentCompactionProgress {
                operation_id: first,
                phase: CompactionPhase::Cancelled,
            },
            now,
        );
        assert_eq!(
            status.activity_label(now).as_deref(),
            Some("◐ compacting 0s")
        );
        status.observation_lost();
        assert_eq!(
            status.activity_label(now).as_deref(),
            Some("compaction status unavailable")
        );
        status.record(
            &AgentCompactionProgress {
                operation_id: second,
                phase: CompactionPhase::Finished {
                    compaction_id: EventId::new(),
                    mechanical_fallback: true,
                },
            },
            now,
        );
        assert_eq!(
            status.activity_label(now).as_deref(),
            Some("compacted (mechanical fallback)")
        );
        status.record(
            &AgentCompactionProgress {
                operation_id: second,
                phase: CompactionPhase::Started,
            },
            now,
        );
        status.observation_lost();
        assert_eq!(
            status.activity_label(now).as_deref(),
            Some("compacted (mechanical fallback)")
        );
        status.clear_activity();
        assert_eq!(status.activity_label(now), None);
    }
}
