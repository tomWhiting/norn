//! Regressions for unavailable checks, invalid declarations and completion truth.
use super::*;
use crate::tools::diagnostics_infra::build_diagnostic_infra;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const MISSING_TOOL: &str = r#"
[rust.patterns]
marker = { matcher = "regex", pattern = "BAD", handling = "block", feedback = "remove marker" }
[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
clippy = { on = "tool|stop", handling = "block" }
"#;

#[tokio::test]
async fn unknown_activated_tool_fails_post_check_and_stop() -> TestResult {
    let dir = tempfile::tempdir()?;
    let file = dir.path().join("src/main.rs");
    std::fs::create_dir_all(file.parent().ok_or("parent missing")?)?;
    std::fs::write(&file, "fn main() {}")?;
    let config = ConventionsConfig::load_from_str(MISSING_TOOL)?;
    let infra = Arc::new(test_infra(dir.path().to_path_buf(), Some(config)));
    let ctx = ToolContext::empty();
    ctx.insert_extension(Arc::clone(&infra));
    let result = DiagnosticsPostCheck
        .check(&make_output(json!({"path":file,"bytes_written":12})), &ctx)
        .await;
    match result.outcome {
        PostValidateOutcome::Fail { errors } => assert!(
            errors
                .iter()
                .any(|e| e.contains("rust.clippy") && e.contains("rust-general"))
        ),
        PostValidateOutcome::Pass => return Err("unknown check passed".into()),
    }
    assert!(matches!(
        DiagnosticStopHook::new(infra).on_stop("done").await,
        HookOutcome::Block { .. }
    ));
    Ok(())
}

#[tokio::test]
async fn outside_workspace_failure_survives_until_stop() -> TestResult {
    let workspace = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let file = outside.path().join("outside.rs");
    std::fs::write(&file, "fn main() {}")?;
    let config = ConventionsConfig::load_from_str(CHECKED_IN_CONVENTIONS)?;
    let infra = Arc::new(test_infra(workspace.path().to_path_buf(), Some(config)));
    let ctx = ToolContext::empty();
    ctx.insert_extension(Arc::clone(&infra));
    let result = DiagnosticsPostCheck
        .check(&make_output(json!({"path":file,"bytes_written":12})), &ctx)
        .await;
    assert!(matches!(result.outcome, PostValidateOutcome::Fail { .. }));
    assert!(infra.modified_files().contains(&file));
    assert!(matches!(
        DiagnosticStopHook::new(infra).on_stop("done").await,
        HookOutcome::Block { .. }
    ));
    Ok(())
}

#[tokio::test]
async fn malformed_and_stripped_activations_are_not_absent_configuration() -> TestResult {
    let dir = tempfile::tempdir()?;
    let absent = build_diagnostic_infra(dir.path(), None, None);
    assert!(absent.configuration_error.is_none());
    assert!(absent.conventions.is_none());
    for source in [
        "[invalid",
        MISSING_TOOL,
        &format!("{MISSING_TOOL}\nlsp = {{ tests = {{ on = \"tool\", scope = \"package\" }} }}"),
        &format!(
            "{MISSING_TOOL}\n[rust.diagnostics]\nclippy = {{ target = \"package\", handling = \"block\" }}"
        ),
    ] {
        std::fs::write(dir.path().join("CONVENTIONS.toml"), source)?;
        let infra = Arc::new(build_diagnostic_infra(dir.path(), None, None));
        assert!(infra.configuration_error.is_some());
        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::clone(&infra));
        let result = DiagnosticsPostCheck
            .check(&make_output(json!({"action":"complete","task":{}})), &ctx)
            .await;
        assert!(matches!(result.outcome, PostValidateOutcome::Fail { .. }));
        assert!(matches!(
            DiagnosticStopHook::new(infra).on_stop("done").await,
            HookOutcome::Block { .. }
        ));
    }
    Ok(())
}
