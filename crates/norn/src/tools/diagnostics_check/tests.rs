//! Behavioural tests for the convention-driven post-check pipeline.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;

use diagnostics::adapter::registry::AdapterRegistry;
use diagnostics::adapter::{AdapterCapabilities, CommandSpec, DiagnosticAdapter, OutputFormat};
use diagnostics::conventions::{ConventionsConfig, TestTrigger};
use diagnostics::event::{DiagnosticEvent, Severity};
use diagnostics::registry::PolicyRegistry;

use super::infra::DiagnosticInfra;
use super::post_check::{DiagnosticsPostCheck, run_diagnostics_for_trigger};
use super::stop_hook::DiagnosticStopHook;
use crate::integration::hooks::{HookOutcome, StopHook};
use crate::tool::context::ToolContext;
use crate::tool::lifecycle::{PostValidateOutcome, RuntimePostValidateCheck};
use crate::tool::traits::ToolOutput;
use crate::tools::lsp::{
    LspBackend, LspBackendError, LspDiagnostic, LspHover, LspLocation, LspSymbol, TestRunnable,
};

const ADMISSION_HELPER_CHILD: &str = "NORN_DIAGNOSTIC_ADMISSION_HELPER_CHILD";

#[test]
fn descriptor_admission_helpers_reserve_exact_weights() -> Result<(), Box<dyn std::error::Error>> {
    const TEST_NAME: &str =
        "tools::diagnostics_check::tests::descriptor_admission_helpers_reserve_exact_weights";
    if std::env::var_os(ADMISSION_HELPER_CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe()?)
            .args(["--exact", TEST_NAME, "--nocapture"])
            .env(ADMISSION_HELPER_CHILD, "1")
            .output()?;
        if output.status.success() {
            return Ok(());
        }
        return Err(std::io::Error::other(format!(
            "isolated diagnostic admission helper failed with {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ))
        .into());
    }

    let governor = crate::resource::DescriptorGovernor::global()?;
    let baseline = governor.available();
    let after_spawn = baseline
        .checked_sub(crate::resource::TWO_PIPE_SPAWN_PEAK as usize)
        .ok_or_else(|| {
            std::io::Error::other(format!(
                "isolated descriptor capacity {baseline} is below the two-pipe spawn peak"
            ))
        })?;
    let spawn = super::acquire_diagnostic_spawn()?;
    assert_eq!(governor.available(), after_spawn);
    drop(spawn);
    assert_eq!(governor.available(), baseline);

    let socket = super::acquire_diagnostic_socket()?;
    assert_eq!(governor.available(), baseline.saturating_sub(1));
    drop(socket);
    assert_eq!(governor.available(), baseline);
    Ok(())
}

fn make_output(content: serde_json::Value) -> ToolOutput {
    ToolOutput::success(content)
}

fn write_file(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent dirs");
    }
    let mut file = std::fs::File::create(path).expect("create file");
    file.write_all(contents.as_bytes()).expect("write file");
}

fn load_conventions(dir: &tempfile::TempDir, contents: &str) -> ConventionsConfig {
    let path = dir.path().join("CONVENTIONS.toml");
    let owned_contents;
    let contents = if contents.contains("clippy = { on") && !contents.contains("[rust.diagnostics]")
    {
        owned_contents = format!(
            "[rust.diagnostics]\nclippy = {{ target = \"package\", handling = \"block\" }}\n\n{contents}"
        );
        owned_contents.as_str()
    } else {
        contents
    };
    write_file(&path, contents);
    ConventionsConfig::load(&path).expect("load conventions")
}

struct EchoDiagnosticAdapter {
    command: CommandSpec,
    patterns: Vec<String>,
    capabilities: AdapterCapabilities,
}

impl EchoDiagnosticAdapter {
    fn new(binary: String) -> Self {
        Self {
            command: CommandSpec {
                binary,
                args: Vec::new(),
                env: Vec::new(),
            },
            patterns: vec!["**/*.rs".to_owned()],
            capabilities: AdapterCapabilities::default(),
        }
    }
}

impl DiagnosticAdapter for EchoDiagnosticAdapter {
    fn name(&self) -> &'static str {
        "echo_diag"
    }
    fn file_patterns(&self) -> &[String] {
        &self.patterns
    }
    fn language(&self) -> &'static str {
        "rust"
    }
    fn output_format(&self) -> OutputFormat {
        OutputFormat::PlainText
    }
    fn capabilities(&self) -> &AdapterCapabilities {
        &self.capabilities
    }
    fn auto_fix_declarations(&self) -> &[diagnostics::adapter::AutoFixDeclaration] {
        &[]
    }
    fn command(&self) -> Option<&CommandSpec> {
        Some(&self.command)
    }

    fn interpret(&self, run: &diagnostics::adapter::ToolRun) -> diagnostics::adapter::ToolOutcome {
        let events = run
            .stdout
            .lines()
            .filter_map(|line| {
                let (file, message) = line.split_once('|')?;
                Some(DiagnosticEvent {
                    severity: Severity::Warning,
                    message: message.to_owned(),
                    file: PathBuf::from(file),
                    line: 1,
                    column: 1,
                    end_line: None,
                    end_column: None,
                    source_tool: "echo_diag".to_owned(),
                    code: Some("echo_diag".to_owned()),
                    snippet: None,
                    entity_context: None,
                })
            })
            .collect();
        diagnostics::adapter::ToolOutcome::Ok(events)
    }
}

fn test_infra(workspace_root: PathBuf, conventions: Option<ConventionsConfig>) -> DiagnosticInfra {
    let socket_path = workspace_root.join(".git/yggdrasil/diag.sock");
    DiagnosticInfra {
        adapters: Arc::new(AdapterRegistry::new()),
        policies: Arc::new(PolicyRegistry::new()),
        workspace_root,
        socket_path,
        conventions,
        lsp_backend: None,
        lsp_bridge: None,
        modified_files: Arc::new(Mutex::new(HashSet::new())),
    }
}

fn test_infra_with_lsp(
    workspace_root: PathBuf,
    conventions: Option<ConventionsConfig>,
    backend: Arc<dyn LspBackend>,
) -> DiagnosticInfra {
    let mut infra = test_infra(workspace_root, conventions);
    infra.lsp_backend = Some(backend);
    infra
}

fn test_infra_with_lsp_bridge(
    workspace_root: PathBuf,
    conventions: Option<ConventionsConfig>,
    bridge: Arc<diagnostics::lsp_bridge::LspBridge>,
) -> DiagnosticInfra {
    let mut infra = test_infra(workspace_root, conventions);
    infra.lsp_bridge = Some(bridge);
    infra
}

/// Upcast `Arc<RecordingBackend>` to `Arc<dyn LspBackend>` via Rust's
/// `CoerceUnsized` — the explicit return type drives the coercion.
fn recording_backend_as_lsp(backend: &Arc<RecordingBackend>) -> Arc<dyn LspBackend> {
    Arc::clone(backend) as Arc<dyn LspBackend>
}

#[tokio::test]
async fn check_passes_immediately_when_conventions_is_none() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("src/lib.rs");
    write_file(&file, "fn main() {}\n");

    let ctx = ToolContext::empty();
    ctx.insert_extension(Arc::new(test_infra(dir.path().to_path_buf(), None)));

    let output = make_output(json!({
        "path": file.display().to_string(),
        "bytes_written": 12,
    }));

    let result = DiagnosticsPostCheck.check(&output, &ctx).await;
    assert!(matches!(result.outcome, PostValidateOutcome::Pass));
}

#[tokio::test]
async fn modified_files_accumulator_records_workspace_relative_paths_and_dedupes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("src/lib.rs");
    write_file(&file, "fn main() {}\n");

    let infra = Arc::new(test_infra(dir.path().to_path_buf(), None));
    let ctx = ToolContext::empty();
    ctx.insert_extension(Arc::clone(&infra));

    let output = make_output(json!({
        "path": file.display().to_string(),
        "bytes_written": 12,
    }));

    let result = DiagnosticsPostCheck.check(&output, &ctx).await;
    assert!(matches!(result.outcome, PostValidateOutcome::Pass));
    assert!(
        infra
            .modified_files()
            .contains(&PathBuf::from("src/lib.rs"))
    );

    let result = DiagnosticsPostCheck.check(&output, &ctx).await;
    assert!(matches!(result.outcome, PostValidateOutcome::Pass));
    assert_eq!(infra.modified_files().len(), 1);
}

#[tokio::test]
async fn rule_pattern_activation_honours_tool_trigger_and_handling_override() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("src/lib.rs");
    write_file(&file, "fn main() { /* TODO */ }\n");

    let conventions = load_conventions(
        &dir,
        r#"
[rust.patterns]
todo_markers = { matcher = "regex", pattern = "TODO", handling = "advise", feedback = "Remove TODO marker." }

[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
todo_markers = { on = "tool", handling = "block" }
"#,
    );

    let ctx = ToolContext::empty();
    ctx.insert_extension(Arc::new(test_infra(
        dir.path().to_path_buf(),
        Some(conventions),
    )));

    let output = make_output(json!({
        "path": file.display().to_string(),
        "bytes_written": 12,
    }));

    let result = DiagnosticsPostCheck.check(&output, &ctx).await;
    assert!(matches!(result.outcome, PostValidateOutcome::Fail { .. }));
    assert!(result.advisories.is_empty());
}

#[tokio::test]
async fn trigger_runner_honours_task_complete_activation_only() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("src/lib.rs");
    write_file(&file, "fn main() { /* TODO */ }\n");

    let conventions = load_conventions(
        &dir,
        r#"
[rust.patterns]
todo_markers = { matcher = "regex", pattern = "TODO", handling = "block", feedback = "Remove TODO marker." }

[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
todo_markers = { on = "task_complete", handling = "block" }
"#,
    );
    let infra = test_infra(dir.path().to_path_buf(), Some(conventions.clone()));

    let tool_result = run_diagnostics_for_trigger(
        TestTrigger::Tool,
        Some("write"),
        &[PathBuf::from("src/lib.rs")],
        &conventions,
        &infra,
    )
    .await;
    assert!(matches!(tool_result.outcome, PostValidateOutcome::Pass));

    let task_result = run_diagnostics_for_trigger(
        TestTrigger::TaskComplete,
        None,
        &[PathBuf::from("src/lib.rs")],
        &conventions,
        &infra,
    )
    .await;
    assert!(matches!(
        task_result.outcome,
        PostValidateOutcome::Fail { .. }
    ));
}

#[tokio::test]
async fn task_complete_output_requires_complete_action_and_task_key() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("src/lib.rs");
    write_file(&file, "fn main() { /* TODO */ }\n");
    let conventions = load_conventions(
        &dir,
        r#"
[rust.patterns]
todo_markers = { matcher = "regex", pattern = "TODO", handling = "block", feedback = "Remove TODO marker." }

[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
todo_markers = { on = "task_complete", handling = "block" }
"#,
    );
    let infra = Arc::new(test_infra(dir.path().to_path_buf(), Some(conventions)));
    infra
        .modified_files
        .lock()
        .expect("modified files lock")
        .insert(PathBuf::from("src/lib.rs"));
    let ctx = ToolContext::empty();
    ctx.insert_extension(Arc::clone(&infra));

    let create_result = DiagnosticsPostCheck
        .check(
            &make_output(json!({ "action": "create", "task": {} })),
            &ctx,
        )
        .await;
    assert!(matches!(create_result.outcome, PostValidateOutcome::Pass));

    let missing_task_result = DiagnosticsPostCheck
        .check(&make_output(json!({ "action": "complete" })), &ctx)
        .await;
    assert!(matches!(
        missing_task_result.outcome,
        PostValidateOutcome::Pass
    ));

    let complete_result = DiagnosticsPostCheck
        .check(
            &make_output(json!({ "action": "complete", "task": {} })),
            &ctx,
        )
        .await;
    assert!(matches!(
        complete_result.outcome,
        PostValidateOutcome::Fail { .. }
    ));
}

#[tokio::test]
async fn trigger_runner_aggregates_multiple_files_for_stop() {
    let dir = tempfile::tempdir().expect("tempdir");
    let first = dir.path().join("src/first.rs");
    let second = dir.path().join("src/second.rs");
    write_file(&first, "fn first() { /* TODO */ }\n");
    write_file(&second, "fn second() { /* TODO */ }\n");

    let conventions = load_conventions(
        &dir,
        r#"
[rust.patterns]
todo_markers = { matcher = "regex", pattern = "TODO", handling = "block", feedback = "Remove TODO marker." }

[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
todo_markers = { on = "stop", handling = "block" }
"#,
    );
    let infra = test_infra(dir.path().to_path_buf(), Some(conventions.clone()));

    let result = run_diagnostics_for_trigger(
        TestTrigger::Stop,
        None,
        &[
            PathBuf::from("src/first.rs"),
            PathBuf::from("src/second.rs"),
        ],
        &conventions,
        &infra,
    )
    .await;

    match result.outcome {
        PostValidateOutcome::Fail { errors } => {
            assert!(
                errors.iter().any(|error| error.contains("first.rs")),
                "first file finding should be aggregated: {errors:?}"
            );
            assert!(
                errors.iter().any(|error| error.contains("second.rs")),
                "second file finding should be aggregated: {errors:?}"
            );
        }
        PostValidateOutcome::Pass => panic!("stop trigger should aggregate both blocking findings"),
    }
}

#[tokio::test]
async fn diagnostic_stop_hook_blocks_only_on_stop_errors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("src/lib.rs");
    write_file(&file, "fn main() { /* TODO */ }\n");
    let conventions = load_conventions(
        &dir,
        r#"
[rust.patterns]
todo_markers = { matcher = "regex", pattern = "TODO", handling = "advise", feedback = "Remove TODO marker." }

[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
todo_markers = { on = "stop", handling = "block" }
"#,
    );
    let infra = Arc::new(test_infra(dir.path().to_path_buf(), Some(conventions)));
    infra
        .modified_files
        .lock()
        .expect("modified files lock")
        .insert(PathBuf::from("src/lib.rs"));
    let hook = DiagnosticStopHook::new(Arc::clone(&infra));

    let blocked = hook.on_stop("done").await;
    match blocked {
        HookOutcome::Block { reason } => {
            assert!(reason.contains("Stop blocked by diagnostic findings"));
            assert!(reason.contains("Remove TODO marker."));
        }
        HookOutcome::Proceed | HookOutcome::Modify { .. } => panic!("stop errors should block"),
    }

    write_file(&file, "fn main() {}\n");
    assert!(matches!(hook.on_stop("done").await, HookOutcome::Proceed));
}

#[tokio::test]
async fn diagnostic_stop_hook_proceeds_for_empty_and_advisory_only_findings() {
    let empty_dir = tempfile::tempdir().expect("tempdir");
    let empty_infra = Arc::new(test_infra(empty_dir.path().to_path_buf(), None));
    let empty_hook = DiagnosticStopHook::new(empty_infra);
    assert!(matches!(
        empty_hook.on_stop("done").await,
        HookOutcome::Proceed
    ));

    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("src/lib.rs");
    write_file(&file, "fn main() { /* TODO */ }\n");
    let conventions = load_conventions(
        &dir,
        r#"
[rust.patterns]
todo_markers = { matcher = "regex", pattern = "TODO", handling = "advise", feedback = "Remove TODO marker." }

[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
todo_markers = { on = "stop", handling = "advise" }
"#,
    );
    let infra = Arc::new(test_infra(dir.path().to_path_buf(), Some(conventions)));
    infra
        .modified_files
        .lock()
        .expect("modified files lock")
        .insert(PathBuf::from("src/lib.rs"));
    let hook = DiagnosticStopHook::new(infra);

    assert!(matches!(hook.on_stop("done").await, HookOutcome::Proceed));
}

#[tokio::test]
async fn rule_diagnostic_activation_runs_subprocess_with_args_env_and_rule_handling() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("src/lib.rs");
    write_file(&file, "fn main() {}\n");
    let script = dir.path().join("echo_diag.sh");
    write_file(
        &script,
        "#!/bin/sh\nif [ \"$ECHO_DIAG_ENV\" != \"yes\" ]; then exit 9; fi\nprintf '%s|%s\\n' \"$1\" \"$2\"\n",
    );
    let status = std::process::Command::new("chmod")
        .arg("+x")
        .arg(&script)
        .status()
        .expect("chmod script");
    assert!(status.success());

    let conventions = load_conventions(
        &dir,
        r#"
[rust.diagnostics]
echo_diag = { target = "file", handling = "advise", args = ["{file}", "subprocess finding"], env = ["ECHO_DIAG_ENV=yes"] }

[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
echo_diag = { on = "tool", handling = "block" }
"#,
    );

    let mut infra = test_infra(dir.path().to_path_buf(), Some(conventions));
    let mut adapters = AdapterRegistry::new();
    adapters.register(Box::new(EchoDiagnosticAdapter::new(
        script.display().to_string(),
    )));
    infra.adapters = Arc::new(adapters);

    let ctx = ToolContext::empty();
    ctx.insert_extension(Arc::new(infra));

    let output = make_output(json!({
        "path": file.display().to_string(),
        "bytes_written": 12,
    }));

    let result = DiagnosticsPostCheck.check(&output, &ctx).await;
    match result.outcome {
        PostValidateOutcome::Fail { errors } => {
            assert!(
                errors
                    .iter()
                    .any(|error| error.contains("subprocess finding"))
            );
        }
        PostValidateOutcome::Pass => panic!("rule handling override should block"),
    }
    assert!(result.advisories.is_empty());
}

#[tokio::test]
async fn rule_diagnostic_activation_missing_blocking_adapter_blocks() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("src/lib.rs");
    write_file(&file, "fn main() {}\n");

    let conventions = load_conventions(
        &dir,
        r#"
[rust.diagnostics]
missing_diag = { target = "file", handling = "advise" }

[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
missing_diag = { on = "tool", handling = "block" }
"#,
    );

    let ctx = ToolContext::empty();
    ctx.insert_extension(Arc::new(test_infra(
        dir.path().to_path_buf(),
        Some(conventions),
    )));

    let output = make_output(json!({
        "path": file.display().to_string(),
        "bytes_written": 12,
    }));

    let result = DiagnosticsPostCheck.check(&output, &ctx).await;
    match result.outcome {
        PostValidateOutcome::Fail { errors } => assert!(
            errors.iter().any(|error| error.contains("missing_diag")),
            "missing blocking adapter must produce a blocking finding: {errors:?}"
        ),
        PostValidateOutcome::Pass => panic!("missing blocking adapter should block"),
    }
}

#[tokio::test]
async fn matching_rs_convention_runs_checks() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("src/lib.rs");
    write_file(&file, "fn one() {}\nfn two() {}\nfn three() {}\n");

    let conventions = load_conventions(
        &dir,
        r#"
[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
loc = { limit = 1, handling = "block" }
"#,
    );

    let ctx = ToolContext::empty();
    ctx.insert_extension(Arc::new(test_infra(
        dir.path().to_path_buf(),
        Some(conventions),
    )));

    let output = make_output(json!({
        "path": file.display().to_string(),
        "bytes_written": 30,
    }));

    let result = DiagnosticsPostCheck.check(&output, &ctx).await;
    match result.outcome {
        PostValidateOutcome::Fail { errors } => {
            assert!(errors.iter().any(|error| error.contains("[file_length]")));
        }
        PostValidateOutcome::Pass => panic!("expected fail"),
    }
}

#[tokio::test]
async fn loc_only_rule_runs_without_tool_activations() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("src/lib.rs");
    write_file(&file, "fn one() {}\nfn two() {}\nfn three() {}\n");

    let conventions = load_conventions(
        &dir,
        r#"
[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
loc = { limit = 1, handling = "block" }
"#,
    );
    let rule = conventions.rule("rust-general").expect("rule");
    assert!(rule.rule.activations.is_empty());

    let infra = test_infra(dir.path().to_path_buf(), Some(conventions.clone()));
    let result = run_diagnostics_for_trigger(
        TestTrigger::Tool,
        Some("write"),
        &[PathBuf::from("src/lib.rs")],
        &conventions,
        &infra,
    )
    .await;

    match result.outcome {
        PostValidateOutcome::Fail { errors } => assert!(
            errors.iter().any(|error| error.contains("[file_length]")),
            "LOC-only rules must still run file-length checks: {errors:?}"
        ),
        PostValidateOutcome::Pass => panic!("LOC-only rule should block on file length"),
    }
}

#[tokio::test]
async fn unmatched_file_passes_when_conventions_exist() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("notes/readme.md");
    write_file(&file, "# hello\n");

    let conventions = load_conventions(
        &dir,
        r#"
[rust-general]
tools = ["write", "edit"]
paths = ["**/*.rs"]
loc = { limit = 1, handling = "block" }
"#,
    );

    let ctx = ToolContext::empty();
    ctx.insert_extension(Arc::new(test_infra(
        dir.path().to_path_buf(),
        Some(conventions),
    )));

    let output = make_output(json!({
        "path": file.display().to_string(),
        "bytes_written": 8,
    }));

    let result = DiagnosticsPostCheck.check(&output, &ctx).await;
    assert!(matches!(result.outcome, PostValidateOutcome::Pass));
}

/// Mock backend that returns a configurable list of runnables and tracks
/// which method the post-check called.
#[derive(Default)]
struct RecordingBackend {
    runnables: Vec<TestRunnable>,
    related: Vec<TestRunnable>,
    runnables_called: std::sync::atomic::AtomicBool,
    related_called: std::sync::atomic::AtomicBool,
}

#[async_trait]
impl LspBackend for RecordingBackend {
    async fn hover(
        &self,
        _path: &Path,
        _line: u32,
        _column: u32,
    ) -> Result<Option<LspHover>, LspBackendError> {
        Ok(None)
    }

    async fn definition(
        &self,
        _path: &Path,
        _line: u32,
        _column: u32,
    ) -> Result<Vec<LspLocation>, LspBackendError> {
        Ok(Vec::new())
    }

    async fn references(
        &self,
        _path: &Path,
        _line: u32,
        _column: u32,
    ) -> Result<Vec<LspLocation>, LspBackendError> {
        Ok(Vec::new())
    }

    async fn symbols(&self, _path: &Path) -> Result<Vec<LspSymbol>, LspBackendError> {
        Ok(Vec::new())
    }

    async fn diagnostics(&self, _path: &Path) -> Result<Vec<LspDiagnostic>, LspBackendError> {
        Ok(Vec::new())
    }

    async fn test_runnables(&self, _path: &Path) -> Result<Vec<TestRunnable>, LspBackendError> {
        self.runnables_called
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(self.runnables.clone())
    }

    async fn related_tests(
        &self,
        _path: &Path,
        _line: u32,
        _column: u32,
    ) -> Result<Vec<TestRunnable>, LspBackendError> {
        self.related_called
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(self.related.clone())
    }
}

#[tokio::test]
async fn lsp_test_path_no_ops_when_backend_is_none() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("src/lib.rs");
    write_file(&file, "fn ok() {}\n");

    let conventions = load_conventions(
        &dir,
        r#"
[rust.lsp]
server = "rust-analyzer"

[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
lsp.tests = { on = "tool", scope = "file" }
"#,
    );

    let ctx = ToolContext::empty();
    ctx.insert_extension(Arc::new(test_infra(
        dir.path().to_path_buf(),
        Some(conventions),
    )));

    let output = make_output(json!({
        "path": file.display().to_string(),
        "bytes_written": 12,
    }));

    let result = DiagnosticsPostCheck.check(&output, &ctx).await;
    assert!(matches!(result.outcome, PostValidateOutcome::Pass));
    assert!(result.advisories.is_empty());
}

#[tokio::test]
async fn rule_triggers_filter_drops_non_tool_triggers() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("src/lib.rs");
    write_file(&file, "fn ok() {}\n");

    let conventions = load_conventions(
        &dir,
        r#"
[rust.lsp]
server = "rust-analyzer"

[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
lsp.tests = { on = "commit", scope = "file" }
"#,
    );

    let backend = Arc::new(RecordingBackend::default());

    let ctx = ToolContext::empty();
    ctx.insert_extension(Arc::new(test_infra_with_lsp(
        dir.path().to_path_buf(),
        Some(conventions),
        recording_backend_as_lsp(&backend),
    )));

    let output = make_output(json!({
        "path": file.display().to_string(),
        "bytes_written": 12,
    }));

    let result = DiagnosticsPostCheck.check(&output, &ctx).await;
    assert!(matches!(result.outcome, PostValidateOutcome::Pass));
    assert!(
        !backend
            .runnables_called
            .load(std::sync::atomic::Ordering::SeqCst),
        "test_runnables must not be called when trigger set excludes Tool"
    );
}

#[tokio::test]
async fn file_scope_calls_test_runnables_not_related_tests() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("src/lib.rs");
    write_file(&file, "fn ok() {}\n");

    let conventions = load_conventions(
        &dir,
        r#"
[rust.lsp]
server = "rust-analyzer"

[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
lsp.tests = { on = "tool", scope = "file" }
"#,
    );

    let backend = Arc::new(RecordingBackend::default());

    let ctx = ToolContext::empty();
    ctx.insert_extension(Arc::new(test_infra_with_lsp(
        dir.path().to_path_buf(),
        Some(conventions),
        recording_backend_as_lsp(&backend),
    )));

    let output = make_output(json!({
        "path": file.display().to_string(),
        "bytes_written": 12,
    }));

    let _ = DiagnosticsPostCheck.check(&output, &ctx).await;
    assert!(
        backend
            .runnables_called
            .load(std::sync::atomic::Ordering::SeqCst),
        "file scope must query test_runnables"
    );
    assert!(
        !backend
            .related_called
            .load(std::sync::atomic::Ordering::SeqCst),
        "file scope must NOT query related_tests"
    );
}

#[tokio::test]
async fn affected_scope_calls_related_tests_not_test_runnables() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("src/lib.rs");
    write_file(&file, "fn ok() {}\n");

    let conventions = load_conventions(
        &dir,
        r#"
[rust.lsp]
server = "rust-analyzer"

[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
lsp.tests = { on = "tool", scope = "affected" }
"#,
    );

    let backend = Arc::new(RecordingBackend::default());

    let ctx = ToolContext::empty();
    ctx.insert_extension(Arc::new(test_infra_with_lsp(
        dir.path().to_path_buf(),
        Some(conventions),
        recording_backend_as_lsp(&backend),
    )));

    let output = make_output(json!({
        "path": file.display().to_string(),
        "bytes_written": 12,
    }));

    let _ = DiagnosticsPostCheck.check(&output, &ctx).await;
    assert!(
        backend
            .related_called
            .load(std::sync::atomic::Ordering::SeqCst),
        "affected scope must query related_tests"
    );
    assert!(
        !backend
            .runnables_called
            .load(std::sync::atomic::Ordering::SeqCst),
        "affected scope must NOT query test_runnables"
    );
}

#[tokio::test]
async fn missing_scope_skips_test_discovery_entirely() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("src/lib.rs");
    write_file(&file, "fn ok() {}\n");

    // No `lsp.tests` block on the rule, no `tests` block on the language.
    // R2 acceptance: "no scope at either level means no test execution".
    let conventions = load_conventions(
        &dir,
        r#"
[rust.lsp]
server = "rust-analyzer"

[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
lsp.diagnostics = { handling = "block" }
"#,
    );

    let backend = Arc::new(RecordingBackend::default());

    let ctx = ToolContext::empty();
    ctx.insert_extension(Arc::new(test_infra_with_lsp(
        dir.path().to_path_buf(),
        Some(conventions),
        recording_backend_as_lsp(&backend),
    )));

    let output = make_output(json!({
        "path": file.display().to_string(),
        "bytes_written": 12,
    }));

    let _ = DiagnosticsPostCheck.check(&output, &ctx).await;
    assert!(
        !backend
            .runnables_called
            .load(std::sync::atomic::Ordering::SeqCst)
            && !backend
                .related_called
                .load(std::sync::atomic::Ordering::SeqCst),
        "no scope at either level must skip discovery"
    );
}

mod lsp_diagnostics_path_tests {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use async_trait::async_trait;
    use diagnostics::lsp_bridge::LspBridge;
    use lsp::features::diagnostics::DiagnosticAggregator;
    use lsp_types::{Diagnostic, DiagnosticSeverity, Position, Range};
    use serde_json::json;

    use crate::tool::context::ToolContext;
    use crate::tool::lifecycle::{PostValidateOutcome, RuntimePostValidateCheck};
    use crate::tools::lsp::{
        LspBackend, LspBackendError, LspDiagnostic, LspHover, LspLocation, LspSymbol,
    };

    use super::super::DiagnosticInfra;
    use super::super::post_check::DiagnosticsPostCheck;
    use super::{
        load_conventions, make_output, test_infra, test_infra_with_lsp_bridge, write_file,
    };

    /// One publish item the [`PublishingBackend`] forwards to the
    /// aggregator when `run_flycheck` is called: `(file, source,
    /// diagnostics)`.
    type PublishItem = (PathBuf, String, Vec<Diagnostic>);

    fn lsp_warning(message: &str, line: u32) -> Diagnostic {
        Diagnostic {
            range: Range {
                start: Position::new(line, 0),
                end: Position::new(line, 4),
            },
            severity: Some(DiagnosticSeverity::WARNING),
            code: None,
            code_description: None,
            source: Some("clippy".to_owned()),
            message: message.to_owned(),
            related_information: None,
            tags: None,
            data: None,
        }
    }

    /// Test backend that publishes a configured list of diagnostics to
    /// the shared aggregator when `run_flycheck` is invoked — mirroring
    /// the rust-analyzer flycheck → publishDiagnostics flow. Records
    /// call order so tests can assert the brief's strict
    /// `clear_flycheck` → `run_flycheck` ordering (R2).
    struct PublishingBackend {
        aggregator: Arc<DiagnosticAggregator>,
        publishes: Mutex<Vec<PublishItem>>,
        clear_called: AtomicBool,
        run_called: AtomicBool,
        call_order: Mutex<Vec<&'static str>>,
    }

    impl PublishingBackend {
        fn new(aggregator: Arc<DiagnosticAggregator>) -> Self {
            Self {
                aggregator,
                publishes: Mutex::new(Vec::new()),
                clear_called: AtomicBool::new(false),
                run_called: AtomicBool::new(false),
                call_order: Mutex::new(Vec::new()),
            }
        }

        fn with_publishes(self, items: Vec<PublishItem>) -> Self {
            *self.publishes.lock().expect("publishes lock") = items;
            self
        }

        fn cleared_flycheck(&self) -> bool {
            self.clear_called.load(Ordering::SeqCst)
        }

        fn ran_flycheck(&self) -> bool {
            self.run_called.load(Ordering::SeqCst)
        }

        fn call_order(&self) -> Vec<&'static str> {
            self.call_order.lock().expect("order lock").clone()
        }
    }

    #[async_trait]
    impl LspBackend for PublishingBackend {
        async fn hover(
            &self,
            _path: &Path,
            _line: u32,
            _column: u32,
        ) -> Result<Option<LspHover>, LspBackendError> {
            Ok(None)
        }

        async fn definition(
            &self,
            _path: &Path,
            _line: u32,
            _column: u32,
        ) -> Result<Vec<LspLocation>, LspBackendError> {
            Ok(Vec::new())
        }

        async fn references(
            &self,
            _path: &Path,
            _line: u32,
            _column: u32,
        ) -> Result<Vec<LspLocation>, LspBackendError> {
            Ok(Vec::new())
        }

        async fn symbols(&self, _path: &Path) -> Result<Vec<LspSymbol>, LspBackendError> {
            Ok(Vec::new())
        }

        async fn diagnostics(&self, _path: &Path) -> Result<Vec<LspDiagnostic>, LspBackendError> {
            Ok(Vec::new())
        }

        async fn clear_flycheck(&self) -> Result<(), LspBackendError> {
            self.clear_called.store(true, Ordering::SeqCst);
            self.call_order
                .lock()
                .expect("order lock")
                .push("clear_flycheck");
            Ok(())
        }

        async fn run_flycheck(&self, _path: &Path) -> Result<(), LspBackendError> {
            self.run_called.store(true, Ordering::SeqCst);
            self.call_order
                .lock()
                .expect("order lock")
                .push("run_flycheck");
            let items = self.publishes.lock().expect("publishes lock").clone();
            for (file, source, diagnostics) in items {
                self.aggregator
                    .update(file, source, diagnostics, None)
                    .await;
            }
            Ok(())
        }
    }

    fn publishing_backend_as_lsp(backend: &Arc<PublishingBackend>) -> Arc<dyn LspBackend> {
        Arc::clone(backend) as Arc<dyn LspBackend>
    }

    fn infra_with_bridge_and_backend(
        workspace_root: PathBuf,
        conventions: Option<diagnostics::conventions::ConventionsConfig>,
        bridge: Arc<LspBridge>,
        backend: Arc<dyn LspBackend>,
    ) -> DiagnosticInfra {
        let mut infra = test_infra_with_lsp_bridge(workspace_root, conventions, bridge);
        infra.lsp_backend = Some(backend);
        infra
    }

    /// Builds the standard aggregator + bridge + publishing-backend
    /// trio for tests that exercise the wait-for-publish path.
    fn publish_trio(items: Vec<PublishItem>) -> (Arc<LspBridge>, Arc<PublishingBackend>) {
        let aggregator = Arc::new(DiagnosticAggregator::new());
        let bridge = Arc::new(LspBridge::new(Arc::clone(&aggregator)));
        let backend = Arc::new(PublishingBackend::new(aggregator).with_publishes(items));
        (bridge, backend)
    }

    #[tokio::test]
    async fn lsp_path_skipped_when_bridge_is_none() {
        // Bridge is None ⇒ LSP path returns FellBack; cascade falls through
        // to the existing server/inline path, which has no adapters wired
        // in test_infra ⇒ outcome is Pass with no errors.
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("src/lib.rs");
        write_file(&file, "fn ok() {}\n");

        let conventions = load_conventions(
            &dir,
            r#"
[rust.lsp]
server = "rust-analyzer"

[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
lsp.diagnostics = { handling = "block" }
"#,
        );

        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(test_infra(
            dir.path().to_path_buf(),
            Some(conventions),
        )));

        let output = make_output(json!({
            "path": file.display().to_string(),
            "bytes_written": 12,
        }));

        let result = DiagnosticsPostCheck.check(&output, &ctx).await;
        assert!(matches!(result.outcome, PostValidateOutcome::Pass));
        assert!(result.advisories.is_empty());
    }

    #[tokio::test]
    async fn lsp_path_skipped_when_no_rule_has_lsp_diagnostics() {
        // Bridge is wired but no rule opts in via `lsp.diagnostics` ⇒
        // FellBack returns before any subscribe / flycheck call; nothing
        // routes to findings from the LSP path and the publishing
        // backend is never asked to run.
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("src/lib.rs");
        write_file(&file, "fn ok() {}\n");

        let conventions = load_conventions(
            &dir,
            r#"
[rust.lsp]
server = "rust-analyzer"

[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
lsp.tests = { on = "tool", scope = "file" }
"#,
        );

        let (bridge, backend) = publish_trio(vec![(
            file.clone(),
            "clippy".to_owned(),
            vec![lsp_warning("avoid unwrap", 0)],
        )]);

        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(infra_with_bridge_and_backend(
            dir.path().to_path_buf(),
            Some(conventions),
            bridge,
            publishing_backend_as_lsp(&backend),
        )));

        let output = make_output(json!({
            "path": file.display().to_string(),
            "bytes_written": 12,
        }));

        let result = DiagnosticsPostCheck.check(&output, &ctx).await;
        assert!(matches!(result.outcome, PostValidateOutcome::Pass));
        assert!(result.advisories.is_empty());
        assert!(
            !backend.ran_flycheck(),
            "no matching lsp.diagnostics rule ⇒ flycheck must not be triggered"
        );
    }

    #[tokio::test]
    async fn lsp_path_block_handling_routes_to_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("src/lib.rs");
        write_file(&file, "fn ok() {}\n");

        let conventions = load_conventions(
            &dir,
            r#"
[rust.lsp]
server = "rust-analyzer"

[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
lsp.diagnostics = { handling = "block", timeout = 5 }
"#,
        );

        let (bridge, backend) = publish_trio(vec![(
            file.clone(),
            "clippy".to_owned(),
            vec![lsp_warning("blocking diagnostic from LSP", 0)],
        )]);

        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(infra_with_bridge_and_backend(
            dir.path().to_path_buf(),
            Some(conventions),
            bridge,
            publishing_backend_as_lsp(&backend),
        )));

        let output = make_output(json!({
            "path": file.display().to_string(),
            "bytes_written": 12,
        }));

        let result = DiagnosticsPostCheck.check(&output, &ctx).await;
        match result.outcome {
            PostValidateOutcome::Fail { errors } => {
                assert!(
                    errors
                        .iter()
                        .any(|error| error.contains("blocking diagnostic from LSP")),
                    "block handling must route LSP diagnostic into errors: {errors:?}"
                );
            }
            PostValidateOutcome::Pass => panic!("expected fail for block handling"),
        }
    }

    #[tokio::test]
    async fn lsp_path_advise_handling_routes_to_advisories() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("src/lib.rs");
        write_file(&file, "fn ok() {}\n");

        let conventions = load_conventions(
            &dir,
            r#"
[rust.lsp]
server = "rust-analyzer"

[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
lsp.diagnostics = { handling = "advise", timeout = 5 }
"#,
        );

        let (bridge, backend) = publish_trio(vec![(
            file.clone(),
            "clippy".to_owned(),
            vec![lsp_warning("advisory LSP diagnostic", 0)],
        )]);

        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(infra_with_bridge_and_backend(
            dir.path().to_path_buf(),
            Some(conventions),
            bridge,
            publishing_backend_as_lsp(&backend),
        )));

        let output = make_output(json!({
            "path": file.display().to_string(),
            "bytes_written": 12,
        }));

        let result = DiagnosticsPostCheck.check(&output, &ctx).await;
        assert!(matches!(result.outcome, PostValidateOutcome::Pass));
        assert!(
            result
                .advisories
                .iter()
                .any(|adv| adv.message.contains("advisory LSP diagnostic")),
            "advise handling must surface LSP diagnostic as advisory: {:?}",
            result.advisories
        );
    }

    #[tokio::test]
    async fn lsp_path_skips_publishes_for_unrelated_files() {
        // R1: the wait-for-publish loop discards updates whose
        // `file_path` does not match the modified file and continues
        // waiting within the same deadline. The unrelated publish is
        // delivered first; the matching publish is delivered second;
        // only the matching diagnostic surfaces in findings.
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("src/lib.rs");
        let other = dir.path().join("src/other.rs");
        write_file(&file, "fn ok() {}\n");
        write_file(&other, "fn other() {}\n");

        let conventions = load_conventions(
            &dir,
            r#"
[rust.lsp]
server = "rust-analyzer"

[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
lsp.diagnostics = { handling = "block", timeout = 5 }
"#,
        );

        let (bridge, backend) = publish_trio(vec![
            (
                other.clone(),
                "clippy".to_owned(),
                vec![lsp_warning("on unrelated file", 0)],
            ),
            (
                file.clone(),
                "clippy".to_owned(),
                vec![lsp_warning("on modified file", 0)],
            ),
        ]);

        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(infra_with_bridge_and_backend(
            dir.path().to_path_buf(),
            Some(conventions),
            bridge,
            publishing_backend_as_lsp(&backend),
        )));

        let output = make_output(json!({
            "path": file.display().to_string(),
            "bytes_written": 12,
        }));

        let result = DiagnosticsPostCheck.check(&output, &ctx).await;
        match result.outcome {
            PostValidateOutcome::Fail { errors } => {
                assert_eq!(errors.len(), 1, "only modified-file diagnostic surfaces");
                assert!(errors[0].contains("on modified file"));
                assert!(!errors[0].contains("on unrelated file"));
            }
            PostValidateOutcome::Pass => panic!("expected fail for block handling"),
        }
    }

    #[tokio::test]
    async fn lsp_path_used_outcome_skips_server_and_inline_paths() {
        // When the LSP path matches, the cascade must not fall through to
        // the server-query (LD-003) or inline-adapter paths. We assert
        // this by counting findings — the LSP bridge produces exactly
        // one error and adapters/socket are not touched. No advisories
        // from `advise_on` adapters appear.
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("src/lib.rs");
        write_file(&file, "fn ok() {}\n");

        let conventions = load_conventions(
            &dir,
            r#"
[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
clippy = { on = "tool", handling = "block" }
lsp.diagnostics = { handling = "block", timeout = 5 }
"#,
        );

        let (bridge, backend) = publish_trio(vec![(
            file.clone(),
            "clippy".to_owned(),
            vec![lsp_warning("only LSP path runs", 0)],
        )]);

        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(infra_with_bridge_and_backend(
            dir.path().to_path_buf(),
            Some(conventions),
            bridge,
            publishing_backend_as_lsp(&backend),
        )));

        let output = make_output(json!({
            "path": file.display().to_string(),
            "bytes_written": 12,
        }));

        let result = DiagnosticsPostCheck.check(&output, &ctx).await;
        match result.outcome {
            PostValidateOutcome::Fail { errors } => {
                assert_eq!(
                    errors.len(),
                    1,
                    "LSP path Used must short-circuit further cascade steps"
                );
                assert!(errors[0].contains("only LSP path runs"));
            }
            PostValidateOutcome::Pass => panic!("expected fail for LSP path Used"),
        }
    }

    #[tokio::test]
    async fn lsp_path_empty_publish_yields_no_findings_but_still_used() {
        // R1 happy path: the backend publishes an empty diagnostic list
        // for the modified file. The wait-for-publish receives the
        // (empty) `LspFileUpdate`, returns Used, and writes nothing into
        // findings — the cascade does NOT fall through to server/inline.
        // Steady-state: language server is wired, file is clean.
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("src/lib.rs");
        write_file(&file, "fn ok() {}\n");

        let conventions = load_conventions(
            &dir,
            r#"
[rust.lsp]
server = "rust-analyzer"

[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
lsp.diagnostics = { handling = "block", timeout = 5 }
"#,
        );

        let (bridge, backend) = publish_trio(vec![(file.clone(), "clippy".to_owned(), Vec::new())]);

        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(infra_with_bridge_and_backend(
            dir.path().to_path_buf(),
            Some(conventions),
            bridge,
            publishing_backend_as_lsp(&backend),
        )));

        let output = make_output(json!({
            "path": file.display().to_string(),
            "bytes_written": 12,
        }));

        let result = DiagnosticsPostCheck.check(&output, &ctx).await;
        assert!(matches!(result.outcome, PostValidateOutcome::Pass));
        assert!(result.advisories.is_empty());
    }

    #[tokio::test]
    async fn lsp_path_block_wins_when_multiple_rules_match() {
        // Two matching rules: one Advise, one Block. The strongest
        // handling (Block) must win so the diagnostic blocks rather
        // than degrades to advisory.
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("src/lib.rs");
        write_file(&file, "fn ok() {}\n");

        let conventions = load_conventions(
            &dir,
            r#"
[rust.lsp]
server = "rust-analyzer"

[rust-advise]
tools = ["write"]
paths = ["**/*.rs"]
lsp.diagnostics = { handling = "advise", timeout = 5 }

[rust-block]
tools = ["write"]
paths = ["**/lib.rs"]
lsp.diagnostics = { handling = "block", timeout = 5 }
"#,
        );

        let (bridge, backend) = publish_trio(vec![(
            file.clone(),
            "clippy".to_owned(),
            vec![lsp_warning("block precedence test", 0)],
        )]);

        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(infra_with_bridge_and_backend(
            dir.path().to_path_buf(),
            Some(conventions),
            bridge,
            publishing_backend_as_lsp(&backend),
        )));

        let output = make_output(json!({
            "path": file.display().to_string(),
            "bytes_written": 12,
        }));

        let result = DiagnosticsPostCheck.check(&output, &ctx).await;
        match result.outcome {
            PostValidateOutcome::Fail { errors } => {
                assert!(errors.iter().any(|e| e.contains("block precedence test")));
            }
            PostValidateOutcome::Pass => {
                panic!("Block handling must win when any matching rule blocks")
            }
        }
    }

    #[tokio::test]
    async fn lsp_path_calls_clear_then_run_flycheck_in_order() {
        // R2 acceptance: `clear_flycheck` is called before
        // `run_flycheck`, and both are called once for the modified
        // file. The wait-for-publish completes via the backend's
        // run_flycheck publish, so the cascade returns Used and we can
        // inspect the recorded call order on the backend.
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("src/lib.rs");
        write_file(&file, "fn ok() {}\n");

        let conventions = load_conventions(
            &dir,
            r#"
[rust.lsp]
server = "rust-analyzer"

[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
lsp.diagnostics = { handling = "block", timeout = 5 }
"#,
        );

        let (bridge, backend) = publish_trio(vec![(file.clone(), "clippy".to_owned(), Vec::new())]);

        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(infra_with_bridge_and_backend(
            dir.path().to_path_buf(),
            Some(conventions),
            bridge,
            publishing_backend_as_lsp(&backend),
        )));

        let output = make_output(json!({
            "path": file.display().to_string(),
            "bytes_written": 12,
        }));

        let _ = DiagnosticsPostCheck.check(&output, &ctx).await;
        assert!(backend.cleared_flycheck(), "clear_flycheck must be called");
        assert!(backend.ran_flycheck(), "run_flycheck must be called");
        assert_eq!(
            backend.call_order(),
            vec!["clear_flycheck", "run_flycheck"],
            "R2 strict order: clear_flycheck precedes run_flycheck"
        );
    }

    #[tokio::test]
    async fn lsp_path_falls_back_when_publish_does_not_arrive() {
        // R3 acceptance: timeout expiry returns FellBack and the
        // cascade runs (server-query → inline adapters). With no
        // adapters wired and no diagnostic server socket, the cascade
        // produces Pass. We verify the wait actually elapsed (≥ the
        // configured 1s) and the backend was asked to run flycheck.
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("src/lib.rs");
        write_file(&file, "fn ok() {}\n");

        let conventions = load_conventions(
            &dir,
            r#"
[rust.lsp]
server = "rust-analyzer"

[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
lsp.diagnostics = { handling = "block", timeout = 1 }
"#,
        );

        // Backend publishes for an unrelated file only — the wait-for-
        // publish will skip and ultimately time out.
        let unrelated = dir.path().join("src/other.rs");
        let (bridge, backend) = publish_trio(vec![(
            unrelated,
            "clippy".to_owned(),
            vec![lsp_warning("noise", 0)],
        )]);

        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(infra_with_bridge_and_backend(
            dir.path().to_path_buf(),
            Some(conventions),
            bridge,
            publishing_backend_as_lsp(&backend),
        )));

        let output = make_output(json!({
            "path": file.display().to_string(),
            "bytes_written": 12,
        }));

        let started = Instant::now();
        let result = DiagnosticsPostCheck.check(&output, &ctx).await;
        let elapsed = started.elapsed();

        assert!(matches!(result.outcome, PostValidateOutcome::Pass));
        assert!(
            elapsed >= Duration::from_millis(900),
            "wait must respect the 1s timeout (elapsed = {elapsed:?})"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "wait must not exceed timeout meaningfully (elapsed = {elapsed:?})"
        );
        assert!(
            backend.ran_flycheck(),
            "backend run_flycheck must be invoked before the wait times out"
        );
    }

    #[tokio::test]
    async fn lsp_path_strongest_timeout_picks_max_of_matching_rules() {
        // R3 acceptance: with multiple matching rules, the largest
        // configured timeout wins so a generous rule cannot be clipped
        // by a stricter sibling. We assert this by configuring two
        // matching rules — one 1s, one 5s — and never publishing. The
        // wait must persist past the 1s stricter timeout (we sample at
        // 1.3s) before falling back.
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("src/lib.rs");
        write_file(&file, "fn ok() {}\n");

        let conventions = load_conventions(
            &dir,
            r#"
[rust.lsp]
server = "rust-analyzer"

[rust-strict]
tools = ["write"]
paths = ["**/*.rs"]
lsp.diagnostics = { handling = "advise", timeout = 1 }

[rust-generous]
tools = ["write"]
paths = ["**/lib.rs"]
lsp.diagnostics = { handling = "advise", timeout = 5 }
"#,
        );

        // No publishes — the wait will time out at the maximum (5s).
        // To keep the test fast we abort with `tokio::time::timeout`
        // at 1.5s and assert the task is still pending.
        let (bridge, backend) = publish_trio(Vec::new());

        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(infra_with_bridge_and_backend(
            dir.path().to_path_buf(),
            Some(conventions),
            bridge,
            publishing_backend_as_lsp(&backend),
        )));

        let output = make_output(json!({
            "path": file.display().to_string(),
            "bytes_written": 12,
        }));

        let outer = tokio::time::timeout(
            Duration::from_millis(1_500),
            DiagnosticsPostCheck.check(&output, &ctx),
        )
        .await;
        assert!(
            outer.is_err(),
            "strongest_timeout must select max(1s, 5s) = 5s, so the inner wait \
             cannot complete within the outer 1.5s budget"
        );
    }

    #[tokio::test]
    async fn lsp_path_drains_stale_publish_before_flycheck() {
        // CO11: a stale publishDiagnostics in-flight when we subscribe
        // must not be consumed by wait_for_publish. The drain_pending
        // call between subscribe and trigger_flycheck discards it. Only
        // the fresh publish (from run_flycheck) should surface.
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("src/lib.rs");
        write_file(&file, "fn ok() {}\n");

        let conventions = load_conventions(
            &dir,
            r#"
[rust.lsp]
server = "rust-analyzer"

[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
lsp.diagnostics = { handling = "block", timeout = 5 }
"#,
        );

        let aggregator = Arc::new(DiagnosticAggregator::new());

        // Seed a stale publish BEFORE the post-check runs. This simulates
        // a previous flycheck completing just as we subscribe.
        aggregator
            .update(
                file.clone(),
                "clippy".to_owned(),
                vec![lsp_warning("stale finding from previous edit", 0)],
                None,
            )
            .await;

        let bridge = Arc::new(LspBridge::new(Arc::clone(&aggregator)));
        let backend = Arc::new(PublishingBackend::new(aggregator).with_publishes(vec![(
            file.clone(),
            "clippy".to_owned(),
            vec![lsp_warning("fresh finding from current edit", 0)],
        )]));

        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(infra_with_bridge_and_backend(
            dir.path().to_path_buf(),
            Some(conventions),
            bridge,
            publishing_backend_as_lsp(&backend),
        )));

        let output = make_output(json!({
            "path": file.display().to_string(),
            "bytes_written": 12,
        }));

        let result = DiagnosticsPostCheck.check(&output, &ctx).await;
        match result.outcome {
            PostValidateOutcome::Fail { errors } => {
                assert_eq!(
                    errors.len(),
                    1,
                    "exactly one finding from the fresh publish"
                );
                assert!(
                    errors[0].contains("fresh finding from current edit"),
                    "must surface the fresh finding, not the stale one: {errors:?}"
                );
                assert!(
                    !errors[0].contains("stale finding"),
                    "stale finding must have been drained: {errors:?}"
                );
            }
            PostValidateOutcome::Pass => {
                panic!("expected fail — fresh publish has a block-handling diagnostic");
            }
        }
    }

    #[tokio::test]
    async fn lsp_path_graceful_when_backend_is_none() {
        // R2 acceptance: when no LspBackend is wired, the function
        // still subscribes-and-waits. With no flycheck trigger nothing
        // publishes, the wait times out, FellBack returns, and the
        // cascade runs through to a final Pass (no adapters in test
        // infra). The post-check does NOT panic and does NOT abort.
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("src/lib.rs");
        write_file(&file, "fn ok() {}\n");

        let conventions = load_conventions(
            &dir,
            r#"
[rust.lsp]
server = "rust-analyzer"

[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
lsp.diagnostics = { handling = "block", timeout = 1 }
"#,
        );

        // Bridge wired, backend deliberately omitted.
        let aggregator = Arc::new(DiagnosticAggregator::new());
        let bridge = Arc::new(LspBridge::new(aggregator));

        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(test_infra_with_lsp_bridge(
            dir.path().to_path_buf(),
            Some(conventions),
            bridge,
        )));

        let output = make_output(json!({
            "path": file.display().to_string(),
            "bytes_written": 12,
        }));

        let result = DiagnosticsPostCheck.check(&output, &ctx).await;
        assert!(matches!(result.outcome, PostValidateOutcome::Pass));
        assert!(result.advisories.is_empty());
    }
}

#[cfg(unix)]
mod server_path_tests {
    use std::io;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use diagnostics::adapter::registry::AdapterRegistry;
    use diagnostics::conventions::{RemediationDef, ReportDef, ToolTarget};
    use diagnostics::event::{DiagnosticEvent, Severity};
    use diagnostics::policy::{Guidance, PolicyVerdict, Tier};
    use diagnostics::server::protocol::{
        DiagnosticQuery, DiagnosticResponse, DiagnosticResult, DiagnosticStatus, read_frame,
        write_frame,
    };
    use serde_json::json;
    use tokio::net::UnixListener;
    use tokio::task::JoinHandle;

    use crate::tool::context::ToolContext;
    use crate::tool::lifecycle::{PostValidateOutcome, RuntimePostValidateCheck};

    use super::super::DiagnosticInfra;
    use super::super::findings::Findings;
    use super::super::post_check::DiagnosticsPostCheck;
    use super::super::remediation;
    use super::{EchoDiagnosticAdapter, load_conventions, make_output, test_infra, write_file};

    /// Conventions for the fallback tests: an `echo_diag` activation that
    /// blocks, dispatched through the server-query path first and then —
    /// when the server is unreachable, stalls, or errors — through the
    /// inline adapter registry.
    const ECHO_DIAG_CONVENTIONS: &str = r#"
[rust.diagnostics]
echo_diag = { target = "file", handling = "block" }

[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
echo_diag = { on = "tool", handling = "block" }
"#;

    /// Registers the [`EchoDiagnosticAdapter`] bound to `true` so the
    /// inline fallback dispatches a real adapter that exits cleanly and
    /// emits no diagnostics. Without a registered adapter the hardened
    /// missing-adapter path emits a blocking "cannot run activated
    /// diagnostic tool" finding instead of passing.
    fn install_clean_inline_adapter(infra: &mut DiagnosticInfra) {
        let mut adapters = AdapterRegistry::new();
        adapters.register(Box::new(EchoDiagnosticAdapter::new("true".to_owned())));
        infra.adapters = Arc::new(adapters);
    }

    fn prepare_socket_dir(workspace_root: &std::path::Path) -> PathBuf {
        let socket_path = workspace_root.join(".git/yggdrasil/diag.sock");
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent).expect("create socket parent dir");
        }
        socket_path
    }

    fn report_verdict(headline: &str) -> PolicyVerdict {
        PolicyVerdict::Report {
            tier: Tier::Safety,
            guidance: Guidance {
                headline: headline.to_owned(),
                why: "policy explanation".to_owned(),
                fix: "fix steps".to_owned(),
                do_not: vec!["do not suppress".to_owned()],
            },
        }
    }

    fn clippy_event(file: PathBuf, message: &str) -> DiagnosticEvent {
        DiagnosticEvent {
            severity: Severity::Warning,
            message: message.to_owned(),
            file,
            line: 1,
            column: 1,
            end_line: None,
            end_column: None,
            source_tool: "clippy".to_owned(),
            code: Some("clippy::unwrap_used".to_owned()),
            snippet: None,
            entity_context: None,
        }
    }

    fn spawn_server(
        listener: UnixListener,
        response: DiagnosticResponse,
    ) -> JoinHandle<io::Result<DiagnosticQuery>> {
        tokio::spawn(async move {
            let (mut stream, _addr) = listener.accept().await?;
            let query: DiagnosticQuery = read_frame(&mut stream).await?;
            write_frame(&mut stream, &response).await?;
            Ok(query)
        })
    }

    fn infra_with_socket(
        workspace_root: PathBuf,
        socket_path: PathBuf,
        conventions: diagnostics::conventions::ConventionsConfig,
    ) -> DiagnosticInfra {
        let mut infra = test_infra(workspace_root, Some(conventions));
        infra.socket_path = socket_path;
        infra
    }

    #[tokio::test]
    async fn server_path_falls_back_when_socket_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("src/lib.rs");
        write_file(&file, "fn ok() {}\n");

        let conventions = load_conventions(&dir, ECHO_DIAG_CONVENTIONS);

        // Default socket path under tempdir's .git/yggdrasil/diag.sock does not exist.
        let mut infra = test_infra(dir.path().to_path_buf(), Some(conventions));
        install_clean_inline_adapter(&mut infra);
        assert!(
            !infra.socket_path.exists(),
            "test precondition: socket file must not exist"
        );

        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(infra));

        let output = make_output(json!({
            "path": file.display().to_string(),
            "bytes_written": 12,
        }));

        let started = Instant::now();
        let result = DiagnosticsPostCheck.check(&output, &ctx).await;
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "post-check must short-circuit when socket is missing"
        );
        // The registered inline adapter runs cleanly (exit 0, no output),
        // so the fallback produces no findings and the outcome is Pass.
        assert!(matches!(result.outcome, PostValidateOutcome::Pass));
    }

    #[tokio::test]
    async fn server_path_falls_back_when_socket_stale() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("src/lib.rs");
        write_file(&file, "fn ok() {}\n");

        let conventions = load_conventions(&dir, ECHO_DIAG_CONVENTIONS);

        let socket_path = prepare_socket_dir(dir.path());
        std::fs::write(&socket_path, "stale-not-a-real-socket").expect("write stale socket file");

        let mut infra = infra_with_socket(dir.path().to_path_buf(), socket_path, conventions);
        install_clean_inline_adapter(&mut infra);
        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(infra));

        let output = make_output(json!({
            "path": file.display().to_string(),
            "bytes_written": 12,
        }));

        let started = Instant::now();
        let result = DiagnosticsPostCheck.check(&output, &ctx).await;
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "post-check must short-circuit when socket is stale"
        );
        // The registered inline adapter runs cleanly (exit 0, no output),
        // so the fallback produces no findings and the outcome is Pass.
        assert!(matches!(result.outcome, PostValidateOutcome::Pass));
    }

    #[tokio::test]
    async fn server_path_consumes_fresh_response() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("src/lib.rs");
        write_file(&file, "fn ok() {}\n");

        let conventions = load_conventions(
            &dir,
            r#"
[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
clippy = { on = "tool", handling = "block" }
"#,
        );

        let socket_path = prepare_socket_dir(dir.path());
        let listener = UnixListener::bind(&socket_path).expect("bind listener");
        let response = DiagnosticResponse {
            status: DiagnosticStatus::Fresh,
            results: vec![DiagnosticResult {
                event: clippy_event(file.clone(), "avoid unwrap"),
                verdict: report_verdict("unwrap can panic"),
            }],
            error: None,
        };
        let server_task = spawn_server(listener, response);

        let infra = infra_with_socket(dir.path().to_path_buf(), socket_path, conventions);
        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(infra));

        let output = make_output(json!({
            "path": file.display().to_string(),
            "bytes_written": 12,
        }));

        let result = DiagnosticsPostCheck.check(&output, &ctx).await;
        let query = server_task.await.expect("join").expect("server io ok");
        assert_eq!(query.file_path, PathBuf::from("src/lib.rs"));
        match result.outcome {
            PostValidateOutcome::Fail { errors } => {
                assert!(
                    errors.iter().any(|error| error.contains("clippy")),
                    "block_on result must surface clippy code in error message: {errors:?}"
                );
            }
            PostValidateOutcome::Pass => panic!("expected fail for block_on clippy result"),
        }
    }

    #[tokio::test]
    async fn server_advise_handling_produces_advisory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("src/lib.rs");
        write_file(&file, "fn ok() {}\n");

        let conventions = load_conventions(
            &dir,
            r#"
[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
clippy = { on = "tool", handling = "advise" }
"#,
        );

        let socket_path = prepare_socket_dir(dir.path());
        let listener = UnixListener::bind(&socket_path).expect("bind listener");
        let response = DiagnosticResponse {
            status: DiagnosticStatus::Fresh,
            results: vec![DiagnosticResult {
                event: clippy_event(file.clone(), "advisory warning"),
                verdict: report_verdict("advisory headline"),
            }],
            error: None,
        };
        let server_task = spawn_server(listener, response);

        let infra = infra_with_socket(dir.path().to_path_buf(), socket_path, conventions);
        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(infra));

        let output = make_output(json!({
            "path": file.display().to_string(),
            "bytes_written": 12,
        }));

        let result = DiagnosticsPostCheck.check(&output, &ctx).await;
        server_task.await.expect("join").expect("server io ok");
        assert!(matches!(result.outcome, PostValidateOutcome::Pass));
        assert_eq!(result.advisories.len(), 1);
        assert_eq!(result.advisories[0].source, "clippy");
    }

    #[tokio::test]
    async fn server_error_response_falls_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("src/lib.rs");
        write_file(&file, "fn ok() {}\n");

        let conventions = load_conventions(&dir, ECHO_DIAG_CONVENTIONS);

        let socket_path = prepare_socket_dir(dir.path());
        let listener = UnixListener::bind(&socket_path).expect("bind listener");
        let response = DiagnosticResponse {
            status: DiagnosticStatus::Error,
            results: Vec::new(),
            error: Some("simulated server failure".to_owned()),
        };
        let server_task = spawn_server(listener, response);

        let mut infra = infra_with_socket(dir.path().to_path_buf(), socket_path, conventions);
        install_clean_inline_adapter(&mut infra);
        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(infra));

        let output = make_output(json!({
            "path": file.display().to_string(),
            "bytes_written": 12,
        }));

        let result = DiagnosticsPostCheck.check(&output, &ctx).await;
        server_task.await.expect("join").expect("server io ok");
        // The registered inline adapter runs cleanly (exit 0, no output),
        // so the fallback produces no findings and the outcome is Pass.
        assert!(matches!(result.outcome, PostValidateOutcome::Pass));
    }

    #[tokio::test]
    async fn server_results_scoped_to_modified_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("src/lib.rs");
        let other = dir.path().join("src/other.rs");
        write_file(&file, "fn ok() {}\n");
        write_file(&other, "fn other() {}\n");

        let conventions = load_conventions(
            &dir,
            r#"
[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
clippy = { on = "tool", handling = "block" }
"#,
        );

        let socket_path = prepare_socket_dir(dir.path());
        let listener = UnixListener::bind(&socket_path).expect("bind listener");
        let response = DiagnosticResponse {
            status: DiagnosticStatus::Fresh,
            results: vec![
                DiagnosticResult {
                    event: clippy_event(file.clone(), "on modified file"),
                    verdict: report_verdict("scoped headline"),
                },
                DiagnosticResult {
                    event: clippy_event(other.clone(), "on unrelated file"),
                    verdict: report_verdict("other-file headline"),
                },
            ],
            error: None,
        };
        let server_task = spawn_server(listener, response);

        let infra = infra_with_socket(dir.path().to_path_buf(), socket_path, conventions);
        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(infra));

        let output = make_output(json!({
            "path": file.display().to_string(),
            "bytes_written": 12,
        }));

        let result = DiagnosticsPostCheck.check(&output, &ctx).await;
        server_task.await.expect("join").expect("server io ok");
        match result.outcome {
            PostValidateOutcome::Fail { errors } => {
                assert_eq!(
                    errors.len(),
                    1,
                    "only the modified-file result should surface"
                );
                assert!(errors[0].contains("scoped headline"));
                assert!(!errors[0].contains("other-file headline"));
            }
            PostValidateOutcome::Pass => panic!("expected fail for modified-file result"),
        }
    }

    /// LD-003b R2: a server that accepts a connection but never replies must
    /// not be allowed to hang the post-check. The fast path bounds the
    /// `read_frame` wait via `QUERY_RESPONSE_TIMEOUT` (5s) and falls back to
    /// the inline adapter dispatch path on timeout. The registered inline
    /// adapter runs cleanly, so the fallback returns Pass.
    ///
    /// The wall-clock budget here is generous — `QUERY_RESPONSE_TIMEOUT`
    /// plus a 2s margin — so the test does not flake on a loaded CI runner.
    /// Any indefinite hang fails the test by exceeding the budget.
    #[tokio::test]
    async fn server_path_falls_back_when_server_stalls() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("src/lib.rs");
        write_file(&file, "fn ok() {}\n");

        let conventions = load_conventions(&dir, ECHO_DIAG_CONVENTIONS);

        let socket_path = prepare_socket_dir(dir.path());
        let listener = UnixListener::bind(&socket_path).expect("bind listener");

        // Server task: accept the connection then hold it open indefinitely.
        // We never call read_frame/write_frame so the client's read_frame
        // wrapper must hit its QUERY_RESPONSE_TIMEOUT and fall back.
        let stall_task = tokio::spawn(async move {
            let (stream, _addr) = listener.accept().await.expect("accept");
            // Keep the connection open beyond the client's expected wait.
            // Sleep well past QUERY_RESPONSE_TIMEOUT (5s) so the client
            // always times out before we drop the stream.
            tokio::time::sleep(Duration::from_secs(15)).await;
            drop(stream);
        });

        let mut infra = infra_with_socket(dir.path().to_path_buf(), socket_path, conventions);
        install_clean_inline_adapter(&mut infra);
        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(infra));

        let output = make_output(json!({
            "path": file.display().to_string(),
            "bytes_written": 12,
        }));

        let started = Instant::now();
        let result = DiagnosticsPostCheck.check(&output, &ctx).await;
        let elapsed = started.elapsed();

        // QUERY_RESPONSE_TIMEOUT is 5s; allow a 2s margin for runner load.
        assert!(
            elapsed < Duration::from_secs(7),
            "post-check must not hang on a stalled server (elapsed: {elapsed:?})"
        );
        // The registered inline adapter runs cleanly (exit 0, no output),
        // so the fallback produces no findings and the outcome is Pass.
        assert!(matches!(result.outcome, PostValidateOutcome::Pass));

        stall_task.abort();
        let _ = stall_task.await;
    }

    // -- R6: Remediation dispatch --

    #[tokio::test]
    async fn remediation_success_produces_no_findings() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("src/lib.rs");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "fn main() {}\n").unwrap();

        let def = RemediationDef {
            target: ToolTarget::File,
            args: Vec::new(),
        };
        let infra = test_infra(dir.path().to_path_buf(), None);
        let mut errors = Vec::new();
        let mut advisories = Vec::new();
        let mut findings = Findings {
            errors: &mut errors,
            advisories: &mut advisories,
        };

        remediation::run_remediation_tool(&file, "true", &def, &infra, &mut findings).await;

        assert!(
            errors.is_empty(),
            "successful remediation should produce no errors"
        );
        assert!(
            advisories.is_empty(),
            "successful remediation should produce no advisories"
        );
    }

    #[tokio::test]
    async fn remediation_failure_produces_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("src/lib.rs");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "fn main() {}\n").unwrap();

        let def = RemediationDef {
            target: ToolTarget::File,
            args: Vec::new(),
        };
        let infra = test_infra(dir.path().to_path_buf(), None);
        let mut errors = Vec::new();
        let mut advisories = Vec::new();
        let mut findings = Findings {
            errors: &mut errors,
            advisories: &mut advisories,
        };

        remediation::run_remediation_tool(&file, "false", &def, &infra, &mut findings).await;

        assert!(
            !errors.is_empty(),
            "failed remediation should produce an error"
        );
        assert!(
            errors[0].contains("[remediation:false]"),
            "error should name the tool: {}",
            errors[0]
        );
    }

    // -- R7: Report dispatch --

    #[tokio::test]
    async fn report_failure_produces_advisory_not_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("src/lib.rs");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "fn main() {}\n").unwrap();

        let def = ReportDef {
            target: ToolTarget::File,
            args: Vec::new(),
        };
        let infra = test_infra(dir.path().to_path_buf(), None);
        let mut errors = Vec::new();
        let mut advisories = Vec::new();
        let mut findings = Findings {
            errors: &mut errors,
            advisories: &mut advisories,
        };

        remediation::run_report_tool(&file, "false", &def, &infra, &mut findings).await;

        assert!(errors.is_empty(), "failed report should NOT produce errors");
        assert!(
            !advisories.is_empty(),
            "failed report should produce an advisory"
        );
        assert!(
            advisories[0].message.contains("[report:false]"),
            "advisory should name the tool: {}",
            advisories[0].message
        );
    }
}
