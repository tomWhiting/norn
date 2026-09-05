//! Exhaustive live-event dispositions with volatile attempts and no provider-state bodies.

use crate::provider::agent_event::{AgentEventKind, AgentMessageLifecycle, SubagentLifecycle};
use crate::provider::events::{ProviderEvent, StopReason};
use crate::provider::reasoning::ReasoningSummaryPart;
use crate::provider::response_audio::ResponseAudioEvent;
use crate::provider::response_item::ResponseTranscriptItem;

use super::body::BodyRepresentation;
use super::contract::{AttemptKey, CoverageGap, ItemId, SegmentKey, ViewItemKind};
use super::error::ViewError;
use super::projection::SessionProjection;
use super::response::{ResponsePartKind, response_parts};

impl SessionProjection {
    pub(super) fn reduce_live(
        &mut self,
        event: &AgentEventKind,
    ) -> Result<(Option<AttemptKey>, bool), ViewError> {
        match event {
            AgentEventKind::Observed(observed) => return self.reduce_observed(observed),
            AgentEventKind::Provider(event) => return self.reduce_provider(event),
            AgentEventKind::StreamRetry(retry) => {
                let attempt = self
                    .execution
                    .as_ref()
                    .ok_or(ViewError::NoExecution)?
                    .attempt
                    .clone();
                if retry.attempt <= attempt.attempt {
                    return Err(ViewError::AttemptMismatch);
                }
                self.invalidate_attempt(&attempt, true);
                if let Some(execution) = &mut self.execution {
                    execution.attempt.attempt = retry.attempt;
                    execution.completed_text = false;
                    execution.completed_thinking = false;
                    execution.generic_text.clear();
                    execution.generic_thinking.clear();
                    execution.last_segment = None;
                }
                self.record_notice(
                    ViewItemKind::Notice,
                    &format!(
                        "Retry attempt {} in {} ms ({})",
                        retry.attempt, retry.delay_ms, retry.error_class
                    ),
                )?;
            }
            AgentEventKind::UsageEstimate(estimate) => {
                self.record_notice(
                    ViewItemKind::Metadata,
                    &format!("Estimated next input: {} tokens", estimate.input_tokens),
                )?;
            }
            AgentEventKind::Subagent(lifecycle) => {
                if lifecycle.child_id() != self.source.agent_id {
                    return Err(ViewError::AgentMismatch {
                        expected: self.source.agent_id,
                        actual: lifecycle.child_id(),
                    });
                }
                let label = match lifecycle {
                    SubagentLifecycle::Started { descriptor, .. } => {
                        format!("Child started: {} ({})", descriptor.role, descriptor.model)
                    }
                    SubagentLifecycle::Completed {
                        descriptor,
                        succeeded,
                        ..
                    } => format!(
                        "Child completed: {} (succeeded: {succeeded})",
                        descriptor.role
                    ),
                };
                self.typed_local(ViewItemKind::Child, &label, lifecycle)?;
            }
            AgentEventKind::Message(message) => {
                let (agent, label) = match message {
                    AgentMessageLifecycle::Sent {
                        from_id, from, to, ..
                    } => (*from_id, format!("Message sent from {from} to {to}")),
                    AgentMessageLifecycle::Delivered { to_id, from, .. } => {
                        (*to_id, format!("Message delivered from {from}"))
                    }
                };
                if agent != self.source.agent_id {
                    return Err(ViewError::AgentMismatch {
                        expected: self.source.agent_id,
                        actual: agent,
                    });
                }
                self.typed_local(ViewItemKind::ExternalInput, &label, message)?;
            }
            AgentEventKind::McpChannel(message) => {
                if message.recipient_id != self.source.agent_id {
                    return Err(ViewError::AgentMismatch {
                        expected: self.source.agent_id,
                        actual: message.recipient_id,
                    });
                }
                self.observe_event(
                    &message.event_id,
                    ViewItemKind::ExternalInput,
                    &format!(
                        "Channel {} generation {} sequence {} (event {})",
                        message.source, message.generation, message.sequence, message.event_id
                    ),
                    &message.content,
                    BodyRepresentation::Text,
                )?;
            }
            AgentEventKind::Compaction(compaction) => {
                let text = serde_json::to_string(compaction).map_err(|source| {
                    ViewError::LiveBodyMalformed {
                        referent: compaction.compaction_id.to_string(),
                        source,
                    }
                })?;
                self.observe_event(
                    &compaction.compaction_id,
                    ViewItemKind::Context,
                    &format!(
                        "Committed compaction {}: {} → {} tokens",
                        compaction.compaction_id, compaction.tokens_before, compaction.tokens_after
                    ),
                    &text,
                    BodyRepresentation::Json,
                )?;
            }
        }
        Ok((None, false))
    }

    pub(super) fn reduce_provider(
        &mut self,
        event: &ProviderEvent,
    ) -> Result<(Option<AttemptKey>, bool), ViewError> {
        match event {
            ProviderEvent::ResponseStreamEvent { .. } => {
                // Its explicit disposition is observable to the adapter. Raw
                // transport frames do not become duplicate transcript rows.
                return Ok((None, true));
            }
            ProviderEvent::ResponseAudioFrame { event, .. } => {
                match event {
                    ResponseAudioEvent::AudioDelta { .. }
                    | ResponseAudioEvent::AudioDone { .. } => {
                        self.record_notice(
                            ViewItemKind::Notice,
                            "Provider audio metadata (audio bytes unavailable)",
                        )?;
                    }
                    ResponseAudioEvent::TranscriptDelta { delta, .. } => self.local_body(
                        ViewItemKind::Text,
                        "Provider audio transcript",
                        delta,
                        BodyRepresentation::Text,
                    )?,
                    ResponseAudioEvent::TranscriptDone { .. } => {
                        self.record_notice(
                            ViewItemKind::Notice,
                            "Provider audio transcript complete",
                        )?;
                    }
                }
                return Ok((None, true));
            }
            ProviderEvent::TextDelta { text } => self.text_fragment(text, false, false)?,
            ProviderEvent::TextComplete { text } => self.text_fragment(text, false, true)?,
            ProviderEvent::ThinkingDelta { text } => self.text_fragment(text, true, false)?,
            ProviderEvent::ThinkingComplete { text } => self.text_fragment(text, true, true)?,
            ProviderEvent::RefusalDelta {
                item_id,
                output_index,
                content_index,
                refusal,
            } => self.refusal(item_id, *output_index, *content_index, refusal, true)?,
            ProviderEvent::RefusalComplete {
                item_id,
                output_index,
                content_index,
                refusal,
            } => self.refusal(item_id, *output_index, *content_index, refusal, false)?,
            ProviderEvent::ToolCallDelta {
                item_id,
                call_id,
                name,
                arguments_delta,
                kind,
            } => self.live_tool_delta(
                item_id,
                call_id.as_deref(),
                name.as_deref(),
                arguments_delta,
                *kind,
            )?,
            ProviderEvent::ToolCallComplete {
                call_id,
                name,
                arguments,
                kind,
            } => self.live_tool_complete(call_id, name, arguments, *kind)?,
            ProviderEvent::ToolResult {
                tool_call_id,
                tool_name,
                output,
                duration_ms,
            } => self.live_tool_result(tool_call_id, tool_name, output, *duration_ms)?,
            ProviderEvent::ReasoningItemDone { item } => {
                let execution = self.execution.as_ref().ok_or(ViewError::NoExecution)?;
                let thinking_present =
                    execution.completed_thinking || !execution.generic_thinking.is_empty();
                if !thinking_present {
                    for (part, summary) in item.summary.iter().enumerate() {
                        let ReasoningSummaryPart::SummaryText { text } = summary;
                        let key = self.key(SegmentKey::ResponsePart {
                            item_id: (!item.id.is_empty()).then(|| item.id.clone()),
                            output_index: None,
                            part,
                        })?;
                        self.put_fragment(
                            key,
                            ViewItemKind::Thinking,
                            "Thinking summary",
                            text,
                            false,
                        )?;
                    }
                }
            }
            ProviderEvent::ResponseItemDone { item } => self.completed_item(item)?,
            ProviderEvent::Compaction { item_type, .. } => {
                self.record_notice(
                    ViewItemKind::Unavailable,
                    &format!("Provider {item_type}: opaque compaction body unavailable"),
                )?;
                return Ok((None, true));
            }
            ProviderEvent::Done {
                stop_reason,
                usage,
                response_id,
            } => {
                let execution = self.execution.as_mut().ok_or(ViewError::NoExecution)?;
                let completed = execution.attempt.clone();
                if self.publication.scope.is_none() {
                    execution.attempt.response = execution.attempt.response.checked_add(1).ok_or(
                        ViewError::CounterExhausted {
                            counter: "response iteration",
                        },
                    )?;
                    execution.attempt.attempt = 1;
                }
                execution.completed_text = false;
                execution.completed_thinking = false;
                execution.generic_text.clear();
                execution.generic_thinking.clear();
                execution.last_segment = None;
                execution.segment = 0;
                let stop = match stop_reason {
                    StopReason::EndTurn => "end turn",
                    StopReason::ContinueTurn => "continue turn",
                    StopReason::ToolUse => "tool use",
                    StopReason::MaxTokens => "max tokens",
                    StopReason::ContentFilter => "content filter",
                };
                let completion_item = self.record_notice(
                    ViewItemKind::Metadata,
                    &format!(
                        "Response {} ended: {stop}; input {}, output {} tokens",
                        response_id.as_deref().unwrap_or("identifier unavailable"),
                        usage.input_tokens,
                        usage.output_tokens
                    ),
                )?;
                self.items.bind_completion(&completion_item, &completed)?;
                self.completion_item = Some(completion_item);
                return Ok((Some(completed), false));
            }
            ProviderEvent::Error { error } => {
                self.coverage.gaps.insert(CoverageGap::Interrupted);
                self.record_notice(ViewItemKind::Error, &error.to_string())?;
            }
        }
        Ok((None, false))
    }

    fn typed_local(
        &mut self,
        kind: ViewItemKind,
        label: &str,
        value: &impl serde::Serialize,
    ) -> Result<(), ViewError> {
        let text = serde_json::to_string(value).map_err(|source| ViewError::LiveBodyMalformed {
            referent: label.to_owned(),
            source,
        })?;
        self.local_body(kind, label, &text, BodyRepresentation::Json)
    }

    fn text_fragment(
        &mut self,
        text: &str,
        thinking: bool,
        complete: bool,
    ) -> Result<(), ViewError> {
        let execution = self.execution.as_mut().ok_or(ViewError::NoExecution)?;
        let already_complete = if thinking {
            execution.completed_thinking
        } else {
            execution.completed_text
        };
        let generic = if thinking {
            &execution.generic_thinking
        } else {
            &execution.generic_text
        };
        if complete && already_complete && generic.is_empty() {
            return Ok(());
        }
        if !complete {
            if thinking {
                execution.completed_thinking = false;
            } else {
                execution.completed_text = false;
            }
        }
        let matches_kind = |segment: &SegmentKey| {
            matches!(
                (thinking, segment),
                (true, SegmentKey::Thinking(_)) | (false, SegmentKey::Text(_))
            )
        };
        let segment = if let Some(segment) = execution
            .last_segment
            .as_ref()
            .filter(|segment| matches_kind(segment))
        {
            segment.clone()
        } else {
            execution.segment =
                execution
                    .segment
                    .checked_add(1)
                    .ok_or(ViewError::CounterExhausted {
                        counter: "semantic segment",
                    })?;
            if thinking {
                SegmentKey::Thinking(execution.segment)
            } else {
                SegmentKey::Text(execution.segment)
            }
        };
        execution.last_segment = Some(segment.clone());
        let generic = if thinking {
            &mut execution.generic_thinking
        } else {
            &mut execution.generic_text
        };
        if !generic.contains(&segment) {
            generic.push(segment.clone());
        }
        let key = self.key(segment)?;
        self.put_fragment(
            key,
            if thinking {
                ViewItemKind::Thinking
            } else {
                ViewItemKind::Text
            },
            if thinking {
                "Thinking summary"
            } else {
                "Assistant"
            },
            text,
            !complete,
        )
    }

    fn refusal(
        &mut self,
        item_id: &str,
        output_index: u64,
        content_index: u64,
        text: &str,
        append: bool,
    ) -> Result<(), ViewError> {
        let key = self.key(SegmentKey::Refusal {
            item_id: item_id.to_owned(),
            output_index,
            content_index,
        })?;
        self.put_fragment(key, ViewItemKind::Refusal, "Model refusal", text, append)
    }

    fn completed_item(&mut self, item: &ResponseTranscriptItem) -> Result<(), ViewError> {
        let parts = response_parts(0, &item.item);
        let has_text = parts
            .iter()
            .any(|part| matches!(part.kind, ResponsePartKind::Text));
        let has_thinking = parts
            .iter()
            .any(|part| matches!(part.kind, ResponsePartKind::Thinking));
        let execution = self.execution.as_mut().ok_or(ViewError::NoExecution)?;
        let attempt = execution.attempt.clone();
        let mut removed = Vec::new();
        if has_text {
            removed.append(&mut execution.generic_text);
            execution.completed_text = true;
        }
        if has_thinking {
            removed.append(&mut execution.generic_thinking);
            execution.completed_thinking = true;
        }
        if !removed.is_empty() {
            execution.last_segment = None;
        }
        for segment in removed {
            let key = super::contract::ProvisionalKey {
                source: self.source.clone(),
                attempt: attempt.clone(),
                segment,
            };
            self.items.remove(&ItemId::Provisional(key.clone()));
            self.bodies.remove(&key);
        }
        let item_id = item
            .item
            .id()
            .map(str::to_owned)
            .or_else(|| item.provenance.item_id.clone());
        for (part_index, part) in parts.into_iter().enumerate() {
            let key = self.key(SegmentKey::ResponsePart {
                item_id: item_id.clone(),
                output_index: item.provenance.output_index,
                part: part_index,
            })?;
            match part.kind {
                ResponsePartKind::Text => {
                    self.put_fragment(key, ViewItemKind::Text, "Assistant", part.text, false)?;
                }
                ResponsePartKind::Thinking => self.put_fragment(
                    key,
                    ViewItemKind::Thinking,
                    "Thinking summary",
                    part.text,
                    false,
                )?,
                ResponsePartKind::Refusal => self.put_fragment(
                    key,
                    ViewItemKind::Refusal,
                    "Model refusal",
                    part.text,
                    false,
                )?,
                ResponsePartKind::Tool {
                    call_id,
                    name,
                    kind,
                } => self.live_tool_complete(call_id, name, part.text, kind)?,
                ResponsePartKind::Unavailable => {
                    self.record_notice(ViewItemKind::Unavailable, part.text)?;
                }
            }
        }
        Ok(())
    }
}
