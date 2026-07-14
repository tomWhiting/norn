//! The unified [`SearchTool`]: argument parsing, mode planning,
//! workspace confinement, and blocking-work dispatch.

use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::ToolError;
use crate::resource::{FilesystemOperationPermit, acquire_recursive_walk};
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::failure::{ToolErrorKind, ToolErrorPayload};
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::{Tool, ToolCategory, ToolOutput};
use crate::tools::confinement::check_read_confinement;

use super::ast_search::run_ast_search;
use super::content::run_content_search;
use super::file_find::run_file_find;
use super::fuzzy::run_fuzzy_search;

/// Default cap on matches returned by a single search invocation,
/// overridable per call via the `max_results` argument.
const DEFAULT_MAX_RESULTS: u32 = 50;

/// Model-supplied arguments for [`SearchTool`].
#[derive(Debug, Default, Deserialize, Serialize)]
struct SearchArgs {
    /// Regex pattern (content mode) or fuzzy needle (fuzzy mode).
    #[serde(default)]
    pattern: Option<String>,
    /// Optional directory root to search within. Relative paths resolve
    /// against the agent working directory, which is also the default.
    #[serde(default)]
    path: Option<String>,
    /// Optional glob filter. With `pattern` it scopes the content walk; on
    /// its own it switches the tool into file-finding mode.
    #[serde(default)]
    glob: Option<String>,
    /// Cap on returned matches/paths. Defaults to `DEFAULT_MAX_RESULTS`.
    #[serde(default)]
    max_results: Option<u32>,
    /// Explicit operating mode. One of `content`, `files`, `fuzzy`, `ast`.
    /// When absent the tool falls back to legacy detection from
    /// `pattern`/`glob`.
    #[serde(default)]
    mode: Option<String>,
    /// Tree-sitter S-expression query, required when `mode == "ast"`.
    #[serde(default)]
    ast_query: Option<String>,
    /// Include entries normally excluded by gitignore/hidden-file rules.
    /// `.git` internals and sensitive files (environment files, keys,
    /// certificates, credential files) stay excluded regardless.
    #[serde(default)]
    include_ignored: bool,
}

/// A fully validated search invocation, ready to run on a blocking thread.
#[derive(Debug)]
enum SearchPlan {
    /// Regex content search (`pattern` is the regex).
    Content {
        /// Regex source to compile and match per line.
        pattern: String,
    },
    /// Glob file finding (`glob` is the pattern, matched relative to root).
    Files {
        /// Glob pattern to expand under the search root.
        glob: String,
    },
    /// Fuzzy path ranking (`needle` scores candidate paths).
    Fuzzy {
        /// Fuzzy needle scored against each candidate path.
        needle: String,
    },
    /// Tree-sitter structural search (`query` is the S-expression source).
    Ast {
        /// S-expression query source, compiled per candidate language.
        query: String,
    },
}

/// Unified Search tool: regex content search, glob file finding, fuzzy
/// matching, and tree-sitter structural search.
#[derive(Debug, Default)]
pub struct SearchTool;

impl SearchTool {
    /// Creates a new `SearchTool`.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for SearchTool {
    fn name(&self) -> &'static str {
        "search"
    }

    fn description(&self) -> &'static str {
        include_str!("../guidance/search.description.md")
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Search
    }

    fn usage_guidance(&self) -> Option<&str> {
        Some(include_str!("../guidance/search.usage.md"))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regex pattern for content search, or fuzzy needle when mode=fuzzy."
                },
                "path": {
                    "type": "string",
                    "description": "Directory root to search within. Relative paths resolve against the agent working directory, which is also the default root."
                },
                "glob": {
                    "type": "string",
                    "description": "Glob filter (e.g. `**/*.rs`). Restricts which files are considered in content, fuzzy, and ast modes; lists matching files in files mode."
                },
                "max_results": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Maximum matches/paths to return. Defaults to 50."
                },
                "mode": {
                    "type": "string",
                    "enum": ["content", "files", "fuzzy", "ast"],
                    "description": "Operating mode. Omit for legacy detection (content if `pattern` given, files if only `glob` given)."
                },
                "ast_query": {
                    "type": "string",
                    "description": "Tree-sitter S-expression query. Required when mode=ast."
                },
                "include_ignored": {
                    "type": "boolean",
                    "description": "Include entries normally excluded by gitignore rules and hidden-file filtering. Defaults to false. Two exclusions always hold: the .git directory is never traversed, and sensitive files (.env*/*.env, key/certificate material like .pem/.key/.p12, credential files like id_rsa/.netrc) are never searched — they are listed under `skipped` instead; use the read tool on the exact path for deliberate access."
                }
            },
            "additionalProperties": false
        })
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    async fn execute(
        &self,
        envelope: &ToolEnvelope,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: SearchArgs =
            serde_json::from_value(envelope.model_args.clone()).map_err(|e| {
                ToolError::pre_validation(
                    ToolErrorKind::InvalidArguments,
                    format!("invalid search arguments: {e}"),
                )
            })?;

        let plan = build_plan(&args)?;
        let max_results = args.max_results.unwrap_or(DEFAULT_MAX_RESULTS);

        let root: PathBuf = match args.path.as_deref() {
            Some(p) => ctx.resolve_path(p),
            None => ctx.working_dir(),
        };

        // Workspace confinement (opt-in): refuse before touching disk so a
        // confined agent cannot exfiltrate content through search that the
        // read tool refuses. Read-class, so it honours the same skill /
        // profile / config carve-out as the read tool (DECISIONS §0.6(b)).
        if let Err(reason) = check_read_confinement(ctx, &root) {
            let requested = args
                .path
                .clone()
                .unwrap_or_else(|| root.to_string_lossy().into_owned());
            return Ok(ToolOutput::failure_with_content(
                json!({ "path": requested, "kind": "confinement_refused" }),
                ToolErrorPayload::new(
                    ToolErrorKind::PermissionDenied,
                    format!("search refused: {reason}"),
                )
                .with_detail(json!({ "path": requested })),
            ));
        }

        let glob = args.glob;
        let include_ignored = args.include_ignored;
        let permit = acquire_search_walk()?;

        // Walks, file reads, and tree-sitter parsing are synchronous and
        // potentially heavy; keep them off the async executor.
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            match plan {
                SearchPlan::Content { pattern } => run_content_search(
                    &pattern,
                    &root,
                    glob.as_deref(),
                    max_results,
                    include_ignored,
                ),
                SearchPlan::Files { glob: pattern } => {
                    run_file_find(&root, &pattern, max_results, include_ignored)
                }
                SearchPlan::Fuzzy { needle } => run_fuzzy_search(
                    &needle,
                    &root,
                    glob.as_deref(),
                    max_results,
                    include_ignored,
                ),
                SearchPlan::Ast { query } => {
                    run_ast_search(&query, &root, glob.as_deref(), max_results, include_ignored)
                }
            }
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed {
            reason: format!("search worker task failed: {e}"),
        })?
    }
}

fn acquire_search_walk() -> Result<FilesystemOperationPermit, ToolError> {
    acquire_recursive_walk().map_err(|error| ToolError::DescriptorAdmission(Box::new(error)))
}

/// Validate mode-specific arguments and produce the invocation plan.
fn build_plan(args: &SearchArgs) -> Result<SearchPlan, ToolError> {
    let normalized_mode = args.mode.as_deref().map(str::to_ascii_lowercase);

    match normalized_mode.as_deref() {
        Some("content") => {
            let pattern = args.pattern.clone().ok_or_else(|| {
                ToolError::pre_validation(
                    ToolErrorKind::InvalidArguments,
                    "mode=content requires `pattern`",
                )
            })?;
            Ok(SearchPlan::Content { pattern })
        }
        Some("files") => {
            let glob = args.glob.clone().ok_or_else(|| {
                ToolError::pre_validation(
                    ToolErrorKind::InvalidArguments,
                    "mode=files requires `glob`",
                )
            })?;
            Ok(SearchPlan::Files { glob })
        }
        Some("fuzzy") => match args.pattern.clone().filter(|p| !p.is_empty()) {
            Some(needle) => Ok(SearchPlan::Fuzzy { needle }),
            None => Err(ToolError::pre_validation(
                ToolErrorKind::InvalidArguments,
                "mode=fuzzy requires a non-empty `pattern`",
            )),
        },
        Some("ast") => match args.ast_query.clone().filter(|q| !q.is_empty()) {
            Some(query) => Ok(SearchPlan::Ast { query }),
            None => Err(ToolError::pre_validation(
                ToolErrorKind::InvalidArguments,
                "mode=ast requires a non-empty `ast_query`",
            )),
        },
        Some(other) => Err(ToolError::pre_validation(
            ToolErrorKind::InvalidArguments,
            format!("unknown search mode `{other}` -- valid: content, files, fuzzy, ast"),
        )),
        None => match (args.pattern.clone(), args.glob.clone()) {
            (Some(pattern), _) => Ok(SearchPlan::Content { pattern }),
            (None, Some(glob)) => Ok(SearchPlan::Files { glob }),
            (None, None) => Err(ToolError::pre_validation(
                ToolErrorKind::InvalidArguments,
                "at least one of `pattern` or `glob` must be supplied (or set `mode` \
                 explicitly)",
            )),
        },
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::no_effect_underscore_binding,
    clippy::useless_vec,
    clippy::missing_const_for_fn,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants
)]
mod tests {
    use super::*;
    use crate::tool::context::{SharedWorkingDir, ToolContext};
    use crate::tool::envelope::ToolEnvelope;
    use serde_json::json;
    use std::path::Path;
    use tempfile::tempdir;

    const SEARCH_ADMISSION_HELPER_CHILD: &str = "NORN_SEARCH_ADMISSION_HELPER_CHILD";

    fn envelope(args: Value) -> ToolEnvelope {
        ToolEnvelope {
            tool_call_id: "call-1".to_owned(),
            tool_name: "search".to_owned(),
            model_args: args,
            metadata: Value::Null,
        }
    }

    fn ctx_with_working_dir(dir: &Path) -> ToolContext {
        ToolContext::with_working_dir(SharedWorkingDir::new(dir.to_path_buf()))
    }

    fn confined_ctx(root: &Path) -> ToolContext {
        let mut ctx = ctx_with_working_dir(root);
        ctx.confine_to_workspace(root.to_path_buf());
        ctx
    }

    #[test]
    fn object_safe() {
        let _: Box<dyn Tool + Send + Sync> = Box::new(SearchTool::new());
    }

    #[test]
    fn effect_is_read_only() {
        assert_eq!(SearchTool::new().effect(), ToolEffect::ReadOnly);
    }

    #[test]
    fn search_walk_admission_reserves_recursive_peak() -> Result<(), Box<dyn std::error::Error>> {
        const TEST_NAME: &str =
            "tools::search::tool::tests::search_walk_admission_reserves_recursive_peak";
        if std::env::var_os(SEARCH_ADMISSION_HELPER_CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe()?)
                .args(["--exact", TEST_NAME, "--nocapture"])
                .env(SEARCH_ADMISSION_HELPER_CHILD, "1")
                .output()?;
            if output.status.success() {
                return Ok(());
            }
            return Err(std::io::Error::other(format!(
                "isolated search admission helper failed with {}\nstdout:\n{}\nstderr:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            ))
            .into());
        }

        let governor = crate::resource::DescriptorGovernor::global()?;
        let baseline = governor.available();
        let expected = baseline
            .checked_sub(crate::resource::RECURSIVE_WALK_PEAK as usize)
            .ok_or_else(|| {
                std::io::Error::other(format!(
                    "isolated descriptor capacity {baseline} is below the recursive-walk peak"
                ))
            })?;
        let permit = acquire_search_walk()?;
        assert_eq!(governor.available(), expected);
        drop(permit);
        assert_eq!(governor.available(), baseline);
        Ok(())
    }

    #[tokio::test]
    async fn content_search_finds_pattern_in_tempdir() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.txt"), "alpha\nbeta needle\ngamma\n").expect("write a");
        std::fs::write(dir.path().join("b.txt"), "no match here\n").expect("write b");
        std::fs::write(dir.path().join("c.txt"), "needle on line 1\nother\n").expect("write c");

        let tool = SearchTool::new();
        let env = envelope(json!({
            "pattern": "needle",
            "path": dir.path().to_string_lossy(),
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("search ok");

        assert!(!out.is_error());
        let matches = out.content["matches"].as_array().expect("matches array");
        assert_eq!(matches.len(), 2);

        let mut got: Vec<(String, u64)> = matches
            .iter()
            .map(|m| {
                (
                    m["path"].as_str().unwrap_or_default().to_owned(),
                    m["line"].as_u64().unwrap_or_default(),
                )
            })
            .collect();
        got.sort();
        assert!(got[0].0.ends_with("a.txt"));
        assert_eq!(got[0].1, 2);
        assert!(got[1].0.ends_with("c.txt"));
        assert_eq!(got[1].1, 1);
    }

    #[tokio::test]
    async fn content_search_respects_max_results_and_marks_truncated() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(dir.path().join("x.txt"), "needle\nneedle\nneedle\n").expect("write x");

        let tool = SearchTool::new();
        let env = envelope(json!({
            "pattern": "needle",
            "path": dir.path().to_string_lossy(),
            "max_results": 2_u32,
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("search ok");

        assert_eq!(out.content["matches"].as_array().unwrap().len(), 2);
        assert_eq!(out.content["truncated"].as_bool(), Some(true));
    }

    #[tokio::test]
    async fn glob_filter_restricts_content_search() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.rs"), "needle here\n").expect("write a.rs");
        std::fs::write(dir.path().join("b.txt"), "needle here\n").expect("write b.txt");

        let tool = SearchTool::new();
        let env = envelope(json!({
            "pattern": "needle",
            "path": dir.path().to_string_lossy(),
            "glob": "**/*.rs",
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("search ok");

        let matches = out.content["matches"].as_array().expect("matches array");
        assert_eq!(matches.len(), 1);
        assert!(matches[0]["path"].as_str().unwrap().ends_with("a.rs"));
    }

    #[tokio::test]
    async fn file_finding_returns_rs_paths() {
        let dir = tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("sub")).expect("mkdir sub");
        std::fs::write(dir.path().join("top.rs"), "").expect("write top");
        std::fs::write(dir.path().join("sub").join("nested.rs"), "").expect("write nested");
        std::fs::write(dir.path().join("other.txt"), "").expect("write other");

        let tool = SearchTool::new();
        let env = envelope(json!({
            "path": dir.path().to_string_lossy(),
            "glob": "**/*.rs",
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("search ok");

        let paths = out.content["paths"].as_array().expect("paths array");
        assert_eq!(paths.len(), 2);
        let mut got: Vec<String> = paths
            .iter()
            .map(|p| p.as_str().unwrap_or_default().to_owned())
            .collect();
        got.sort();
        assert!(got[0].ends_with("sub/nested.rs"));
        assert!(got[1].ends_with("top.rs"));
    }

    #[tokio::test]
    async fn missing_pattern_and_glob_is_pre_validation_failure() {
        let tool = SearchTool::new();
        let env = envelope(json!({}));
        let err = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect_err("must fail");
        assert!(matches!(err, ToolError::PreValidationFailed { .. }));
    }

    #[tokio::test]
    async fn invalid_regex_reports_pre_validation_failure() {
        let dir = tempdir().expect("tempdir");
        let tool = SearchTool::new();
        let env = envelope(json!({
            "pattern": "[unterminated",
            "path": dir.path().to_string_lossy(),
        }));
        let err = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect_err("invalid regex must fail");
        assert!(matches!(err, ToolError::PreValidationFailed { .. }));
    }

    #[tokio::test]
    async fn fuzzy_search_ranks_filename_match_first() {
        let dir = tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src/auth")).expect("mkdir auth");
        std::fs::create_dir_all(dir.path().join("src/db")).expect("mkdir db");
        std::fs::write(dir.path().join("src/auth/login.rs"), "").expect("write login");
        std::fs::write(dir.path().join("src/auth/logout.rs"), "").expect("write logout");
        std::fs::write(dir.path().join("src/db/pool.rs"), "").expect("write pool");

        let tool = SearchTool::new();
        let env = envelope(json!({
            "mode": "fuzzy",
            "pattern": "login",
            "path": dir.path().to_string_lossy(),
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("fuzzy ok");

        let matches = out.content["matches"].as_array().expect("matches array");
        assert!(!matches.is_empty(), "expected at least one fuzzy match");

        let first = &matches[0];
        let first_path = first["path"].as_str().unwrap_or_default();
        assert!(
            first_path.ends_with("login.rs"),
            "expected login.rs ranked first, got `{first_path}`"
        );
        assert!(
            first["score"].as_u64().unwrap_or(0) > 0,
            "expected positive score for ranked-first match"
        );

        // Every other returned path should score no higher than login.rs.
        let first_score = first["score"].as_u64().unwrap_or(0);
        for m in matches.iter().skip(1) {
            assert!(m["score"].as_u64().unwrap_or(0) <= first_score);
        }
    }

    #[tokio::test]
    async fn fuzzy_empty_pattern_is_pre_validation_failure() {
        let dir = tempdir().expect("tempdir");
        let tool = SearchTool::new();
        let env = envelope(json!({
            "mode": "fuzzy",
            "path": dir.path().to_string_lossy(),
        }));
        let err = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect_err("empty fuzzy pattern must fail");
        assert!(matches!(err, ToolError::PreValidationFailed { .. }));
    }

    #[tokio::test]
    async fn ast_search_captures_function_name() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.rs"), "fn alpha() {}\nfn beta() {}\n")
            .expect("write rust file");

        let tool = SearchTool::new();
        let env = envelope(json!({
            "mode": "ast",
            "ast_query": "(function_item name: (identifier) @name)",
            "path": dir.path().to_string_lossy(),
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("ast ok");

        let matches = out.content["matches"].as_array().expect("matches array");
        let mut names: Vec<String> = matches
            .iter()
            .map(|m| m["text"].as_str().unwrap_or_default().to_owned())
            .collect();
        names.sort();
        assert_eq!(names, vec!["alpha".to_owned(), "beta".to_owned()]);

        let first = &matches[0];
        assert_eq!(first["node_kind"].as_str(), Some("identifier"));
        assert_eq!(first["capture_name"].as_str(), Some("name"));
        assert!(first["line"].as_u64().unwrap_or(0) >= 1);
        assert!(first["column"].as_u64().unwrap_or(0) >= 1);
        assert!(first["path"].as_str().unwrap_or_default().ends_with("a.rs"));
    }

    #[tokio::test]
    async fn ast_empty_query_is_pre_validation_failure() {
        let dir = tempdir().expect("tempdir");
        let tool = SearchTool::new();
        let env = envelope(json!({
            "mode": "ast",
            "path": dir.path().to_string_lossy(),
        }));
        let err = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect_err("empty ast_query must fail");
        assert!(matches!(err, ToolError::PreValidationFailed { .. }));
    }

    #[tokio::test]
    async fn ast_invalid_query_for_some_languages_reports_query_errors() {
        // Use a query that's valid for Python but invalid for Rust. The
        // tool degrades gracefully -- Python matches still come back --
        // but the Rust compile failure must be surfaced in query_errors.
        let dir = tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.rs"), "fn alpha() {}\n").expect("write rs");
        std::fs::write(dir.path().join("b.py"), "def beta():\n    pass\n").expect("write py");

        let tool = SearchTool::new();
        let env = envelope(json!({
            "mode": "ast",
            "ast_query": "(function_definition name: (identifier) @name)",
            "path": dir.path().to_string_lossy(),
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("ast ok despite mixed language compile results");

        let matches = out.content["matches"].as_array().expect("matches array");
        let names: Vec<&str> = matches
            .iter()
            .map(|m| m["text"].as_str().unwrap_or_default())
            .collect();
        assert!(names.contains(&"beta"), "expected Python beta in {names:?}");

        let query_errors = out.content["query_errors"]
            .as_array()
            .expect("query_errors array");
        assert_eq!(query_errors.len(), 1, "exactly the Rust compile failure");
        assert_eq!(query_errors[0]["language"].as_str(), Some("Rust"));
        assert!(
            query_errors[0]["error"]
                .as_str()
                .unwrap_or_default()
                .contains("invalid for Rust"),
            "error must carry the compile message, got {:?}",
            query_errors[0]
        );
    }

    #[tokio::test]
    async fn ast_query_invalid_for_all_languages_is_typed_error() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.rs"), "fn alpha() {}\n").expect("write rs");
        std::fs::write(dir.path().join("b.py"), "def beta():\n    pass\n").expect("write py");

        let tool = SearchTool::new();
        let env = envelope(json!({
            "mode": "ast",
            "ast_query": "(bogus_node_kind_that_no_grammar_has) @x",
            "path": dir.path().to_string_lossy(),
        }));
        let err = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect_err("query invalid for every language must fail");

        match err {
            ToolError::PreValidationFailed { payload } => {
                assert_eq!(payload.kind, ToolErrorKind::InvalidArguments);
                assert!(
                    payload.message.contains("every candidate language"),
                    "message must state the all-languages failure: {}",
                    payload.message
                );
                assert!(
                    payload.message.contains("Rust") && payload.message.contains("Python"),
                    "message must carry per-language details: {}",
                    payload.message
                );
            }
            other => panic!("expected PreValidationFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_mode_returns_clear_error() {
        let dir = tempdir().expect("tempdir");
        let tool = SearchTool::new();
        let env = envelope(json!({
            "mode": "nonsense",
            "path": dir.path().to_string_lossy(),
        }));
        let err = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect_err("unknown mode must fail");
        match err {
            ToolError::PreValidationFailed { payload } => {
                assert_eq!(payload.kind, ToolErrorKind::InvalidArguments);
                let message = &payload.message;
                assert!(
                    message.contains("content")
                        && message.contains("files")
                        && message.contains("fuzzy")
                        && message.contains("ast"),
                    "error message should list valid modes, got `{message}`"
                );
            }
            other => panic!("expected PreValidationFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn all_four_modes_produce_results_on_same_tree() {
        let dir = tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("src")).expect("mkdir src");
        std::fs::write(
            dir.path().join("src").join("login.rs"),
            "fn login() {}\nfn signin() {}\n",
        )
        .expect("write login.rs");
        std::fs::write(dir.path().join("src").join("notes.txt"), "login here\n")
            .expect("write notes.txt");

        let tool = SearchTool::new();
        let dir_str = dir.path().to_string_lossy().into_owned();

        // mode = content
        let env = envelope(json!({
            "mode": "content",
            "pattern": "login",
            "path": dir_str.clone(),
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("content ok");
        let content_matches = out.content["matches"].as_array().expect("content array");
        assert!(!content_matches.is_empty(), "content mode produced no hits");

        // mode = files
        let env = envelope(json!({
            "mode": "files",
            "glob": "**/*.rs",
            "path": dir_str.clone(),
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("files ok");
        let paths = out.content["paths"].as_array().expect("paths array");
        assert!(!paths.is_empty(), "files mode produced no paths");

        // mode = fuzzy
        let env = envelope(json!({
            "mode": "fuzzy",
            "pattern": "login",
            "path": dir_str.clone(),
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("fuzzy ok");
        let fuzzy_matches = out.content["matches"].as_array().expect("fuzzy array");
        assert!(!fuzzy_matches.is_empty(), "fuzzy mode produced no matches");

        // mode = ast
        let env = envelope(json!({
            "mode": "ast",
            "ast_query": "(function_item name: (identifier) @name)",
            "path": dir_str,
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("ast ok");
        let ast_matches = out.content["matches"].as_array().expect("ast array");
        assert!(!ast_matches.is_empty(), "ast mode produced no matches");
    }

    #[tokio::test]
    async fn brace_expansion_in_glob_produces_results() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.rs"), "needle\n").expect("write a.rs");
        std::fs::write(dir.path().join("b.md"), "needle\n").expect("write b.md");
        std::fs::write(dir.path().join("c.txt"), "needle\n").expect("write c.txt");

        let tool = SearchTool::new();
        let env = envelope(json!({
            "pattern": "needle",
            "path": dir.path().to_string_lossy(),
            "glob": "**/*.{rs,md}",
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("search ok");

        let matches = out.content["matches"].as_array().expect("matches array");
        assert_eq!(matches.len(), 2, "should match .rs and .md but not .txt");

        let mut paths: Vec<String> = matches
            .iter()
            .map(|m| m["path"].as_str().unwrap_or_default().to_owned())
            .collect();
        paths.sort();
        assert!(paths[0].ends_with("a.rs"));
        assert!(paths[1].ends_with("b.md"));
    }

    #[tokio::test]
    async fn file_path_as_root_searches_single_file() {
        let dir = tempdir().expect("tempdir");
        let file_path = dir.path().join("target.rs");
        std::fs::write(&file_path, "fn main() {}\nfn helper() {}\n").expect("write file");

        let tool = SearchTool::new();
        let env = envelope(json!({
            "pattern": "fn",
            "path": file_path.to_string_lossy(),
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("search ok");

        let matches = out.content["matches"].as_array().expect("matches array");
        assert_eq!(matches.len(), 2, "should find both fn lines in the file");
    }

    #[tokio::test]
    async fn plain_filename_glob_matches_files_in_subdirs() {
        let dir = tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("sub")).expect("mkdir sub");
        std::fs::write(dir.path().join("README.md"), "needle\n").expect("write top");
        std::fs::write(dir.path().join("sub").join("README.md"), "needle\n").expect("write nested");
        std::fs::write(dir.path().join("other.txt"), "needle\n").expect("write other");

        let tool = SearchTool::new();
        let env = envelope(json!({
            "pattern": "needle",
            "path": dir.path().to_string_lossy(),
            "glob": "README.md",
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("search ok");

        let matches = out.content["matches"].as_array().expect("matches array");
        assert_eq!(
            matches.len(),
            2,
            "should match README.md in root and sub, but not other.txt"
        );
    }

    #[tokio::test]
    async fn brace_expansion_in_file_find_mode() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.rs"), "").expect("write a.rs");
        std::fs::write(dir.path().join("b.md"), "").expect("write b.md");
        std::fs::write(dir.path().join("c.txt"), "").expect("write c.txt");

        let tool = SearchTool::new();
        let env = envelope(json!({
            "mode": "files",
            "path": dir.path().to_string_lossy(),
            "glob": "**/*.{rs,md}",
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("search ok");

        let paths = out.content["paths"].as_array().expect("paths array");
        assert_eq!(paths.len(), 2, "should find .rs and .md but not .txt");
    }

    // -- Confinement and path resolution --------------------------------

    #[tokio::test]
    async fn confined_context_refuses_root_outside_workspace() {
        let workspace = tempdir().expect("workspace");
        let outside = tempdir().expect("outside");
        std::fs::write(outside.path().join("secret.txt"), "top secret needle\n")
            .expect("write secret");

        let ctx = confined_ctx(workspace.path());
        let tool = SearchTool::new();
        let env = envelope(json!({
            "pattern": ".",
            "path": outside.path().to_string_lossy(),
        }));
        let out = tool.execute(&env, &ctx).await.expect("refusal is output");

        assert!(out.is_error(), "out-of-workspace search must be refused");
        assert_eq!(out.content["kind"].as_str(), Some("confinement_refused"));
        let payload = out.error().expect("typed error payload");
        assert_eq!(payload.kind, ToolErrorKind::PermissionDenied);
        assert!(
            out.content.get("matches").is_none(),
            "no match data may leak on refusal"
        );
        assert!(
            !out.content.to_string().contains("top secret"),
            "secret content must not leak"
        );
    }

    #[tokio::test]
    async fn confined_context_refuses_files_mode_outside_workspace() {
        let workspace = tempdir().expect("workspace");
        let outside = tempdir().expect("outside");
        std::fs::write(outside.path().join("secret.txt"), "s\n").expect("write secret");

        let ctx = confined_ctx(workspace.path());
        let tool = SearchTool::new();
        let env = envelope(json!({
            "mode": "files",
            "glob": "**",
            "path": outside.path().to_string_lossy(),
        }));
        let out = tool.execute(&env, &ctx).await.expect("refusal is output");

        assert!(out.is_error());
        assert_eq!(out.content["kind"].as_str(), Some("confinement_refused"));
        assert!(out.content.get("paths").is_none(), "no paths may leak");
    }

    #[tokio::test]
    async fn confined_context_refuses_relative_escape() {
        let outer = tempdir().expect("outer");
        let root = outer.path().join("ws");
        std::fs::create_dir(&root).expect("mkdir ws");
        std::fs::write(outer.path().join("secret.txt"), "needle\n").expect("write secret");

        let ctx = confined_ctx(&root);
        let tool = SearchTool::new();
        let env = envelope(json!({ "pattern": "needle", "path": ".." }));
        let out = tool.execute(&env, &ctx).await.expect("refusal is output");

        assert!(out.is_error(), "`..` escape must be refused");
        assert_eq!(out.content["kind"].as_str(), Some("confinement_refused"));
    }

    #[tokio::test]
    async fn confined_context_allows_search_inside_workspace() {
        let workspace = tempdir().expect("workspace");
        std::fs::write(workspace.path().join("a.txt"), "needle\n").expect("write a");

        let ctx = confined_ctx(workspace.path());
        let tool = SearchTool::new();
        let env = envelope(json!({ "pattern": "needle" }));
        let out = tool.execute(&env, &ctx).await.expect("search ok");

        assert!(!out.is_error());
        assert_eq!(out.content["matches"].as_array().unwrap().len(), 1);
    }

    /// DECISIONS §0.6(b): the search tool is read-class, so a file inside a
    /// declared read-exempt root (a home-level skill dir) is searchable
    /// under confinement even though it lies outside the workspace root —
    /// the same carve-out the read tool honours. A non-exempt sibling stays
    /// refused.
    #[tokio::test]
    async fn confined_search_admits_exempt_skill_dir() {
        let outer = tempdir().expect("outer");
        let root = outer.path().join("ws");
        let skills = outer.path().join("home-skills");
        std::fs::create_dir(&root).expect("mkdir ws");
        std::fs::create_dir(&skills).expect("mkdir skills");
        std::fs::write(skills.join("SKILL.md"), "look for the needle here\n").expect("write skill");

        let mut ctx = confined_ctx(&root);
        ctx.set_read_exempt_roots(vec![skills.clone()]);
        let tool = SearchTool::new();

        // Content search rooted at the exempt dir finds the match.
        let env = envelope(json!({
            "pattern": "needle",
            "path": skills.to_string_lossy(),
        }));
        let out = tool.execute(&env, &ctx).await.expect("search ok");
        assert!(
            !out.is_error(),
            "exempt skill dir must be searchable: {:?}",
            out.content
        );
        assert_eq!(out.content["matches"].as_array().unwrap().len(), 1);

        // A non-exempt sibling outside the root is still refused.
        let secret = outer.path().join("secret");
        std::fs::create_dir(&secret).expect("mkdir secret");
        std::fs::write(secret.join("s.txt"), "needle\n").expect("write secret");
        let refused = envelope(json!({
            "pattern": "needle",
            "path": secret.to_string_lossy(),
        }));
        let out = tool
            .execute(&refused, &ctx)
            .await
            .expect("refusal is output");
        assert!(out.is_error(), "non-exempt outside path must be refused");
        assert_eq!(out.content["kind"].as_str(), Some("confinement_refused"));
    }

    #[tokio::test]
    async fn relative_path_resolves_against_agent_working_dir() {
        let dir = tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("sub")).expect("mkdir sub");
        std::fs::write(dir.path().join("sub").join("f.txt"), "needle\n").expect("write f");

        // The agent working dir is the tempdir; the process CWD is not.
        let ctx = ctx_with_working_dir(dir.path());
        let tool = SearchTool::new();
        let env = envelope(json!({ "pattern": "needle", "path": "sub" }));
        let out = tool.execute(&env, &ctx).await.expect("search ok");

        let matches = out.content["matches"].as_array().expect("matches array");
        assert_eq!(matches.len(), 1, "relative path must resolve via context");
        assert!(matches[0]["path"].as_str().unwrap().ends_with("sub/f.txt"));
    }

    // -- Ignore rules ----------------------------------------------------

    /// Lay out a tree with a gitignored dir, a hidden file, a plain file,
    /// `.git` internals, and sensitive files (env + key material).
    fn ignore_fixture() -> tempfile::TempDir {
        let dir = tempdir().expect("tempdir");
        std::fs::write(dir.path().join(".gitignore"), "target/\n").expect("write .gitignore");
        std::fs::create_dir(dir.path().join("target")).expect("mkdir target");
        std::fs::write(dir.path().join("target").join("gen.txt"), "needle\n")
            .expect("write gen.txt");
        std::fs::write(dir.path().join(".hidden.txt"), "needle\n").expect("write hidden");
        std::fs::create_dir(dir.path().join("src")).expect("mkdir src");
        std::fs::write(dir.path().join("src").join("a.txt"), "needle\n").expect("write a.txt");
        std::fs::create_dir(dir.path().join(".git")).expect("mkdir .git");
        std::fs::write(dir.path().join(".git").join("config"), "needle\n")
            .expect("write .git/config");
        std::fs::write(dir.path().join(".env"), "needle\n").expect("write .env");
        std::fs::write(dir.path().join("server.pem"), "needle\n").expect("write server.pem");
        dir
    }

    fn match_paths(out: &ToolOutput) -> Vec<String> {
        let mut paths: Vec<String> = out.content["matches"]
            .as_array()
            .expect("matches array")
            .iter()
            .map(|m| m["path"].as_str().unwrap_or_default().to_owned())
            .collect();
        paths.sort();
        paths
    }

    #[tokio::test]
    async fn gitignored_and_hidden_entries_excluded_by_default() {
        let dir = ignore_fixture();
        let tool = SearchTool::new();
        let env = envelope(json!({
            "pattern": "needle",
            "path": dir.path().to_string_lossy(),
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("search ok");

        let paths = match_paths(&out);
        assert_eq!(paths.len(), 1, "only src/a.txt should match: {paths:?}");
        assert!(paths[0].ends_with("src/a.txt"));
    }

    #[tokio::test]
    async fn include_ignored_flag_includes_gitignored_and_hidden_entries() {
        let dir = ignore_fixture();
        let tool = SearchTool::new();
        let env = envelope(json!({
            "pattern": "needle",
            "path": dir.path().to_string_lossy(),
            "include_ignored": true,
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("search ok");

        let paths = match_paths(&out);
        assert_eq!(paths.len(), 3, "all three needle files: {paths:?}");
        assert!(paths.iter().any(|p| p.ends_with(".hidden.txt")));
        assert!(paths.iter().any(|p| p.ends_with("target/gen.txt")));
        assert!(paths.iter().any(|p| p.ends_with("src/a.txt")));

        // Even with the flag, `.git` internals and sensitive files stay out.
        assert!(
            !paths.iter().any(|p| p.contains("/.git/")),
            "never inside .git: {paths:?}"
        );
        assert!(
            !paths
                .iter()
                .any(|p| p.ends_with("/.env") || p.ends_with("server.pem")),
            "never sensitive files: {paths:?}"
        );

        // The sensitive exclusions are reported, not silent.
        let skipped = out.content["skipped"].as_array().expect("skipped array");
        for name in [".env", "server.pem"] {
            assert!(
                skipped.iter().any(|s| {
                    s["path"].as_str().is_some_and(|p| p.ends_with(name))
                        && s["reason"]
                            .as_str()
                            .is_some_and(|r| r.contains("secret material"))
                }),
                "{name} must appear in skipped with the sensitive reason: {skipped:?}"
            );
        }
    }

    /// Sensitive files are excluded from DEFAULT walks too — `server.pem`
    /// is neither hidden nor gitignored, so without the sensitivity rule
    /// it would match here.
    #[tokio::test]
    async fn sensitive_files_excluded_from_default_walk() {
        let dir = ignore_fixture();
        let tool = SearchTool::new();
        let out = tool
            .execute(
                &envelope(json!({
                    "pattern": "needle",
                    "path": dir.path().to_string_lossy(),
                })),
                &ToolContext::empty(),
            )
            .await
            .expect("search ok");

        let paths = match_paths(&out);
        assert!(
            !paths.iter().any(|p| p.ends_with("server.pem")),
            "plain-visible key material must not match: {paths:?}"
        );
        let skipped = out.content["skipped"].as_array().expect("skipped array");
        assert!(
            skipped.iter().any(|s| {
                s["path"]
                    .as_str()
                    .is_some_and(|p| p.ends_with("server.pem"))
            }),
            "server.pem exclusion must be reported: {skipped:?}"
        );
    }

    /// An explicitly named sensitive file is still searchable — the
    /// exclusion guards incidental sweeps, not deliberate access.
    #[tokio::test]
    async fn explicitly_named_sensitive_file_is_searched() {
        let dir = ignore_fixture();
        let tool = SearchTool::new();
        let out = tool
            .execute(
                &envelope(json!({
                    "pattern": "needle",
                    "path": dir.path().join(".env").to_string_lossy(),
                })),
                &ToolContext::empty(),
            )
            .await
            .expect("search ok");

        let paths = match_paths(&out);
        assert_eq!(paths.len(), 1, "explicit .env root is searched: {paths:?}");
        assert!(paths[0].ends_with("/.env"));
    }

    /// Naming `.git` itself as the walk root still works — the pruning
    /// applies below the root only.
    #[tokio::test]
    async fn explicit_git_root_is_walkable() {
        let dir = ignore_fixture();
        let tool = SearchTool::new();
        let out = tool
            .execute(
                &envelope(json!({
                    "pattern": "needle",
                    "path": dir.path().join(".git").to_string_lossy(),
                    "include_ignored": true,
                })),
                &ToolContext::empty(),
            )
            .await
            .expect("search ok");

        let paths = match_paths(&out);
        assert_eq!(paths.len(), 1, "explicit .git root is walked: {paths:?}");
        assert!(paths[0].ends_with(".git/config"));
    }

    #[tokio::test]
    async fn results_identical_modulo_ignore_rules() {
        let dir = ignore_fixture();
        let tool = SearchTool::new();

        let out_default = tool
            .execute(
                &envelope(json!({
                    "pattern": "needle",
                    "path": dir.path().to_string_lossy(),
                })),
                &ToolContext::empty(),
            )
            .await
            .expect("default search ok");
        let out_all = tool
            .execute(
                &envelope(json!({
                    "pattern": "needle",
                    "path": dir.path().to_string_lossy(),
                    "include_ignored": true,
                })),
                &ToolContext::empty(),
            )
            .await
            .expect("include_ignored search ok");

        let default_paths = match_paths(&out_default);
        let all_paths = match_paths(&out_all);

        // The flagged run minus entries excluded by the ignore rules must
        // be exactly the default run -- nothing else may differ.
        let all_minus_ignored: Vec<String> = all_paths
            .iter()
            .filter(|path| {
                let fixture_relative = std::path::Path::new(path).strip_prefix(dir.path()).ok();
                !fixture_relative.is_some_and(|relative| relative.starts_with("target"))
                    && !path.ends_with(".hidden.txt")
            })
            .cloned()
            .collect();
        assert_eq!(all_minus_ignored, default_paths);
        assert!(
            all_paths.len() > default_paths.len(),
            "flagged run must be a strict superset here"
        );
    }

    #[tokio::test]
    async fn file_find_honours_ignore_rules_and_flag() {
        let dir = ignore_fixture();
        let tool = SearchTool::new();

        let out_default = tool
            .execute(
                &envelope(json!({
                    "mode": "files",
                    "glob": "**/*.txt",
                    "path": dir.path().to_string_lossy(),
                })),
                &ToolContext::empty(),
            )
            .await
            .expect("files default ok");
        let default_paths = out_default.content["paths"].as_array().unwrap().len();
        assert_eq!(default_paths, 1, "only src/a.txt by default");

        let out_all = tool
            .execute(
                &envelope(json!({
                    "mode": "files",
                    "glob": "**/*.txt",
                    "path": dir.path().to_string_lossy(),
                    "include_ignored": true,
                })),
                &ToolContext::empty(),
            )
            .await
            .expect("files include_ignored ok");
        let all_paths = out_all.content["paths"].as_array().unwrap().len();
        assert_eq!(all_paths, 3, "gen.txt, .hidden.txt, and a.txt with flag");
    }

    // -- Walk-error surfacing ---------------------------------------------

    #[cfg(unix)]
    #[tokio::test]
    async fn permission_denied_subtree_is_reported_in_skipped() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().expect("tempdir");
        std::fs::write(dir.path().join("open.txt"), "needle\n").expect("write open");
        let locked = dir.path().join("locked");
        std::fs::create_dir(&locked).expect("mkdir locked");
        std::fs::write(locked.join("secret.txt"), "needle\n").expect("write secret");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000))
            .expect("chmod 000");

        if std::fs::read_dir(&locked).is_ok() {
            // Running as root: the permission gate cannot be exercised.
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755))
                .expect("chmod restore");
            tracing::info!("skipping permission-denied test: process can read 0o000 dirs");
            return;
        }

        let tool = SearchTool::new();
        let env = envelope(json!({
            "pattern": "needle",
            "path": dir.path().to_string_lossy(),
        }));
        let result = tool.execute(&env, &ToolContext::empty()).await;

        // Restore before asserting so tempdir cleanup always succeeds.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755))
            .expect("chmod restore");

        let out = result.expect("search ok");
        assert!(!out.is_error());

        let paths = match_paths(&out);
        assert_eq!(paths.len(), 1, "the readable file still matches");
        assert!(paths[0].ends_with("open.txt"));

        let skipped = out.content["skipped"].as_array().expect("skipped array");
        assert!(
            skipped
                .iter()
                .any(|s| s["path"].as_str().unwrap_or_default().contains("locked")),
            "the unreadable subtree must appear in skipped: {skipped:?}"
        );
        assert!(
            skipped
                .iter()
                .all(|s| !s["reason"].as_str().unwrap_or_default().is_empty()),
            "every skipped entry carries a reason: {skipped:?}"
        );
        assert_eq!(
            out.content["truncated"].as_bool(),
            Some(false),
            "an unreadable subtree is skipped, not truncated"
        );
    }

    #[tokio::test]
    async fn clean_walk_reports_empty_skipped() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.txt"), "needle\n").expect("write a");

        let tool = SearchTool::new();
        let env = envelope(json!({
            "pattern": "needle",
            "path": dir.path().to_string_lossy(),
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("search ok");

        assert_eq!(
            out.content["skipped"].as_array().map(Vec::len),
            Some(0),
            "clean walks report an empty skipped array"
        );
    }

    // -- Glob-metacharacter base paths ------------------------------------

    #[tokio::test]
    async fn bracketed_directory_name_is_treated_literally_in_file_find() {
        let dir = tempdir().expect("tempdir");
        let bracketed = dir.path().join("br[ack]ets");
        std::fs::create_dir(&bracketed).expect("mkdir bracketed");
        std::fs::write(bracketed.join("a.rs"), "").expect("write a.rs");

        let tool = SearchTool::new();
        let env = envelope(json!({
            "mode": "files",
            "glob": "*.rs",
            "path": bracketed.to_string_lossy(),
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("search ok");

        let paths = out.content["paths"].as_array().expect("paths array");
        assert_eq!(
            paths.len(),
            1,
            "brackets in the base dir must not be parsed as glob syntax"
        );
        assert!(paths[0].as_str().unwrap().ends_with("a.rs"));
    }

    #[tokio::test]
    async fn single_star_in_file_find_does_not_cross_directories() {
        let dir = tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("sub")).expect("mkdir sub");
        std::fs::write(dir.path().join("top.rs"), "").expect("write top");
        std::fs::write(dir.path().join("sub").join("nested.rs"), "").expect("write nested");

        let tool = SearchTool::new();
        let env = envelope(json!({
            "mode": "files",
            "glob": "*.rs",
            "path": dir.path().to_string_lossy(),
        }));
        let out = tool
            .execute(&env, &ToolContext::empty())
            .await
            .expect("search ok");

        let paths: Vec<&str> = out.content["paths"]
            .as_array()
            .expect("paths array")
            .iter()
            .map(|p| p.as_str().unwrap_or_default())
            .collect();
        assert_eq!(paths.len(), 1, "`*.rs` is anchored to the root: {paths:?}");
        assert!(paths[0].ends_with("top.rs"));
    }

    #[test]
    fn build_plan_legacy_detection_matches_documented_rules() {
        let content = build_plan(&SearchArgs {
            pattern: Some("x".to_owned()),
            glob: Some("**/*.rs".to_owned()),
            ..SearchArgs::default()
        })
        .expect("plan ok");
        assert!(matches!(content, SearchPlan::Content { .. }));

        let files = build_plan(&SearchArgs {
            glob: Some("**/*.rs".to_owned()),
            ..SearchArgs::default()
        })
        .expect("plan ok");
        assert!(matches!(files, SearchPlan::Files { .. }));

        let err = build_plan(&SearchArgs::default()).expect_err("no inputs must fail");
        assert!(matches!(err, ToolError::PreValidationFailed { .. }));
    }
}
