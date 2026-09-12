//! Model-facing branch directory dispatch and unsupported-filter refusal.

use std::sync::Arc;

use parking_lot::RwLock;
use serde_json::json;

use crate::agent::{AgentRegistry, MessageRouter, PendingAgentMessages};
use crate::provider::mock::MockProvider;
use crate::session::branch::{
    ChildBranchRequest, ChildDurability, SessionBinding, SessionBrancher,
};
use crate::session::events::ChildBranchKind;
use crate::session::store::DurabilityPolicy;
use crate::session::{CreateSessionOptions, SessionManager};
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::traits::Tool;
use crate::tools::{ActionLogTool, AgentToolInfra};

#[tokio::test]
async fn branches_runs_without_a_live_action_log_or_child_registry()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let manager = SessionManager::new(temp.path());
    let opened = manager.create(
        CreateSessionOptions {
            model: "fixture".into(),
            working_dir: "/fixture".into(),
            name: None,
        },
        DurabilityPolicy::Flush,
    )?;
    let root_id = opened.entry.id.clone();
    let brancher = Arc::new(SessionBrancher::new(
        manager.clone(),
        root_id.clone(),
        DurabilityPolicy::Flush,
    ));
    let session = SessionBinding::persistent_root(Arc::clone(&brancher), &opened.entry, &[]);
    let child = session.branch_child(
        &opened.store,
        &ChildBranchRequest {
            child_session_id: "persisted-child".into(),
            name_stem: "worker".into(),
            kind: ChildBranchKind::Spawn,
            durability: ChildDurability::Persist,
            model: "fixture".into(),
            working_dir: "/child".into(),
        },
    )?;
    drop((child, opened));
    let resumed = manager.resume(&root_id, DurabilityPolicy::Flush)?;
    let session = Arc::new(SessionBinding::persistent_root(
        brancher,
        &resumed.entry,
        &resumed.store.events(),
    ));
    let provider = Arc::new(MockProvider::new(vec![]));
    let shared_provider = Arc::clone(&provider);
    let ctx = ToolContext::empty();
    ctx.insert_extension(Arc::new(AgentToolInfra {
        registry: Arc::new(RwLock::new(AgentRegistry::new())),
        router: Arc::new(MessageRouter::new()),
        pending_messages: Arc::new(PendingAgentMessages::new()),
        provider: shared_provider,
        event_store: Arc::new(resumed.store),
        agent_id: uuid::Uuid::new_v4(),
        parent_id: None,
        grant: None,
        tool_registry: None,
        session,
    }));
    let envelope = |args| ToolEnvelope {
        tool_call_id: "directory-query".into(),
        tool_name: "action_log".into(),
        model_args: args,
        metadata: serde_json::Value::Null,
    };
    let output = ActionLogTool::new()
        .execute(&envelope(json!({"query":"branches"})), &ctx)
        .await?;
    assert_eq!(output.content["directory"]["kind"], "registered");
    assert_eq!(
        output.content["directory"]["sessions"][0]["session_id"],
        root_id
    );
    assert_eq!(
        output.content["directory"]["sessions"][1]["session_id"],
        "persisted-child"
    );
    assert_eq!(
        output.content["coverage"]["timeline_readability"],
        "not_inspected"
    );
    assert_eq!(
        output.content["coverage"]["live_recipients"],
        "not_inspected"
    );
    for extra in [
        json!({"filter":{}}),
        json!({"scope":"all"}),
        json!({"call_id":"old"}),
    ] {
        let mut args = extra;
        args["query"] = json!("branches");
        let result = ActionLogTool::new().execute(&envelope(args), &ctx).await?;
        assert_eq!(result.content["error"]["kind"], "invalid_arguments");
    }
    assert_eq!(provider.call_count(), 0);
    let missing = ActionLogTool::new()
        .execute(
            &envelope(json!({"query":"branches"})),
            &ToolContext::empty(),
        )
        .await;
    assert!(missing.is_err());
    Ok(())
}
