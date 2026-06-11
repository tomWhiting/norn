//! Interactive chat: no schema, no tools — just text in, text out via `OpenAI`.

use std::io::{self, BufRead, Write as _};
use std::time::Duration;

use serde_json::Value;

use norn::error::ToolError;
use norn::r#loop::config::{AgentLoopConfig, AgentStepResult, ToolExecutor};
use norn::r#loop::loop_context::LoopContext;
use norn::r#loop::runner::{AgentStepRequest, run_agent_step};
use norn::provider::auth::AuthSource;
use norn::provider::openai::OpenAiProvider;
use norn::provider::request::ProviderConfig;
use norn::session::store::EventStore;

struct NoTools;

#[async_trait::async_trait]
impl ToolExecutor for NoTools {
    async fn execute(
        &self,
        name: &str,
        call_id: &str,
        _arguments: Value,
    ) -> Result<Value, ToolError> {
        let _ = call_id;
        Err(ToolError::ToolNotFound {
            name: name.to_string(),
        })
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

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
            eprintln!("Failed to create provider: {e}");
            eprintln!("Run `cargo run --example login -p norn` first.");
            std::process::exit(1);
        }
    };

    let executor = NoTools;
    let loop_config = AgentLoopConfig {
        step_timeout: Some(Duration::from_mins(2)),
        ..AgentLoopConfig::default()
    };

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    eprintln!("=== Norn chat (no-schema mode, gpt-4.1-mini) ===");
    eprintln!("Type a message and press Enter. Ctrl-C to quit.\n");

    loop {
        print!("> ");
        stdout.flush().ok();

        let mut line = String::new();
        if stdin.lock().read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        let prompt = line.trim();
        if prompt.is_empty() {
            continue;
        }

        let store = EventStore::new();
        let mut loop_ctx = LoopContext::new("You are a helpful assistant. Be concise.");

        let result = run_agent_step(AgentStepRequest {
            provider: &provider,
            executor: &executor,
            store: &store,
            user_prompt: prompt,
            tools: &[],
            output_schema: None,
            model: "gpt-4.1-mini",
            config: &loop_config,
            event_tx: None,
            inbound: None,
            loop_context: &mut loop_ctx,
            cancel: None,
        })
        .await;

        match result {
            Ok(AgentStepResult::Completed { output, usage }) => {
                let fallback = output.to_string();
                let text = output.as_str().unwrap_or(&fallback);
                println!("\n{text}\n");
                eprintln!(
                    "  [{} in / {} out tokens]\n",
                    usage.input_tokens, usage.output_tokens
                );
            }
            Ok(other) => {
                eprintln!("  Unexpected result: {other:?}\n");
            }
            Err(e) => {
                eprintln!("  Error: {e}\n");
            }
        }
    }
}
