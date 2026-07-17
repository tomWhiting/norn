use serde_json::{Number, Value};
use sha2::{Digest as _, Sha256};

const DOMAIN: &[u8] = b"norn.responses.frame-signature.v1\0";

/// Fixed-width identity for one parsed Responses stream frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FrameSignature([u8; 32]);

impl FrameSignature {
    /// Hash the event discriminator and parsed envelope with explicit type and
    /// length framing, including order-independent JSON object keys.
    pub(super) fn new(event_type: &str, data: &Value) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(DOMAIN);
        hash_bytes(&mut hasher, event_type.as_bytes());
        hash_value(&mut hasher, data);
        Self(hasher.finalize().into())
    }
}

fn hash_value(hasher: &mut Sha256, value: &Value) {
    match value {
        Value::Null => hasher.update([0]),
        Value::Bool(value) => hasher.update([1, u8::from(*value)]),
        Value::Number(value) => hash_number(hasher, value),
        Value::String(value) => {
            hasher.update([3]);
            hash_bytes(hasher, value.as_bytes());
        }
        Value::Array(values) => {
            hasher.update([4]);
            hash_length(hasher, values.len());
            for value in values {
                hash_value(hasher, value);
            }
        }
        Value::Object(values) => {
            hasher.update([5]);
            hash_length(hasher, values.len());
            let mut entries = values.iter().collect::<Vec<_>>();
            entries.sort_unstable_by(|left, right| left.0.cmp(right.0));
            for (key, value) in entries {
                hash_bytes(hasher, key.as_bytes());
                hash_value(hasher, value);
            }
        }
    }
}

fn hash_number(hasher: &mut Sha256, value: &Number) {
    hasher.update([2]);
    if Number::from_f64(0.0)
        .as_ref()
        .is_some_and(|positive_zero| value == positive_zero)
    {
        hash_bytes(hasher, b"0.0");
    } else {
        hash_bytes(hasher, value.to_string().as_bytes());
    }
}

fn hash_bytes(hasher: &mut Sha256, bytes: &[u8]) {
    hash_length(hasher, bytes.len());
    hasher.update(bytes);
}

fn hash_length(hasher: &mut Sha256, length: usize) {
    hasher.update(length.to_be_bytes());
}
