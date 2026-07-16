//! Stable tool-call correlation across announcement and completion authority.

use serde_json::Value;

use super::{ResponseItemIdentity, ResponseReconciler, ResponseReconciliationError};

impl ResponseReconciler {
    pub(super) fn bind_announced_call(
        &mut self,
        identity: &ResponseItemIdentity,
        raw: &Value,
        item_type: &str,
        event_type: &'static str,
    ) -> Result<(), ResponseReconciliationError> {
        let Some((call_id, _)) = call_identity(raw, item_type, event_type)? else {
            return Ok(());
        };
        self.bind_call_id(identity, call_id)
    }

    pub(super) fn validate_authoritative_call(
        &mut self,
        identity: &ResponseItemIdentity,
        raw: &Value,
        item_type: &str,
        event_type: &'static str,
    ) -> Result<(), ResponseReconciliationError> {
        let Some((call_id, name)) = call_identity(raw, item_type, event_type)? else {
            return Ok(());
        };
        if let Some(announcement) = self.added.get(identity) {
            let announced_call_id = announcement.raw.get("call_id").and_then(Value::as_str);
            let announced_name = announcement.raw.get("name").and_then(Value::as_str);
            if announced_call_id != Some(call_id) {
                return Err(ResponseReconciliationError::AnnouncedCallIdConflict);
            }
            if announced_name != Some(name) {
                return Err(ResponseReconciliationError::AnnouncedCallNameConflict);
            }
            if let Some(announced_caller) = announcement.raw.get("caller")
                && Some(announced_caller) != raw.get("caller")
            {
                return Err(ResponseReconciliationError::AnnouncedCallCallerConflict);
            }
        }
        self.bind_call_id(identity, call_id)
    }

    fn bind_call_id(
        &mut self,
        identity: &ResponseItemIdentity,
        call_id: &str,
    ) -> Result<(), ResponseReconciliationError> {
        if let Some(prior) = self.call_ids_to_items.get(call_id)
            && prior != identity
        {
            return Err(ResponseReconciliationError::CallIdReused);
        }
        self.call_ids_to_items
            .insert(call_id.to_owned(), identity.clone());
        Ok(())
    }
}

fn call_identity<'a>(
    raw: &'a Value,
    item_type: &str,
    event_type: &'static str,
) -> Result<Option<(&'a str, &'a str)>, ResponseReconciliationError> {
    if !matches!(item_type, "function_call" | "custom_tool_call") {
        return Ok(None);
    }
    let call_id = raw.get("call_id").and_then(Value::as_str).ok_or(
        ResponseReconciliationError::InvalidEnvelopeField {
            event_type,
            field: "item.call_id",
        },
    )?;
    let name = raw.get("name").and_then(Value::as_str).ok_or(
        ResponseReconciliationError::InvalidEnvelopeField {
            event_type,
            field: "item.name",
        },
    )?;
    Ok(Some((call_id, name)))
}
