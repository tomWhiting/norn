//! Stdio transport for the MCP client.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

use super::mcp_client::{JsonRpcResponse, MCP_REQUEST_TIMEOUT, Transport};
use crate::error::IntegrationError;

pub(super) struct StdioTransport {
    state: Mutex<StdioState>,
}

struct StdioState {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    invalidated: bool,
    _permit: crate::resource::DescriptorPermit,
}

impl StdioTransport {
    pub(super) fn spawn(
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
        working_dir: Option<&Path>,
    ) -> Result<Self, IntegrationError> {
        let governor = crate::resource::DescriptorGovernor::global().map_err(|error| {
            IntegrationError::McpError {
                reason: format!("MCP descriptor admission unavailable: {error}"),
            }
        })?;
        let mut launch_permit = governor
            .try_acquire(crate::resource::TWO_PIPE_SPAWN_PEAK)
            .map_err(|error| IntegrationError::McpError {
                reason: format!("MCP descriptor admission failed: {error}"),
            })?;
        let retained_permit = launch_permit
            .split(2)
            .ok_or_else(|| IntegrationError::McpError {
                reason: "MCP launch admission did not contain two retained pipes".to_owned(),
            })?;
        let mut process = Command::new(command);
        process
            .args(args)
            .envs(env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if let Some(working_dir) = working_dir {
            process.current_dir(working_dir);
        }
        let mut child = process
            .spawn()
            .map_err(|error| IntegrationError::McpError {
                reason: format!("failed to spawn MCP server '{command}': {error}"),
            })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| IntegrationError::McpError {
                reason: "stdin handle missing on spawned MCP server".to_owned(),
            })?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| IntegrationError::McpError {
                reason: "stdout handle missing on spawned MCP server".to_owned(),
            })?;
        Ok(Self {
            state: Mutex::new(StdioState {
                child,
                stdin,
                stdout: BufReader::new(stdout),
                invalidated: false,
                _permit: retained_permit,
            }),
        })
    }

    async fn write(state: &mut StdioState, payload: &str) -> Result<(), IntegrationError> {
        if state.invalidated {
            return Err(IntegrationError::McpError {
                reason: "MCP stdio connection is no longer usable".to_owned(),
            });
        }
        state
            .stdin
            .write_all(payload.as_bytes())
            .await
            .map_err(|error| write_error(&error))?;
        state
            .stdin
            .write_all(b"\n")
            .await
            .map_err(|error| write_error(&error))?;
        state
            .stdin
            .flush()
            .await
            .map_err(|error| write_error(&error))
    }

    fn invalidate_locked(state: &mut StdioState) {
        state.invalidated = true;
        let _ = state.child.start_kill();
    }
}

struct ExchangeGuard<'a> {
    state: &'a mut StdioState,
    complete: bool,
}

impl<'a> ExchangeGuard<'a> {
    fn new(state: &'a mut StdioState) -> Self {
        Self {
            state,
            complete: false,
        }
    }

    async fn write(&mut self, payload: &str) -> Result<(), IntegrationError> {
        StdioTransport::write(self.state, payload).await
    }

    async fn read_message(&mut self) -> Result<serde_json::Value, IntegrationError> {
        let mut line = String::new();
        let read = self
            .state
            .stdout
            .read_line(&mut line)
            .await
            .map_err(|error| IntegrationError::McpError {
                reason: format!("MCP stdio read failed: {error}"),
            })?;
        if read == 0 {
            return Err(IntegrationError::McpError {
                reason: "MCP server closed stdout".to_owned(),
            });
        }
        serde_json::from_str(line.trim()).map_err(|error| IntegrationError::McpError {
            reason: format!("invalid JSON-RPC response: {error}"),
        })
    }

    async fn handle_server_message(
        &mut self,
        message: &serde_json::Value,
    ) -> Result<bool, IntegrationError> {
        let Some(method) = message.get("method").and_then(serde_json::Value::as_str) else {
            return Ok(false);
        };
        let Some(id) = message.get("id") else {
            return Ok(true);
        };
        let response = if method == "ping" {
            serde_json::json!({"jsonrpc": "2.0", "id": id, "result": {}})
        } else {
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": "method not supported by norn"}
            })
        };
        let payload =
            serde_json::to_string(&response).map_err(|error| IntegrationError::McpError {
                reason: format!("failed to serialize MCP client response: {error}"),
            })?;
        self.write(&payload).await?;
        Ok(true)
    }

    fn finish(&mut self) {
        self.complete = true;
    }
}

impl Drop for ExchangeGuard<'_> {
    fn drop(&mut self) {
        if !self.complete {
            StdioTransport::invalidate_locked(self.state);
        }
    }
}

fn write_error(error: &std::io::Error) -> IntegrationError {
    IntegrationError::McpError {
        reason: format!("MCP stdio write failed: {error}"),
    }
}

#[async_trait]
impl Transport for StdioTransport {
    async fn request(
        &self,
        payload: String,
        request_id: u64,
    ) -> Result<JsonRpcResponse, IntegrationError> {
        let exchange = async {
            let mut state = self.state.lock().await;
            let mut exchange = ExchangeGuard::new(&mut state);
            exchange.write(&payload).await?;
            loop {
                let message = exchange.read_message().await?;
                if exchange.handle_server_message(&message).await? {
                    continue;
                }
                let response: JsonRpcResponse =
                    serde_json::from_value(message).map_err(|error| {
                        IntegrationError::McpError {
                            reason: format!("invalid JSON-RPC response: {error}"),
                        }
                    })?;
                if response.id.as_ref() != Some(&serde_json::json!(request_id)) {
                    return Err(IntegrationError::McpError {
                        reason: format!("JSON-RPC response id did not match request {request_id}"),
                    });
                }
                exchange.finish();
                return Ok(response);
            }
        };
        tokio::time::timeout(MCP_REQUEST_TIMEOUT, exchange)
            .await
            .map_err(|_elapsed| IntegrationError::McpError {
                reason: "MCP request timed out; the stdio connection was closed".to_owned(),
            })?
    }

    async fn notify(&self, payload: String) -> Result<(), IntegrationError> {
        let mut state = self.state.lock().await;
        Self::write(&mut state, &payload).await
    }

    fn supports_protocol_version(&self, version: &str) -> bool {
        matches!(
            version,
            "2025-11-25" | "2025-06-18" | "2025-03-26" | "2024-11-05"
        )
    }

    async fn invalidate(&self) {
        let mut state = self.state.lock().await;
        Self::invalidate_locked(&mut state);
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.try_lock() {
            let _ = state.child.start_kill();
        }
    }
}

#[cfg(test)]
#[path = "mcp_stdio_tests.rs"]
mod tests;
