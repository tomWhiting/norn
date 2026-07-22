use super::*;

#[test]
fn new_seeds_single_base_section() {
    let ctx = LoopContext::new("base");
    assert_eq!(ctx.system_sections, vec!["base".to_owned()]);
    assert_eq!(ctx.system_instruction(), "base");
}

#[test]
fn base_system_instruction_returns_only_first_section() {
    let mut ctx = LoopContext::new("base");
    ctx.append_system_section("dynamic-one");
    ctx.append_system_section("dynamic-two");
    assert_eq!(ctx.base_system_instruction(), "base");
}

#[test]
fn base_system_instruction_empty_when_default() {
    let ctx = LoopContext::default();
    assert_eq!(ctx.base_system_instruction(), "");
}

#[test]
fn dynamic_context_none_when_only_base() {
    let ctx = LoopContext::new("base");
    assert!(ctx.dynamic_context().is_none());
}

#[test]
fn dynamic_context_joins_sections_past_base() {
    let mut ctx = LoopContext::new("base");
    ctx.append_system_section("dyn-one");
    ctx.append_system_section("dyn-two");
    assert_eq!(ctx.dynamic_context().as_deref(), Some("dyn-one\n\ndyn-two"));
}

#[test]
fn dynamic_context_none_after_clear() {
    let mut ctx = LoopContext::new("base");
    ctx.append_system_section("extra");
    ctx.clear_dynamic_sections();
    assert!(ctx.dynamic_context().is_none());
}

#[test]
fn append_and_join_with_double_newline() {
    let mut ctx = LoopContext::new("base");
    ctx.append_system_section("dynamic-one");
    ctx.append_system_section("dynamic-two");
    assert_eq!(
        ctx.system_instruction(),
        "base\n\ndynamic-one\n\ndynamic-two",
    );
}

#[test]
fn clear_dynamic_sections_retains_base() {
    let mut ctx = LoopContext::new("base");
    ctx.append_system_section("extra");
    ctx.append_system_section("more");
    ctx.clear_dynamic_sections();
    assert_eq!(ctx.system_sections, vec!["base".to_owned()]);
    assert_eq!(ctx.system_instruction(), "base");
}

#[test]
fn default_has_no_components() {
    let ctx = LoopContext::default();
    assert!(ctx.rules.is_none());
    assert!(ctx.hooks.is_none());
    assert!(ctx.event_schemas.is_none());
    assert!(ctx.iteration_monitor.is_none());
    assert!(ctx.reasoning_effort.is_none());
    assert!(ctx.slash_commands.is_none());
    assert!(ctx.prompt_commands.is_empty());
    assert!(ctx.prompt_command_cache.is_empty());
    assert_eq!(ctx.retry_policy.max_retries, 2);
    assert!(ctx.token_estimator.is_none());
    assert!(ctx.variables.is_none());
    assert!(ctx.context_edits.is_none());
    assert!(ctx.diagnostics.is_none());
    assert!(ctx.action_log.is_none());
    assert!(ctx.context_loader.is_none());
    assert!(ctx.base_prefix.is_empty());
    assert!(ctx.base_suffix.is_empty());
    assert!(ctx.environment.is_none());
    assert_eq!(ctx.collaboration_mode, CollaborationMode::Default);
    assert!(ctx.child_result_rx.is_none());
    assert_eq!(ctx.children_usage.snapshot().input_tokens, 0);
    assert_eq!(ctx.children_usage.snapshot().output_tokens, 0);
    assert!(ctx.system_sections.is_empty());
    assert!(ctx.developer_sections.is_empty());
    assert_eq!(ctx.system_instruction(), "");
}

#[test]
fn rebuild_base_section_writes_prefix_only_when_loader_and_suffix_absent() {
    let mut ctx = LoopContext::new(String::new());
    ctx.base_prefix = "PREFIX".to_owned();
    ctx.rebuild_base_section();
    assert_eq!(ctx.system_sections, vec!["PREFIX".to_owned()]);
}

#[test]
fn rebuild_base_section_joins_prefix_and_suffix_with_double_newline() {
    let mut ctx = LoopContext::new(String::new());
    ctx.base_prefix = "PREFIX".to_owned();
    ctx.base_suffix = "SUFFIX".to_owned();
    ctx.rebuild_base_section();
    assert_eq!(ctx.system_sections, vec!["PREFIX\n\nSUFFIX".to_owned()]);
}

#[test]
fn rebuild_base_section_skips_empty_parts() {
    let mut ctx = LoopContext::new(String::new());
    ctx.base_suffix = "ONLY-SUFFIX".to_owned();
    ctx.rebuild_base_section();
    assert_eq!(ctx.system_sections, vec!["ONLY-SUFFIX".to_owned()]);
}

#[test]
fn rebuild_base_section_yields_empty_when_everything_absent() {
    let mut ctx = LoopContext::new(String::new());
    ctx.rebuild_base_section();
    assert_eq!(ctx.system_sections, vec![String::new()]);
}

#[test]
fn rebuild_base_section_pushes_when_sections_empty() {
    let mut ctx = LoopContext::default();
    assert!(ctx.system_sections.is_empty());
    ctx.base_prefix = "P".to_owned();
    ctx.rebuild_base_section();
    assert_eq!(ctx.system_sections, vec!["P".to_owned()]);
}

#[test]
fn typed_base_rebuild_preserves_norn_layer_authorities() {
    use crate::context::{ContextFile, ContextLoader};
    use crate::provider::request::MessageRole;
    use crate::system_prompt::{PromptPlan, PromptSource};

    let mut ctx = LoopContext::new("legacy");
    ctx.context_loader = Some(ContextLoader {
        user: Some(ContextFile {
            path: std::path::PathBuf::from("/user/NORN.md"),
            content: "user instructions".to_owned(),
            mtime: None,
        }),
        project: Some(ContextFile {
            path: std::path::PathBuf::from("/repo/NORN.md"),
            content: "project instructions".to_owned(),
            mtime: None,
        }),
        cwd: std::path::PathBuf::from("/repo"),
    });
    let mut plan = PromptPlan::new();
    plan.set(PromptSource::ProductPolicy, "product");
    ctx.install_stable_prompt_plan(plan);

    let messages = ctx.stable_prompt_messages();
    let roles = messages
        .iter()
        .map(|message| message.role.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        roles,
        [
            MessageRole::System,
            MessageRole::Developer,
            MessageRole::User
        ]
    );
    assert_eq!(
        messages
            .iter()
            .filter_map(|message| message.content.as_deref())
            .collect::<Vec<_>>(),
        ["product", "user instructions", "project instructions"]
    );
}

#[test]
fn typed_plan_without_a_loader_preserves_explicit_context_fragments() {
    use crate::system_prompt::{PromptPlan, PromptSource};

    let mut plan = PromptPlan::new();
    plan.set(PromptSource::ProductPolicy, "product");
    plan.set(PromptSource::UserContextFile, "explicit user context");
    plan.set(PromptSource::ProjectContextFile, "explicit project context");
    let mut ctx = LoopContext::new("legacy");
    ctx.install_stable_prompt_plan(plan);

    assert_eq!(
        ctx.stable_prompt_plan()
            .map(|plan| (plan.fragments().len(), plan.flattened_content())),
        Some((
            3,
            "product\n\nexplicit user context\n\nexplicit project context".to_owned()
        ))
    );
}

#[test]
fn refresh_context_if_stale_returns_false_without_loader() {
    let mut ctx = LoopContext::new("base");
    assert!(ctx.context_loader.is_none());
    assert!(
        !ctx.refresh_context_if_stale(),
        "no loader wired must surface as not stale"
    );
}

#[test]
#[serial_test::serial]
fn rebuild_base_section_runs_when_norn_md_changes() -> Result<(), Box<dyn std::error::Error>> {
    // Simulate the runner's iteration top: a single
    // `refresh_context_if_stale` + `rebuild_base_section` cycle must
    // pick up a rewritten project NORN.md. Only the project layer
    // is exercised (cwd is a tempdir) so the test does not touch
    // `$NORN_HOME`.
    let cwd = tempfile::tempdir()?;
    let project_path = cwd.path().join("NORN.md");
    std::fs::write(&project_path, "v1")?;

    let mut ctx = LoopContext::new(String::new());
    ctx.base_prefix = "PRE".to_owned();
    ctx.base_suffix = "POST".to_owned();
    ctx.context_loader = Some(crate::context::ContextLoader::load(cwd.path()));
    ctx.rebuild_base_section();
    assert!(
        ctx.system_sections[0].contains("v1"),
        "precondition: v1 content must appear in the base section",
    );

    // Rewrite the project NORN.md and bump its mtime forward to
    // defeat same-second filesystem-clock granularity.
    std::fs::write(&project_path, "v2")?;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&project_path)?;
    let future = std::time::SystemTime::now() + std::time::Duration::from_mins(1);
    file.set_modified(future)?;

    assert!(
        ctx.refresh_context_if_stale(),
        "rewritten NORN.md must produce a stale signal",
    );
    ctx.rebuild_base_section();
    assert!(
        ctx.system_sections[0].contains("v2"),
        "rebuild after staleness must inject the new content; got: {}",
        ctx.system_sections[0],
    );
    assert!(
        !ctx.system_sections[0].contains("v1"),
        "rebuild after staleness must drop the prior content; got: {}",
        ctx.system_sections[0],
    );
    Ok(())
}

#[test]
#[serial_test::serial]
fn refresh_context_if_stale_delegates_to_loader_when_present()
-> Result<(), Box<dyn std::error::Error>> {
    // Construct a loader pointing at an empty cwd and home — staleness
    // check has no files to observe, so the method must report false.
    let tmp = tempfile::tempdir()?;
    let mut ctx = LoopContext::new("base");
    ctx.context_loader = Some(ContextLoader::load(tmp.path()));

    // Two stat()s on absent files, no state change, no observable
    // staleness — false.
    assert!(!ctx.refresh_context_if_stale());
    Ok(())
}

#[test]
fn new_has_no_diagnostics() {
    let ctx = LoopContext::new("base");
    assert!(ctx.diagnostics.is_none());
}

#[test]
fn diagnostics_roundtrips() -> Result<(), Box<dyn std::error::Error>> {
    use crate::integration::{DiagnosticCollector, DiagnosticSeverity, NornDiagnostic};

    let mut ctx = LoopContext::new("base");
    let collector = Arc::new(DiagnosticCollector::new());
    ctx.diagnostics = Some(Arc::clone(&collector));

    let diag = NornDiagnostic {
        severity: DiagnosticSeverity::Warning,
        code: "tool-blocked".to_owned(),
        message: "blocked".to_owned(),
        source_tool: Some("write".to_owned()),
        file_path: None,
        suggestion: None,
    };
    ctx.diagnostics
        .as_ref()
        .ok_or_else(|| std::io::Error::other("diagnostic collector was not attached"))?
        .report(diag);

    let drained = collector.drain();
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].code, "tool-blocked");
    assert_eq!(drained[0].severity, DiagnosticSeverity::Warning);
    Ok(())
}

#[test]
fn override_reasoning_effort_returns_prior_none_and_sets_some() {
    let mut ctx = LoopContext::new("base");
    assert!(ctx.reasoning_effort.is_none());
    let prior = ctx.override_reasoning_effort(ReasoningEffort::High);
    assert!(prior.is_none());
    assert_eq!(ctx.reasoning_effort, Some(ReasoningEffort::High));
}

#[test]
fn override_reasoning_effort_returns_prior_some_value() {
    let mut ctx = LoopContext::new("base");
    ctx.reasoning_effort = Some(ReasoningEffort::Low);
    let prior = ctx.override_reasoning_effort(ReasoningEffort::XHigh);
    assert_eq!(prior, Some(ReasoningEffort::Low));
    assert_eq!(ctx.reasoning_effort, Some(ReasoningEffort::XHigh));
}

#[test]
fn restore_reasoning_effort_returns_field_to_prior_some() {
    let mut ctx = LoopContext::new("base");
    ctx.reasoning_effort = Some(ReasoningEffort::Medium);
    let prior = ctx.override_reasoning_effort(ReasoningEffort::XHigh);
    ctx.restore_reasoning_effort(prior);
    assert_eq!(ctx.reasoning_effort, Some(ReasoningEffort::Medium));
}

#[test]
fn restore_reasoning_effort_returns_field_to_prior_none() {
    let mut ctx = LoopContext::new("base");
    let prior = ctx.override_reasoning_effort(ReasoningEffort::High);
    ctx.restore_reasoning_effort(prior);
    assert!(ctx.reasoning_effort.is_none());
}

#[test]
fn caller_that_skips_override_leaves_reasoning_effort_untouched() {
    // Models the call-site contract: when a skill carries no effort
    // field, the caller skips override_reasoning_effort entirely and
    // the loop's value stays untouched.
    let mut ctx = LoopContext::new("base");
    ctx.reasoning_effort = Some(ReasoningEffort::Medium);
    let mapped: Option<ReasoningEffort> = None; // stands in for None skill effort
    if let Some(eff) = mapped {
        let _ = ctx.override_reasoning_effort(eff);
    }
    assert_eq!(ctx.reasoning_effort, Some(ReasoningEffort::Medium));
}
