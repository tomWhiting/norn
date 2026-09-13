//! Subtree-scoped selection of actual agent histories; no agent or session activation.

use std::sync::Arc;

use uuid::Uuid;

use crate::session::store::{HistoryReadError, SessionHistoryReader};
use crate::session_view::ViewSource;

use super::ActionLogTree;

/// A requested agent conversation cannot be read through this tree.
#[derive(Debug, thiserror::Error)]
pub enum AgentHistoryError {
    /// The caller or target has no registered log; no liveness is inferred.
    #[error("agent {agent_id} has no registered conversation in this tree")]
    Unregistered {
        /// Missing caller or target identity.
        agent_id: Uuid,
    },
    /// A registered target lies outside the caller's own subtree.
    #[error("agent {caller} cannot read agent {target} outside its subtree")]
    OutsideSubtree {
        /// Requesting agent from the runtime's authenticated context.
        caller: Uuid,
        /// Selected conversation owner.
        target: Uuid,
    },
    /// The registered log is bound to another agent or parent.
    #[error("agent {target} with parent {parent:?} has mismatched history source {view_source:?}")]
    SourceMismatch {
        /// Registered agent identity.
        target: Uuid,
        /// Parent recorded in this tree.
        parent: Option<Uuid>,
        /// Actual source reported by the selected store.
        view_source: Box<ViewSource>,
    },
    /// The selected store's actual owner has not bound it or no longer admits it.
    #[error("agent {agent_id} history could not be opened: {source}")]
    History {
        /// Agent whose store refused the read capability.
        agent_id: Uuid,
        /// Source/binding failure, preserved for diagnosis.
        #[source]
        source: HistoryReadError,
    },
}

impl ActionLogTree {
    /// Select the caller's own or a descendant's actual conversation store.
    ///
    /// The caller identity must come from the runtime, not model-supplied input.
    /// Registration and ancestry are checked in one tree snapshot; store binding
    /// validation happens after releasing the tree lock. Completed children remain
    /// readable while their registered log is retained. Reading grants no message
    /// delivery or execution authority and says nothing about current liveness.
    ///
    /// # Errors
    /// Refuses absent callers/targets, out-of-subtree requests, unbound stores and
    /// logs whose bound agent/parent disagrees with their tree registration.
    pub fn history_reader(
        &self,
        caller: Uuid,
        target: Uuid,
    ) -> Result<SessionHistoryReader, AgentHistoryError> {
        let (log, parent) = {
            let inner = self.inner.read();
            if !inner.logs.contains_key(&caller) {
                return Err(AgentHistoryError::Unregistered { agent_id: caller });
            }
            let log = inner
                .logs
                .get(&target)
                .ok_or(AgentHistoryError::Unregistered { agent_id: target })?;
            if !inner.is_in_subtree(caller, target) {
                return Err(AgentHistoryError::OutsideSubtree { caller, target });
            }
            (Arc::clone(log), inner.parents.get(&target).copied())
        };
        let reader = log
            .history_reader()
            .map_err(|source| AgentHistoryError::History {
                agent_id: target,
                source,
            })?;
        if reader.source().agent_id != target || reader.source().parent_agent_id != parent {
            return Err(AgentHistoryError::SourceMismatch {
                target,
                parent,
                view_source: Box::new(reader.source().clone()),
            });
        }
        Ok(reader)
    }
}
