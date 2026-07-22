use std::collections::BTreeMap;
use std::sync::Arc;

use super::support::{TestResult, build_context_with_provider, require, wait_for_terminal};
use crate::agent::variants::{VariantCatalog, VariantPromptOrigin};
use crate::config::types::VariantSettings;
use crate::integration::rhai::context::build_norn_engine;
use crate::provider::events::{ProviderEvent, StopReason};
use crate::provider::mock::MockProvider;
use crate::provider::request::MessageRole;
use crate::provider::usage::Usage;
use crate::tool::registry::ToolRegistry;
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::{Tool, ToolOutput};

const CHILD_POLICY: &str = "You are a sub-agent. Complete the task and stop.";
const CONFIGURED_PROMPT: &str = "RHAI-CONFIGURED-VARIANT-SENTINEL";
const TASK: &str = "RHAI-HUMAN-TASK-SENTINEL";

type PromptPlanObservation = (crate::system_prompt::PromptPlan, bool);

async fn assert_variant_request(
    catalog: VariantCatalog,
    variant_name: &str,
    variant_prompt: &str,
    variant_role: MessageRole,
) -> TestResult {
    let provider = Arc::new(MockProvider::new(vec![vec![
        ProviderEvent::TextDelta {
            text: "done".to_owned(),
        },
        ProviderEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            response_id: None,
        },
    ]]));
    let mut context = build_context_with_provider(Arc::<MockProvider>::clone(&provider));
    let mut registry = ToolRegistry::new();
    let shared = crate::tool::context::ToolContext::empty();
    shared.insert_extension(Arc::new(catalog));
    registry.set_context(Arc::new(shared));
    context.tool_registry = Some(Arc::new(registry));
    let agent_registry = Arc::clone(&context.registry);

    let model = crate::model_catalog::default_selection().model;
    let handle = {
        let engine = build_norn_engine(&context);
        engine.eval::<crate::integration::rhai::AgentHandle>(&format!(
            r#"spawn_agent(#{{ task: "{TASK}", variant: "{variant_name}", model: "{model}" }})"#
        ))?
    };
    wait_for_terminal(&agent_registry, handle.id()).await?;

    let requests = provider.requests()?;
    assert_eq!(requests.len(), 1, "test child must make one provider call");
    let first = require(requests.first(), "one provider request must be captured")?;
    let relevant = first
        .messages
        .iter()
        .filter_map(|message| {
            let body = message.content.as_deref()?;
            (body == CHILD_POLICY || body == variant_prompt || body == TASK)
                .then_some((message.role.clone(), body))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        relevant,
        [
            (MessageRole::System, CHILD_POLICY),
            (variant_role, variant_prompt),
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

#[tokio::test(flavor = "multi_thread")]
async fn rhai_spawn_preserves_builtin_and_configured_prompt_authority() -> TestResult {
    let builtins = VariantCatalog::build(None, &std::env::temp_dir())?;
    let explorer = require(builtins.get("explorer"), "explorer variant must exist")?;
    assert_eq!(explorer.prompt_origin, Some(VariantPromptOrigin::Builtin));
    let builtin_prompt = require(
        explorer.prompt.as_deref(),
        "explorer variant must have a prompt",
    )?
    .trim_end()
    .to_owned();
    assert_variant_request(builtins, "explorer", &builtin_prompt, MessageRole::System).await?;

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
        require(
            configured.get("configured-scout"),
            "configured variant must exist",
        )?
        .prompt_origin,
        Some(VariantPromptOrigin::Configured),
    );
    assert_variant_request(
        configured,
        "configured-scout",
        CONFIGURED_PROMPT,
        MessageRole::User,
    )
    .await?;
    Ok(())
}

struct PromptPlanProbe {
    seen: Arc<parking_lot::Mutex<Option<PromptPlanObservation>>>,
}

#[async_trait::async_trait]
impl Tool for PromptPlanProbe {
    fn name(&self) -> &'static str {
        "prompt_plan_probe"
    }

    fn description(&self) -> &'static str {
        "records typed and legacy parent prompt extensions"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({})
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    async fn execute(
        &self,
        _envelope: &crate::tool::envelope::ToolEnvelope,
        context: &crate::tool::context::ToolContext,
    ) -> Result<ToolOutput, crate::error::ToolError> {
        *self.seen.lock() = context
            .get_extension::<crate::agent::fork::ParentPromptPlan>()
            .map(|extension| {
                (
                    extension.plan().clone(),
                    context
                        .get_extension::<crate::agent::fork::ParentSystemInstruction>()
                        .is_some(),
                )
            });
        Ok(ToolOutput::success(serde_json::json!({"ok": true})))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn rhai_child_context_publishes_typed_prompt_plan_only() -> TestResult {
    let provider = Arc::new(MockProvider::new(vec![
        vec![
            ProviderEvent::ToolCallDelta {
                item_id: "tc-probe".to_owned(),
                call_id: None,
                name: Some("prompt_plan_probe".to_owned()),
                arguments_delta: "{}".to_owned(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            ProviderEvent::Done {
                stop_reason: StopReason::ToolUse,
                usage: Usage::default(),
                response_id: None,
            },
        ],
        vec![
            ProviderEvent::TextDelta {
                text: "done".to_owned(),
            },
            ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                response_id: None,
            },
        ],
    ]));
    let mut context = build_context_with_provider(Arc::<MockProvider>::clone(&provider));
    let seen = Arc::new(parking_lot::Mutex::new(None));
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(PromptPlanProbe {
        seen: Arc::clone(&seen),
    }));
    context.tool_registry = Some(Arc::new(registry));
    let agent_registry = Arc::clone(&context.registry);

    let model = crate::model_catalog::default_selection().model;
    let handle = {
        let engine = build_norn_engine(&context);
        engine.eval::<crate::integration::rhai::AgentHandle>(&format!(
            r#"spawn_agent(#{{ task: "{TASK}", model: "{model}", tools: ["prompt_plan_probe"] }})"#
        ))?
    };
    wait_for_terminal(&agent_registry, handle.id()).await?;

    let (plan, legacy_published) = require(seen.lock().clone(), "probe must execute")?;
    assert!(!legacy_published);
    assert_eq!(plan.fragments().len(), 1);
    assert_eq!(
        require(
            plan.fragments().first(),
            "one prompt fragment must be present"
        )?
        .source(),
        crate::system_prompt::PromptSource::ChildAgentPolicy,
    );
    assert!(
        plan.fragments()
            .iter()
            .all(|fragment| !fragment.content().contains(TASK)),
    );
    Ok(())
}
