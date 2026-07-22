use super::*;

use crate::rules::engine::RuleEngine;
use crate::rules::source::RuleOrigin;
use crate::tool::context::SharedWorkingDir;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

#[tokio::test]
async fn nested_repository_norn_reaches_next_request_once_as_user() -> TestResult {
    const NESTED_RULE: &str = "NESTED_REPOSITORY_AUTHORITY_SENTINEL";

    let workspace = tempfile::tempdir()?;
    let nested = workspace.path().join("src").join("api");
    std::fs::create_dir_all(&nested)?;
    std::fs::write(nested.join("NORN.md"), NESTED_RULE)?;
    std::fs::write(nested.join("handler.rs"), "// fixture")?;

    let first = vec![
        tool_call_delta(
            "read_nested",
            Some("read"),
            r#"{"path":"src/api/handler.rs"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];
    let second = vec![
        tool_call_delta("finish", Some("structured_output"), r#"{"answer":"done"}"#),
        done_event(StopReason::ToolUse),
    ];
    let provider = MockProvider::new(vec![first, second]);

    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "read".to_owned(),
        Box::new(|_| Ok(serde_json::json!({"content": "// fixture"}))),
    );
    let executor = MockToolExecutor::new(handlers);
    let read_tool = ToolDefinition {
        name: "read".to_owned(),
        description: "Read one workspace file".to_owned(),
        parameters: serde_json::json!({}),
    };
    let schema = simple_schema();
    let store = EventStore::new();
    let mut loop_context = LoopContext::with_working_dir(
        "product-system",
        SharedWorkingDir::new(workspace.path().to_path_buf()),
    );
    loop_context.rules = Some(RuleEngine::new(Vec::new()));

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[read_tool],
            schema: Some(&schema),
            config: &default_config(),
            event_tx: None,
            inbound: None,
        },
        &mut loop_context,
    )
    .await;
    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "done");

    let requests = provider.requests()?;
    assert_eq!(requests.len(), 2, "the rule must arrive on the next call");
    assert!(
        requests[0]
            .messages
            .iter()
            .all(|message| message.content.as_deref() != Some(NESTED_RULE)),
        "the nested rule cannot precede the path event",
    );

    let matching = requests[1]
        .messages
        .iter()
        .filter(|message| message.content.as_deref() == Some(NESTED_RULE))
        .collect::<Vec<_>>();
    assert_eq!(
        matching.len(),
        1,
        "the nested rule must appear exactly once"
    );
    assert_eq!(matching[0].role, MessageRole::User);
    assert!(requests[1].messages.iter().all(|message| {
        message.content.as_deref() != Some(NESTED_RULE)
            || !matches!(message.role, MessageRole::System | MessageRole::Developer)
    }));

    let persisted = store
        .events()
        .into_iter()
        .filter_map(|event| match event {
            SessionEvent::RuleInjection {
                rule_id,
                origin,
                content,
                ..
            } if content == NESTED_RULE => Some((rule_id, origin)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        persisted,
        [("norn-md:src/api".to_owned(), Some(RuleOrigin::Workspace))],
        "the one wire message must retain its durable workspace provenance",
    );

    let events = store.events();
    let replayed = crate::session::conversion::events_to_messages(&events);
    let replayed_rule = replayed
        .iter()
        .filter(|message| message.content.as_deref() == Some(NESTED_RULE))
        .collect::<Vec<_>>();
    assert_eq!(
        replayed_rule.len(),
        1,
        "resume projection must reconstruct the nested rule exactly once",
    );
    assert_eq!(replayed_rule[0].role, MessageRole::User);
    Ok(())
}
