use super::*;

/// Read the catalog description `tool_search` reports for `web_search`
/// through the built agent's live tool context.
async fn catalog_web_search_description(agent: &Agent) -> String {
    use crate::tool::envelope::ToolEnvelope;

    let ctx = agent
        .registry
        .shared_context()
        .expect("registry exposes its shared tool context");
    let tool = agent
        .registry
        .get("tool_search")
        .expect("tool_search registered");
    let envelope = ToolEnvelope {
        tool_call_id: "surface-test".to_string(),
        tool_name: "tool_search".to_string(),
        model_args: serde_json::json!({"query": "", "max_results": 500}),
        metadata: Value::Null,
    };
    let out = tool
        .execute(&envelope, ctx.as_ref())
        .await
        .expect("tool_search runs through the built context");
    out.content["results"]
        .as_array()
        .expect("results array")
        .iter()
        .find(|result| result["name"] == "web_search")
        .expect("web_search entry present in catalog dump")["description"]
        .as_str()
        .expect("description is a string")
        .to_owned()
}

/// The same registry resolved against two capability sets — hosted web
/// search on and off — must flip all three projections of the resolved
/// tool surface: the system-prompt tools section, the `tool_search`
/// catalog, and the provider request definitions. The second build is a
/// resume-style rebuild (`.session(store)` from the first run), proving
/// a provider change between resumes re-resolves the whole surface and
/// nothing stale is carried over.
#[tokio::test]
async fn provider_capability_switch_flips_all_three_projections_across_resume() {
    use crate::provider::mock::MockProvider;
    use crate::provider::tools::{
        HostedToolDefinition, ProviderCapabilities, ProviderToolDefinition,
    };

    // --- Hosted-capable provider: every projection shows the hosted truth.
    let hosted_provider = Arc::new(MockProvider::with_capabilities(
        text_completion("first"),
        ProviderCapabilities {
            hosted_web_search: true,
            ..ProviderCapabilities::default()
        },
    ));
    let agent = AgentBuilder::new(Arc::clone(&hosted_provider) as Arc<dyn Provider>)
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .build()
        .expect("hosted build succeeds");

    let prompt = agent.loop_context.base_system_instruction();
    assert!(
        prompt.contains("not a callable function"),
        "hosted prompt must carry the provider truth for web_search",
    );
    assert!(
        !prompt.contains("Search the public web"),
        "the function-mode description must not survive hosted reframing",
    );

    let description = catalog_web_search_description(&agent).await;
    assert!(
        description.contains("not a callable function"),
        "hosted catalog entry must carry the provider truth: {description}",
    );

    let outcome = agent.run("go").await.expect("hosted run succeeds");
    let requests = hosted_provider.requests().expect("requests recorded");
    let wire = &requests[0].tools;
    assert!(
        wire.iter().any(|tool| matches!(
            tool,
            ProviderToolDefinition::Hosted(HostedToolDefinition::WebSearch(_))
        )),
        "hosted provider must receive the hosted web-search tool",
    );
    assert!(
        !wire.iter().any(|tool| matches!(
            tool,
            ProviderToolDefinition::Function(function) if function.name == "web_search"
        )),
        "the web_search function definition must not also be sent",
    );

    // --- Resume-style rebuild against a provider WITHOUT hosted search:
    // every projection flips back to the callable-function truth.
    let store = outcome
        .into_output()
        .event_store
        .expect("event store returned");
    let plain_provider = Arc::new(MockProvider::new(text_completion("second")));
    let agent = AgentBuilder::new(Arc::clone(&plain_provider) as Arc<dyn Provider>)
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .session(store)
        .build()
        .expect("resumed build succeeds");

    let prompt = agent.loop_context.base_system_instruction();
    assert!(
        prompt.contains("Search the public web"),
        "function-mode prompt must list web_search as a callable function",
    );
    assert!(
        !prompt.contains("not a callable function"),
        "no hosted framing may leak across the provider switch",
    );

    let description = catalog_web_search_description(&agent).await;
    assert!(
        description.contains("Search the public web"),
        "function-mode catalog entry must keep the function description: {description}",
    );
    assert!(!description.contains("not a callable function"));

    let outcome = agent.run("again").await.expect("resumed run succeeds");
    assert!(outcome.is_completed());
    let requests = plain_provider.requests().expect("requests recorded");
    let wire = &requests[0].tools;
    assert!(
        wire.iter().any(|tool| matches!(
            tool,
            ProviderToolDefinition::Function(function) if function.name == "web_search"
        )),
        "without the capability web_search is sent as a function tool",
    );
    assert!(
        !wire.iter().any(|tool| matches!(
            tool,
            ProviderToolDefinition::Hosted(HostedToolDefinition::WebSearch(_))
        )),
        "no hosted definition may be sent without the capability",
    );
}
