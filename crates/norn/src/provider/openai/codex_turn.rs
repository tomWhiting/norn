//! Trusted Codex request projection for one live Norn turn.

use std::io;

use reqwest::header::{HeaderMap, HeaderName};
use serde::Serialize;

use crate::error::ProviderError;
use crate::provider::turn::{CODEX_TURN_STATE_HEADER, ProviderTurnContext};

#[derive(Serialize)]
struct TurnMetadata<'identity> {
    session_id: &'identity str,
    thread_id: &'identity str,
    turn_id: &'identity str,
    request_kind: &'static str,
}

#[derive(Serialize)]
struct ClientMetadata<'identity> {
    session_id: &'identity str,
    thread_id: &'identity str,
    turn_id: &'identity str,
    #[serde(rename = "x-codex-turn-metadata")]
    turn_metadata: String,
}

/// Builds the sticky-routing header for a later request in this turn.
pub(super) fn request_headers(context: Option<&ProviderTurnContext>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Some(value) = context.and_then(ProviderTurnContext::codex_turn_state_header) {
        headers.insert(HeaderName::from_static(CODEX_TURN_STATE_HEADER), value);
    }
    headers
}

/// Adds Norn's honest projection of the pinned Codex client metadata fields.
///
/// The caller supplies a context only for the compiled OAuth Codex backend.
/// Missing identities are omitted rather than replaced with invented values.
pub(super) fn insert_client_metadata(
    payload: &mut serde_json::Value,
    context: Option<&ProviderTurnContext>,
) -> Result<(), ProviderError> {
    let Some(context) = context else {
        return Ok(());
    };
    let Some(session_id) = context.session_id() else {
        return Ok(());
    };
    let turn_id = context.turn_id();
    if turn_id.is_empty() {
        return Ok(());
    }

    let turn_metadata = TurnMetadata {
        session_id,
        thread_id: session_id,
        turn_id,
        request_kind: "turn",
    };
    let turn_metadata = to_ascii_json_string(&turn_metadata).map_err(|error| {
        ProviderError::RequestSerializationFailed {
            reason: format!("failed to serialize Codex turn metadata: {error}"),
        }
    })?;
    let client_metadata = serde_json::to_value(ClientMetadata {
        session_id,
        thread_id: session_id,
        turn_id,
        turn_metadata,
    })
    .map_err(|error| ProviderError::RequestSerializationFailed {
        reason: format!("failed to serialize Codex client metadata: {error}"),
    })?;
    let object =
        payload
            .as_object_mut()
            .ok_or_else(|| ProviderError::RequestSerializationFailed {
                reason: "responses payload was not a JSON object".to_owned(),
            })?;
    object.insert("client_metadata".to_owned(), client_metadata);
    Ok(())
}

/// Serializer used by Codex for the nested metadata string at source pin
/// `0396f99cf1a27fc87dd12d23403b25e840b6ecbd`.
fn to_ascii_json_string<T: Serialize + ?Sized>(value: &T) -> serde_json::Result<String> {
    let mut bytes = Vec::new();
    let mut serializer = serde_json::Serializer::with_formatter(&mut bytes, AsciiJsonFormatter);
    value.serialize(&mut serializer)?;
    String::from_utf8(bytes)
        .map_err(|error| serde_json::Error::io(io::Error::new(io::ErrorKind::InvalidData, error)))
}

struct AsciiJsonFormatter;

impl serde_json::ser::Formatter for AsciiJsonFormatter {
    fn write_string_fragment<W>(&mut self, writer: &mut W, fragment: &str) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        let mut start = 0;
        for (index, character) in fragment.char_indices() {
            if character.is_ascii() {
                continue;
            }
            if start < index {
                writer.write_all(&fragment.as_bytes()[start..index])?;
            }
            let mut utf16 = [0; 2];
            for code_unit in character.encode_utf16(&mut utf16) {
                write!(writer, "\\u{code_unit:04x}")?;
            }
            start = index + character.len_utf8();
        }
        if start < fragment.len() {
            writer.write_all(&fragment.as_bytes()[start..])?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_metadata_uses_real_ids_and_ascii_nested_json() -> Result<(), ProviderError> {
        let context = ProviderTurnContext::new(Some("sess-é".to_owned()), "turn-λ".to_owned());
        let mut payload = serde_json::json!({"model": "gpt-test"});
        insert_client_metadata(&mut payload, Some(&context))?;

        assert_eq!(payload["client_metadata"]["session_id"], "sess-é");
        assert_eq!(payload["client_metadata"]["thread_id"], "sess-é");
        assert_eq!(payload["client_metadata"]["turn_id"], "turn-λ");
        assert_eq!(
            payload["client_metadata"]["x-codex-turn-metadata"],
            r#"{"session_id":"sess-\u00e9","thread_id":"sess-\u00e9","turn_id":"turn-\u03bb","request_kind":"turn"}"#
        );
        Ok(())
    }

    #[test]
    fn missing_identity_omits_client_metadata() -> Result<(), ProviderError> {
        let mut payload = serde_json::json!({"model": "gpt-test"});
        insert_client_metadata(&mut payload, Some(&ProviderTurnContext::default()))?;
        assert!(payload.get("client_metadata").is_none());
        Ok(())
    }
}
