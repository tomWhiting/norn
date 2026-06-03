//! Expiry evaluation for follow-up actions.
//!
//! A registered follow-up carries an
//! [`ExpiryCondition`](crate::tool::follow_up::ExpiryCondition) that
//! invalidates it once the world it was registered against has moved on. The
//! `follow_up` tool checks expiry at execution time — never on a timer — so a
//! deferred action that referenced a file's prior contents (or a turn that has
//! since ended) is refused with a clear, structured error instead of silently
//! dispatching against stale assumptions.
//!
//! File hashing reuses
//! [`session::action_log::hash_content`](crate::session::action_log::hash_content)
//! so the digest matches the hashes mutation tools already store. A file that
//! cannot be read (deleted, moved, permission-denied) is treated as expired:
//! the follow-up's referenced state no longer exists.

use std::path::Path;

use serde_json::Value;

use crate::session::action_log::hash_content;
use crate::tool::follow_up::ExpiryCondition;

/// A follow-up action that is no longer valid.
///
/// Serialised into the `follow_up` tool's structured error output. Every field
/// is populated so the model knows which action expired, against which call,
/// why, and what to do instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpiredError {
    /// Human-readable explanation of which file changed or turn ended.
    pub reason: String,
    /// Tool-call id the expired follow-up was registered against.
    pub tool_call_id: String,
    /// Name of the expired action.
    pub action: String,
    /// Recovery guidance for the model.
    pub suggestion: String,
}

impl ExpiredError {
    /// Render the error as the structured JSON content of a `ToolOutput`.
    #[must_use]
    pub fn to_content(&self) -> Value {
        serde_json::json!({
            "error": "follow-up expired",
            "reason": self.reason,
            "tool_call_id": self.tool_call_id,
            "action": self.action,
            "suggestion": self.suggestion,
        })
    }
}

/// Standard recovery guidance shared by every expiry reason.
fn suggestion() -> String {
    "The state this follow-up referenced has changed. Re-read the affected \
     file(s) or re-run the original tool to regenerate a fresh action, or call \
     the target tool directly with current arguments."
        .to_owned()
}

/// Evaluate whether the follow-up identified by `tool_call_id`/`action` with
/// the given `condition` is still valid.
///
/// Returns `Ok(())` when the follow-up is still valid and `Err(ExpiredError)`
/// otherwise. `current_turn_id` is the runtime's current turn id when known;
/// [`ExpiryCondition::TurnScoped`] follow-ups are treated as expired when it is
/// `None` (the runtime has not threaded turn state through). `resolve` maps a
/// recorded path to an absolute filesystem path (typically
/// [`ToolContext::resolve_path`](crate::tool::context::ToolContext::resolve_path)).
///
/// # Errors
///
/// Returns [`ExpiredError`] describing the first condition that no longer holds.
pub fn check_not_expired<F>(
    condition: &ExpiryCondition,
    current_turn_id: Option<&str>,
    tool_call_id: &str,
    action: &str,
    resolve: &F,
) -> Result<(), ExpiredError>
where
    F: Fn(&Path) -> std::path::PathBuf,
{
    let expired = |reason: String| ExpiredError {
        reason,
        tool_call_id: tool_call_id.to_owned(),
        action: action.to_owned(),
        suggestion: suggestion(),
    };

    match condition {
        ExpiryCondition::FileModified { path, content_hash } => {
            match read_and_hash(path, resolve) {
                Some(current) if current == *content_hash => Ok(()),
                Some(_) => Err(expired(format!(
                    "file {} has been modified since the follow-up was registered",
                    path.display(),
                ))),
                None => Err(expired(format!(
                    "file {} is no longer readable (it may have been deleted or moved)",
                    path.display(),
                ))),
            }
        }
        ExpiryCondition::AnyFileModified { files } => {
            // Sort paths so the reported "first changed file" is deterministic
            // regardless of the underlying map's iteration order.
            let mut entries: Vec<(&std::path::PathBuf, &String)> = files.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            for (path, expected) in entries {
                match read_and_hash(path, resolve) {
                    Some(current) if current == *expected => {}
                    Some(_) => {
                        return Err(expired(format!(
                            "file {} has been modified since the follow-up was registered",
                            path.display(),
                        )));
                    }
                    None => {
                        return Err(expired(format!(
                            "file {} is no longer readable (it may have been deleted or moved)",
                            path.display(),
                        )));
                    }
                }
            }
            Ok(())
        }
        ExpiryCondition::TurnScoped { turn_id } => {
            if current_turn_id == Some(turn_id.as_str()) {
                Ok(())
            } else {
                Err(expired(format!(
                    "the turn that scoped this follow-up ({turn_id}) has ended",
                )))
            }
        }
        ExpiryCondition::Never => Ok(()),
    }
}

/// Read the resolved file and return its content hash, or `None` when the file
/// cannot be read.
fn read_and_hash<F>(path: &Path, resolve: &F) -> Option<String>
where
    F: Fn(&Path) -> std::path::PathBuf,
{
    let resolved = resolve(path);
    match std::fs::read(&resolved) {
        Ok(bytes) => Some(hash_content(&bytes)),
        Err(error) => {
            tracing::debug!(
                path = %resolved.display(),
                %error,
                "follow-up expiry: file unreadable, treating action as expired",
            );
            None
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use super::*;

    fn identity(p: &Path) -> PathBuf {
        p.to_path_buf()
    }

    #[test]
    fn file_modified_ok_when_hash_matches() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, b"contents").unwrap();
        let condition = ExpiryCondition::FileModified {
            path,
            content_hash: hash_content(b"contents"),
        };
        assert!(check_not_expired(&condition, None, "tc-1", "reapply", &identity).is_ok());
    }

    #[test]
    fn file_modified_err_when_hash_differs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, b"changed").unwrap();
        let condition = ExpiryCondition::FileModified {
            path: path.clone(),
            content_hash: hash_content(b"original"),
        };
        let err = check_not_expired(&condition, None, "tc-1", "reapply", &identity)
            .expect_err("modified file expires");
        assert!(err.reason.contains("has been modified"));
        assert!(err.reason.contains(&path.display().to_string()));
        assert_eq!(err.tool_call_id, "tc-1");
        assert_eq!(err.action, "reapply");
        assert!(!err.suggestion.is_empty());
    }

    #[test]
    fn file_read_error_is_treated_as_expired() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("never_created.txt");
        let condition = ExpiryCondition::FileModified {
            path: missing,
            content_hash: hash_content(b"anything"),
        };
        let err = check_not_expired(&condition, None, "tc-1", "reapply", &identity)
            .expect_err("missing file expires");
        assert!(err.reason.contains("no longer readable"));
    }

    #[test]
    fn any_file_modified_err_names_first_changed_file() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        std::fs::write(&a, b"a-original").unwrap();
        std::fs::write(&b, b"b-changed").unwrap();
        let mut files = HashMap::new();
        files.insert(a, hash_content(b"a-original"));
        files.insert(b.clone(), hash_content(b"b-original"));
        let condition = ExpiryCondition::AnyFileModified { files };
        let err = check_not_expired(&condition, None, "tc-1", "reapply", &identity)
            .expect_err("changed file expires");
        // Sorted order: a.txt is unchanged, b.txt changed → b.txt reported.
        assert!(err.reason.contains(&b.display().to_string()));
    }

    #[test]
    fn any_file_modified_ok_when_all_match() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();
        let mut files = HashMap::new();
        files.insert(a, hash_content(b"a"));
        files.insert(b, hash_content(b"b"));
        let condition = ExpiryCondition::AnyFileModified { files };
        assert!(check_not_expired(&condition, None, "tc-1", "reapply", &identity).is_ok());
    }

    #[test]
    fn turn_scoped_ok_when_turn_matches() {
        let condition = ExpiryCondition::TurnScoped {
            turn_id: "turn-1".to_owned(),
        };
        assert!(check_not_expired(&condition, Some("turn-1"), "tc-1", "retry", &identity).is_ok());
    }

    #[test]
    fn turn_scoped_err_when_turn_differs() {
        let condition = ExpiryCondition::TurnScoped {
            turn_id: "turn-1".to_owned(),
        };
        let err = check_not_expired(&condition, Some("turn-2"), "tc-1", "retry", &identity)
            .expect_err("turn mismatch expires");
        assert!(err.reason.contains("turn-1"));
    }

    #[test]
    fn turn_scoped_err_when_turn_unknown() {
        let condition = ExpiryCondition::TurnScoped {
            turn_id: "turn-1".to_owned(),
        };
        assert!(check_not_expired(&condition, None, "tc-1", "retry", &identity).is_err());
    }

    #[test]
    fn never_always_ok() {
        assert!(
            check_not_expired(&ExpiryCondition::Never, None, "tc-1", "undo", &identity).is_ok()
        );
    }

    #[test]
    fn expired_error_content_carries_all_fields() {
        let err = ExpiredError {
            reason: "file x changed".to_owned(),
            tool_call_id: "tc-9".to_owned(),
            action: "reapply".to_owned(),
            suggestion: "do something".to_owned(),
        };
        let content = err.to_content();
        assert_eq!(content["error"], "follow-up expired");
        assert_eq!(content["reason"], "file x changed");
        assert_eq!(content["tool_call_id"], "tc-9");
        assert_eq!(content["action"], "reapply");
        assert_eq!(content["suggestion"], "do something");
    }
}
