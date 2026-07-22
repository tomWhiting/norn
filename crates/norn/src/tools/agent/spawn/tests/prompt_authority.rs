use std::collections::BTreeMap;

use super::*;
use crate::agent::variants::{VariantCatalog, VariantPromptOrigin};
use crate::config::types::VariantSettings;
use crate::provider::request::MessageRole;

const CHILD_POLICY: &str = "You are a sub-agent. Complete the task and stop.";
const TASK: &str = "SPAWN-HUMAN-TASK-SENTINEL";
const CONFIGURED_PROMPT: &str = "SPAWN-CONFIGURED-VARIANT-SENTINEL";

fn capturing_context() -> (Arc<MockProvider>, ToolContext) {
    let provider = Arc::new(MockProvider::new(vec![vec![
        ProviderEvent::TextDelta {
            text: "done".to_owned(),
        },
        done_event(),
    ]]));
    let registry = AgentRegistry::shared();
    let context = parent_ctx(
        Arc::<MockProvider>::clone(&provider),
        Uuid::new_v4(),
        &registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );
    (provider, context)
}

async fn assert_prompt_request(
    provider: &MockProvider,
    context: &ToolContext,
    arguments: serde_json::Value,
    guidance: &str,
    guidance_role: MessageRole,
) -> TestResult {
    spawn_and_join(&SpawnAgentTool::new(), context, arguments).await;

    let requests = provider.requests()?;
    assert_eq!(requests.len(), 1, "test child must make one provider call");
    let first = &requests[0];
    let relevant = first
        .messages
        .iter()
        .filter_map(|message| {
            let body = message.content.as_deref()?;
            (body == CHILD_POLICY || body == guidance || body == TASK)
                .then_some((message.role.clone(), body))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        relevant,
        [
            (MessageRole::System, CHILD_POLICY),
            (guidance_role, guidance),
            (MessageRole::User, TASK),
        ],
        "stable provenance and the current task must retain exact roles/order",
    );
    assert_eq!(
        first
            .messages
            .iter()
            .filter(|message| message.content.as_deref() == Some(TASK))
            .count(),
        1,
        "the human task must be sent exactly once",
    );
    assert!(first.messages.iter().all(|message| {
        message.role != MessageRole::System
            || message
                .content
                .as_deref()
                .is_none_or(|content| !content.contains(TASK))
    }));
    Ok(())
}

#[tokio::test]
async fn variant_prompts_preserve_builtin_and_configured_authority() -> TestResult {
    let builtins = VariantCatalog::build(None, &std::env::temp_dir())?;
    let explorer = builtins.get("explorer").ok_or("explorer exists")?;
    assert_eq!(explorer.prompt_origin, Some(VariantPromptOrigin::Builtin));
    let builtin_prompt = explorer
        .prompt
        .as_deref()
        .ok_or("explorer prompt")?
        .trim_end()
        .to_owned();
    let (provider, context) = capturing_context();
    context.insert_extension(Arc::new(builtins));
    assert_prompt_request(
        &provider,
        &context,
        json!({"task": TASK, "variant": "explorer", "model": CATALOG_MODEL}),
        &builtin_prompt,
        MessageRole::System,
    )
    .await?;

    let mut settings = BTreeMap::new();
    settings.insert(
        "configured-scout".to_owned(),
        VariantSettings {
            prompt: Some(CONFIGURED_PROMPT.to_owned()),
            ..VariantSettings::default()
        },
    );
    let configured = VariantCatalog::build(Some(&settings), &std::env::temp_dir())?;
    assert_eq!(
        configured
            .get("configured-scout")
            .ok_or("configured variant exists")?
            .prompt_origin,
        Some(VariantPromptOrigin::Configured),
    );
    let (provider, context) = capturing_context();
    context.insert_extension(Arc::new(configured));
    assert_prompt_request(
        &provider,
        &context,
        json!({
            "task": TASK,
            "variant": "configured-scout",
            "model": CATALOG_MODEL,
        }),
        CONFIGURED_PROMPT,
        MessageRole::User,
    )
    .await
}

#[tokio::test]
async fn workspace_profile_prompt_remains_user_authority() -> TestResult {
    const PROFILE: &str = "SPAWN-WORKSPACE-PROFILE-SENTINEL";
    let workspace = tempfile::tempdir()?;
    let profile_dir = workspace.path().join(".norn/profiles");
    std::fs::create_dir_all(&profile_dir)?;
    std::fs::write(
        profile_dir.join("workspace.md"),
        format!("---\nname: workspace\nmodel: {CATALOG_MODEL}\n---\n{PROFILE}\n"),
    )?;
    let launch_root = workspace.path().canonicalize()?;
    let (provider, context) = capturing_context();
    context.insert_extension(Arc::new(crate::runtime_init::extensions::LaunchWorkingDir(
        launch_root.clone(),
    )));
    context.set_working_dir(launch_root);
    assert_prompt_request(
        &provider,
        &context,
        json!({
            "task": TASK,
            "profile": "workspace",
            "model": CATALOG_MODEL,
            "role": "worker",
        }),
        PROFILE,
        MessageRole::User,
    )
    .await
}

#[tokio::test]
#[serial_test::serial]
async fn operator_profile_prompt_remains_developer_authority() -> TestResult {
    const PROFILE: &str = "SPAWN-OPERATOR-PROFILE-SENTINEL";
    let home = tempfile::tempdir()?;
    let workspace = tempfile::tempdir()?;
    let profile_dir = home.path().join("profiles");
    std::fs::create_dir_all(&profile_dir)?;
    std::fs::write(
        profile_dir.join("operator.md"),
        format!("---\nname: operator\nmodel: {CATALOG_MODEL}\n---\n{PROFILE}\n"),
    )?;

    temp_env::async_with_vars([("NORN_HOME", Some(home.path().as_os_str()))], async {
        let launch_root = workspace.path().canonicalize()?;
        let (provider, context) = capturing_context();
        context.insert_extension(Arc::new(crate::runtime_init::extensions::LaunchWorkingDir(
            launch_root.clone(),
        )));
        context.set_working_dir(launch_root);
        assert_prompt_request(
            &provider,
            &context,
            json!({
                "task": TASK,
                "profile": "operator",
                "model": CATALOG_MODEL,
                "role": "worker",
            }),
            PROFILE,
            MessageRole::Developer,
        )
        .await
    })
    .await
}
