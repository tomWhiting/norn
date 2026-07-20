use super::*;

#[tokio::test]
async fn evaluate_prompt_commands_appends_stdout_section() {
    let mut ctx = LoopContext::new("base");
    ctx.prompt_commands.push(PromptCommand {
        name: "greet".to_owned(),
        command: "echo hello".to_owned(),
        cache_ttl: None,
    });
    ctx.evaluate_prompt_commands(None).await;
    let combined = ctx.system_instruction();
    assert!(
        combined.contains("hello"),
        "system instruction must contain command stdout: {combined}",
    );
    assert!(
        combined.contains("greet"),
        "system instruction must contain command name heading: {combined}",
    );
}

#[tokio::test]
async fn evaluate_prompt_commands_failure_skips_section() {
    let mut ctx = LoopContext::new("base");
    ctx.prompt_commands.push(PromptCommand {
        name: "fail".to_owned(),
        command: "exit 7".to_owned(),
        cache_ttl: None,
    });
    ctx.evaluate_prompt_commands(None).await;
    let combined = ctx.system_instruction();
    assert_eq!(
        combined, "base",
        "failing prompt command must not append a section",
    );
}

#[tokio::test]
async fn evaluate_prompt_commands_caches_within_ttl() {
    let mut ctx = LoopContext::new("base");
    ctx.prompt_commands.push(PromptCommand {
        name: "stamp".to_owned(),
        command: "date +%N || echo stable".to_owned(),
        cache_ttl: Some(Duration::from_mins(1)),
    });
    ctx.evaluate_prompt_commands(None).await;
    let first = ctx.system_instruction();
    ctx.clear_dynamic_sections();
    ctx.evaluate_prompt_commands(None).await;
    let second = ctx.system_instruction();
    assert_eq!(
        first, second,
        "second evaluation within TTL must reuse cache"
    );
}

/// Two 300ms commands must evaluate concurrently: the iteration's
/// prompt-command cost is the slowest command, not the sum. Serial
/// evaluation would take at least 600ms.
#[tokio::test]
async fn evaluate_prompt_commands_runs_misses_concurrently_in_order() {
    let mut ctx = LoopContext::new("base");
    ctx.prompt_commands.push(PromptCommand {
        name: "first".to_owned(),
        command: "sleep 0.3 && echo one".to_owned(),
        cache_ttl: None,
    });
    ctx.prompt_commands.push(PromptCommand {
        name: "second".to_owned(),
        command: "sleep 0.3 && echo two".to_owned(),
        cache_ttl: None,
    });

    let started = Instant::now();
    ctx.evaluate_prompt_commands(None).await;
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_millis(550),
        "two 300ms commands must overlap, took {elapsed:?}",
    );
    // Sections append in registration order, not completion order.
    assert_eq!(ctx.system_sections.len(), 3);
    assert_eq!(ctx.system_sections[1], "# first\none");
    assert_eq!(ctx.system_sections[2], "# second\ntwo");
}

/// A configured `prompt_command_timeout` overrides the documented
/// 5-second default: a command slower than the budget is cut and its
/// section skipped, without waiting out the default.
#[tokio::test]
async fn evaluate_prompt_commands_honors_configured_timeout() {
    let mut ctx = LoopContext::new("base");
    ctx.prompt_commands.push(PromptCommand {
        name: "slowpoke".to_owned(),
        command: "sleep 2 && echo late".to_owned(),
        cache_ttl: None,
    });

    let started = Instant::now();
    ctx.evaluate_prompt_commands(Some(Duration::from_millis(100)))
        .await;
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_millis(900),
        "the configured budget must cut the command, took {elapsed:?}",
    );
    assert_eq!(
        ctx.system_instruction(),
        "base",
        "a timed-out prompt command must not append a section",
    );
}

#[test]
fn inject_environment_section_appends_when_configured() {
    let mut ctx = LoopContext::new("base");
    ctx.environment = Some(EnvironmentConfig {
        session_id: Some("test-session".to_owned()),
        model: "gpt-5.5".to_owned(),
    });
    ctx.inject_environment_section();
    let combined = ctx.system_instruction();
    assert!(
        combined.contains("# Environment"),
        "environment section must be appended: {combined}",
    );
    assert!(
        combined.contains("Model: gpt-5.5"),
        "environment section must include model: {combined}",
    );
    assert!(
        combined.contains("Session: test-session"),
        "environment section must include session: {combined}",
    );
}

#[test]
fn inject_environment_section_noop_when_not_configured() {
    let mut ctx = LoopContext::new("base");
    assert!(ctx.environment.is_none());
    ctx.inject_environment_section();
    assert_eq!(
        ctx.system_instruction(),
        "base",
        "no environment config means no section appended",
    );
}

#[test]
fn inject_environment_section_refreshes_after_clear() {
    let mut ctx = LoopContext::new("base");
    ctx.environment = Some(EnvironmentConfig {
        session_id: None,
        model: "gpt-5.5".to_owned(),
    });
    ctx.inject_environment_section();
    assert!(ctx.system_instruction().contains("# Environment"));

    ctx.clear_dynamic_sections();
    assert!(
        !ctx.system_instruction().contains("# Environment"),
        "clear must remove environment section",
    );

    ctx.inject_environment_section();
    assert!(
        ctx.system_instruction().contains("# Environment"),
        "re-injection must restore environment section",
    );
}

#[test]
fn inject_collaboration_mode_default_appends_section() {
    let mut ctx = LoopContext::new("base");
    ctx.inject_collaboration_mode();
    let instruction = ctx.system_instruction();
    assert!(
        instruction.contains("# Collaboration Mode"),
        "default mode should inject a section",
    );
    assert!(
        instruction.contains("prefer making reasonable assumptions"),
        "default mode should contain default guidance",
    );
}

#[test]
fn inject_collaboration_mode_plan_contains_phases() {
    let mut ctx = LoopContext::new("base");
    ctx.collaboration_mode = CollaborationMode::Plan;
    ctx.inject_collaboration_mode();
    let instruction = ctx.system_instruction();
    assert!(instruction.contains("plan mode"));
    assert!(instruction.contains("Ground in the environment"));
    assert!(instruction.contains("Not allowed"));
}

#[test]
fn inject_collaboration_mode_autonomous_contains_persist() {
    let mut ctx = LoopContext::new("base");
    ctx.collaboration_mode = CollaborationMode::Autonomous;
    ctx.inject_collaboration_mode();
    let instruction = ctx.system_instruction();
    assert!(instruction.contains("autonomous execution mode"));
    assert!(instruction.contains("Persist until the task is fully handled"));
}

#[test]
fn collaboration_mode_changes_mid_session() {
    let mut ctx = LoopContext::new("base");
    ctx.inject_collaboration_mode();
    assert!(ctx.system_instruction().contains("reasonable assumptions"));

    ctx.clear_dynamic_sections();
    ctx.collaboration_mode = CollaborationMode::Plan;
    ctx.inject_collaboration_mode();
    assert!(ctx.system_instruction().contains("plan mode"));
    assert!(!ctx.system_instruction().contains("reasonable assumptions"));
}
