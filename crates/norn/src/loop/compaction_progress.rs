//! Single-owner compaction progress: terminal status is published before any later audit await.

use uuid::Uuid;

use crate::provider::{AgentCompactionProgress, AgentEventSender, CompactionPhase};
use crate::session::events::EventId;

/// Owns exactly one actual admitted operation, resolving abandonment on drop.
pub(super) struct CompactionProgressGuard<'a> {
    sender: Option<&'a AgentEventSender>,
    operation_id: Uuid,
    resolved: bool,
}

impl<'a> CompactionProgressGuard<'a> {
    pub(super) fn start(sender: Option<&'a AgentEventSender>) -> Self {
        let guard = Self {
            sender,
            operation_id: Uuid::new_v4(),
            resolved: false,
        };
        guard.emit(CompactionPhase::Started);
        guard
    }

    pub(super) fn finished(&mut self, compaction_id: EventId, mechanical_fallback: bool) {
        self.resolve(CompactionPhase::Finished {
            compaction_id,
            mechanical_fallback,
        });
    }

    pub(super) fn failed(&mut self) {
        self.resolve(CompactionPhase::Failed);
    }

    fn resolve(&mut self, phase: CompactionPhase) {
        self.resolved = true;
        self.emit(phase);
    }

    fn emit(&self, phase: CompactionPhase) {
        if let Some(sender) = self.sender {
            sender.send_compaction_progress(AgentCompactionProgress {
                operation_id: self.operation_id,
                phase,
            });
        }
    }
}

impl Drop for CompactionProgressGuard<'_> {
    fn drop(&mut self) {
        if !self.resolved {
            self.emit(CompactionPhase::Cancelled);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{AgentEvent, AgentEventKind};

    #[test]
    fn guard_publishes_one_terminal_transition_with_the_same_operation_and_agent()
    -> Result<(), Box<dyn std::error::Error>> {
        for outcome in [CompactionPhase::Failed, CompactionPhase::Cancelled] {
            let (tx, mut rx) = tokio::sync::broadcast::channel::<AgentEvent>(4);
            let agent = Uuid::new_v4();
            let sender = AgentEventSender::new(tx, agent, "root".into());
            {
                let mut guard = CompactionProgressGuard::start(Some(&sender));
                if outcome == CompactionPhase::Failed {
                    guard.failed();
                }
            }
            let started = rx.try_recv()?;
            let ended = rx.try_recv()?;
            assert_eq!(started.agent_id, agent);
            assert_eq!(ended.agent_id, agent);
            let AgentEventKind::CompactionProgress(started) = started.event else {
                return Err("missing actual start".into());
            };
            let AgentEventKind::CompactionProgress(ended) = ended.event else {
                return Err("missing actual terminal status".into());
            };
            assert_eq!(started.phase, CompactionPhase::Started);
            assert_eq!(started.operation_id, ended.operation_id);
            assert_eq!(ended.phase, outcome);
            assert!(matches!(
                rx.try_recv(),
                Err(tokio::sync::broadcast::error::TryRecvError::Empty)
            ));
        }
        Ok(())
    }
}
