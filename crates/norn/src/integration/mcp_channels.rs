//! Public channel contracts: host policy, retained budgets and untrusted wire data.

use std::collections::BTreeMap;
use std::fmt;
use std::num::NonZeroUsize;

use serde::Serialize;
use uuid::Uuid;

/// Published experimental capability for ordinary Claude Code channel input.
pub const MCP_CHANNEL_CAPABILITY: &str = "claude/channel";
/// Published JSON-RPC notification method for ordinary channel input.
pub const MCP_CHANNEL_NOTIFICATION: &str = "notifications/claude/channel";

/// Host-selected handling of an admitted external message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpChannelPolicy {
    /// Retain until the operator explicitly releases or denies the message.
    Hold,
    /// Join the next independently started turn without waking an idle host.
    NextTurn,
    /// Make an idle host eligible to run; busy work uses its next safe boundary.
    Wake,
}

/// Host selection for one source, separate from an admitted message's policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpChannelSourcePolicy {
    /// Keep the server's tools without granting channel input.
    Off,
    /// Use this delivery policy if the source is selected for channel input.
    Delivery(McpChannelPolicy),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum McpChannelCapabilityRequirement {
    Required,
    IfAdvertised,
}

/// Explicit behaviour when the host cannot retain another channel message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpChannelOverflow {
    /// Refuse the new event visibly while allowing MCP responses to continue.
    RejectNew,
}

/// Caller-supplied bounds for retained channel payloads; there are no defaults.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct McpChannelLimits {
    max_retained_messages: NonZeroUsize,
    max_retained_bytes: NonZeroUsize,
}

impl McpChannelLimits {
    /// Validate explicit positive retained-count and retained-payload-byte limits.
    pub fn new(messages: usize, bytes: usize) -> Result<Self, McpChannelError> {
        let max_retained_messages =
            NonZeroUsize::new(messages).ok_or(McpChannelError::InvalidLimit {
                name: "max_retained_messages",
            })?;
        let max_retained_bytes = NonZeroUsize::new(bytes).ok_or(McpChannelError::InvalidLimit {
            name: "max_retained_bytes",
        })?;
        Ok(Self {
            max_retained_messages,
            max_retained_bytes,
        })
    }

    /// Maximum staged, held, queued and claimed-but-unconsumed messages together.
    pub const fn max_retained_messages(self) -> usize {
        self.max_retained_messages.get()
    }

    /// Maximum combined UTF-8 bytes of source labels, content and metadata.
    pub const fn max_retained_bytes(self) -> usize {
        self.max_retained_bytes.get()
    }
}

/// Why the host refused an offered channel event; no event content is included.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum McpChannelRefusal {
    /// The owner did not attach an ingress capability to this connection.
    #[error("channel input was not enabled by the session owner")]
    NotEnabled,
    /// The initialize result did not declare the required empty-object capability.
    #[error("server did not declare the required channel capability")]
    NotDeclared,
    /// The event did not contain valid string content and string metadata.
    #[error("channel params must contain string content and optional string metadata")]
    InvalidPayload,
    /// A metadata key violates the published identifier contract.
    #[error("channel metadata keys must contain only ASCII letters, digits and underscores")]
    InvalidMetadataKey,
    /// The total retained message count reached its explicit budget.
    #[error("channel inbox retained-message limit reached")]
    FullCount,
    /// The total retained payload bytes reached their explicit budget.
    #[error("channel inbox retained-byte limit reached")]
    FullBytes,
    /// A newer connection or an explicit owner decision retired this ingress.
    #[error("channel connection generation has been retired")]
    Retired,
    /// The sole inbox receiver has closed.
    #[error("channel inbox receiver has closed")]
    Closed,
    /// Host sequence or byte accounting cannot represent the next value.
    #[error("channel inbox accounting capacity exhausted")]
    AccountingExhausted,
    /// An unactivated candidate was abandoned before publishing its staged events.
    #[error("channel candidate was abandoned before activation")]
    CandidateAbandoned,
}

/// Redacted host-attributed refusal that can be displayed without message content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct McpChannelRejection {
    /// Configured name of the source whose event was refused.
    pub source: String,
    /// Host-minted MCP connection instance.
    pub generation: u64,
    /// Intended recipient fixed when the inbox was constructed.
    pub recipient_id: Uuid,
    /// Named refusal independent of sender-supplied data.
    pub reason: McpChannelRefusal,
}

/// Current inbox occupancy and refusals, published through a push watch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct McpChannelStatus {
    /// Recipient that owns this inbox.
    pub recipient_id: Uuid,
    /// Explicit operator/caller limits in force.
    pub limits: McpChannelLimits,
    /// Staged, held, queued and claimed messages still retained.
    pub retained_messages: usize,
    /// Charged UTF-8 source/content/metadata bytes of retained messages.
    pub retained_bytes: usize,
    /// Number of refusals observed, saturating only when `rejection_count_exhausted` is true.
    pub rejected: u64,
    /// Whether the exact rejection count exceeded its representable range.
    pub rejection_count_exhausted: bool,
    /// Latest host-attributed refusal; it contains no message text or metadata.
    pub last_rejection: Option<McpChannelRejection>,
    /// Whether the owner has closed admission.
    pub closed: bool,
}

/// Negotiated channel declaration and source-supplied initialize instructions.
#[derive(Clone, Serialize)]
pub struct McpChannelInfo {
    /// The exact advertised capability object, required to be empty.
    pub capability: BTreeMap<String, serde_json::Value>,
    /// Optional untrusted server instructions; these never confer system authority.
    pub instructions: Option<String>,
}

impl fmt::Debug for McpChannelInfo {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpChannelInfo")
            .field("capability", &self.capability)
            .field("instructions_present", &self.instructions.is_some())
            .finish()
    }
}

/// Host-attributed external input; metadata and content remain untrusted data.
#[derive(Serialize)]
pub struct McpChannelMessage {
    pub(super) id: Uuid,
    pub(super) source: String,
    pub(super) generation: u64,
    pub(super) recipient_id: Uuid,
    pub(super) sequence: u64,
    pub(super) content: String,
    pub(super) meta: BTreeMap<String, String>,
}

impl McpChannelMessage {
    /// Local identity for this offered event; upstream metadata is not deduplicated.
    pub const fn id(&self) -> Uuid {
        self.id
    }
    /// Configured server name, never the sender's `meta.source` value.
    pub fn source(&self) -> &str {
        &self.source
    }
    /// Host-minted connection instance that admitted this event.
    pub const fn generation(&self) -> u64 {
        self.generation
    }
    /// Sole recipient fixed by the host inbox.
    pub const fn recipient_id(&self) -> Uuid {
        self.recipient_id
    }
    /// Monotonic accepted-event order within this inbox.
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }
    /// Untrusted event text received from the server.
    pub fn content(&self) -> &str {
        &self.content
    }
    /// Untrusted string metadata, including any claimed `source` attribute.
    pub const fn meta(&self) -> &BTreeMap<String, String> {
        &self.meta
    }
}

impl fmt::Debug for McpChannelMessage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpChannelMessage")
            .field("id", &self.id)
            .field("source", &self.source)
            .field("generation", &self.generation)
            .field("recipient_id", &self.recipient_id)
            .field("sequence", &self.sequence)
            .field("content", &"[REDACTED]")
            .field("metadata_entries", &self.meta.len())
            .finish()
    }
}

/// Invalid host operation or unavailable inbox ownership.
#[derive(Debug, thiserror::Error)]
pub enum McpChannelError {
    /// A caller attempted to create an inbox without a positive declared budget.
    #[error("channel setting {name} must be positive")]
    InvalidLimit {
        /// Name of the invalid setting.
        name: &'static str,
    },
    /// The sole receiving owner closed the inbox.
    #[error("channel inbox for recipient {recipient_id} is closed")]
    Closed {
        /// Intended recipient.
        recipient_id: Uuid,
    },
    /// A requested state transition cannot operate on this connection.
    #[error("channel source {source_name} generation {generation}: {reason}")]
    Source {
        /// Configured source label.
        source_name: String,
        /// Connection instance.
        generation: u64,
        /// Named state failure.
        reason: McpChannelRefusal,
    },
    /// No enabled channel exists on the selected MCP connection.
    #[error("MCP connection has no enabled channel attachment")]
    NotEnabled,
    /// The requested message is absent or its claim belongs to another operation.
    #[error("channel message {message_id} is unavailable for this operation")]
    MessageUnavailable {
        /// Local event identity.
        message_id: Uuid,
    },
}

pub(super) struct ChannelParams {
    pub(super) content: String,
    pub(super) meta: BTreeMap<String, String>,
}

impl ChannelParams {
    pub(super) fn parse(value: serde_json::Value) -> Result<Self, McpChannelRefusal> {
        let serde_json::Value::Object(mut fields) = value else {
            return Err(McpChannelRefusal::InvalidPayload);
        };
        let Some(serde_json::Value::String(content)) = fields.remove("content") else {
            return Err(McpChannelRefusal::InvalidPayload);
        };
        let mut meta = BTreeMap::new();
        match fields.remove("meta") {
            None => {}
            Some(serde_json::Value::Object(values)) => {
                for (key, value) in values {
                    let serde_json::Value::String(value) = value else {
                        return Err(McpChannelRefusal::InvalidPayload);
                    };
                    meta.insert(key, value);
                }
            }
            Some(_) => return Err(McpChannelRefusal::InvalidPayload),
        }
        let params = Self { content, meta };
        if params.meta.keys().any(|key| {
            key.is_empty()
                || !key
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        }) {
            return Err(McpChannelRefusal::InvalidMetadataKey);
        }
        Ok(params)
    }

    pub(super) fn retained_bytes(&self, source: &str) -> Option<usize> {
        self.meta.iter().try_fold(
            source.len().checked_add(self.content.len())?,
            |total, (key, value)| total.checked_add(key.len())?.checked_add(value.len()),
        )
    }
}

#[cfg(test)]
#[path = "mcp_channel_tests.rs"]
mod tests;
