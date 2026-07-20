use super::*;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

// -- Provider tool surface: wire and prompt recomputed per request
//    from the live provider's capabilities --------------------------

fn web_search_tool_def() -> ToolDefinition {
    ToolDefinition {
        name: "web_search".to_string(),
        description: "Search the public web.".to_string(),
        parameters: serde_json::json!({"type": "object"}),
    }
}

#[tokio::test]
async fn hosted_capability_swaps_wire_tool_and_injects_surface_section() -> TestResult {
    use crate::provider::tools::{
        HostedToolDefinition, ProviderCapabilities, ProviderToolDefinition,
    };

    let provider = MockProvider::with_capabilities(
        vec![vec![text_delta("done"), done_event(StopReason::EndTurn)]],
        ProviderCapabilities {
            hosted_web_search: true,
            ..ProviderCapabilities::default()
        },
    );
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();

    let result = run_step(
        &provider,
        &executor,
        &store,
        &[read_file_tool_def(), web_search_tool_def()],
        None,
        &default_config(),
        None,
    )
    .await;
    assert_completed(result);

    let requests = provider.requests()?;
    let request = requests
        .first()
        .ok_or_else(|| std::io::Error::other("provider recorded no request"))?;
    assert!(
        matches!(
            request.tools.as_slice(),
            [
                ProviderToolDefinition::Function(read),
                ProviderToolDefinition::Hosted(HostedToolDefinition::WebSearch(_)),
            ] if read.name == "read_file"
        ),
        "hosted-capable provider must receive the hosted replacement: {:?}",
        request.tools,
    );
    // The per-iteration surface note rides on the dynamic-context
    // Developer message, never on the cache-stable System message.
    assert!(
        request.messages.iter().any(|m| {
            m.role == MessageRole::Developer
                && m.content
                    .as_deref()
                    .is_some_and(|c| c.contains("# Provider Tool Surface"))
        }),
        "the hosted surface note must reach the request's developer context",
    );
    assert!(
        !request.messages.iter().any(|m| {
            m.role == MessageRole::System
                && m.content
                    .as_deref()
                    .is_some_and(|c| c.contains("# Provider Tool Surface"))
        }),
        "the surface note is dynamic — the System message stays cache-stable",
    );
    Ok(())
}

#[tokio::test]
async fn function_capability_keeps_wire_tool_and_omits_surface_section() -> TestResult {
    use crate::provider::tools::ProviderToolDefinition;

    let provider = MockProvider::new(vec![vec![
        text_delta("done"),
        done_event(StopReason::EndTurn),
    ]]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();

    let result = run_step(
        &provider,
        &executor,
        &store,
        &[read_file_tool_def(), web_search_tool_def()],
        None,
        &default_config(),
        None,
    )
    .await;
    assert_completed(result);

    let requests = provider.requests()?;
    let request = requests
        .first()
        .ok_or_else(|| std::io::Error::other("provider recorded no request"))?;
    assert!(
        request
            .tools
            .iter()
            .all(|tool| matches!(tool, ProviderToolDefinition::Function(_))),
        "without the capability every tool is a callable function: {:?}",
        request.tools,
    );
    assert!(
        request.tools.iter().any(|tool| matches!(
            tool,
            ProviderToolDefinition::Function(function) if function.name == "web_search"
        )),
        "web_search stays on the wire as a function tool",
    );
    assert!(
        !request.messages.iter().any(|m| {
            m.content
                .as_deref()
                .is_some_and(|c| c.contains("# Provider Tool Surface"))
        }),
        "function mode needs no surface correction",
    );
    Ok(())
}
