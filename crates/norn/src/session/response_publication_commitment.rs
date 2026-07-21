//! Durable identity for one framed provider-response publication.

use serde::{Deserialize, Serialize};
use serde_json::{Number, Value};
use sha2::{Digest as _, Sha256};
use thiserror::Error;

use super::events::{EventBase, SessionEvent};

const DOMAIN: &[u8] = b"norn.session.response-state-publication.v1\0";

/// Length and digest committed by a V1 response-publication boundary.
///
/// The digest binds the ordered event group for retry integrity. It is not a
/// signature and does not authenticate storage against an actor who can rewrite
/// both the boundary and the remaining rows.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponsePublicationCommitment {
    event_count: u64,
    group_sha256: String,
}

impl ResponsePublicationCommitment {
    /// Number of events in the complete group, including its boundary.
    #[must_use]
    pub const fn event_count(&self) -> u64 {
        self.event_count
    }

    /// Lowercase SHA-256 digest of the commitment-free canonical group.
    #[must_use]
    pub fn group_sha256(&self) -> &str {
        &self.group_sha256
    }
}

#[derive(Clone, Copy, Debug, Error)]
#[error("response publication commitment is invalid")]
pub(crate) struct ResponsePublicationCommitmentError;

pub(crate) fn calculate(
    boundary_base: &EventBase,
    suffix: &[SessionEvent],
) -> Result<ResponsePublicationCommitment, ResponsePublicationCommitmentError> {
    let event_count = u64::try_from(suffix.len())
        .ok()
        .and_then(|count| count.checked_add(1))
        .ok_or(ResponsePublicationCommitmentError)?;
    let mut hasher = Sha256::new();
    hasher.update(DOMAIN);
    hasher.update(event_count.to_be_bytes());

    let boundary = serde_json::json!({
        "type": "ProviderEpochBoundary",
        "base": boundary_base,
        "reason": "response_state_publication_v1",
    });
    hash_value(&mut hasher, &boundary)?;
    for event in suffix {
        let value =
            serde_json::to_value(event).map_err(|_error| ResponsePublicationCommitmentError)?;
        hash_value(&mut hasher, &value)?;
    }

    Ok(ResponsePublicationCommitment {
        event_count,
        group_sha256: format!("{:x}", hasher.finalize()),
    })
}

pub(crate) fn verify(
    commitment: &ResponsePublicationCommitment,
    boundary_base: &EventBase,
    suffix: &[SessionEvent],
) -> Result<(), ResponsePublicationCommitmentError> {
    if commitment.group_sha256.len() != 64
        || !commitment
            .group_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ResponsePublicationCommitmentError);
    }
    let expected = calculate(boundary_base, suffix)?;
    if commitment != &expected {
        return Err(ResponsePublicationCommitmentError);
    }
    Ok(())
}

fn hash_value(
    hasher: &mut Sha256,
    value: &Value,
) -> Result<(), ResponsePublicationCommitmentError> {
    match value {
        Value::Null => hasher.update([0]),
        Value::Bool(value) => hasher.update([1, u8::from(*value)]),
        Value::Number(value) => hash_number(hasher, value)?,
        Value::String(value) => {
            hasher.update([3]);
            hash_bytes(hasher, value.as_bytes())?;
        }
        Value::Array(values) => {
            hasher.update([4]);
            hash_length(hasher, values.len())?;
            for value in values {
                hash_value(hasher, value)?;
            }
        }
        Value::Object(values) => {
            hasher.update([5]);
            hash_length(hasher, values.len())?;
            let mut entries = values.iter().collect::<Vec<_>>();
            entries.sort_unstable_by(|left, right| left.0.cmp(right.0));
            for (key, value) in entries {
                hash_bytes(hasher, key.as_bytes())?;
                hash_value(hasher, value)?;
            }
        }
    }
    Ok(())
}

fn hash_number(
    hasher: &mut Sha256,
    value: &Number,
) -> Result<(), ResponsePublicationCommitmentError> {
    hasher.update([2]);
    if Number::from_f64(0.0)
        .as_ref()
        .is_some_and(|positive_zero| value == positive_zero)
    {
        hash_bytes(hasher, b"0.0")
    } else {
        hash_bytes(hasher, value.to_string().as_bytes())
    }
}

fn hash_bytes(hasher: &mut Sha256, bytes: &[u8]) -> Result<(), ResponsePublicationCommitmentError> {
    hash_length(hasher, bytes.len())?;
    hasher.update(bytes);
    Ok(())
}

fn hash_length(
    hasher: &mut Sha256,
    length: usize,
) -> Result<(), ResponsePublicationCommitmentError> {
    let length = u64::try_from(length).map_err(|_error| ResponsePublicationCommitmentError)?;
    hasher.update(length.to_be_bytes());
    Ok(())
}
