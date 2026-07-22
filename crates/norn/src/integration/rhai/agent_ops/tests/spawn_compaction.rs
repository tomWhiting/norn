use std::sync::Arc;

use uuid::Uuid;

use super::support::{TestResult, wait_for_terminal};
use crate::agent::message_router::MessageRouter;
use crate::agent::registry::AgentRegistry;
use crate::integration::rhai::context::{NornRhaiContext, build_norn_engine};
use crate::provider::traits::Provider;
use crate::session::store::EventStore;
use crate::tool::registry::ToolRegistry;

struct NoopTool;

#[async_trait::async_trait]
impl crate::tool::traits::Tool for NoopTool {
    fn name(&self) -> &'static str {
        "noop"
    }

    fn description(&self) -> &'static str {
        "no-op"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({})
    }

    fn effect(&self) -> crate::tool::scheduling::ToolEffect {
        crate::tool::scheduling::ToolEffect::ReadOnly
    }

    async fn execute(
        &self,
        _envelope: &crate::tool::envelope::ToolEnvelope,
        _ctx: &crate::tool::context::ToolContext,
    ) -> Result<crate::tool::traits::ToolOutput, crate::error::ToolError> {
        Ok(crate::tool::traits::ToolOutput::success(
            serde_json::json!({"ok": true}),
        ))
    }
}

struct CompactionDetectingProvider {
    saw_summarization: Arc<std::sync::atomic::AtomicBool>,
    tool_turns_remaining: parking_lot::Mutex<usize>,
}

impl Provider for CompactionDetectingProvider {
    fn stream(
        &self,
        request: crate::provider::request::ProviderRequest,
    ) -> Result<crate::provider::traits::ProviderStream, crate::error::ProviderError> {
        use crate::provider::events::{ProviderEvent, StopReason};
        use crate::provider::usage::Usage;

        let is_summarization = request.messages.iter().any(|message| {
            message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("You write compaction summaries"))
        });
        let events = if is_summarization {
            self.saw_summarization
                .store(true, std::sync::atomic::Ordering::SeqCst);
            vec![
                ProviderEvent::TextDelta {
                    text: "summary of earlier turns".to_owned(),
                },
                ProviderEvent::Done {
                    stop_reason: StopReason::EndTurn,
                    usage: Usage::default(),
                    response_id: None,
                },
            ]
        } else {
            let mut remaining = self.tool_turns_remaining.lock();
            if *remaining > 0 {
                *remaining -= 1;
                vec![
                    ProviderEvent::ToolCallDelta {
                        item_id: "tc-noop".to_owned(),
                        call_id: None,
                        name: Some("noop".to_owned()),
                        arguments_delta: "{}".to_owned(),
                        kind: crate::provider::request::ToolCallKind::Function,
                    },
                    ProviderEvent::Done {
                        stop_reason: StopReason::ToolUse,
                        usage: Usage {
                            input_tokens: 100_000_000,
                            output_tokens: 0,
                            ..Usage::default()
                        },
                        response_id: None,
                    },
                ]
            } else {
                vec![
                    ProviderEvent::TextDelta {
                        text: "final".to_owned(),
                    },
                    ProviderEvent::Done {
                        stop_reason: StopReason::EndTurn,
                        usage: Usage::default(),
                        response_id: None,
                    },
                ]
            }
        };
        Ok(Box::pin(futures_util::stream::iter(
            events.into_iter().map(Ok),
        )))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn rhai_spawn_child_arms_auto_compaction() -> TestResult {
    let saw_summarization = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let provider: Arc<dyn Provider> = Arc::new(CompactionDetectingProvider {
        saw_summarization: Arc::clone(&saw_summarization),
        tool_turns_remaining: parking_lot::Mutex::new(16),
    });

    let mut host_registry = ToolRegistry::new();
    host_registry.register(Box::new(NoopTool));
    let ctx = NornRhaiContext {
        registry: AgentRegistry::shared(),
        router: Arc::new(MessageRouter::new()),
        provider,
        agent_id: Uuid::new_v4(),
        runtime: tokio::runtime::Handle::current(),
        event_store: Arc::new(EventStore::new()),
        tool_registry: Some(Arc::new(host_registry)),
        working_dir: crate::tool::context::SharedWorkingDir::default(),
        child_policy: crate::agent::child_policy::ChildPolicy {
            messaging: crate::agent::child_policy::MessagingScope::SiblingsAndParent,
            delegation: crate::agent::child_policy::DelegationBudget {
                remaining_depth: 2,
                max_concurrent_children: 8,
            },
            inbound_capacity: 8,
            loop_config: None,
        },
        session: Arc::new(crate::session::SessionBinding::ephemeral_root()),
        events: None,
    };
    let registry = Arc::clone(&ctx.registry);
    let catalog_model = crate::model_catalog::default_selection().model;
    let handle = {
        let engine = build_norn_engine(&ctx);
        engine.eval::<crate::integration::rhai::AgentHandle>(&format!(
            r#"spawn_agent(#{{ task: "t", model: "{catalog_model}" }})"#
        ))?
    };

    wait_for_terminal(&registry, handle.id()).await?;
    assert!(
        saw_summarization.load(std::sync::atomic::Ordering::SeqCst),
        "the rhai child must issue an auto-compaction summarization request",
    );
    Ok(())
}
