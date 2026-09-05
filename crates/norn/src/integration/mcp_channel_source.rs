//! Connection-scoped channel authority, startup staging and generation publication.

use std::sync::Arc;

use uuid::Uuid;

use super::mcp_channel_inbox::{InboxShared, InboxState, RetainedMessage};
use super::mcp_channels::{
    ChannelParams, McpChannelCapabilityRequirement, McpChannelError, McpChannelMessage,
    McpChannelOverflow, McpChannelPolicy, McpChannelRefusal, McpChannelRejection,
};

/// Caller-created authority for exactly one connection; source identity is host-bound later.
pub struct McpChannelAttachment {
    pub(super) shared: Arc<InboxShared>,
    pub(super) policy: McpChannelPolicy,
    pub(super) overflow: McpChannelOverflow,
    pub(super) capability_requirement: McpChannelCapabilityRequirement,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SourcePhase {
    Initializing,
    Ready,
    Active,
    NotDeclared,
    Retired,
}

pub(super) struct SourceRecord {
    pub(super) name: String,
    pub(super) phase: SourcePhase,
    pub(super) policy: McpChannelPolicy,
    pub(super) terminal_refusal: Option<McpChannelRefusal>,
}

pub(crate) struct ChannelSource {
    shared: Arc<InboxShared>,
    name: String,
    generation: u64,
    overflow: McpChannelOverflow,
}

impl McpChannelAttachment {
    /// Keep ordinary MCP tools when initialization does not advertise channel input.
    /// Staged notifications from an undeclared source are still refused.
    #[must_use]
    pub fn if_advertised(mut self) -> Self {
        self.capability_requirement = McpChannelCapabilityRequirement::IfAdvertised;
        self
    }

    pub(crate) fn bind(
        self,
        name: String,
        generation: u64,
    ) -> Result<ChannelSource, McpChannelError> {
        {
            let mut state = self.shared.state.lock();
            if state.closed {
                return Err(McpChannelError::Closed {
                    recipient_id: state.recipient_id,
                });
            }
            if state.sources.contains_key(&generation) {
                return Err(McpChannelError::Source {
                    source_name: name,
                    generation,
                    reason: McpChannelRefusal::AccountingExhausted,
                });
            }
            state.sources.insert(
                generation,
                SourceRecord {
                    name: name.clone(),
                    phase: SourcePhase::Initializing,
                    policy: self.policy,
                    terminal_refusal: None,
                },
            );
        }
        Ok(ChannelSource {
            shared: self.shared,
            name,
            generation,
            overflow: self.overflow,
        })
    }
}

impl ChannelSource {
    fn rejection(&self, state: &InboxState, reason: McpChannelRefusal) -> McpChannelRejection {
        McpChannelRejection {
            source: self.name.clone(),
            generation: self.generation,
            recipient_id: state.recipient_id,
            reason,
        }
    }

    fn error(&self, reason: McpChannelRefusal) -> McpChannelError {
        McpChannelError::Source {
            source_name: self.name.clone(),
            generation: self.generation,
            reason,
        }
    }

    pub(crate) fn reject(&self, reason: McpChannelRefusal) {
        let mut state = self.shared.state.lock();
        if reason == McpChannelRefusal::NotDeclared
            && let Some(source) = state.sources.get_mut(&self.generation)
        {
            source.terminal_refusal = Some(reason);
        }
        let rejection = self.rejection(&state, reason);
        self.shared.reject(&mut state, rejection);
    }

    /// Nonblocking admission; the reader never awaits the receiving task or free capacity.
    pub(crate) fn receive(&self, params: serde_json::Value) {
        let params = match ChannelParams::parse(params) {
            Ok(params) => params,
            Err(reason) => {
                self.reject(reason);
                return;
            }
        };
        let mut state = self.shared.state.lock();
        if let Err(reason) = self.admit(&mut state, params) {
            match self.overflow {
                McpChannelOverflow::RejectNew => {
                    let rejection = self.rejection(&state, reason);
                    self.shared.reject(&mut state, rejection);
                }
            }
        } else {
            self.shared.publish(&state);
        }
    }

    fn admit(
        &self,
        state: &mut InboxState,
        params: ChannelParams,
    ) -> Result<(), McpChannelRefusal> {
        if state.closed {
            return Err(McpChannelRefusal::Closed);
        }
        let source = state
            .sources
            .get(&self.generation)
            .ok_or(McpChannelRefusal::Retired)?;
        if source.phase == SourcePhase::Retired {
            return Err(McpChannelRefusal::Retired);
        }
        if source.phase == SourcePhase::NotDeclared {
            return Err(McpChannelRefusal::NotDeclared);
        }
        if state.queue.len() >= state.limits.max_retained_messages() {
            return Err(McpChannelRefusal::FullCount);
        }
        let bytes = params
            .retained_bytes(&self.name)
            .ok_or(McpChannelRefusal::AccountingExhausted)?;
        let retained_bytes = state
            .retained_bytes
            .checked_add(bytes)
            .ok_or(McpChannelRefusal::AccountingExhausted)?;
        if retained_bytes > state.limits.max_retained_bytes() {
            return Err(McpChannelRefusal::FullBytes);
        }
        let sequence = state
            .sequence
            .checked_add(1)
            .ok_or(McpChannelRefusal::AccountingExhausted)?;
        state.queue.push_back(RetainedMessage {
            message: Arc::new(McpChannelMessage {
                id: Uuid::new_v4(),
                source: self.name.clone(),
                generation: self.generation,
                recipient_id: state.recipient_id,
                sequence,
                content: params.content,
                meta: params.meta,
            }),
            bytes,
            policy: source.policy,
            published: source.phase == SourcePhase::Active,
            claimed: false,
        });
        state.retained_bytes = retained_bytes;
        state.sequence = sequence;
        Ok(())
    }

    pub(crate) fn negotiated(&self) -> Result<(), McpChannelError> {
        let mut state = self.shared.state.lock();
        if state.closed {
            return Err(McpChannelError::Closed {
                recipient_id: state.recipient_id,
            });
        }
        let source = state
            .sources
            .get_mut(&self.generation)
            .ok_or_else(|| self.error(McpChannelRefusal::Retired))?;
        if source.phase != SourcePhase::Initializing {
            return Err(self.error(McpChannelRefusal::Retired));
        }
        source.phase = SourcePhase::Ready;
        Ok(())
    }

    pub(crate) fn not_declared(&self) -> Result<(), McpChannelError> {
        let mut state = self.shared.state.lock();
        let source = state
            .sources
            .get_mut(&self.generation)
            .ok_or_else(|| self.error(McpChannelRefusal::Retired))?;
        if source.phase != SourcePhase::Initializing {
            return Err(self.error(McpChannelRefusal::Retired));
        }
        source.phase = SourcePhase::NotDeclared;
        source.terminal_refusal = Some(McpChannelRefusal::NotDeclared);
        self.discard_staged(&mut state);
        self.shared.publish(&state);
        Ok(())
    }

    pub(crate) fn active_policy(&self) -> Option<McpChannelPolicy> {
        let state = self.shared.state.lock();
        state.sources.get(&self.generation).and_then(|source| {
            (source.phase == SourcePhase::Active && !state.closed).then_some(source.policy)
        })
    }

    pub(crate) fn activate(&self) -> Result<(), McpChannelError> {
        let mut state = self.shared.state.lock();
        if state.closed {
            return Err(McpChannelError::Closed {
                recipient_id: state.recipient_id,
            });
        }
        let phase = state
            .sources
            .get(&self.generation)
            .ok_or_else(|| self.error(McpChannelRefusal::Retired))?
            .phase;
        if phase == SourcePhase::Active {
            return Ok(());
        }
        if phase == SourcePhase::NotDeclared {
            return Err(McpChannelError::NotEnabled);
        }
        if phase == SourcePhase::Retired {
            return Err(self.error(McpChannelRefusal::Retired));
        }
        if phase != SourcePhase::Ready {
            return Err(self.error(McpChannelRefusal::NotDeclared));
        }
        for (generation, source) in &mut state.sources {
            if source.name == self.name
                && *generation != self.generation
                && source.phase == SourcePhase::Active
            {
                source.phase = SourcePhase::Retired;
            }
        }
        let source = state
            .sources
            .get_mut(&self.generation)
            .ok_or_else(|| self.error(McpChannelRefusal::Retired))?;
        source.phase = SourcePhase::Active;
        for item in &mut state.queue {
            if item.message.generation == self.generation {
                item.published = true;
            }
        }
        self.shared.publish(&state);
        Ok(())
    }

    pub(crate) fn retire(&self) -> Result<(), McpChannelError> {
        let mut state = self.shared.state.lock();
        let source = state
            .sources
            .get_mut(&self.generation)
            .ok_or_else(|| self.error(McpChannelRefusal::Retired))?;
        if source.phase != SourcePhase::NotDeclared {
            source.phase = SourcePhase::Retired;
        }
        self.discard_staged(&mut state);
        self.shared.publish(&state);
        Ok(())
    }

    fn discard_staged(&self, state: &mut InboxState) {
        // Published messages belong to the inbox and remain available after disconnect.
        // Only never-activated candidate messages are rejected when their source retires.
        let reason = state
            .sources
            .get(&self.generation)
            .and_then(|source| source.terminal_refusal)
            .unwrap_or(McpChannelRefusal::CandidateAbandoned);
        let mut index = 0;
        while index < state.queue.len() {
            let discard = state
                .queue
                .get(index)
                .is_some_and(|item| item.message.generation == self.generation && !item.published);
            if discard {
                if let Some(item) = state.queue.remove(index) {
                    if let Some(bytes) = state.retained_bytes.checked_sub(item.bytes) {
                        state.retained_bytes = bytes;
                    } else {
                        state.closed = true;
                        let rejection =
                            self.rejection(state, McpChannelRefusal::AccountingExhausted);
                        self.shared.reject(state, rejection);
                    }
                    let rejection = self.rejection(state, reason);
                    self.shared.reject(state, rejection);
                }
            } else {
                index += 1;
            }
        }
    }
}

impl Drop for ChannelSource {
    fn drop(&mut self) {
        let mut state = self.shared.state.lock();
        self.discard_staged(&mut state);
        state.sources.remove(&self.generation);
        self.shared.publish(&state);
    }
}
