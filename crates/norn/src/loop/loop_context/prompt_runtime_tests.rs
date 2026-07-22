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
    let combined = ctx.managed_developer_context().unwrap_or_default();
    assert!(
        combined.contains("hello"),
        "Developer context must contain command stdout: {combined}",
    );
    assert!(
        combined.contains("greet"),
        "Developer context must contain command name heading: {combined}",
    );
    assert_eq!(ctx.system_instruction(), "base");
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
    assert!(ctx.managed_developer_context().is_none());
    assert_eq!(ctx.system_instruction(), "base");
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
    let first = ctx.managed_developer_context();
    ctx.clear_dynamic_sections();
    ctx.evaluate_prompt_commands(None).await;
    let second = ctx.managed_developer_context();
    assert_eq!(
        first, second,
        "second evaluation within TTL must reuse cache"
    );
}

#[tokio::test]
async fn evaluate_prompt_commands_cache_binds_the_complete_definition() -> Result<(), std::io::Error>
{
    let working_dir = tempfile::tempdir()?;
    let execution_log = working_dir.path().join("prompt-command-cache-runs");
    let mut ctx = LoopContext::with_working_dir(
        "base",
        crate::tool::context::SharedWorkingDir::new(working_dir.path().to_path_buf()),
    );
    ctx.prompt_commands.push(PromptCommand {
        name: "stamp".to_owned(),
        command: "printf first; printf x >> prompt-command-cache-runs".to_owned(),
        cache_ttl: Some(Duration::from_mins(5)),
    });

    ctx.evaluate_prompt_commands(None).await;
    assert_eq!(
        ctx.managed_developer_context().as_deref(),
        Some("# stamp\nfirst")
    );

    ctx.clear_dynamic_sections();
    ctx.prompt_commands[0].command =
        "printf second; printf y >> prompt-command-cache-runs".to_owned();
    ctx.evaluate_prompt_commands(None).await;
    assert_eq!(
        ctx.managed_developer_context().as_deref(),
        Some("# stamp\nsecond"),
        "same-name command changes must not reuse live cached authority",
    );

    ctx.clear_dynamic_sections();
    ctx.prompt_commands[0].cache_ttl = Some(Duration::from_mins(1));
    ctx.evaluate_prompt_commands(None).await;

    ctx.clear_dynamic_sections();
    ctx.prompt_commands[0].cache_ttl = None;
    ctx.evaluate_prompt_commands(None).await;

    assert_eq!(
        std::fs::read_to_string(execution_log)?,
        "xyyy",
        "command, TTL, and cache disablement must each invalidate the old entry",
    );
    Ok(())
}

#[tokio::test]
async fn evaluate_prompt_commands_cache_binds_the_working_directory() -> Result<(), std::io::Error>
{
    let root = tempfile::tempdir()?;
    let first_dir = root.path().join("first");
    let second_dir = root.path().join("second");
    std::fs::create_dir_all(&first_dir)?;
    std::fs::create_dir_all(&second_dir)?;
    let working_dir = crate::tool::context::SharedWorkingDir::new(first_dir.clone());
    let mut ctx = LoopContext::with_working_dir("base", working_dir.clone());
    ctx.prompt_commands.push(PromptCommand {
        name: "current-directory".to_owned(),
        command: "pwd".to_owned(),
        cache_ttl: Some(Duration::from_mins(5)),
    });

    ctx.evaluate_prompt_commands(None).await;
    assert!(
        ctx.managed_developer_context()
            .is_some_and(|content| content.contains(&first_dir.display().to_string()))
    );

    working_dir.set(second_dir.clone());
    ctx.clear_dynamic_sections();
    ctx.evaluate_prompt_commands(None).await;
    assert!(
        ctx.managed_developer_context()
            .is_some_and(|content| content.contains(&second_dir.display().to_string())),
        "a live cache entry from the old working directory must not retain authority",
    );
    Ok(())
}

#[tokio::test]
async fn evaluate_prompt_commands_cache_hits_do_not_extend_the_deadline()
-> Result<(), std::io::Error> {
    let working_dir = tempfile::tempdir()?;
    let execution_log = working_dir.path().join("deadline-runs");
    let mut ctx = LoopContext::with_working_dir(
        "base",
        crate::tool::context::SharedWorkingDir::new(working_dir.path().to_path_buf()),
    );
    ctx.prompt_commands.push(PromptCommand {
        name: "absolute-deadline".to_owned(),
        command: "printf x >> deadline-runs; printf available".to_owned(),
        cache_ttl: Some(Duration::from_hours(1)),
    });

    ctx.evaluate_prompt_commands(None).await;
    let original_deadline = ctx.prompt_command_cache["absolute-deadline"].expires_at;
    ctx.clear_dynamic_sections();
    ctx.evaluate_prompt_commands(None).await;
    assert_eq!(
        ctx.prompt_command_cache["absolute-deadline"].expires_at, original_deadline,
        "a hit must not turn an absolute TTL into a sliding expiration",
    );

    let entry = ctx
        .prompt_command_cache
        .get_mut("absolute-deadline")
        .ok_or_else(|| std::io::Error::other("cache entry disappeared before expiry test"))?;
    entry.expires_at = Some(Instant::now());
    ctx.clear_dynamic_sections();
    ctx.evaluate_prompt_commands(None).await;
    assert_eq!(std::fs::read_to_string(execution_log)?, "xx");
    Ok(())
}

#[tokio::test]
async fn evaluate_prompt_commands_skips_an_unrepresentable_cache_expiry() {
    let mut ctx = LoopContext::new("base");
    ctx.prompt_commands.push(PromptCommand {
        name: "huge-ttl".to_owned(),
        command: "printf available".to_owned(),
        cache_ttl: Some(Duration::MAX),
    });

    ctx.evaluate_prompt_commands(None).await;

    assert!(ctx.managed_developer_context().is_some_and(|context| {
        context.contains("# huge-ttl") && context.contains("available")
    }));
    assert!(
        !ctx.prompt_command_cache.contains_key("huge-ttl"),
        "an unrepresentable expiry must skip caching rather than panic",
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
    assert_eq!(ctx.developer_sections.len(), 2);
    assert_eq!(ctx.developer_sections[0], "# first\none");
    assert_eq!(ctx.developer_sections[1], "# second\ntwo");
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
    assert!(ctx.managed_developer_context().is_none());
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
