use super::*;

// ---- Rule context lifecycle (N-007 R7 / N-017 R3 / NX-004) ----------

use crate::rules::engine::RuleEngine;
use crate::rules::types::{
    DeliveryMode, PathOperation, Rule, RuleId, RuntimeEvent, TriggerCondition, TriggerTiming,
};
use crate::session::events::{EventBase, SessionEvent};
use crate::session::store::EventStore;
use crate::tool::context::SharedWorkingDir;

fn append_rule_event(
    store: &EventStore,
    rule_id: &str,
    delivery: DeliveryMode,
    content: &str,
) -> Result<crate::session::events::EventId, crate::error::SessionError> {
    store.append(SessionEvent::RuleInjection {
        base: EventBase::new(store.last_event_id()),
        rule_id: rule_id.to_owned(),
        origin: None,
        delivery,
        timing: TriggerTiming::After,
        content: content.to_owned(),
    })
}

fn attached_rules(ctx: &LoopContext) -> Result<&RuleEngine, std::io::Error> {
    ctx.rules
        .as_ref()
        .ok_or_else(|| std::io::Error::other("rules were not attached"))
}

fn rs_rule(id: &str, body: &str, delivery: DeliveryMode) -> Rule {
    Rule {
        id: RuleId::from(id),
        name: id.to_owned(),
        triggers: vec![TriggerCondition::PathGlob {
            pattern: "**/*.rs".to_owned(),
        }],
        delivery,
        timing: TriggerTiming::After,
        body: body.to_owned(),
        shell_source: None,
    }
}

#[test]
fn legacy_system_context_rule_projects_conservatively_as_user()
-> Result<(), Box<dyn std::error::Error>> {
    let store = EventStore::new();
    append_rule_event(
        &store,
        "sys-rule",
        DeliveryMode::SystemContextAppend,
        "APPEND_BODY",
    )?;

    let messages = crate::session::conversion::events_to_messages(&store.events());
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].role,
        crate::provider::request::MessageRole::User
    );
    assert_eq!(messages[0].content.as_deref(), Some("APPEND_BODY"));
    Ok(())
}

#[test]
fn legacy_user_projection_respects_persisted_suppression_without_tracker()
-> Result<(), Box<dyn std::error::Error>> {
    let store = EventStore::new();
    let id = append_rule_event(
        &store,
        "sys-rule",
        DeliveryMode::SystemContextAppend,
        "APPEND_BODY",
    )?;

    let mut persisted = ContextEdits::new();
    persisted.suppress(&store, id.clone())?;
    let visible = crate::r#loop::context::construct_prompt(&store, &persisted);
    let messages = crate::session::conversion::events_to_messages(&visible.events);
    assert!(
        messages.is_empty(),
        "a compacted/suppressed rule event must not reach the prompt",
    );
    assert!(
        store.events().iter().any(|event| event.base().id == id),
        "the suppressed rule remains in the canonical audit log",
    );
    Ok(())
}

#[tokio::test]
async fn presence_rebuild_suppresses_then_re_fires_after_eviction()
-> Result<(), Box<dyn std::error::Error>> {
    let store = EventStore::new();
    let mut ctx = LoopContext::new("base");
    ctx.rules = Some(RuleEngine::new(vec![rs_rule(
        "broad",
        "BODY",
        DeliveryMode::ContextInjection,
    )]));
    ctx.context_edits = Some(ContextEdits::new());

    let event = RuntimeEvent::PathChanged {
        path: "src/lib.rs".to_owned(),
        operation: PathOperation::Read,
    };

    // Fires once when nothing is in context.
    ctx.rebuild_rule_presence(&store);
    let first = attached_rules(&ctx)?.process_event(&event).await;
    assert_eq!(first.len(), 1, "broad rule must fire on first match");

    // Persist its presence marker; rebuild sees it in context → no re-fire.
    let id = append_rule_event(&store, "broad", DeliveryMode::ContextInjection, "BODY")?;
    ctx.rebuild_rule_presence(&store);
    assert!(
        attached_rules(&ctx)?.process_event(&event).await.is_empty(),
        "rule present in context must not re-fire",
    );

    // Compact the marker out of the view → rule re-fires on next trigger.
    let edits = ctx
        .context_edits
        .as_mut()
        .ok_or_else(|| std::io::Error::other("context edits were not attached"))?;
    edits.suppress(&store, id)?;
    ctx.rebuild_rule_presence(&store);
    let after = attached_rules(&ctx)?.process_event(&event).await;
    assert_eq!(after.len(), 1, "evicted rule must re-fire");
    Ok(())
}

#[tokio::test]
async fn presence_without_tracker_projects_persisted_suppression()
-> Result<(), Box<dyn std::error::Error>> {
    let store = EventStore::new();
    let id = append_rule_event(&store, "broad", DeliveryMode::ContextInjection, "BODY")?;
    let mut persisted = ContextEdits::new();
    persisted.suppress(&store, id.clone())?;
    let mut ctx = LoopContext::new("base");
    ctx.rules = Some(RuleEngine::new(vec![rs_rule(
        "broad",
        "BODY",
        DeliveryMode::ContextInjection,
    )]));
    assert!(ctx.context_edits.is_none());

    ctx.rebuild_rule_presence(&store);
    let event = RuntimeEvent::PathChanged {
        path: "src/lib.rs".to_owned(),
        operation: PathOperation::Read,
    };
    let injections = attached_rules(&ctx)?.process_event(&event).await;

    assert_eq!(injections.len(), 1, "suppressed rule must re-fire");
    assert!(
        store.events().iter().any(|event| event.base().id == id),
        "presence rebuilding must not delete the canonical rule row",
    );
    Ok(())
}

#[tokio::test]
async fn scan_nested_norn_surfaces_nested_context_once() -> Result<(), Box<dyn std::error::Error>> {
    let cwd = tempfile::tempdir()?;
    let api = cwd.path().join("src").join("api");
    std::fs::create_dir_all(&api)?;
    std::fs::write(api.join("NORN.md"), "API_CONVENTIONS")?;
    std::fs::write(api.join("handler.rs"), "// stub")?;

    let mut ctx =
        LoopContext::with_working_dir("base", SharedWorkingDir::new(cwd.path().to_path_buf()));
    ctx.rules = Some(RuleEngine::new(vec![]));
    ctx.nested_scanner = Some(crate::context::scanner::NestedScanner::new(cwd.path()));

    // Touching a file under src/api registers the nested NORN.md rule
    // lazily through the scanner's immutable launch root.
    ctx.scan_nested_norn(&["src/api/handler.rs".to_owned()]);
    // Re-touch: must not register a duplicate.
    ctx.scan_nested_norn(&["src/api/other.rs".to_owned()]);

    let injections = ctx
        .rules
        .as_ref()
        .ok_or_else(|| std::io::Error::other("rules were not attached"))?
        .process_event(&RuntimeEvent::PathChanged {
            path: "src/api/handler.rs".to_owned(),
            operation: PathOperation::Read,
        })
        .await;
    assert_eq!(injections.len(), 1, "nested NORN.md surfaces exactly once");
    assert_eq!(injections[0].rule_id.as_str(), "norn-md:src/api");
    assert_eq!(
        injections[0].origin,
        crate::rules::source::RuleOrigin::Workspace
    );
    assert_eq!(injections[0].content, "API_CONVENTIONS");
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn default_context_keeps_nested_scanning_when_rules_are_attached_later()
-> Result<(), Box<dyn std::error::Error>> {
    let launch_root = std::env::current_dir()?.canonicalize()?;
    let nested = tempfile::Builder::new()
        .prefix(".norn-default-context-")
        .tempdir_in(&launch_root)?;
    std::fs::write(nested.path().join("NORN.md"), "DEFAULT_CONTEXT_RULE")?;
    let relative_dir = nested.path().strip_prefix(&launch_root)?;
    let touched = relative_dir.join("file.rs").to_string_lossy().into_owned();

    let mut ctx = LoopContext {
        rules: Some(RuleEngine::new(vec![])),
        ..LoopContext::default()
    };
    ctx.scan_nested_norn(std::slice::from_ref(&touched));
    let injections = ctx
        .rules
        .as_ref()
        .ok_or_else(|| std::io::Error::other("rules were not attached"))?
        .process_event(&RuntimeEvent::PathChanged {
            path: touched,
            operation: PathOperation::Read,
        })
        .await;

    assert_eq!(injections.len(), 1);
    assert_eq!(injections[0].content, "DEFAULT_CONTEXT_RULE");
    Ok(())
}

#[tokio::test]
async fn nested_scanner_keeps_launch_root_after_working_directory_changes()
-> Result<(), Box<dyn std::error::Error>> {
    let launch_root = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let outside_api = outside.path().join("src/api");
    std::fs::create_dir_all(&outside_api)?;
    std::fs::write(outside_api.join("NORN.md"), "SENTINEL_OUTSIDE_CONTEXT")?;

    let working_dir = SharedWorkingDir::new(launch_root.path().to_path_buf());
    let mut ctx = LoopContext::with_working_dir("base", working_dir.clone());
    ctx.rules = Some(RuleEngine::new(vec![]));
    ctx.nested_scanner = Some(crate::context::scanner::NestedScanner::new(
        launch_root.path(),
    ));
    working_dir.set(outside.path().to_path_buf());

    ctx.scan_nested_norn(&["src/api/file.rs".to_owned()]);
    let injections = ctx
        .rules
        .as_ref()
        .ok_or_else(|| std::io::Error::other("rules were not attached"))?
        .process_event(&RuntimeEvent::PathChanged {
            path: "src/api/file.rs".to_owned(),
            operation: PathOperation::Read,
        })
        .await;

    assert!(injections.is_empty());
    Ok(())
}
