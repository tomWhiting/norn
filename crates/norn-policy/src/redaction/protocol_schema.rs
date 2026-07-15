use serde::Deserialize;

use super::model::SyntheticPurpose;
use super::path_policy::has_exact_extension;
use super::protocol_literals::include_accepts;
use super::validate::RedactionCode;

mod context;

pub(super) use context::{
    is_approved_source_url, is_category, is_concern, is_finding_id, is_fixed_url, is_fixture_id,
    is_fixture_path, is_hex_pin, is_json_pointer, is_pinned_source_path,
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProtocolDialect {
    Corpus,
    Public,
    Codex,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProtocolArtifactKind {
    BackendStateMatrix,
    ContractPins,
    Index,
    Manifest,
    Request,
    Stream,
    Transport,
}

#[derive(Clone, Copy)]
pub(crate) enum EnumSet {
    AssistantPhase,
    ArtifactKind,
    CacheMode,
    CacheTtl,
    Dialect,
    Effort,
    Expectation,
    IncompleteReason,
    Include,
    Object,
    OwnerPhase,
    ReasoningSummary,
    Retention,
    Role,
    SecretProfile,
    Status,
    ToolChoice,
    Truncation,
}

#[derive(Clone, Copy)]
pub(crate) struct RuleContext {
    pub(crate) dialect: ProtocolDialect,
    pub(crate) kind: ProtocolArtifactKind,
    pub(crate) assistant_message: bool,
    pub(crate) role: ProtocolObjectRole,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum ProtocolObjectRole {
    Event,
    IncompleteDetails,
    Other,
    RequestPayload,
    RequestReasoning,
    Response,
}

#[derive(Clone, Copy)]
pub(crate) enum StringRule {
    ApprovedSourceUrl,
    Category,
    Concern,
    Enum(EnumSet),
    FindingId,
    FixtureId,
    Synthetic(SyntheticPurpose),
    FixturePath,
    FixedUrl,
    HexPin,
    JsonPointer,
    PinnedSourcePath,
}

#[derive(Clone, Copy)]
pub(crate) enum FieldRule {
    String {
        rule: StringRule,
        nullable: bool,
    },
    Number {
        nullable: bool,
    },
    NumberOrString(StringRule),
    Boolean {
        nullable: bool,
    },
    Container {
        leaf: SyntheticPurpose,
        nullable: bool,
        scalar: bool,
    },
    MachineMap {
        leaf: SyntheticPurpose,
    },
    StringList {
        rule: StringRule,
        nullable: bool,
    },
    StringOrContainer {
        rule: StringRule,
        leaf: SyntheticPurpose,
        nullable: bool,
    },
    BooleanOrContainer,
    SchemaLiteral,
    JsonSchema,
    ProtocolType,
}

pub(crate) fn expected_shape(path: &str) -> Option<(ProtocolDialect, ProtocolArtifactKind)> {
    let dialect = if path.contains("/public/") {
        ProtocolDialect::Public
    } else if path.contains("/codex/") {
        ProtocolDialect::Codex
    } else {
        ProtocolDialect::Corpus
    };
    let kind = if has_exact_extension(path, "sse") && path.contains("/streams/") {
        ProtocolArtifactKind::Stream
    } else if path.ends_with("/manifest.json") {
        ProtocolArtifactKind::Manifest
    } else if path.contains("/requests/") && has_exact_extension(path, "json") {
        ProtocolArtifactKind::Request
    } else if path.contains("/transport/") && has_exact_extension(path, "json") {
        ProtocolArtifactKind::Transport
    } else if path.ends_with("/contract-pins.json") {
        ProtocolArtifactKind::ContractPins
    } else if path.ends_with("/backend-state-matrix.json") {
        ProtocolArtifactKind::BackendStateMatrix
    } else if path.ends_with("/index.json") {
        ProtocolArtifactKind::Index
    } else {
        return None;
    };
    Some((dialect, kind))
}

pub(crate) fn field_rule(key: &str, context: RuleContext) -> Option<FieldRule> {
    match (context.kind, key) {
        (ProtocolArtifactKind::Manifest, "id") => Some(FieldRule::String {
            rule: StringRule::FixtureId,
            nullable: false,
        }),
        (ProtocolArtifactKind::Manifest, "categories") => Some(FieldRule::StringList {
            rule: StringRule::Category,
            nullable: false,
        }),
        (ProtocolArtifactKind::Manifest, "finding_ids") => Some(FieldRule::StringList {
            rule: StringRule::FindingId,
            nullable: false,
        }),
        (ProtocolArtifactKind::Manifest, "source_references") => Some(FieldRule::StringList {
            rule: StringRule::ApprovedSourceUrl,
            nullable: false,
        }),
        (ProtocolArtifactKind::ContractPins | ProtocolArtifactKind::Index, "path") => {
            Some(FieldRule::String {
                rule: StringRule::PinnedSourcePath,
                nullable: false,
            })
        }
        (ProtocolArtifactKind::BackendStateMatrix, "concern") => Some(FieldRule::String {
            rule: StringRule::Concern,
            nullable: false,
        }),
        (_, "phase") if context.assistant_message => Some(FieldRule::String {
            rule: StringRule::Enum(EnumSet::AssistantPhase),
            nullable: true,
        }),
        (_, "reason") if context.role == ProtocolObjectRole::IncompleteDetails => {
            Some(FieldRule::String {
                rule: StringRule::Enum(EnumSet::IncompleteReason),
                nullable: false,
            })
        }
        (_, "generate_summary" | "summary")
            if context.role == ProtocolObjectRole::RequestReasoning =>
        {
            Some(FieldRule::String {
                rule: StringRule::Enum(EnumSet::ReasoningSummary),
                nullable: true,
            })
        }
        (_, "type") => Some(FieldRule::ProtocolType),
        (_, key) => general_field_rule(key),
    }
}

fn general_field_rule(key: &str) -> Option<FieldRule> {
    match key {
        "access_token" | "api_key" | "authorization" | "credential" | "id_token"
        | "refresh_token" | "token" => Some(synthetic(SyntheticPurpose::Credential, false)),
        "account_id" | "chatgpt_account_id" | "email" | "user_id" => {
            Some(synthetic(SyntheticPurpose::AccountId, false))
        }
        "conversation_id" | "previous_response_id" | "session_state" | "turn_state" => {
            Some(synthetic(SyntheticPurpose::TurnState, true))
        }
        "prompt_cache_key" | "raw_cache_key" => Some(synthetic(SyntheticPurpose::CacheKey, true)),
        "delta" | "instructions" | "message" | "query" | "refusal" => Some(synthetic(
            SyntheticPurpose::PromptContent,
            matches!(key, "delta" | "refusal"),
        )),
        "code"
        | "current_observation"
        | "description"
        | "encrypted_content"
        | "file_id"
        | "filename"
        | "id"
        | "item_id"
        | "call_id"
        | "container_id"
        | "model"
        | "name"
        | "reason"
        | "server_label"
        | "title" => Some(synthetic(
            SyntheticPurpose::Generic,
            key == "encrypted_content",
        )),
        "artifact_kind" => Some(enumeration(EnumSet::ArtifactKind)),
        "mode" => Some(enumeration(EnumSet::CacheMode)),
        "dialect" => Some(enumeration(EnumSet::Dialect)),
        "effort" => Some(enumeration(EnumSet::Effort)),
        "expectation_class" => Some(enumeration(EnumSet::Expectation)),
        "object" => Some(enumeration(EnumSet::Object)),
        "owner_phase" => Some(enumeration(EnumSet::OwnerPhase)),
        "retention" | "prompt_cache_retention" => Some(enumeration(EnumSet::Retention)),
        "role" => Some(enumeration(EnumSet::Role)),
        "secret_profile" => Some(enumeration(EnumSet::SecretProfile)),
        "status" => Some(enumeration(EnumSet::Status)),
        "truncation" => Some(enumeration(EnumSet::Truncation)),
        "url" | "server_url" => Some(FieldRule::String {
            rule: StringRule::FixedUrl,
            nullable: false,
        }),
        "fixture_path" => Some(FieldRule::String {
            rule: StringRule::FixturePath,
            nullable: false,
        }),
        "blob" | "commit" | "sha256" => Some(FieldRule::String {
            rule: StringRule::HexPin,
            nullable: false,
        }),
        "annotation_index" | "bytes" | "cache_write_tokens" | "cached_tokens" | "completed_at"
        | "content_index" | "created_at" | "end_index" | "index" | "input_tokens"
        | "max_output_tokens" | "maximum" | "minimum" | "output_index" | "output_tokens"
        | "position" | "reasoning_tokens" | "retry_after" | "sequence_number" | "start_index"
        | "threshold" | "top_p" | "total_tokens" => Some(FieldRule::Number { nullable: false }),
        "ttl" => Some(FieldRule::NumberOrString(StringRule::Enum(
            EnumSet::CacheTtl,
        ))),
        "approved"
        | "background"
        | "enabled"
        | "end_turn"
        | "foreign_write"
        | "lock"
        | "parallel_tool_calls"
        | "refresh"
        | "reload"
        | "save"
        | "store"
        | "stream" => Some(FieldRule::Boolean { nullable: false }),
        "content" | "input" | "summary" => Some(FieldRule::Container {
            leaf: SyntheticPurpose::PromptContent,
            nullable: true,
            scalar: true,
        }),
        "arguments" | "codex_overlay" | "output" | "p1_treatment" | "target_assertions" => {
            Some(FieldRule::Container {
                leaf: SyntheticPurpose::Generic,
                nullable: false,
                scalar: true,
            })
        }
        "action"
        | "annotation"
        | "annotations"
        | "anyOf"
        | "allOf"
        | "capabilities"
        | "codex_manifest"
        | "codex_source"
        | "details"
        | "entries"
        | "enum"
        | "error"
        | "fixtures"
        | "format"
        | "incomplete_details"
        | "input_tokens_details"
        | "item"
        | "items"
        | "logprobs"
        | "manifest"
        | "oneOf"
        | "output_tokens_details"
        | "pins"
        | "public_manifest"
        | "reasoning"
        | "requests"
        | "response"
        | "results"
        | "sources"
        | "streams"
        | "tools"
        | "transport"
        | "usage" => Some(FieldRule::Container {
            leaf: SyntheticPurpose::Generic,
            nullable: matches!(
                key,
                "annotations" | "error" | "incomplete_details" | "logprobs" | "usage"
            ),
            scalar: false,
        }),
        "cache_control" | "prompt_cache_breakpoint" | "prompt_cache_options" => {
            Some(FieldRule::Container {
                leaf: SyntheticPurpose::Generic,
                nullable: false,
                scalar: false,
            })
        }
        "metadata" | "properties" | "$defs" => Some(FieldRule::MachineMap {
            leaf: SyntheticPurpose::Generic,
        }),
        "required" | "vector_store_ids" => Some(FieldRule::StringList {
            rule: StringRule::Synthetic(SyntheticPurpose::Generic),
            nullable: false,
        }),
        "include" => Some(FieldRule::StringList {
            rule: StringRule::Enum(EnumSet::Include),
            nullable: false,
        }),
        "param" => Some(synthetic(SyntheticPurpose::Generic, true)),
        "public_contract" => Some(FieldRule::StringOrContainer {
            rule: StringRule::Synthetic(SyntheticPurpose::Generic),
            leaf: SyntheticPurpose::Generic,
            nullable: false,
        }),
        "text" => Some(FieldRule::StringOrContainer {
            rule: StringRule::Synthetic(SyntheticPurpose::PromptContent),
            leaf: SyntheticPurpose::Generic,
            nullable: false,
        }),
        "tool_choice" => Some(FieldRule::StringOrContainer {
            rule: StringRule::Enum(EnumSet::ToolChoice),
            leaf: SyntheticPurpose::Generic,
            nullable: false,
        }),
        "additionalProperties" => Some(FieldRule::BooleanOrContainer),
        "parameters" | "schema" => Some(FieldRule::JsonSchema),
        "$ref" => Some(FieldRule::String {
            rule: StringRule::JsonPointer,
            nullable: false,
        }),
        "const" | "default" => Some(FieldRule::SchemaLiteral),
        _ => None,
    }
}

const fn synthetic(purpose: SyntheticPurpose, nullable: bool) -> FieldRule {
    FieldRule::String {
        rule: StringRule::Synthetic(purpose),
        nullable,
    }
}

const fn enumeration(set: EnumSet) -> FieldRule {
    FieldRule::String {
        rule: StringRule::Enum(set),
        nullable: false,
    }
}

pub(crate) fn enum_accepts(set: EnumSet, value: &str) -> bool {
    match set {
        EnumSet::AssistantPhase => matches!(value, "commentary" | "final_answer"),
        EnumSet::ArtifactKind => matches!(
            value,
            "backend_state_matrix"
                | "contract_pins"
                | "index"
                | "manifest"
                | "request"
                | "stream"
                | "transport"
        ),
        EnumSet::CacheMode => matches!(value, "explicit" | "implicit"),
        EnumSet::CacheTtl => value == "30m",
        EnumSet::Dialect => matches!(value, "codex" | "corpus" | "public"),
        EnumSet::Effort => matches!(
            value,
            "none" | "minimal" | "low" | "medium" | "high" | "xhigh"
        ),
        EnumSet::Expectation => matches!(
            value,
            "accepted_evidence"
                | "baseline_red"
                | "contract_target"
                | "dialect_only"
                | "supported_green"
        ),
        EnumSet::IncompleteReason => matches!(value, "content_filter" | "max_output_tokens"),
        EnumSet::Include => include_accepts(value),
        EnumSet::Object => matches!(value, "list" | "response"),
        EnumSet::OwnerPhase => matches!(
            value,
            "P0" | "P1" | "P2" | "P3" | "P4" | "P5" | "P6" | "P7" | "P8" | "P9"
        ),
        EnumSet::ReasoningSummary => matches!(value, "auto" | "concise" | "detailed"),
        EnumSet::Retention => matches!(value, "24h" | "in_memory"),
        EnumSet::Role => matches!(value, "assistant" | "developer" | "system" | "user"),
        EnumSet::SecretProfile => matches!(value, "none" | "registered_synthetic"),
        EnumSet::Status => matches!(
            value,
            "cancelled" | "completed" | "failed" | "in_progress" | "incomplete" | "queued"
        ),
        EnumSet::ToolChoice => matches!(value, "auto" | "none" | "required"),
        EnumSet::Truncation => matches!(value, "auto" | "disabled"),
    }
}

pub(crate) fn string_rule_code(rule: StringRule) -> RedactionCode {
    match rule {
        StringRule::Synthetic(purpose) => sensitive_code(Some(purpose)),
        StringRule::ApprovedSourceUrl | StringRule::FixedUrl => RedactionCode::UnregisteredString,
        StringRule::Category
        | StringRule::Concern
        | StringRule::Enum(_)
        | StringRule::FindingId
        | StringRule::FixtureId
        | StringRule::FixturePath
        | StringRule::HexPin
        | StringRule::JsonPointer
        | StringRule::PinnedSourcePath => RedactionCode::SchemaMismatch,
    }
}

pub(crate) fn sensitive_code(purpose: Option<SyntheticPurpose>) -> RedactionCode {
    match purpose {
        Some(SyntheticPurpose::TurnState | SyntheticPurpose::CacheKey) => {
            RedactionCode::ReusableState
        }
        Some(
            SyntheticPurpose::Generic
            | SyntheticPurpose::AccountId
            | SyntheticPurpose::Credential
            | SyntheticPurpose::PromptContent,
        )
        | None => RedactionCode::ProhibitedField,
    }
}
