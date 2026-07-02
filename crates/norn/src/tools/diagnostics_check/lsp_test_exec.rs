//! LSP test execution and `libtest-json-plus` failure parsing.
//!
//! Given a [`TestRunnable`] reported by a language server, this module
//! spawns its cargo command (forcing `--message-format libtest-json-plus`
//! + `--no-fail-fast`), captures the JSON stream, and turns each failed
//!   record into a [`DiagnosticEvent`] suitable for the policy registry.

use std::path::{Path, PathBuf};

use diagnostics::adapter::invoke::invoke_tool_with_env;
use diagnostics::conventions::{CompiledRule, Handling};
use diagnostics::event::{DiagnosticEvent, Severity};

use crate::tools::lsp::TestRunnable;

/// Source tool name attached to every test-derived [`DiagnosticEvent`].
/// Used by reporters to distinguish LSP-discovered test failures from
/// adapter subprocess output (R3 acceptance: "Source tool is `lsp-test`
/// to distinguish from adapter diagnostics").
pub(super) const LSP_TEST_SOURCE: &str = "lsp-test";

/// Default per-test invocation timeout when none is configured. Matches
/// the adapter-pipeline default (CO22). 120 seconds is a coarse upper
/// bound — individual tests should complete in milliseconds, but a hung
/// cargo build must not deadlock the post-check.
pub(super) const TEST_INVOCATION_TIMEOUT_MS: u64 = 120_000;

/// Spawn the runnable's cargo command, parse `libtest-json-plus` output,
/// and return one [`DiagnosticEvent`] per failing test. Successes
/// produce nothing (R3 acceptance: "Test successes do not produce
/// `DiagnosticEvents`").
pub(super) async fn execute_runnable(
    runnable: &TestRunnable,
    workspace_root: &Path,
    fallback_file: &Path,
) -> Result<Vec<DiagnosticEvent>, String> {
    let mut cargo_args = runnable.cargo_args.clone();
    enrich_args_for_json_output(&mut cargo_args, &runnable.executable_args);

    let working_dir = runnable
        .cwd
        .as_ref()
        .map(PathBuf::from)
        .or_else(|| runnable.workspace_root.as_ref().map(PathBuf::from))
        .unwrap_or_else(|| workspace_root.to_path_buf());

    let result = invoke_tool_with_env(
        "cargo",
        &cargo_args,
        &[(
            "NEXTEST_EXPERIMENTAL_LIBTEST_JSON".to_owned(),
            "1".to_owned(),
        )],
        &working_dir,
        TEST_INVOCATION_TIMEOUT_MS,
    )
    .await
    .map_err(|e| format!("test invocation failed: {e}"))?;

    Ok(parse_libtest_failures(
        &result.stdout,
        runnable,
        fallback_file,
    ))
}

/// Inject `--message-format libtest-json-plus` and `--no-fail-fast` into
/// the runnable's cargo args (before the `--` executable-args separator).
pub(super) fn enrich_args_for_json_output(
    cargo_args: &mut Vec<String>,
    executable_args: &[String],
) {
    if !cargo_args.iter().any(|a| a == "--message-format") {
        cargo_args.push("--message-format".to_owned());
        cargo_args.push("libtest-json-plus".to_owned());
    }
    if !cargo_args.iter().any(|a| a == "--no-fail-fast") {
        cargo_args.push("--no-fail-fast".to_owned());
    }
    if !executable_args.is_empty() {
        cargo_args.push("--".to_owned());
        cargo_args.extend(executable_args.iter().cloned());
    }
}

/// Parse the line-delimited `libtest-json-plus` stream for `event=failed`
/// records and convert each into a [`DiagnosticEvent`].
///
/// [`LspLocation`](crate::tools::lsp::LspLocation) positions are already
/// one-based — every producer converts from the LSP wire protocol's
/// zero-based positions at the boundary — so they carry through to the
/// one-based `DiagnosticEvent` contract unshifted. When the runnable has
/// no location, line/column fall through as `0` — matching the nextest
/// adapter's fallback.
pub(super) fn parse_libtest_failures(
    stdout: &str,
    runnable: &TestRunnable,
    fallback_file: &Path,
) -> Vec<DiagnosticEvent> {
    let mut events = Vec::new();
    for (line_index, line) in stdout.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }

        let record: serde_json::Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(
                    source = LSP_TEST_SOURCE,
                    line = line_index,
                    error = %error,
                    "skipping malformed libtest-json-plus line"
                );
                continue;
            }
        };

        let is_failed_test = record
            .get("type")
            .and_then(|v| v.as_str())
            .is_some_and(|t| t == "test")
            && record
                .get("event")
                .and_then(|v| v.as_str())
                .is_some_and(|e| e == "failed");

        if !is_failed_test {
            continue;
        }

        let test_name = record
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("(no test name)");

        let failure_snippet = record
            .get("stdout")
            .and_then(|v| v.as_str())
            .map(|s| s.lines().take(5).collect::<Vec<_>>().join("\n"))
            .filter(|s| !s.is_empty());

        let failure_message = failure_snippet.as_ref().map_or_else(
            || test_name.to_owned(),
            |s| s.lines().next().unwrap_or("").to_owned(),
        );

        let (file, line, column) = if let Some(location) = runnable.location.as_ref() {
            (
                PathBuf::from(&location.path),
                usize::try_from(location.line).unwrap_or(usize::MAX),
                usize::try_from(location.column).unwrap_or(usize::MAX),
            )
        } else {
            (fallback_file.to_path_buf(), 0, 0)
        };

        events.push(DiagnosticEvent {
            severity: Severity::Error,
            message: format!("Test {test_name} failed: {failure_message}"),
            file,
            line,
            column,
            end_line: None,
            end_column: None,
            source_tool: LSP_TEST_SOURCE.to_owned(),
            code: None,
            snippet: failure_snippet,
            entity_context: None,
        });
    }
    events
}

/// Rule-level override for advise/block; defaults to `Advise` when the
/// rule does not configure diagnostics handling. Per R3 brief context:
/// "use the rule's `lsp.diagnostics.handling` if set, else default to
/// `Handling::Advise`".
pub(super) fn handling_for_rule(rule: &CompiledRule) -> Handling {
    rule.rule
        .lsp
        .as_ref()
        .and_then(|lsp| lsp.diagnostics.as_ref())
        .map_or(Handling::Advise, |diagnostics| diagnostics.handling)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::tools::lsp::{LspLocation, TestRunnableKind};
    use diagnostics::conventions::ConventionsConfig;
    use std::io::Write;

    fn load_config(toml_src: &str) -> ConventionsConfig {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("CONVENTIONS.toml");
        std::fs::File::create(&path)
            .and_then(|mut f| f.write_all(toml_src.as_bytes()))
            .expect("write");
        ConventionsConfig::load(&path).expect("load")
    }

    fn make_runnable(location: Option<LspLocation>) -> TestRunnable {
        TestRunnable {
            label: "test::foo".to_owned(),
            kind: TestRunnableKind::Test,
            location,
            cargo_args: vec!["test".to_owned()],
            executable_args: Vec::new(),
            cwd: None,
            workspace_root: None,
        }
    }

    #[test]
    fn parse_libtest_failures_emits_event_per_failed_record() {
        let stdout = "{\"type\":\"test\",\"event\":\"failed\",\"name\":\"foo::bar\",\"stdout\":\"thread 'foo' panicked\\nat src/lib.rs:42\\n\"}\n\
                      {\"type\":\"test\",\"event\":\"ok\",\"name\":\"foo::baz\"}\n";
        let runnable = make_runnable(Some(LspLocation {
            path: "/abs/src/lib.rs".to_owned(),
            line: 42,
            column: 1,
            end_line: 42,
            end_column: 11,
        }));

        let events = parse_libtest_failures(stdout, &runnable, &PathBuf::from("fallback.rs"));

        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(event.severity, Severity::Error);
        assert!(event.message.contains("foo::bar"));
        assert!(event.message.starts_with("Test "));
        assert_eq!(event.source_tool, LSP_TEST_SOURCE);
        assert!(event.code.is_none());
        assert_eq!(event.file, PathBuf::from("/abs/src/lib.rs"));
        assert_eq!(event.line, 42);
        assert_eq!(event.column, 1);
    }

    /// Regression: `LspLocation` is one-based at every producer, so the
    /// parser must surface it unshifted — the old `saturating_add(1)`
    /// double-incremented and pointed diagnostics one line/column past
    /// the real test location.
    #[test]
    fn parse_libtest_failures_surfaces_one_based_location_unshifted() {
        let stdout =
            "{\"type\":\"test\",\"event\":\"failed\",\"name\":\"top\",\"stdout\":\"boom\"}\n";
        // Line 1, column 1: the first possible one-based location. Any
        // offset applied by the parser would be visible immediately.
        let runnable = make_runnable(Some(LspLocation {
            path: "/abs/src/first.rs".to_owned(),
            line: 1,
            column: 1,
            end_line: 1,
            end_column: 4,
        }));

        let events = parse_libtest_failures(stdout, &runnable, &PathBuf::from("fallback.rs"));

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].line, 1, "one-based line must pass through");
        assert_eq!(events[0].column, 1, "one-based column must pass through");
    }

    #[test]
    fn parse_libtest_failures_falls_back_to_zero_when_no_location() {
        let stdout =
            "{\"type\":\"test\",\"event\":\"failed\",\"name\":\"alone\",\"stdout\":\"oops\"}\n";
        let runnable = make_runnable(None);

        let events =
            parse_libtest_failures(stdout, &runnable, &PathBuf::from("crates/x/src/lib.rs"));

        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(event.file, PathBuf::from("crates/x/src/lib.rs"));
        assert_eq!(event.line, 0);
        assert_eq!(event.column, 0);
    }

    #[test]
    fn parse_libtest_failures_skips_non_failed_and_malformed_lines() {
        let stdout = "{\"type\":\"test\",\"event\":\"ok\",\"name\":\"good\"}\n\
                      not-json\n\
                      {\"type\":\"suite\",\"event\":\"started\"}\n";
        let runnable = make_runnable(None);

        let events = parse_libtest_failures(stdout, &runnable, &PathBuf::from("x.rs"));

        assert!(events.is_empty());
    }

    #[test]
    fn enrich_args_appends_json_format_and_no_fail_fast() {
        let mut cargo_args = vec!["test".to_owned()];
        enrich_args_for_json_output(&mut cargo_args, &[]);
        assert!(cargo_args.iter().any(|a| a == "--message-format"));
        assert!(cargo_args.iter().any(|a| a == "libtest-json-plus"));
        assert!(cargo_args.iter().any(|a| a == "--no-fail-fast"));
        assert!(!cargo_args.iter().any(|a| a == "--"));
    }

    #[test]
    fn enrich_args_appends_executable_args_after_separator() {
        let mut cargo_args = vec!["test".to_owned()];
        enrich_args_for_json_output(&mut cargo_args, &["--nocapture".to_owned()]);
        let separator_position = cargo_args
            .iter()
            .position(|a| a == "--")
            .expect("separator");
        assert!(
            cargo_args
                .iter()
                .position(|a| a == "--nocapture")
                .expect("exec arg")
                > separator_position
        );
    }

    #[test]
    fn enrich_args_does_not_duplicate_existing_flags() {
        let mut cargo_args = vec![
            "test".to_owned(),
            "--message-format".to_owned(),
            "libtest-json-plus".to_owned(),
            "--no-fail-fast".to_owned(),
        ];
        let before = cargo_args.len();
        enrich_args_for_json_output(&mut cargo_args, &[]);
        assert_eq!(cargo_args.len(), before);
    }

    #[test]
    fn handling_for_rule_defaults_to_advise() {
        let config = load_config(
            r#"
[rust.lsp]
server = "rust-analyzer"

[rust-general]
tools = ["edit"]
paths = ["**/*.rs"]
lsp.tests = { on = "tool", scope = "file" }
"#,
        );
        let rule = config.rule("rust-general").expect("rule");
        assert_eq!(handling_for_rule(rule), Handling::Advise);
    }

    #[test]
    fn handling_for_rule_uses_rule_diagnostics_handling() {
        let config = load_config(
            r#"
[rust.lsp]
server = "rust-analyzer"

[rust-general]
tools = ["edit"]
paths = ["**/*.rs"]
lsp.diagnostics = { handling = "block" }
lsp.tests = { on = "tool", scope = "file" }
"#,
        );
        let rule = config.rule("rust-general").expect("rule");
        assert_eq!(handling_for_rule(rule), Handling::Block);
    }
}
