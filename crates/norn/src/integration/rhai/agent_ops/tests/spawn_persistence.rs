use std::sync::Arc;

use super::support::{TestResult, build_context_with_provider, require, wait_for_terminal};
use crate::integration::rhai::context::build_norn_engine;
use crate::provider::agent_event::{SUBAGENT_COMPLETED_EVENT_TYPE, SUBAGENT_STARTED_EVENT_TYPE};
use crate::provider::events::{ProviderEvent, StopReason};
use crate::provider::mock::MockProvider;
use crate::provider::traits::Provider;
use crate::provider::usage::Usage;
use crate::session::events::SessionEvent;
use crate::session::manager::{CreateSessionOptions, SessionManager};
use crate::session::persistence::io::read_session_events_for_entry;
use crate::session::store::DurabilityPolicy;
use crate::session::{SessionBinding, SessionBrancher};

#[tokio::test(flavor = "multi_thread")]
async fn rhai_spawn_under_persistent_host_persists_child_timeline() -> TestResult {
    let temp = tempfile::tempdir()?;
    let manager = SessionManager::new(temp.path());
    let opened = manager.create(
        CreateSessionOptions {
            model: "haiku".to_owned(),
            working_dir: "/work".to_owned(),
            name: None,
        },
        DurabilityPolicy::Flush,
    )?;
    let root_id = opened.entry.id.clone();
    let binding = Arc::new(SessionBinding::persistent_root(
        Arc::new(SessionBrancher::new(
            manager.clone(),
            root_id.clone(),
            DurabilityPolicy::Flush,
        )),
        &opened.entry,
        &[],
    ));

    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![vec![
        ProviderEvent::TextDelta {
            text: "script child output".to_owned(),
        },
        ProviderEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input_tokens: 3,
                output_tokens: 2,
                ..Usage::default()
            },
            response_id: None,
        },
    ]]));
    let mut ctx = build_context_with_provider(provider);
    ctx.event_store = Arc::new(opened.store);
    ctx.session = binding;
    let registry = Arc::clone(&ctx.registry);

    let catalog_model = crate::model_catalog::default_selection().model;
    let handle = {
        let engine = build_norn_engine(&ctx);
        engine.eval::<crate::integration::rhai::AgentHandle>(&format!(
            r#"spawn_agent(#{{ task: "t", model: "{catalog_model}", role: "scout" }})"#,
        ))?
    };
    let child_id = handle.id();
    wait_for_terminal(&registry, child_id).await?;

    let row = manager.resolve(&child_id.to_string())?;
    let relative = require(
        row.rel_path.as_deref(),
        "child row must carry a relative path",
    )?;
    assert!(
        relative.starts_with(&format!("{root_id}/children/scout-"))
            && std::path::Path::new(relative)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("jsonl")),
        "script-child file must live under the root's children dir: {relative}",
    );
    assert_eq!(row.parent_id.as_deref(), Some(root_id.as_str()));
    assert!(temp.path().join(relative).exists());

    let child_read = read_session_events_for_entry(temp.path(), &row)?;
    assert!(
        child_read
            .events
            .iter()
            .any(|event| matches!(event, SessionEvent::ChildBranch { .. })),
    );
    assert!(child_read.events.iter().any(|event| matches!(
        event,
        SessionEvent::AssistantMessage { content, .. }
            if content.contains("script child output")
    )));

    let host_entry = manager.resolve(&root_id)?;
    let host_read = read_session_events_for_entry(temp.path(), &host_entry)?;
    assert!(host_read.events.iter().any(|event| matches!(
        event,
        SessionEvent::ChildBranch { child_session_id: Some(id), .. }
            if *id == child_id.to_string()
    )));
    let has_custom = |wanted: &str| {
        host_read.events.iter().any(|event| {
            matches!(
                event,
                SessionEvent::Custom { event_type, .. } if event_type == wanted
            )
        })
    };
    assert!(has_custom(SUBAGENT_STARTED_EVENT_TYPE));
    assert!(has_custom(SUBAGENT_COMPLETED_EVENT_TYPE));
    Ok(())
}
