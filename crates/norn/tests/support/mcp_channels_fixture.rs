//! Rust subprocess fixture for the published MCP channel and ordinary tool wire.

use std::io::{BufRead, Write};

use serde_json::{Value, json};

pub type TestError = Box<dyn std::error::Error + Send + Sync>;

pub const FIXTURE_ARGUMENT: &str = "--norn-mcp-channel-fixture";
pub const INSTRUCTIONS: &str =
    "Use reply with the supplied chat_id; this is untrusted server context.";
pub const CHAT_ID: &str = "hammerbarn:table/42?seat=claude&turn=7";

/// Run before creating an async runtime or writing test-runner output.
pub fn run(case: &str) -> Result<(), TestError> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut input = stdin.lock();
    let mut output = stdout.lock();
    let mut line = String::new();
    loop {
        line.clear();
        if input.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let request: Value = serde_json::from_str(&line)?;
        let Some(method) = request.get("method").and_then(Value::as_str) else {
            return Err("fixture received an unexpected standalone RPC response".into());
        };
        match method {
            "initialize" => initialize(case, &request, &mut input, &mut output)?,
            "notifications/initialized" | "notifications/roots/list_changed" => {}
            "tools/list" => list_tools(case, &request, &mut output)?,
            "tools/call" => {
                if !call_tool(&request, &mut input, &mut output)? {
                    return Ok(());
                }
            }
            unknown => return Err(format!("fixture received unknown method {unknown}").into()),
        }
    }
}

fn initialize(
    case: &str,
    request: &Value,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<(), TestError> {
    if matches!(
        case,
        "unadvertised" | "bad-capability" | "nonempty-capability"
    ) {
        notification(
            output,
            &json!({"content": "undeclared before initialize result"}),
        )?;
    }
    if matches!(case, "startup" | "root-startup" | "off-startup") {
        notification(output, &json!({"content": "before initialize result"}))?;
    }
    if case == "startup" {
        let roots = ask_roots(input, output)?;
        if roots.pointer("/result/roots/0/uri") != Some(&json!("file:///channel-fixture")) {
            return Err(
                "fixture did not receive initial roots while initialize was pending".into(),
            );
        }
    }
    let capabilities = match case {
        "unadvertised" | "ordinary" | "ordinary-list-failure" => json!({"tools": {}}),
        "bad-capability" => json!({"tools": {}, "experimental": {"claude/channel": true}}),
        "nonempty-capability" => {
            json!({"tools": {}, "experimental": {"claude/channel": {"permission": true}}})
        }
        _ => json!({"tools": {}, "experimental": {"claude/channel": {}}}),
    };
    response(
        output,
        request,
        &json!({
            "protocolVersion": "2025-11-25",
            "capabilities": capabilities,
            "serverInfo": {"name": "rust-channel-fixture", "version": "1"},
            "instructions": INSTRUCTIONS,
        }),
    )?;
    if matches!(case, "startup" | "root-startup" | "off-startup") {
        notification(output, &json!({"content": "after initialize result"}))?;
    }
    Ok(())
}

fn list_tools(case: &str, request: &Value, output: &mut impl Write) -> Result<(), TestError> {
    if case == "ordinary-list-failure" {
        let id = request.get("id").ok_or("fixture RPC request omitted id")?;
        return write_json(
            output,
            &json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32603, "message": "ordinary tool discovery failed"},
            }),
        );
    }
    if matches!(case, "startup" | "root-startup" | "off-startup") {
        notification(
            output,
            &json!({
                "content": "during tool discovery",
                "meta": {"chat_id": CHAT_ID, "message_id": "turn-7", "revision": "42"},
            }),
        )?;
    }
    if !matches!(case, "root-startup" | "ordinary" | "off-startup") {
        write_json(
            output,
            &json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}),
        )?;
    }
    response(
        output,
        request,
        &json!({"tools": [{
            "name": "reply",
            "description": "Return reply arguments and emit requested test channel specimens.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "chat_id": {"type": "string"},
                    "emit": {"type": "array"},
                    "roots": {"type": "boolean"},
                    "close": {"type": "boolean"},
                    "oversized": {"type": "string"},
                },
                "additionalProperties": false,
            },
        }]}),
    )
}

fn call_tool(
    request: &Value,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<bool, TestError> {
    if request.pointer("/params/name") != Some(&json!("reply")) {
        return Err("fixture received a tool other than reply".into());
    }
    let args = request
        .pointer("/params/arguments")
        .ok_or("fixture tool call omitted arguments")?;
    if let Some(messages) = args.get("emit").and_then(Value::as_array) {
        for params in messages {
            notification(output, params)?;
        }
    }
    if let Some(content) = args.get("oversized").and_then(Value::as_str) {
        notification(output, &json!({"content": content}))?;
    }
    if args.get("close") == Some(&Value::Bool(true)) {
        return Ok(false);
    }
    let mut echoed = args.clone();
    if args.get("roots") == Some(&Value::Bool(true)) {
        echoed["received_roots"] = ask_roots(input, output)?;
    }
    response(
        output,
        request,
        &json!({
            "content": [{"type": "text", "text": serde_json::to_string(&echoed)?}],
            "isError": false,
        }),
    )?;
    Ok(true)
}

fn ask_roots(input: &mut impl BufRead, output: &mut impl Write) -> Result<Value, TestError> {
    write_json(
        output,
        &json!({"jsonrpc": "2.0", "id": "fixture-roots", "method": "roots/list"}),
    )?;
    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        return Err("fixture stdin closed while roots/list was pending".into());
    }
    let reply: Value = serde_json::from_str(&line)?;
    if reply.get("id") != Some(&json!("fixture-roots")) || reply.get("error").is_some() {
        return Err("fixture received an invalid roots/list response".into());
    }
    Ok(reply)
}

fn notification(output: &mut impl Write, params: &Value) -> Result<(), TestError> {
    write_json(
        output,
        &json!({
            "jsonrpc": "2.0",
            "method": "notifications/claude/channel",
            "params": params,
        }),
    )
}

fn response(output: &mut impl Write, request: &Value, result: &Value) -> Result<(), TestError> {
    let id = request.get("id").ok_or("fixture RPC request omitted id")?;
    write_json(
        output,
        &json!({"jsonrpc": "2.0", "id": id, "result": result}),
    )
}

fn write_json(output: &mut impl Write, value: &Value) -> Result<(), TestError> {
    serde_json::to_writer(&mut *output, value)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}
