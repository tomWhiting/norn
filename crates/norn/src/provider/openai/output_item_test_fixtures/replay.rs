use serde_json::Value;

/// Adds synthetic opaque state to reasoning fixtures that will be replayed.
///
/// The optional-shape inventory intentionally retains reasoning without
/// `encrypted_content` because that is a valid response shape. Tests that send
/// the inventory back on a fresh request must opt into this replayable form.
pub(crate) fn with_replayable_reasoning_state(mut items: Vec<Value>) -> Vec<Value> {
    let mut amended = 0;
    for item in &mut items {
        let is_reasoning = item.get("type").and_then(Value::as_str) == Some("reasoning");
        let lacks_state = item
            .get("encrypted_content")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty);
        if is_reasoning
            && lacks_state
            && let Some(object) = item.as_object_mut()
        {
            object.insert(
                "encrypted_content".to_owned(),
                Value::String("opaque-replay-fixture".to_owned()),
            );
            amended += 1;
        }
    }
    assert_eq!(
        amended, 1,
        "the shape matrix must contain exactly one reasoning item without replay state"
    );
    items
}
