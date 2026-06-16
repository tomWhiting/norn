//! Follow-up action vocabulary: `FollowUpAction`, `ExpiryCondition`, `BeforeContentSource`.
//!
//! These are foundational types that downstream briefs (lifecycle phase,
//! follow-up tool dispatch, undo registration, macros) build on. This module
//! defines only the serializable shapes — no expiry checking, no dispatch,
//! no undo logic.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A deferred action the agent can execute later via the follow-up tool.
///
/// Registered by tools at the end of their lifecycle (success or error) and
/// surfaced to the model in the tool result's `follow_ups` array. The model
/// invokes one by passing the original `tool_call_id` and this action's
/// `action` name to the follow-up tool.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FollowUpAction {
    /// Short action name, unique within a single tool call (e.g. "undo",
    /// "`apply_structural`", "`apply_at_occurrence_2`").
    pub action: String,
    /// Human-readable description shown to the model.
    pub description: String,
    /// Name of the target tool to invoke when the follow-up is executed.
    pub tool: String,
    /// Pre-populated arguments or argument overrides used when the action is
    /// executed. Interpretation is controlled by [`Self::args_mode`].
    pub args: serde_json::Value,
    /// How [`Self::args`] is applied to the original tool-call arguments.
    ///
    /// Defaults to [`FollowUpArgsMode::MergeOriginal`] so existing persisted
    /// and in-memory follow-ups retain their historical behavior.
    #[serde(default)]
    pub args_mode: FollowUpArgsMode,
    /// Condition that, when no longer true, invalidates this follow-up.
    pub expires: ExpiryCondition,
    /// Confidence the registering tool has in this suggestion.
    pub confidence: Confidence,

    /// Source of pre-mutation content for undo follow-ups. Non-undo actions
    /// use `BeforeContentSource::Unavailable`.
    pub before_content: BeforeContentSource,
}

impl FollowUpAction {
    /// Returns the model-facing subset of this action: `action`, `description`,
    /// and `expires` as a compact string. Internal fields (`tool`, `args`,
    /// `confidence`, `before_content`) are omitted to keep the model-visible
    /// payload minimal.
    #[must_use]
    pub fn model_facing_json(&self) -> serde_json::Value {
        serde_json::json!({
            "action": self.action,
            "description": self.description,
            "expires": self.expires.model_facing(),
        })
    }
}

/// How a follow-up action builds the target tool's arguments.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FollowUpArgsMode {
    /// Shallowly merge [`FollowUpAction::args`] onto the original call's
    /// arguments. This is the historical mode and is suited to same-tool
    /// refinements such as "rerun with a longer timeout".
    #[default]
    MergeOriginal,
    /// Dispatch the target tool with [`FollowUpAction::args`] exactly. This
    /// is required for cross-tool affordances such as "resume the recipient"
    /// where inheriting the original tool's fields would be invalid.
    Replace,
}

/// How likely a registering tool believes a suggested follow-up action is to
/// succeed if executed.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Confidence {
    /// The action is very likely to succeed.
    High,
    /// The action may succeed but conditions are uncertain.
    Medium,
    /// The action is speculative.
    Low,
}

/// Condition under which a follow-up action is no longer valid.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ExpiryCondition {
    /// Expires when the file's content hash no longer matches the recorded
    /// hash.
    FileModified {
        /// Path of the file whose modification invalidates this action.
        path: PathBuf,
        /// Hash of the file's content at the time the follow-up was
        /// registered.
        content_hash: String,
    },
    /// Expires when any file in the set has been modified since registration.
    AnyFileModified {
        /// Content hash recorded at registration for each file path.
        files: HashMap<PathBuf, String>,
    },
    /// Expires at the end of the named turn.
    TurnScoped {
        /// Identifier of the turn that scopes this action's lifetime.
        turn_id: String,
    },
    /// Never expires. Reserved for actions whose validity does not depend on
    /// file or turn state.
    Never,
}

impl ExpiryCondition {
    /// Returns the compact, model-facing description of this expiry condition.
    ///
    /// Maps to the shapes documented in the design doc (D2):
    /// `"file_modified:<path>"`, `"any_file_modified"`, `"turn_scoped"`,
    /// `"never"`. The hash and turn identifier are omitted — those are
    /// implementation details the model does not need.
    #[must_use]
    pub fn model_facing(&self) -> serde_json::Value {
        match self {
            Self::FileModified { path, .. } => {
                serde_json::Value::String(format!("file_modified:{}", path.display()))
            }
            Self::AnyFileModified { .. } => serde_json::Value::String("any_file_modified".into()),
            Self::TurnScoped { .. } => serde_json::Value::String("turn_scoped".into()),
            Self::Never => serde_json::Value::String("never".into()),
        }
    }
}

/// Source of the pre-mutation content used to satisfy an undo follow-up.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub enum BeforeContentSource {
    /// Undo by reverting a Yggdrasil operation. Preferred when libyggd
    /// tracking is active because it provides atomic multi-file revert and
    /// integrates with the operation audit trail.
    YggdrasilOp {
        /// Identifier of the Yggdrasil operation to revert.
        operation_id: String,
    },
    /// Undo by writing the stored original content directly. Used when
    /// Yggdrasil tracking is not available, and for per-file undo of
    /// otherwise atomic multi-file operations.
    StoredContent {
        /// Original content keyed by file path.
        files: HashMap<PathBuf, String>,
    },
    /// No undo source available. Undo attempts return a clear error rather
    /// than failing silently.
    #[default]
    Unavailable,
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn sample_action() -> FollowUpAction {
        FollowUpAction {
            action: "apply_structural".into(),
            description: "Apply using structural matching (entity: fn process_event, drift: 5)"
                .into(),
            tool: "apply_patch".into(),
            args: serde_json::json!({ "mode": "structural" }),
            args_mode: FollowUpArgsMode::MergeOriginal,
            expires: ExpiryCondition::FileModified {
                path: PathBuf::from("src/handler.rs"),
                content_hash: "abc123".into(),
            },
            confidence: Confidence::High,
            before_content: BeforeContentSource::Unavailable,
        }
    }

    #[test]
    fn follow_up_action_serde_roundtrip() -> Result<(), serde_json::Error> {
        let original = sample_action();
        let json = serde_json::to_string(&original)?;
        let parsed: FollowUpAction = serde_json::from_str(&json)?;

        let original_value = serde_json::to_value(&original)?;
        let parsed_value = serde_json::to_value(&parsed)?;
        assert_eq!(original_value, parsed_value);

        let obj = original_value.as_object().expect("top-level object");
        assert_eq!(obj.len(), 8);
        for key in [
            "action",
            "description",
            "tool",
            "args",
            "args_mode",
            "expires",
            "confidence",
            "before_content",
        ] {
            assert!(obj.contains_key(key), "missing key: {key}");
        }
        Ok(())
    }

    #[test]
    fn follow_up_action_model_facing_json_shape() {
        let action = sample_action();
        let facing = action.model_facing_json();
        let obj = facing.as_object().expect("object");
        assert_eq!(obj.len(), 3);
        assert_eq!(
            obj.get("action").and_then(|v| v.as_str()),
            Some("apply_structural")
        );
        assert!(obj.contains_key("description"));
        assert_eq!(
            obj.get("expires").and_then(|v| v.as_str()),
            Some("file_modified:src/handler.rs")
        );

        let expires_val = obj.get("expires").expect("expires key");
        let expires_str = expires_val.as_str().expect("expires is a string");
        assert!(
            expires_str.contains("src/handler.rs"),
            "expires should contain the file path"
        );
        assert!(
            !expires_str.contains("content_hash"),
            "expires should not leak content_hash"
        );

        assert!(
            !obj.contains_key("tool"),
            "model-facing JSON must not include internal tool field"
        );
        assert!(
            !obj.contains_key("args"),
            "model-facing JSON must not include internal args field"
        );
        assert!(
            !obj.contains_key("args_mode"),
            "model-facing JSON must not include internal args mode field"
        );
        assert!(
            !obj.contains_key("confidence"),
            "model-facing JSON must not include internal confidence field"
        );
        assert!(
            !obj.contains_key("before_content"),
            "model-facing JSON must not include internal before_content field"
        );
    }

    #[test]
    fn expiry_file_modified_roundtrip() -> Result<(), serde_json::Error> {
        let original = ExpiryCondition::FileModified {
            path: PathBuf::from("src/lib.rs"),
            content_hash: "deadbeef".into(),
        };
        let json = serde_json::to_string(&original)?;
        let parsed: ExpiryCondition = serde_json::from_str(&json)?;
        assert_eq!(
            serde_json::to_value(&original)?,
            serde_json::to_value(&parsed)?
        );
        match parsed {
            ExpiryCondition::FileModified { path, content_hash } => {
                assert_eq!(path, PathBuf::from("src/lib.rs"));
                assert_eq!(content_hash, "deadbeef");
            }
            other => panic!("expected FileModified, got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn expiry_any_file_modified_roundtrip() -> Result<(), serde_json::Error> {
        let mut files = HashMap::new();
        files.insert(PathBuf::from("a.rs"), "h1".into());
        files.insert(PathBuf::from("b.rs"), "h2".into());
        let original = ExpiryCondition::AnyFileModified { files };
        let json = serde_json::to_string(&original)?;
        let parsed: ExpiryCondition = serde_json::from_str(&json)?;
        assert_eq!(
            serde_json::to_value(&original)?,
            serde_json::to_value(&parsed)?
        );
        match parsed {
            ExpiryCondition::AnyFileModified { files } => {
                assert_eq!(files.len(), 2);
                assert_eq!(files.get(&PathBuf::from("a.rs")), Some(&"h1".to_string()));
            }
            other => panic!("expected AnyFileModified, got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn expiry_turn_scoped_roundtrip() -> Result<(), serde_json::Error> {
        let original = ExpiryCondition::TurnScoped {
            turn_id: "turn-42".into(),
        };
        let json = serde_json::to_string(&original)?;
        let parsed: ExpiryCondition = serde_json::from_str(&json)?;
        assert_eq!(
            serde_json::to_value(&original)?,
            serde_json::to_value(&parsed)?
        );
        match parsed {
            ExpiryCondition::TurnScoped { turn_id } => assert_eq!(turn_id, "turn-42"),
            other => panic!("expected TurnScoped, got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn expiry_never_roundtrip() -> Result<(), serde_json::Error> {
        let original = ExpiryCondition::Never;
        let json = serde_json::to_string(&original)?;
        let parsed: ExpiryCondition = serde_json::from_str(&json)?;
        assert_eq!(
            serde_json::to_value(&original)?,
            serde_json::to_value(&parsed)?
        );
        assert!(matches!(parsed, ExpiryCondition::Never));
        Ok(())
    }

    #[test]
    fn before_content_yggdrasil_op_roundtrip() -> Result<(), serde_json::Error> {
        let original = BeforeContentSource::YggdrasilOp {
            operation_id: "op-001".into(),
        };
        let json = serde_json::to_string(&original)?;
        let parsed: BeforeContentSource = serde_json::from_str(&json)?;
        assert_eq!(
            serde_json::to_value(&original)?,
            serde_json::to_value(&parsed)?
        );
        match parsed {
            BeforeContentSource::YggdrasilOp { operation_id } => assert_eq!(operation_id, "op-001"),
            other => panic!("expected YggdrasilOp, got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn before_content_stored_content_roundtrip() -> Result<(), serde_json::Error> {
        let mut files = HashMap::new();
        files.insert(PathBuf::from("src/x.rs"), "old contents".into());
        let original = BeforeContentSource::StoredContent { files };
        let json = serde_json::to_string(&original)?;
        let parsed: BeforeContentSource = serde_json::from_str(&json)?;
        assert_eq!(
            serde_json::to_value(&original)?,
            serde_json::to_value(&parsed)?
        );
        match parsed {
            BeforeContentSource::StoredContent { files } => {
                assert_eq!(
                    files.get(&PathBuf::from("src/x.rs")),
                    Some(&"old contents".to_string())
                );
            }
            other => panic!("expected StoredContent, got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn before_content_unavailable_is_default_and_roundtrips() -> Result<(), serde_json::Error> {
        let default = BeforeContentSource::default();
        assert!(matches!(default, BeforeContentSource::Unavailable));

        let json = serde_json::to_string(&default)?;
        let parsed: BeforeContentSource = serde_json::from_str(&json)?;
        assert!(matches!(parsed, BeforeContentSource::Unavailable));
        Ok(())
    }
}
