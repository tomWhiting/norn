/// Explicit window for test fixtures: "test-model" is deliberately
/// uncatalogued, and `build` now hard-errors on an unarmed window
/// (2026-07-05 incident guard). `272_000` is gpt-5.5's catalogued
/// standard window (assets/models.json) — factual, not invented.
const TEST_CONTEXT_WINDOW: u64 = 272_000;
use serde_json::Value;

use super::*;
use crate::agent::child_policy::CoordinationEnvelope;
use crate::agent::output::AgentStopReason;
use crate::agent::session_spec::SessionSpec;
use crate::integration::hooks::{Hook, HookOutcome, StopHook};
use crate::provider::events::{ProviderEvent, StopReason};
use crate::provider::mock::MockProvider;
use crate::provider::usage::Usage;
use crate::session::SessionManager;
use crate::session::store::DurabilityPolicy;
use crate::system_prompt::{PromptAuthority, PromptSource};
use crate::tool::context::ToolContext;
use crate::tools::diagnostics::build_diagnostic_infra;

fn provider_with(events: Vec<Vec<ProviderEvent>>) -> Arc<dyn Provider> {
    Arc::new(MockProvider::new(events))
}

/// The documented-proposal coordination envelope used by tests that
/// wire `.agent_registry(..)` — a deliberate test-caller choice, not
/// a library default.
fn test_child_policy() -> ChildPolicy {
    use crate::agent::child_policy::{DelegationBudget, MessagingScope};
    ChildPolicy {
        messaging: MessagingScope::SiblingsAndParent,
        delegation: DelegationBudget {
            remaining_depth: 1,
            max_concurrent_children: 32,
        },
        inbound_capacity: 32,
        loop_config: None,
    }
}

struct BlockingStopHook;

#[async_trait::async_trait]
impl StopHook for BlockingStopHook {
    async fn on_stop(&self, final_text: &str) -> HookOutcome {
        HookOutcome::Block {
            reason: format!("user-stop-hook: {} bytes", final_text.len()),
        }
    }
}

fn text_completion(text: &str) -> Vec<Vec<ProviderEvent>> {
    vec![vec![
        ProviderEvent::TextDelta {
            text: text.to_string(),
        },
        ProviderEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            response_id: None,
        },
    ]]
}

fn invalid_config_reason(result: Result<Agent, NornError>) -> String {
    match result {
        Err(NornError::Config(ConfigError::InvalidConfig { reason })) => reason,
        Err(other) => panic!("expected a typed config error, got: {other}"),
        Ok(_) => panic!("build must fail"),
    }
}

mod base_runtime;
mod channels_and_resume;
mod compaction;
mod context_wiring;
mod coordination;
mod execution;
mod filesystem_tools;
mod managed_sessions;
mod provider_surface;
mod scheduling;
mod schema_and_model;
mod session_creation;
mod setters;
