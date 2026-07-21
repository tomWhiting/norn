use std::io;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn message_round_trip_preserves_phase_annotations_refusal_and_unknown_fields() -> TestResult {
    let raw = serde_json::json!({
        "type": "message",
        "id": "msg_1",
        "role": "assistant",
        "status": "completed",
        "phase": "commentary",
        "future_top_level": {"kept": true},
        "content": [
            {
                "type": "output_text",
                "text": "answer",
                "annotations": [{"type": "url_citation", "url": "https://example.com"}],
                "logprobs": [],
                "future_part_field": 7
            },
            {"type": "refusal", "refusal": "cannot comply", "future_refusal_field": true},
            {"type": "future_media", "payload": {"bytes": "opaque"}}
        ]
    });
    let item = ResponseItem::from_value(raw.clone())?;
    let Some(message) = item.as_message() else {
        return Err(io::Error::other("message item was not classified").into());
    };
    assert_eq!(message.phase(), ResponseNullable::Value("commentary"),);
    assert_eq!(message.content().len(), 3);
    let encoded = serde_json::to_value(&item)?;
    assert_eq!(encoded, raw);
    let decoded: ResponseItem = serde_json::from_value(encoded)?;
    assert_eq!(decoded.raw(), &raw);
    Ok(())
}

#[test]
fn message_phase_distinguishes_absent_null_and_value() -> TestResult {
    for (phase, expected) in [
        (None, ResponseNullable::Absent),
        (Some(Value::Null), ResponseNullable::Null),
        (
            Some(Value::String("final_answer".to_owned())),
            ResponseNullable::Value("final_answer"),
        ),
    ] {
        let mut raw = serde_json::json!({
            "type": "message",
            "id": "msg_phase",
            "role": "assistant",
            "status": "completed",
            "content": []
        });
        if let Some(phase) = phase {
            raw["phase"] = phase;
        }
        let item = ResponseItem::from_value(raw.clone())?;
        let Some(message) = item.as_message() else {
            return Err(io::Error::other("message item was not classified").into());
        };
        assert_eq!(message.phase(), expected);
        assert_eq!(serde_json::to_value(item)?, raw);
    }
    Ok(())
}

#[test]
fn reasoning_round_trip_preserves_unknown_parts() -> TestResult {
    let raw = serde_json::json!({
        "type": "reasoning",
        "id": "rs_1",
        "summary": [{"type": "future_summary", "payload": [1, 2]}],
        "content": [{"type": "future_reasoning", "value": {"x": 1}}],
        "encrypted_content": "ciphertext",
        "future_field": "kept"
    });
    let item = ResponseItem::from_value(raw.clone())?;
    let Some(reasoning) = item.as_reasoning() else {
        return Err(io::Error::other("reasoning item was not classified").into());
    };
    assert_eq!(reasoning.summary().len(), 1);
    assert_eq!(reasoning.content().map(<[Value]>::len), Some(1));
    assert_eq!(reasoning.encrypted_content(), Some("ciphertext"));
    assert_eq!(
        reasoning.encrypted_content_field(),
        ResponseNullable::Value("ciphertext"),
    );
    assert_eq!(serde_json::to_value(item)?, raw);
    Ok(())
}

#[test]
fn unknown_item_round_trips_without_becoming_executable() -> TestResult {
    let raw = serde_json::json!({
        "type": "future_hosted_call",
        "id": "future_1",
        "action": {"arbitrary": true}
    });
    let item = ResponseItem::from_value(raw.clone())?;
    let ResponseItem::Opaque(opaque) = &item else {
        return Err(io::Error::other("unknown item did not remain opaque").into());
    };
    assert_eq!(opaque.item_type(), "future_hosted_call");
    assert!(item.as_function_call().is_none());
    assert!(item.as_custom_tool_call().is_none());
    assert_eq!(serde_json::to_value(item)?, raw);
    Ok(())
}

#[test]
fn pinned_non_core_item_is_known_not_future_opaque() -> TestResult {
    let raw = serde_json::json!({
        "type": "image_generation_call",
        "id": "img_1",
        "status": "completed",
        "result": "base64-provider-data"
    });
    let item = ResponseItem::from_value(raw.clone())?;
    let ResponseItem::Known(known) = &item else {
        return Err(io::Error::other("public image item was not classified as known").into());
    };
    assert_eq!(known.kind(), KnownResponseItemKind::ImageGenerationCall);
    assert_eq!(item.item_type(), "image_generation_call");
    assert!(item.as_function_call().is_none());
    assert_eq!(serde_json::to_value(item)?, raw);
    Ok(())
}

#[test]
fn stream_provenance_is_outside_replayable_item_json() -> TestResult {
    let item = ResponseItem::from_value(serde_json::json!({
        "type": "web_search_call",
        "id": "ws_1",
        "status": "completed",
        "action": {"type": "search", "query": "norn"}
    }))?;
    let transcript_item = ResponseTranscriptItem {
        item,
        provenance: ResponseStreamProvenance {
            item_id: Some("ws_1".to_owned()),
            output_index: Some(3),
            content_index: None,
            sequence_number: Some(9),
        },
    };
    let replay = serde_json::to_value(&transcript_item.item)?;
    assert!(replay.get("output_index").is_none());
    assert!(replay.get("sequence_number").is_none());
    let persisted = serde_json::to_value(&transcript_item)?;
    assert_eq!(persisted["provenance"]["output_index"], 3);
    assert_eq!(persisted["item"], replay);
    Ok(())
}

#[test]
fn malformed_known_item_is_rejected_instead_of_downgraded_to_opaque() {
    let error = ResponseItem::from_value(serde_json::json!({
        "type": "function_call",
        "id": "fc_1",
        "name": "read",
        "arguments": "{}"
    }));
    assert!(error.is_err());
}

#[test]
fn known_message_requires_contract_fields_and_output_text_arrays() -> TestResult {
    let valid = serde_json::json!({
        "type": "message",
        "id": "msg_contract",
        "role": "assistant",
        "status": "completed",
        "content": [{
            "type": "output_text",
            "text": "answer",
            "annotations": [],
            "logprobs": []
        }]
    });
    let item = ResponseItem::from_value(valid.clone())?;
    let Some(message) = item.as_message() else {
        return Err(io::Error::other("message item was not classified").into());
    };
    assert_eq!(item.id(), Some("msg_contract"));
    assert_eq!(message.role(), "assistant");
    assert_eq!(message.status(), "completed");
    assert_eq!(serde_json::to_value(item)?, valid);

    for key in ["id", "status"] {
        let mut malformed = valid.clone();
        let Some(object) = malformed.as_object_mut() else {
            return Err(io::Error::other("message fixture was not an object").into());
        };
        object.remove(key);
        assert!(
            ResponseItem::from_value(malformed).is_err(),
            "missing {key} must be rejected",
        );
    }
    for key in ["annotations", "logprobs"] {
        let mut malformed = valid.clone();
        let Some(part) = malformed["content"][0].as_object_mut() else {
            return Err(io::Error::other("content fixture was not an object").into());
        };
        part.remove(key);
        assert!(
            ResponseItem::from_value(malformed).is_err(),
            "missing {key} must be rejected",
        );
    }
    for (key, value) in [
        ("role", Value::String("user".to_owned())),
        ("status", Value::String("future_status".to_owned())),
        ("phase", Value::String("future_phase".to_owned())),
    ] {
        let mut malformed = valid.clone();
        malformed[key] = value;
        assert!(
            ResponseItem::from_value(malformed).is_err(),
            "invalid {key} must be rejected",
        );
    }
    Ok(())
}

#[test]
fn reasoning_preserves_nullable_encrypted_content_and_nonnullable_optionals() -> TestResult {
    for (field, expected) in [
        (None, ResponseNullable::Absent),
        (Some(Value::Null), ResponseNullable::Null),
        (
            Some(Value::String("ciphertext".to_owned())),
            ResponseNullable::Value("ciphertext"),
        ),
    ] {
        let mut raw = serde_json::json!({
            "type": "reasoning",
            "id": "rs_nullable",
            "summary": [],
            "status": "completed"
        });
        if let Some(field) = field {
            raw["encrypted_content"] = field;
        }
        let item = ResponseItem::from_value(raw.clone())?;
        let Some(reasoning) = item.as_reasoning() else {
            return Err(io::Error::other("reasoning item was not classified").into());
        };
        assert_eq!(reasoning.encrypted_content_field(), expected);
        assert_eq!(reasoning.status(), Some("completed"));
        assert_eq!(serde_json::to_value(item)?, raw);
    }

    for key in ["id", "summary"] {
        let mut malformed = serde_json::json!({
            "type": "reasoning",
            "id": "rs_required",
            "summary": []
        });
        let Some(object) = malformed.as_object_mut() else {
            return Err(io::Error::other("reasoning fixture was not an object").into());
        };
        object.remove(key);
        assert!(
            ResponseItem::from_value(malformed).is_err(),
            "missing {key} must be rejected",
        );
    }
    for (key, value) in [("content", Value::Null), ("status", Value::Null)] {
        let mut malformed = serde_json::json!({
            "type": "reasoning",
            "id": "rs_nonnullable",
            "summary": []
        });
        malformed[key] = value;
        assert!(
            ResponseItem::from_value(malformed).is_err(),
            "explicit null {key} must be rejected",
        );
    }
    Ok(())
}

#[test]
fn hosted_search_and_compaction_enforce_required_public_shapes() -> TestResult {
    let search = serde_json::json!({
        "type": "web_search_call",
        "id": "ws_required",
        "status": "completed",
        "action": {"type": "search", "query": "norn"}
    });
    let parsed = ResponseItem::from_value(search.clone())?;
    let ResponseItem::WebSearchCall(search_item) = &parsed else {
        return Err(io::Error::other("web search item was not classified").into());
    };
    assert_eq!(search_item.status(), "completed");
    assert!(search_item.action().is_object());
    assert_eq!(serde_json::to_value(parsed)?, search);

    for key in ["id", "status", "action"] {
        let mut malformed = search.clone();
        let Some(object) = malformed.as_object_mut() else {
            return Err(io::Error::other("web search fixture was not an object").into());
        };
        object.remove(key);
        assert!(
            ResponseItem::from_value(malformed).is_err(),
            "missing {key} must be rejected",
        );
    }

    let compaction = serde_json::json!({
        "type": "compaction",
        "id": "cmp_required",
        "encrypted_content": "opaque-state"
    });
    let parsed = ResponseItem::from_value(compaction.clone())?;
    let ResponseItem::Compaction(compaction_item) = &parsed else {
        return Err(io::Error::other("compaction item was not classified").into());
    };
    assert_eq!(compaction_item.encrypted_content(), "opaque-state");
    assert_eq!(serde_json::to_value(parsed)?, compaction);

    for key in ["id", "encrypted_content"] {
        let mut malformed = compaction.clone();
        let Some(object) = malformed.as_object_mut() else {
            return Err(io::Error::other("compaction fixture was not an object").into());
        };
        object.remove(key);
        assert!(
            ResponseItem::from_value(malformed).is_err(),
            "missing {key} must be rejected",
        );
    }
    Ok(())
}

#[test]
fn unpinned_aliases_and_unknown_id_shapes_remain_exactly_opaque() -> TestResult {
    for raw in [
        serde_json::json!({
            "type": "compaction_summary",
            "id": "legacy_alias",
            "encrypted_content": "kept"
        }),
        serde_json::json!({
            "type": "future_item",
            "id": null,
            "payload": {"kept": true}
        }),
        serde_json::json!({
            "type": "future_item",
            "id": {"new_shape": 1},
            "payload": [1, 2, 3]
        }),
    ] {
        let item = ResponseItem::from_value(raw.clone())?;
        assert!(matches!(item, ResponseItem::Opaque(_)));
        assert_eq!(serde_json::to_value(item)?, raw);
    }
    Ok(())
}

#[test]
fn compaction_rejects_empty_encrypted_content_without_echoing_provider_data() {
    let result = ResponseItem::from_value(serde_json::json!({
        "type": "compaction",
        "id": "provider-controlled-id",
        "encrypted_content": ""
    }));

    assert!(result.is_err());
    let rendered = result.err().map(|error| format!("{error:?}"));
    assert!(rendered.as_deref().is_some_and(|text| {
        text.contains("compaction item encrypted_content was empty")
            && !text.contains("provider-controlled-id")
    }));
}

#[test]
fn function_call_optional_fields_keep_contract_nullability() -> TestResult {
    let valid = serde_json::json!({
        "type": "function_call",
        "id": "fc_optional",
        "call_id": "call_optional",
        "name": "read",
        "arguments": "{}",
        "caller": null,
        "namespace": "filesystem",
        "status": "completed"
    });
    let item = ResponseItem::from_value(valid.clone())?;
    assert_eq!(serde_json::to_value(item)?, valid);

    for (key, value) in [
        ("id", Value::Null),
        ("namespace", Value::Null),
        ("status", Value::Null),
        ("status", Value::String("future_status".to_owned())),
        ("caller", Value::String("not-an-object".to_owned())),
    ] {
        let mut malformed = valid.clone();
        malformed[key] = value;
        assert!(
            ResponseItem::from_value(malformed).is_err(),
            "invalid {key} shape must be rejected",
        );
    }
    Ok(())
}
