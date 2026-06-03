//! Rules loading for the Norn CLI (NC-004 R4).
//!
//! Reads a single YAML-front-matter rule file and constructs a
//! [`RuleEngine`](norn::rules::engine::RuleEngine) wrapping it. The file
//! stem (e.g. `coding` from `coding.yaml`) becomes the rule's
//! [`RuleId`](norn::rules::types::RuleId), matching the existing one-rule-
//! per-file convention enforced by [`norn::rules::parser::parse_rule_file`].
//!
//! Multi-rule documents and directory walking are deliberately out of
//! scope per the brief's R4 scope note — extending this module to a
//! directory walker is a future brief once libnorn grows a multi-rule
//! parser.

use std::path::Path;

use norn::rules::engine::RuleEngine;
use norn::rules::parser::parse_rule_file;
use norn::rules::types::RuleId;

use crate::cli::BuildError;

/// Load a rules file from disk and construct a [`RuleEngine`] containing
/// the single parsed rule.
///
/// The rule's identifier is derived from the file stem (the file name
/// minus its extension). If the path has no stem (only an extension or
/// an empty name), the full file name is used as the identifier.
///
/// # Errors
///
/// Returns [`BuildError::Argument`] when the file cannot be read or when
/// [`parse_rule_file`] rejects the contents.
pub fn load_rule_engine(path: &Path) -> Result<RuleEngine, BuildError> {
    let contents = std::fs::read_to_string(path).map_err(|err| {
        BuildError::Argument(format!(
            "failed to read rules file {}: {err}",
            path.display(),
        ))
    })?;
    let id = derive_rule_id(path);
    let rule = parse_rule_file(id, &contents)?;
    Ok(RuleEngine::new(vec![rule]))
}

fn derive_rule_id(path: &Path) -> RuleId {
    let raw = path
        .file_stem()
        .or_else(|| path.file_name())
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("rule");
    RuleId::from(raw)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    const VALID_RULE: &str = r#"---
name: Test Rule
triggers:
  - type: path_glob
    pattern: "**/*.rs"
delivery: context_injection
---
Use yg diagnostics."#;

    #[test]
    fn load_rule_engine_returns_engine_for_valid_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("coding.yaml");
        std::fs::write(&path, VALID_RULE).unwrap();
        // RuleEngine has no public accessor for rule count, but constructing
        // the engine without error is the brief's acceptance criterion.
        let _engine = load_rule_engine(&path).expect("valid rule must load");
    }

    #[test]
    fn missing_file_returns_argument_error() {
        let result = load_rule_engine(Path::new("/definitely/does-not/exist.yaml"));
        match result {
            Err(BuildError::Argument(reason)) => assert!(reason.contains("exist.yaml")),
            Err(other) => panic!("expected Argument error, got {other:?}"),
            Ok(_) => panic!("expected error for missing file"),
        }
    }

    #[test]
    fn invalid_yaml_returns_argument_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.yaml");
        std::fs::write(&path, "this has no front matter at all").unwrap();
        match load_rule_engine(&path) {
            Err(BuildError::Argument(_)) => {}
            Err(other) => panic!("expected Argument error, got {other:?}"),
            Ok(_) => panic!("expected error for invalid YAML"),
        }
    }

    #[test]
    fn rule_id_derived_from_file_stem() {
        let path = Path::new("/some/dir/coding-rules.yaml");
        let id = derive_rule_id(path);
        assert_eq!(id.as_str(), "coding-rules");
    }

    #[test]
    fn rule_id_falls_back_to_file_name_when_no_stem() {
        let path = Path::new(".rulesfile");
        let id = derive_rule_id(path);
        // `file_stem()` returns the same as `file_name()` when the name is
        // dot-prefixed without a separate extension — accept either as the
        // derived id; the important property is that we never panic.
        assert!(!id.as_str().is_empty());
    }
}
