use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use serde_json::Value;

use super::*;
use crate::agent::fork::ParentPromptPlan;
use crate::context::ContextLoader;
use crate::error::ToolError;
use crate::profile::PromptCommand;
use crate::system_prompt::{PromptPlan, PromptSource};
use crate::tool::ToolContext;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

struct LeasedPromptExecutor {
    context: Arc<ToolContext>,
    project_context_path: PathBuf,
    observed: Arc<Mutex<Option<PromptPlan>>>,
}

#[async_trait::async_trait]
impl ToolExecutor for LeasedPromptExecutor {
    async fn execute(
        &self,
        name: &str,
        _call_id: &str,
        _arguments: Value,
    ) -> Result<Value, ToolError> {
        match name {
            "rewrite_norn" => {
                std::fs::write(&self.project_context_path, "repository-v2").map_err(|error| {
                    execution_error(format!("failed to rewrite NORN.md: {error}"))
                })?;
                let file = std::fs::OpenOptions::new()
                    .write(true)
                    .open(&self.project_context_path)
                    .map_err(|error| {
                        execution_error(format!("failed to reopen NORN.md: {error}"))
                    })?;
                file.set_modified(SystemTime::now() + Duration::from_secs(60))
                    .map_err(|error| {
                        execution_error(format!("failed to advance NORN.md mtime: {error}"))
                    })?;
                Ok(serde_json::json!({"rewritten": true}))
            }
            "observe_parent_prompt" => {
                let parent = self.context.require_extension::<ParentPromptPlan>()?;
                let plan = parent.plan().clone();
                *self.observed.lock().map_err(|error| {
                    execution_error(format!("observation lock poisoned: {error}"))
                })? = Some(plan);
                Ok(serde_json::json!({"observed": true}))
            }
            _ => Err(ToolError::ToolNotFound {
                name: name.to_owned(),
            }),
        }
    }

    fn shared_context(&self) -> Option<Arc<ToolContext>> {
        Some(Arc::clone(&self.context))
    }
}

struct OuterLeasingExecutor {
    context: Arc<ToolContext>,
    snapshot: ToolExecutionSnapshot,
}

#[async_trait::async_trait]
impl ToolExecutor for OuterLeasingExecutor {
    async fn execute(
        &self,
        _name: &str,
        _call_id: &str,
        _arguments: Value,
    ) -> Result<Value, ToolError> {
        Err(execution_error(
            "tool dispatch bypassed the request's execution lease",
        ))
    }

    fn shared_context(&self) -> Option<Arc<ToolContext>> {
        Some(Arc::clone(&self.context))
    }

    fn execution_snapshot(&self) -> Option<ToolExecutionSnapshot> {
        Some(self.snapshot.clone())
    }
}

fn execution_error(reason: impl Into<String>) -> ToolError {
    ToolError::ExecutionFailed {
        reason: reason.into(),
    }
}

fn prompt_plan(project_context: &str) -> PromptPlan {
    let mut plan = PromptPlan::new();
    plan.set(PromptSource::ProductPolicy, "product-policy");
    plan.set(PromptSource::ForkAgentPolicy, "request-local-fork-identity");
    plan.set(PromptSource::OperatorProfile, "operator-profile");
    plan.set(PromptSource::WorkspaceProfile, "workspace-profile");
    plan.set(PromptSource::ProjectContextFile, project_context);
    plan
}

fn tool_definition(name: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.to_owned(),
        description: format!("Test tool {name}"),
        parameters: serde_json::json!({"type": "object"}),
    }
}

#[tokio::test]
async fn refreshed_parent_prompt_reaches_outer_and_exact_leased_contexts() -> TestResult {
    let workspace = tempfile::tempdir()?;
    let project_context_path = workspace.path().join("NORN.md");
    std::fs::write(&project_context_path, "repository-v1")?;

    let outer_context = Arc::new(ToolContext::empty());
    let leased_context = Arc::new(ToolContext::empty());
    assert!(!Arc::ptr_eq(&outer_context, &leased_context));

    let stale_parent = Arc::new(ParentPromptPlan::new(prompt_plan("stale-project")));
    outer_context.insert_extension(Arc::clone(&stale_parent));
    leased_context.insert_extension(stale_parent);

    let observed = Arc::new(Mutex::new(None));
    let leased_executor: Arc<dyn ToolExecutor> = Arc::new(LeasedPromptExecutor {
        context: Arc::clone(&leased_context),
        project_context_path: project_context_path.clone(),
        observed: Arc::clone(&observed),
    });
    let executor = OuterLeasingExecutor {
        context: Arc::clone(&outer_context),
        snapshot: ToolExecutionSnapshot {
            revision: 7,
            executor: leased_executor,
            definitions: Arc::from(vec![
                tool_definition("rewrite_norn"),
                tool_definition("observe_parent_prompt"),
            ]),
            dynamic_prompt_entries: Arc::from(Vec::new()),
        },
    };

    let provider = MockProvider::new(vec![
        vec![
            tool_call_delta("rewrite", Some("rewrite_norn"), "{}"),
            done_event(StopReason::ToolUse),
        ],
        vec![
            tool_call_delta("observe", Some("observe_parent_prompt"), "{}"),
            done_event(StopReason::ToolUse),
        ],
        vec![text_delta("done"), done_event(StopReason::EndTurn)],
    ]);
    let mut loop_context = LoopContext::new("legacy");
    loop_context.context_loader = Some(ContextLoader::load(workspace.path()));
    loop_context.install_stable_prompt_plan(prompt_plan("repository-v1"));
    let config = AgentLoopConfig {
        max_iterations: Some(3),
        ..AgentLoopConfig::default()
    };
    let store = EventStore::new();

    assert_completed(
        run_agent_step(AgentStepRequest {
            provider: &provider,
            executor: &executor,
            store: &store,
            user_prompt: "refresh inherited prompt authority",
            tools: &[],
            output_schema: None,
            model: "test-model",
            config: &config,
            event_tx: None,
            inbound: None,
            loop_context: &mut loop_context,
            cancel: None,
        })
        .await?,
    );

    let observed_plan = observed
        .lock()
        .map_err(|error| std::io::Error::other(format!("observation lock poisoned: {error}")))?
        .clone()
        .ok_or_else(|| std::io::Error::other("leased tool did not observe a parent prompt"))?;
    let mut expected = loop_context
        .stable_prompt_plan()
        .ok_or_else(|| std::io::Error::other("typed loop prompt plan disappeared"))?
        .clone();
    expected.remove(PromptSource::ForkAgentPolicy);

    assert_eq!(observed_plan, expected);
    assert_eq!(
        outer_context
            .require_extension::<ParentPromptPlan>()?
            .plan(),
        &expected,
    );
    assert_eq!(
        observed_plan
            .fragments()
            .iter()
            .find(|fragment| fragment.source() == PromptSource::ProjectContextFile)
            .map(crate::system_prompt::PromptFragment::content),
        Some("repository-v2"),
    );
    assert!(
        observed_plan
            .fragments()
            .iter()
            .all(|fragment| fragment.source() != PromptSource::ForkAgentPolicy),
    );
    Ok(())
}

#[tokio::test]
async fn prompt_command_file_change_waits_for_the_next_request_boundary() -> TestResult {
    let workspace = tempfile::tempdir()?;
    let project_context_path = workspace.path().join("NORN.md");
    std::fs::write(&project_context_path, "repository-v1")?;

    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "continue_work".to_owned(),
        Box::new(|_| Ok(serde_json::json!({"continued": true}))),
    );
    let executor = MockToolExecutor::new(handlers);
    let provider = MockProvider::new(vec![
        vec![
            tool_call_delta("continue", Some("continue_work"), "{}"),
            done_event(StopReason::ToolUse),
        ],
        vec![text_delta("done"), done_event(StopReason::EndTurn)],
    ]);
    let mut loop_context = LoopContext::with_working_dir(
        "legacy",
        crate::tool::context::SharedWorkingDir::new(workspace.path().to_path_buf()),
    );
    loop_context.context_loader = Some(ContextLoader::load(workspace.path()));
    loop_context.install_stable_prompt_plan(prompt_plan("repository-v1"));
    loop_context.prompt_commands.push(PromptCommand {
        name: "rewrite-context".to_owned(),
        command: "printf repository-v2-expanded > NORN.md; printf command-context".to_owned(),
        cache_ttl: Some(Duration::from_secs(60)),
    });
    let store = EventStore::new();

    assert_completed(
        run_agent_step(AgentStepRequest {
            provider: &provider,
            executor: &executor,
            store: &store,
            user_prompt: "preserve a coherent first request",
            tools: &[tool_definition("continue_work")],
            output_schema: None,
            model: "test-model",
            config: &AgentLoopConfig::default(),
            event_tx: None,
            inbound: None,
            loop_context: &mut loop_context,
            cancel: None,
        })
        .await?,
    );

    let requests = provider.requests()?;
    assert_eq!(requests.len(), 2);
    let first = requests[0]
        .messages
        .iter()
        .filter_map(|message| message.content.as_deref())
        .collect::<Vec<_>>()
        .join("\n");
    let second = requests[1]
        .messages
        .iter()
        .filter_map(|message| message.content.as_deref())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(first.contains("repository-v1"));
    assert!(!first.contains("repository-v2-expanded"));
    assert!(second.contains("repository-v2-expanded"));
    Ok(())
}
