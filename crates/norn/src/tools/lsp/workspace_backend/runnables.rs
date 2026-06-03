//! Parsing and fallback discovery for [`TestRunnable`] values.
//!
//! Houses the pure JSON parser for `rust-analyzer/relatedTests` responses
//! and the references-plus-call-hierarchy fallback used when the server
//! does not implement the experimental method (C77).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use lsp::workspace::LspWorkspace;

use super::super::backend::{LspBackendError, LspLocation, TestRunnable, TestRunnableKind};
use super::mapping::{range_to_location, uri_to_path};

/// Parse a rust-analyzer `LocationLink`-shaped JSON value into [`LspLocation`].
///
/// Mirrors the 1-based conversion used by the typed `map_location_link` in
/// [`super::mapping`].
pub(super) fn parse_location_link_value(val: &serde_json::Value) -> Option<LspLocation> {
    let target_uri = val.get("targetUri")?.as_str()?;
    let sel = val.get("targetSelectionRange")?;
    let start = sel.get("start")?;
    let end = sel.get("end")?;
    let path = url::Url::parse(target_uri)
        .ok()
        .and_then(|u| u.to_file_path().ok())
        .map_or_else(
            || target_uri.to_owned(),
            |p| p.to_string_lossy().into_owned(),
        );
    let to_u32 = |v: &serde_json::Value, key: &str| -> Option<u32> {
        u32::try_from(v.get(key)?.as_u64()?).ok()
    };
    Some(LspLocation {
        path,
        line: to_u32(start, "line")? + 1,
        column: to_u32(start, "character")? + 1,
        end_line: to_u32(end, "line")? + 1,
        end_column: to_u32(end, "character")? + 1,
    })
}

/// Parse the JSON response of `rust-analyzer/relatedTests` into
/// [`TestRunnable`]s.
///
/// The response shape is `Vec<TestInfo>` where each `TestInfo` carries a
/// `runnable` envelope with `label`, `location`, `kind` and `args`.
/// Elements that fail to parse are skipped — a malformed entry does not
/// fail the whole request.
pub(super) fn parse_related_tests_response(val: &serde_json::Value) -> Vec<TestRunnable> {
    let Some(items) = val.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let Some(r) = item.get("runnable") else {
            continue;
        };
        let Some(label) = r.get("label").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let location = r.get("location").and_then(parse_location_link_value);
        let kind_tag = r
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let mut cargo_args = Vec::new();
        let mut executable_args = Vec::new();
        let mut cwd = None;
        let mut workspace_root = None;
        if kind_tag == "cargo"
            && let Some(args) = r.get("args")
        {
            cargo_args = string_array(args.get("cargoArgs"));
            executable_args = string_array(args.get("executableArgs"));
            cwd = args
                .get("cwd")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            workspace_root = args
                .get("workspaceRoot")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
        }
        out.push(TestRunnable {
            label: label.to_owned(),
            kind: TestRunnableKind::Test,
            location,
            cargo_args,
            executable_args,
            cwd,
            workspace_root,
        });
    }
    out
}

fn string_array(val: Option<&serde_json::Value>) -> Vec<String> {
    val.and_then(serde_json::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Fallback path for `related_tests` when the server does not support
/// `rust-analyzer/relatedTests`. Uses `textDocument/references` followed
/// by incoming call hierarchy, filtering to symbols that look like tests.
pub(super) async fn fallback_related_tests_via_callhierarchy(
    workspace: &LspWorkspace,
    path: &Path,
    line: u32,
    column: u32,
) -> Result<Vec<TestRunnable>, LspBackendError> {
    let position = lsp_types::Position::new(line, column);
    let refs = match workspace.find_references(path, position, false).await {
        Ok(r) => r.unwrap_or_default(),
        Err(e) => {
            tracing::warn!(error = %e, "references lookup failed in related_tests fallback");
            return Ok(Vec::new());
        }
    };
    let mut seen: HashSet<(String, u32, u32, String)> = HashSet::new();
    let mut out: Vec<TestRunnable> = Vec::new();
    for loc in &refs {
        let ref_path = PathBuf::from(uri_to_path(&loc.uri));
        let ref_pos = loc.range.start;
        let items = match workspace.prepare_call_hierarchy(&ref_path, ref_pos).await {
            Ok(opt) => opt.unwrap_or_default(),
            Err(_) => continue,
        };
        for item in &items {
            let incoming = match workspace.incoming_calls(&ref_path, item).await {
                Ok(opt) => opt.unwrap_or_default(),
                Err(_) => continue,
            };
            for call in incoming {
                let from = call.from;
                let name_lc = from.name.to_lowercase();
                let is_test = from.name.starts_with("test_")
                    || from.name == "tests"
                    || (from.kind == lsp_types::SymbolKind::FUNCTION && name_lc.contains("test"));
                if !is_test {
                    continue;
                }
                let from_path = PathBuf::from(uri_to_path(&from.uri));
                let location = range_to_location(from.selection_range, &from_path);
                let key = (
                    from.name.clone(),
                    location.line,
                    location.column,
                    location.path.clone(),
                );
                if seen.insert(key) {
                    out.push(TestRunnable {
                        label: from.name,
                        kind: TestRunnableKind::Test,
                        location: Some(location),
                        cargo_args: Vec::new(),
                        executable_args: Vec::new(),
                        cwd: None,
                        workspace_root: None,
                    });
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn parse_related_tests_response_cargo() {
        let raw = serde_json::json!([{
            "runnable": {
                "label": "test foo::bar",
                "location": {
                    "targetUri": "file:///tmp/x.rs",
                    "targetSelectionRange": {
                        "start": { "line": 0, "character": 4 },
                        "end": { "line": 0, "character": 12 }
                    }
                },
                "kind": "cargo",
                "args": {
                    "workspaceRoot": "/tmp",
                    "cwd": "/tmp",
                    "cargoArgs": ["test", "--package", "x"],
                    "executableArgs": ["foo::bar", "--exact"]
                }
            }
        }]);
        let out = parse_related_tests_response(&raw);
        assert_eq!(out.len(), 1);
        let r = &out[0];
        assert_eq!(r.label, "test foo::bar");
        assert_eq!(r.kind, TestRunnableKind::Test);
        assert_eq!(r.cargo_args, vec!["test", "--package", "x"]);
        assert_eq!(r.executable_args, vec!["foo::bar", "--exact"]);
        assert_eq!(r.cwd.as_deref(), Some("/tmp"));
        assert_eq!(r.workspace_root.as_deref(), Some("/tmp"));
        let loc = r.location.as_ref().expect("location present");
        assert_eq!(loc.line, 1);
        assert_eq!(loc.column, 5);
        assert_eq!(loc.end_line, 1);
        assert_eq!(loc.end_column, 13);
    }

    #[test]
    fn parse_related_tests_response_shell() {
        let raw = serde_json::json!([{
            "runnable": {
                "label": "shell test",
                "kind": "shell",
                "args": { "program": "/bin/true" }
            }
        }]);
        let out = parse_related_tests_response(&raw);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].label, "shell test");
        assert!(out[0].cargo_args.is_empty());
        assert!(out[0].executable_args.is_empty());
        assert!(out[0].cwd.is_none());
        assert!(out[0].workspace_root.is_none());
    }

    #[test]
    fn parse_related_tests_response_empty() {
        let raw = serde_json::json!([]);
        let out = parse_related_tests_response(&raw);
        assert!(out.is_empty());
    }

    #[test]
    fn parse_related_tests_response_malformed_skips() {
        let raw = serde_json::json!([
            { "runnable": { "kind": "cargo" } },
            {
                "runnable": {
                    "label": "ok",
                    "kind": "cargo",
                    "args": { "cargoArgs": ["test"], "executableArgs": [] }
                }
            }
        ]);
        let out = parse_related_tests_response(&raw);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].label, "ok");
    }

    #[test]
    fn parse_related_tests_response_non_array_is_empty() {
        let raw = serde_json::json!({ "not": "an array" });
        let out = parse_related_tests_response(&raw);
        assert!(out.is_empty());
    }

    #[test]
    fn parse_location_link_value_translates_zero_based_to_one_based() {
        let raw = serde_json::json!({
            "targetUri": "file:///tmp/x.rs",
            "targetSelectionRange": {
                "start": { "line": 9, "character": 0 },
                "end": { "line": 9, "character": 7 }
            }
        });
        let loc = parse_location_link_value(&raw).expect("parses");
        assert_eq!(loc.line, 10);
        assert_eq!(loc.column, 1);
        assert_eq!(loc.end_line, 10);
        assert_eq!(loc.end_column, 8);
    }

    #[test]
    fn parse_location_link_value_missing_fields_returns_none() {
        let raw = serde_json::json!({ "targetUri": "file:///tmp/x.rs" });
        assert!(parse_location_link_value(&raw).is_none());
    }
}
