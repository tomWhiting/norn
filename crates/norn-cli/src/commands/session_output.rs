//! Public JSON projection for private session-index rows.

use chrono::{DateTime, Utc};
use norn::session::{ResumeFidelity, SessionRecordOrigin};
use serde::Serialize;
use uuid::Uuid;

use crate::session::{SessionIndexEntry, SessionStatus};

/// Session metadata safe to expose through CLI JSON output.
///
/// This projection deliberately omits the stable provider-state identity.
/// The durable index row keeps that field for affinity enforcement, but a
/// reusable equality value must not cross into operator-facing output.
#[derive(Serialize)]
pub(super) struct PublicSessionIndexEntry<'a> {
    id: &'a str,
    generation: &'a Uuid,
    name: Option<&'a str>,
    model: &'a str,
    working_dir: &'a str,
    created_at: &'a DateTime<Utc>,
    updated_at: &'a DateTime<Utc>,
    event_count: u64,
    status: SessionStatus,
    format_version: u32,
    total_input_tokens: u64,
    total_output_tokens: u64,
    total_cache_read_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    rel_path: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_id: Option<&'a str>,
    fidelity: ResumeFidelity,
    origin: &'a SessionRecordOrigin,
}

impl<'a> From<&'a SessionIndexEntry> for PublicSessionIndexEntry<'a> {
    fn from(entry: &'a SessionIndexEntry) -> Self {
        let SessionIndexEntry {
            id,
            generation,
            name,
            model,
            working_dir,
            created_at,
            updated_at,
            event_count,
            status,
            format_version,
            total_input_tokens,
            total_output_tokens,
            total_cache_read_tokens,
            rel_path,
            parent_id,
            fidelity,
            origin,
            provider_state_identity: _,
        } = entry;
        Self {
            id,
            generation,
            name: name.as_deref(),
            model,
            working_dir,
            created_at,
            updated_at,
            event_count: *event_count,
            status: *status,
            format_version: *format_version,
            total_input_tokens: *total_input_tokens,
            total_output_tokens: *total_output_tokens,
            total_cache_read_tokens: *total_cache_read_tokens,
            rel_path: rel_path.as_deref(),
            parent_id: parent_id.as_deref(),
            fidelity: *fidelity,
            origin,
        }
    }
}
