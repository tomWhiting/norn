//! Item-scoped content, media-preview, and hosted-tool reconciliation.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use super::roles::{HostedFamily, HostedPhase, ItemStringKind};
use super::wire::required_u64;
use super::{
    ReconcileUpdate, ResponseItemIdentity, ResponseReconciler, ResponseReconciliationError,
};
use crate::provider::openai::sse::SseEvent;

mod authority;
mod schema;

use schema::{validate_content_part, validate_reasoning_summary_part};

#[derive(Debug, Default)]
pub(super) struct ItemChannelState {
    parts: BTreeMap<(ResponseItemIdentity, u64), ContentPartState>,
    summary_parts: BTreeMap<(ResponseItemIdentity, u64), ContentPartState>,
    annotations: BTreeMap<(ResponseItemIdentity, u64, u64), Value>,
    image_previews: BTreeMap<(ResponseItemIdentity, u64), String>,
    strings: BTreeMap<(ResponseItemIdentity, ItemStringKind), StringState>,
    lifecycles: BTreeMap<(ResponseItemIdentity, HostedFamily), HostedPhase>,
    touched: BTreeSet<ResponseItemIdentity>,
}

#[derive(Debug, Default)]
struct ContentPartState {
    seed: Option<Value>,
    done: Option<Value>,
}

#[derive(Debug, Default)]
struct StringState {
    preview: String,
    done: Option<String>,
}

impl ResponseReconciler {
    pub(super) fn add_content_part(
        &mut self,
        event: &SseEvent,
    ) -> Result<ReconcileUpdate, ResponseReconciliationError> {
        let part = required_object_value(event, "response.content_part.added", "part")?;
        let item_type = validate_content_part(&part, "response.content_part.added")?;
        let identity = self.announced_item_identity(event, item_type)?;
        let content_index = required_u64(event, "response.content_part.added", "content_index")?;
        let key = (identity.clone(), content_index);
        let state = self.item_channels.parts.entry(key).or_default();
        if state.done.is_some() {
            return Err(ResponseReconciliationError::ItemScopedEventAfterCompletion);
        }
        match &state.seed {
            Some(seed) if seed != &part => {
                return Err(ResponseReconciliationError::ConflictingItemScopedPreview);
            }
            Some(_) => {}
            None => state.seed = Some(part),
        }
        self.item_channels.touched.insert(identity);
        Ok(ReconcileUpdate::Accepted)
    }

    pub(super) fn complete_content_part(
        &mut self,
        event: &SseEvent,
    ) -> Result<ReconcileUpdate, ResponseReconciliationError> {
        let part = required_object_value(event, "response.content_part.done", "part")?;
        let item_type = validate_content_part(&part, "response.content_part.done")?;
        let identity = self.announced_item_identity(event, item_type)?;
        let content_index = required_u64(event, "response.content_part.done", "content_index")?;
        let state = self
            .item_channels
            .parts
            .entry((identity.clone(), content_index))
            .or_default();
        if let Some(done) = &state.done {
            return if done == &part {
                Ok(ReconcileUpdate::DuplicateChannelCompletion)
            } else {
                Err(ResponseReconciliationError::ConflictingItemScopedCompletion)
            };
        }
        state.done = Some(part);
        self.item_channels.touched.insert(identity);
        Ok(ReconcileUpdate::Accepted)
    }

    pub(super) fn add_reasoning_summary_part(
        &mut self,
        event: &SseEvent,
    ) -> Result<ReconcileUpdate, ResponseReconciliationError> {
        let part = required_object_value(event, "response.reasoning_summary_part.added", "part")?;
        validate_reasoning_summary_part(&part, "response.reasoning_summary_part.added")?;
        let identity = self.announced_item_identity(event, "reasoning")?;
        let summary_index = required_u64(
            event,
            "response.reasoning_summary_part.added",
            "summary_index",
        )?;
        let state = self
            .item_channels
            .summary_parts
            .entry((identity.clone(), summary_index))
            .or_default();
        if state.done.is_some() {
            return Err(ResponseReconciliationError::ItemScopedEventAfterCompletion);
        }
        match &state.seed {
            Some(seed) if seed != &part => {
                return Err(ResponseReconciliationError::ConflictingItemScopedPreview);
            }
            Some(_) => {}
            None => state.seed = Some(part),
        }
        self.item_channels.touched.insert(identity);
        Ok(ReconcileUpdate::Accepted)
    }

    pub(super) fn complete_reasoning_summary_part(
        &mut self,
        event: &SseEvent,
    ) -> Result<ReconcileUpdate, ResponseReconciliationError> {
        let part = required_object_value(event, "response.reasoning_summary_part.done", "part")?;
        validate_reasoning_summary_part(&part, "response.reasoning_summary_part.done")?;
        let identity = self.announced_item_identity(event, "reasoning")?;
        let summary_index = required_u64(
            event,
            "response.reasoning_summary_part.done",
            "summary_index",
        )?;
        let state = self
            .item_channels
            .summary_parts
            .entry((identity.clone(), summary_index))
            .or_default();
        if let Some(done) = &state.done {
            return if done == &part {
                Ok(ReconcileUpdate::DuplicateChannelCompletion)
            } else {
                Err(ResponseReconciliationError::ConflictingItemScopedCompletion)
            };
        }
        state.done = Some(part);
        self.item_channels.touched.insert(identity);
        Ok(ReconcileUpdate::Accepted)
    }

    pub(super) fn add_annotation(
        &mut self,
        event: &SseEvent,
    ) -> Result<ReconcileUpdate, ResponseReconciliationError> {
        let identity = self.announced_item_identity(event, "message")?;
        let content_index = required_u64(
            event,
            "response.output_text.annotation.added",
            "content_index",
        )?;
        let annotation_index = required_u64(
            event,
            "response.output_text.annotation.added",
            "annotation_index",
        )?;
        let part = self
            .item_channels
            .parts
            .get(&(identity.clone(), content_index))
            .ok_or(ResponseReconciliationError::AnnotationWithoutContentPart)?;
        if part.done.is_some() {
            return Err(ResponseReconciliationError::ItemScopedEventAfterCompletion);
        }
        let part_type = part
            .seed
            .as_ref()
            .and_then(|value| value.get("type"))
            .and_then(Value::as_str);
        if part_type != Some("output_text") {
            return Err(ResponseReconciliationError::ItemScopedFamilyConflict);
        }
        let annotation = event
            .data
            .get("annotation")
            .filter(|value| !value.is_null())
            .cloned()
            .ok_or(ResponseReconciliationError::InvalidEnvelopeField {
                event_type: "response.output_text.annotation.added",
                field: "annotation",
            })?;
        let key = (identity.clone(), content_index, annotation_index);
        if let Some(prior) = self.item_channels.annotations.get(&key) {
            return if prior == &annotation {
                Ok(ReconcileUpdate::Accepted)
            } else {
                Err(ResponseReconciliationError::ConflictingItemScopedPreview)
            };
        }
        self.item_channels.annotations.insert(key, annotation);
        self.item_channels.touched.insert(identity);
        Ok(ReconcileUpdate::Accepted)
    }

    pub(super) fn add_image_partial(
        &mut self,
        event: &SseEvent,
    ) -> Result<ReconcileUpdate, ResponseReconciliationError> {
        let identity = self.announced_item_identity(event, "image_generation_call")?;
        self.reject_closed_hosted_channel(&identity, HostedFamily::ImageGeneration)?;
        let partial_index = required_u64(
            event,
            "response.image_generation_call.partial_image",
            "partial_image_index",
        )?;
        let image = required_string(
            event,
            "response.image_generation_call.partial_image",
            "partial_image_b64",
        )?;
        let key = (identity.clone(), partial_index);
        if let Some(prior) = self.item_channels.image_previews.get(&key) {
            return if prior == image {
                Ok(ReconcileUpdate::Accepted)
            } else {
                Err(ResponseReconciliationError::ConflictingItemScopedPreview)
            };
        }
        self.item_channels
            .image_previews
            .insert(key, image.to_owned());
        self.item_channels.touched.insert(identity);
        Ok(ReconcileUpdate::Accepted)
    }

    pub(super) fn append_item_string(
        &mut self,
        event: &SseEvent,
        kind: ItemStringKind,
    ) -> Result<ReconcileUpdate, ResponseReconciliationError> {
        let family = family_for_string(kind);
        let identity = self.announced_item_identity(event, item_type_for_family(family))?;
        self.reject_closed_hosted_channel(&identity, family)?;
        let delta = required_string(event, delta_event_name(kind), "delta")?;
        let state = self
            .item_channels
            .strings
            .entry((identity.clone(), kind))
            .or_default();
        if state.done.is_some() {
            return Err(ResponseReconciliationError::ItemScopedEventAfterCompletion);
        }
        state.preview.push_str(delta);
        self.item_channels.touched.insert(identity);
        Ok(ReconcileUpdate::Accepted)
    }

    pub(super) fn complete_item_string(
        &mut self,
        event: &SseEvent,
        kind: ItemStringKind,
    ) -> Result<ReconcileUpdate, ResponseReconciliationError> {
        let family = family_for_string(kind);
        let identity = self.announced_item_identity(event, item_type_for_family(family))?;
        self.reject_closed_hosted_channel(&identity, family)?;
        let authoritative = required_string(event, done_event_name(kind), done_field(kind))?;
        let state = self
            .item_channels
            .strings
            .entry((identity.clone(), kind))
            .or_default();
        if let Some(done) = &state.done {
            return if done == authoritative {
                Ok(ReconcileUpdate::DuplicateChannelCompletion)
            } else {
                Err(ResponseReconciliationError::ConflictingItemScopedCompletion)
            };
        }
        authoritative.clone_into(&mut state.preview);
        state.done = Some(authoritative.to_owned());
        self.item_channels.touched.insert(identity);
        Ok(ReconcileUpdate::Accepted)
    }

    pub(super) fn observe_hosted_lifecycle(
        &mut self,
        event: &SseEvent,
        family: HostedFamily,
        phase: HostedPhase,
    ) -> Result<ReconcileUpdate, ResponseReconciliationError> {
        let identity = self.announced_item_identity(event, item_type_for_family(family))?;
        let key = (identity.clone(), family);
        if let Some(prior) = self.item_channels.lifecycles.get(&key) {
            if prior == &phase {
                return Ok(ReconcileUpdate::Accepted);
            }
            if !valid_lifecycle_transition(*prior, phase) {
                return Err(ResponseReconciliationError::ConflictingHostedLifecycle);
            }
        }
        self.item_channels.lifecycles.insert(key, phase);
        self.item_channels.touched.insert(identity);
        Ok(ReconcileUpdate::Accepted)
    }

    pub(super) fn content_part_is_done(
        &self,
        identity: &ResponseItemIdentity,
        content_index: u64,
    ) -> bool {
        self.item_channels
            .parts
            .get(&(identity.clone(), content_index))
            .is_some_and(|part| part.done.is_some())
    }

    pub(super) fn reasoning_summary_part_is_done(
        &self,
        identity: &ResponseItemIdentity,
        summary_index: u64,
    ) -> bool {
        self.item_channels
            .summary_parts
            .get(&(identity.clone(), summary_index))
            .is_some_and(|part| part.done.is_some())
    }

    fn announced_item_identity(
        &mut self,
        event: &SseEvent,
        expected_type: &'static str,
    ) -> Result<ResponseItemIdentity, ResponseReconciliationError> {
        let identity = self.bind_envelope_identity(event)?;
        if self.completed.contains_key(&identity) {
            return Err(ResponseReconciliationError::ItemScopedEventAfterCompletion);
        }
        let item_type = self
            .added
            .get(&identity)
            .and_then(|item| item.raw.get("type"))
            .and_then(Value::as_str)
            .ok_or(ResponseReconciliationError::UnannouncedItemScopedIdentity)?;
        if item_type != expected_type {
            return Err(ResponseReconciliationError::ItemScopedFamilyConflict);
        }
        Ok(identity)
    }

    fn reject_closed_hosted_channel(
        &self,
        identity: &ResponseItemIdentity,
        family: HostedFamily,
    ) -> Result<(), ResponseReconciliationError> {
        match self
            .item_channels
            .lifecycles
            .get(&(identity.clone(), family))
        {
            Some(HostedPhase::Completed | HostedPhase::Failed) => {
                Err(ResponseReconciliationError::ItemScopedEventAfterCompletion)
            }
            Some(HostedPhase::InProgress | HostedPhase::Active) | None => Ok(()),
        }
    }
}

fn required_object_value(
    event: &SseEvent,
    event_type: &'static str,
    field: &'static str,
) -> Result<Value, ResponseReconciliationError> {
    event
        .data
        .get(field)
        .filter(|value| value.is_object())
        .cloned()
        .ok_or(ResponseReconciliationError::InvalidEnvelopeField { event_type, field })
}

fn required_string<'a>(
    event: &'a SseEvent,
    event_type: &'static str,
    field: &'static str,
) -> Result<&'a str, ResponseReconciliationError> {
    event
        .data
        .get(field)
        .and_then(Value::as_str)
        .ok_or(ResponseReconciliationError::InvalidEnvelopeField { event_type, field })
}

const fn family_for_string(kind: ItemStringKind) -> HostedFamily {
    match kind {
        ItemStringKind::McpArguments => HostedFamily::McpCall,
        ItemStringKind::CodeInterpreterCode => HostedFamily::CodeInterpreter,
    }
}

const fn item_type_for_family(family: HostedFamily) -> &'static str {
    match family {
        HostedFamily::FileSearch => "file_search_call",
        HostedFamily::WebSearch => "web_search_call",
        HostedFamily::ImageGeneration => "image_generation_call",
        HostedFamily::McpCall => "mcp_call",
        HostedFamily::McpListTools => "mcp_list_tools",
        HostedFamily::CodeInterpreter => "code_interpreter_call",
    }
}

const fn delta_event_name(kind: ItemStringKind) -> &'static str {
    match kind {
        ItemStringKind::McpArguments => "response.mcp_call_arguments.delta",
        ItemStringKind::CodeInterpreterCode => "response.code_interpreter_call_code.delta",
    }
}

const fn done_event_name(kind: ItemStringKind) -> &'static str {
    match kind {
        ItemStringKind::McpArguments => "response.mcp_call_arguments.done",
        ItemStringKind::CodeInterpreterCode => "response.code_interpreter_call_code.done",
    }
}

const fn done_field(kind: ItemStringKind) -> &'static str {
    match kind {
        ItemStringKind::McpArguments => "arguments",
        ItemStringKind::CodeInterpreterCode => "code",
    }
}

const fn valid_lifecycle_transition(prior: HostedPhase, next: HostedPhase) -> bool {
    match (prior, next) {
        (HostedPhase::Completed | HostedPhase::Failed, _)
        | (HostedPhase::Active, HostedPhase::InProgress) => false,
        (HostedPhase::InProgress | HostedPhase::Active, _) => true,
    }
}
