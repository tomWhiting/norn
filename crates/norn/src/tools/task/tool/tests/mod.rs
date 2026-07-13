use serde_json::json;

use super::*;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::traits::Tool;
use crate::tools::task::TaskStatus;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn envelope_for(args: Value) -> ToolEnvelope {
    ToolEnvelope {
        tool_call_id: "call-1".to_string(),
        tool_name: "task".to_string(),
        model_args: args,
        metadata: Value::Null,
    }
}

fn ctx_with_store() -> (ToolContext, Arc<InMemoryTaskStore>) {
    let store = Arc::new(InMemoryTaskStore::new());
    let ctx = ToolContext::empty();
    let shared = Arc::new(SharedTaskStore(Arc::clone(&store) as Arc<dyn TaskStore>));
    ctx.insert_extension(shared);
    (ctx, store)
}

fn as_tool(tool: &TaskTool) -> &dyn Tool {
    tool
}

async fn execute(tool: &TaskTool, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
    as_tool(tool).execute(&envelope_for(args), ctx).await
}

fn output_error(output: &ToolOutput) -> Result<&ToolErrorPayload, std::io::Error> {
    output
        .error()
        .ok_or_else(|| std::io::Error::other("tool output did not contain an error payload"))
}

fn json_array<'a>(value: &'a Value, label: &str) -> Result<&'a Vec<Value>, std::io::Error> {
    value
        .as_array()
        .ok_or_else(|| std::io::Error::other(format!("{label} was not an array")))
}

fn json_string<'a>(value: &'a Value, label: &str) -> Result<&'a str, std::io::Error> {
    value
        .as_str()
        .ok_or_else(|| std::io::Error::other(format!("{label} was not a string")))
}

// -- Composite derivation -------------------------------------------

mod behavior;
mod schema;
