//! Summary-only projection removes known encrypted blobs while leaving replay authority untouched.

use crate::provider::response_item::ResponseItem;
use serde_json::Value;

/// Render canonical fields without presenting opaque ciphertext as readable history.
/// Only known reasoning/compaction payloads are omitted; similarly named application
/// fields and unknown item kinds retain their exact contents.
pub(super) fn render(item: &ResponseItem) -> String {
    let encrypted = match item {
        ResponseItem::Reasoning(reasoning) => reasoning.encrypted_content(),
        ResponseItem::Compaction(compaction) => Some(compaction.encrypted_content()),
        _ => None,
    };
    if let Some(payload) = encrypted.filter(|value| !value.is_empty())
        && let Some(fields) = item.raw().as_object()
    {
        let visible = fields
            .iter()
            .filter(|(key, _)| key.as_str() != "encrypted_content")
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        return format!(
            "{}\n[Summary projection: omitted {} bytes of opaque encrypted_content; original provider item retained in session history. This payload is not readable text and its contents are not summarized here.]",
            Value::Object(visible),
            payload.len(),
        );
    }
    item.raw().to_string()
}

#[cfg(test)]
#[path = "summary_item_tests.rs"]
mod tests;
