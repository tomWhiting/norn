//! Mapping for authoritative `response.output_item.done` frames.

use crate::error::ProviderError;
use crate::provider::events::ProviderEvent;
use crate::provider::response_item::{
    ResponseItem, ResponseStreamProvenance, ResponseTranscriptItem,
};

use super::sse::SseEvent;

pub(super) fn map_completed_item(event: &SseEvent) -> Result<ProviderEvent, ProviderError> {
    let Some(raw_item) = event.data.get("item") else {
        return Err(ProviderError::ResponseParseError {
            reason: "response.output_item.done carried no item".to_owned(),
        });
    };
    let item = match ResponseItem::from_value(raw_item.clone()) {
        Ok(item) => item,
        Err(error) => {
            return Err(ProviderError::ResponseParseError {
                reason: format!("response.output_item.done carried a malformed item: {error}"),
            });
        }
    };
    let provenance = ResponseStreamProvenance {
        item_id: event
            .data
            .get("item_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .or_else(|| item.id().map(str::to_owned)),
        output_index: event
            .data
            .get("output_index")
            .and_then(serde_json::Value::as_u64),
        content_index: event
            .data
            .get("content_index")
            .and_then(serde_json::Value::as_u64),
        sequence_number: event
            .data
            .get("sequence_number")
            .and_then(serde_json::Value::as_u64),
    };
    Ok(ProviderEvent::ResponseItemDone {
        item: ResponseTranscriptItem { item, provenance },
    })
}
