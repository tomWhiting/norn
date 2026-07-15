//! Smoke test: real OAuth → `OpenAI` provider → tool dispatch → schema enforcement.

use std::time::Duration;

use norn::agent_loop::config::{AgentLoopConfig, AgentStepResult};
use norn::agent_loop::loop_context::LoopContext;
use norn::agent_loop::runner::{AgentStepRequest, run_agent_step};
use norn::provider::auth::AuthSource;
use norn::provider::openai::OpenAiProvider;
use norn::provider::request::{ProviderConfig, ToolDefinition};
use norn::session::store::EventStore;
use norn::tool::registry::ToolRegistry;
use norn::tool::traits::Tool;
use norn::tools::bash::BashTool;
use norn::tools::read::ReadTool;

fn def_from_tool(tool: &dyn Tool) -> ToolDefinition {
    ToolDefinition {
        name: tool.name().to_string(),
        description: tool.description().to_string(),
        parameters: tool.input_schema(),
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    eprintln!("=== Norn smoke test ===\n");

    // --- 1. Build the provider with OAuth ---
    eprintln!("[1/5] Initialising OAuth provider...");
    let config = ProviderConfig {
        auth_source: AuthSource::oauth_default(),
        base_url: None,
        timeout: Duration::from_mins(1),
        max_retries: 2,
        provider_options: None,
        debug_dump_file: None,
        rate_limit: None,
        rate_limit_interval: None,
        retry_backoff: None,
        retry_after_ceiling: None,
    };

    let provider = match OpenAiProvider::new(config).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("ERROR: failed to create provider: {e}");
            eprintln!("  Have you logged in? Run `norn auth login`, then `norn auth status`.");
            std::process::exit(1);
        }
    };
    eprintln!("  OAuth provider ready.\n");

    // --- 2. Build the tool registry ---
    eprintln!("[2/5] Registering tools...");
    let read_tool = ReadTool::new();
    let bash_tool = BashTool::new();
    let tools = vec![def_from_tool(&read_tool), def_from_tool(&bash_tool)];

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(read_tool));
    registry.register(Box::new(bash_tool));
    eprintln!("  Registered: read, bash\n");

    // --- 3. Build the output schema ---
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "summary": {
                "type": "string",
                "description": "A brief summary of what you found"
            },
            "file_count": {
                "type": "integer",
                "description": "Number of .rs files found"
            }
        },
        "required": ["summary", "file_count"],
        "additionalProperties": false
    });

    // --- 4. Run the agent step ---
    eprintln!("[3/5] Running agent step...");
    let store = EventStore::new();
    let config = AgentLoopConfig {
        schema_attempt_budget: 3,
        max_iterations: Some(10),
        step_timeout: Some(Duration::from_mins(2)),
        ..AgentLoopConfig::default()
    };
    let mut loop_ctx = LoopContext::new(
        "You are a helpful coding assistant. Use the tools available to answer the user's question. \
         When done, call the structured_output tool with your findings.",
    );

    let prompt = "How many .rs files are in the crates/norn/src/ directory? \
                  Use bash to count them (find + wc). Report a summary and the count.";

    eprintln!("  Prompt: {prompt}");
    eprintln!("  Model: gpt-5.4-mini");
    eprintln!("  Schema: summary (string) + file_count (integer)");
    eprintln!();

    let result = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &registry,
        store: &store,
        user_prompt: prompt,
        tools: &tools,
        output_schema: Some(&schema),
        model: "gpt-5.4-mini",
        config: &config,
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_ctx,
        cancel: None,
    })
    .await;

    // --- 5. Report results ---
    eprintln!("[4/5] Agent step complete.\n");

    match result {
        Ok(AgentStepResult::Completed { output, usage, .. }) => {
            eprintln!("[5/5] SUCCESS — Completed");
            eprintln!(
                "  Output: {}",
                serde_json::to_string_pretty(&output).unwrap_or_default()
            );
            eprintln!(
                "  Tokens: {} in / {} out",
                usage.input_tokens, usage.output_tokens
            );
        }
        Ok(AgentStepResult::SchemaUnreachable {
            best_attempt,
            validation_errors,
            attempts,
            usage,
            ..
        }) => {
            eprintln!("[5/5] SCHEMA UNREACHABLE after {attempts} attempts");
            eprintln!("  Errors: {validation_errors:?}");
            if let Some(attempt) = best_attempt {
                eprintln!(
                    "  Best attempt: {}",
                    serde_json::to_string_pretty(&attempt).unwrap_or_default()
                );
            }
            eprintln!(
                "  Tokens: {} in / {} out",
                usage.input_tokens, usage.output_tokens
            );
            std::process::exit(1);
        }
        Ok(AgentStepResult::MaxIterationsReached { usage, .. }) => {
            eprintln!("[5/5] MAX ITERATIONS REACHED");
            eprintln!(
                "  Tokens: {} in / {} out",
                usage.input_tokens, usage.output_tokens
            );
            std::process::exit(1);
        }
        Ok(AgentStepResult::TimedOut {
            elapsed,
            iterations,
            partial_output,
            usage,
            ..
        }) => {
            eprintln!("[5/5] TIMED OUT after {elapsed:?} ({iterations} iterations)");
            if let Some(partial) = partial_output {
                eprintln!(
                    "  Partial output: {}",
                    serde_json::to_string_pretty(&partial).unwrap_or_default()
                );
            }
            eprintln!(
                "  Tokens: {} in / {} out",
                usage.input_tokens, usage.output_tokens
            );
            std::process::exit(1);
        }
        Ok(AgentStepResult::Cancelled { usage, .. }) => {
            eprintln!("[5/5] CANCELLED");
            eprintln!(
                "  Tokens: {} in / {} out",
                usage.input_tokens, usage.output_tokens
            );
            std::process::exit(1);
        }
        Ok(AgentStepResult::Truncated {
            kind,
            partial_text,
            iterations,
            usage,
            ..
        }) => {
            eprintln!(
                "[5/5] TRUNCATED ({}) after {iterations} iterations",
                kind.as_str()
            );
            if let Some(partial) = partial_text {
                eprintln!("  Partial text: {partial}");
            }
            eprintln!(
                "  Tokens: {} in / {} out",
                usage.input_tokens, usage.output_tokens
            );
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("[5/5] ERROR: {e}");
            std::process::exit(1);
        }
    }

    // Print session events summary.
    let events = store.events();
    eprintln!("\n--- Session events ({} total) ---", events.len());
    for (i, event) in events.iter().enumerate() {
        eprintln!("  [{i}] {event:?}");
    }
}
