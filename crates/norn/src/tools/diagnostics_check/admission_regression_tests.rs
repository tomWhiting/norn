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
async fn malformed_and_unknown_activations_are_not_absent_configuration() -> TestResult {
    let dir = tempfile::tempdir()?;
    let absent = build_diagnostic_infra(dir.path(), None, None);
    assert!(absent.configuration_error.is_none());
    assert!(absent.conventions.is_none());
    for source in [
        "[invalid",
        MISSING_TOOL,
        &format!("{MISSING_TOOL}\nlsp = {{ tests = {{ on = \"tool\", scope = \"package\" }} }}"),
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

#[tokio::test]
async fn generated_liminal_conventions_open_and_preserve_advisory_checks() -> TestResult {
    let dir = tempfile::tempdir()?;
    std::fs::write(
        dir.path().join("CONVENTIONS.toml"),
        include_str!("admission_liminal_fixture.toml"),
    )?;
    let infra = Arc::new(build_diagnostic_infra(dir.path(), None, None));
    assert!(infra.configuration_error.is_none());
    assert!(infra.conventions.is_some());
    let ctx = ToolContext::empty();
    ctx.insert_extension(Arc::clone(&infra));
    for (name, content, expected) in [
        (
            "example.py",
            "value = 1\n",
            vec!["python.ruff", "lsp.diagnostics"],
        ),
        (
            "example.rs",
            "fn main() {}\n",
            vec!["rust.clippy", "rust.rustfmt"],
        ),
    ] {
        let file = dir.path().join(name);
        std::fs::write(&file, content)?;
        let result = DiagnosticsPostCheck
            .check(
                &make_output(json!({"path":file,"bytes_written":content.len()})),
                &ctx,
            )
            .await;
        assert!(matches!(result.outcome, PostValidateOutcome::Pass));
        for check in expected {
            assert!(
                result
                    .advisories
                    .iter()
                    .any(|a| a.message.contains(check) && a.message.contains("was not run")),
                "{check}"
            );
        }
    }
    let file = dir.path().join("example.py");
    std::fs::write(&file, "# TODO: exercise retained pattern\n")?;
    let result = DiagnosticsPostCheck
        .check(&make_output(json!({"path":file,"bytes_written":34})), &ctx)
        .await;
    assert!(result.advisories.iter().any(|a| a.message.contains("TODO")));
    assert!(matches!(
        DiagnosticStopHook::new(infra).on_stop("done").await,
        HookOutcome::Proceed
    ));
    Ok(())
}

#[tokio::test]
async fn restricted_blocking_checks_apply_only_to_declared_paths_tools_and_triggers() -> TestResult
{
    let dir = tempfile::tempdir()?;
    let source = format!(
        "{MISSING_TOOL}\n[rust.diagnostics]\nclippy = {{ target = \"package\", handling = \"block\" }}"
    );
    std::fs::write(dir.path().join("CONVENTIONS.toml"), source)?;
    let infra = Arc::new(build_diagnostic_infra(dir.path(), None, None));
    assert!(infra.configuration_error.is_none());
    let ctx = ToolContext::empty();
    ctx.insert_extension(Arc::clone(&infra));
    assert!(matches!(
        DiagnosticStopHook::new(Arc::clone(&infra))
            .on_stop("no edits")
            .await,
        HookOutcome::Proceed
    ));
    let config = infra.conventions.as_ref().ok_or("config missing")?;
    let file = dir.path().join("example.rs");
    std::fs::write(&file, "fn main() {}")?;
    for (trigger, tool) in [
        (TestTrigger::TaskComplete, None),
        (TestTrigger::Tool, Some("edit")),
    ] {
        let result =
            run_diagnostics_for_trigger(trigger, tool, std::slice::from_ref(&file), config, &infra)
                .await;
        assert!(matches!(result.outcome, PostValidateOutcome::Pass));
        assert!(result.advisories.is_empty());
    }
    let result = DiagnosticsPostCheck
        .check(&make_output(json!({"path":file,"bytes_written":12})), &ctx)
        .await;
    assert!(
        matches!(result.outcome, PostValidateOutcome::Fail { errors } if errors.iter().any(|e| e.contains("rust.clippy") && e.contains("was not run")))
    );
    assert!(matches!(
        DiagnosticStopHook::new(infra).on_stop("done").await,
        HookOutcome::Block { .. }
    ));
    Ok(())
}

#[tokio::test]
async fn restricted_lsp_only_rule_retains_stop_trigger_and_blocking_handling() -> TestResult {
    let dir = tempfile::tempdir()?;
    std::fs::write(
        dir.path().join("CONVENTIONS.toml"),
        r#"
[rust.lsp]
server = "never-execute-this"
[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
lsp = { diagnostics = { handling = "block" }, tests = { on = "stop", scope = "package" } }
"#,
    )?;
    let infra = Arc::new(build_diagnostic_infra(dir.path(), None, None));
    assert!(infra.configuration_error.is_none());
    let config = infra.conventions.as_ref().ok_or("config missing")?;
    let file = dir.path().join("example.rs");
    std::fs::write(&file, "fn main() {}")?;
    let result =
        run_diagnostics_for_trigger(TestTrigger::Stop, None, &[file], config, &infra).await;
    assert!(
        matches!(result.outcome, PostValidateOutcome::Fail { errors } if errors.len() == 1 && errors[0].contains("lsp.tests"))
    );
    let other = dir.path().join("example.py");
    std::fs::write(&other, "value = 1")?;
    let result =
        run_diagnostics_for_trigger(TestTrigger::Tool, Some("write"), &[other], config, &infra)
            .await;
    assert!(matches!(result.outcome, PostValidateOutcome::Pass));
    assert!(result.advisories.is_empty());
    Ok(())
}
