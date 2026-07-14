//! Stdio transport for the MCP client.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard};

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, oneshot};

use super::mcp_client::{JsonRpcResponse, MCP_REQUEST_TIMEOUT, Transport};
use super::mcp_protocol::{ClientProtocolState, InboundMessage};
use crate::error::IntegrationError;

pub(super) struct StdioTransport {
    shared: Arc<StdioShared>,
    reader: tokio::task::AbortHandle,
}

struct StdioShared {
    child: StdMutex<Child>,
    stdin: Mutex<ChildStdin>,
    pending: StdMutex<HashMap<u64, PendingResponse>>,
    invalidated: AtomicBool,
    protocol: Arc<ClientProtocolState>,
    _permit: crate::resource::DescriptorPermit,
}

type PendingResponse = oneshot::Sender<Result<JsonRpcResponse, String>>;

impl StdioTransport {
    pub(super) fn spawn(
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
        working_dir: Option<&Path>,
        protocol: Arc<ClientProtocolState>,
    ) -> Result<Self, IntegrationError> {
        let governor = crate::resource::DescriptorGovernor::global().map_err(|error| {
            IntegrationError::McpError {
                reason: format!("MCP descriptor admission unavailable: {error}"),
            }
        })?;
        Self::spawn_with_governor(command, args, env, working_dir, protocol, &governor)
    }

    fn spawn_with_governor(
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
        working_dir: Option<&Path>,
        protocol: Arc<ClientProtocolState>,
        governor: &crate::resource::DescriptorGovernor,
    ) -> Result<Self, IntegrationError> {
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
        let shared = Arc::new(StdioShared {
            child: StdMutex::new(child),
            stdin: Mutex::new(stdin),
            pending: StdMutex::new(HashMap::new()),
            invalidated: AtomicBool::new(false),
            protocol,
            _permit: retained_permit,
        });
        let task_shared = Arc::clone(&shared);
        let reader = tokio::spawn(async move {
            read_pump(task_shared, BufReader::new(stdout)).await;
        })
        .abort_handle();
        Ok(Self { shared, reader })
    }
}

impl StdioShared {
    fn pending(&self) -> Result<MutexGuard<'_, HashMap<u64, PendingResponse>>, IntegrationError> {
        self.pending.lock().map_err(|_poisoned| channel_error())
    }

    fn child(&self) -> Result<MutexGuard<'_, Child>, IntegrationError> {
        self.child.lock().map_err(|_poisoned| channel_error())
    }

    fn register_request(
        &self,
        request_id: u64,
    ) -> Result<oneshot::Receiver<Result<JsonRpcResponse, String>>, IntegrationError> {
        let (sender, receiver) = oneshot::channel();
        match self.pending()?.entry(request_id) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(sender);
                Ok(receiver)
            }
            std::collections::hash_map::Entry::Occupied(_entry) => {
                Err(IntegrationError::McpError {
                    reason: "MCP stdio request id was already active".to_owned(),
                })
            }
        }
    }

    async fn write(&self, payload: &str) -> Result<(), IntegrationError> {
        if self.invalidated.load(Ordering::Acquire) {
            return Err(channel_error());
        }
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(payload.as_bytes())
            .await
            .map_err(|error| write_error(&error))?;
        stdin
            .write_all(b"\n")
            .await
            .map_err(|error| write_error(&error))?;
        stdin.flush().await.map_err(|error| write_error(&error))
    }

    fn invalidate(&self, reason: &str) {
        if self.invalidated.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Ok(mut child) = self.child() {
            let _kill_result = child.start_kill();
        }
        if let Ok(mut pending) = self.pending() {
            for (_id, sender) in pending.drain() {
                let _send_result = sender.send(Err(reason.to_owned()));
            }
        }
    }
}

struct RequestGuard {
    shared: Arc<StdioShared>,
    request_id: u64,
    complete: bool,
}

impl RequestGuard {
    fn new(shared: Arc<StdioShared>, request_id: u64) -> Self {
        Self {
            shared,
            request_id,
            complete: false,
        }
    }

    fn finish(&mut self) {
        self.complete = true;
    }
}

impl Drop for RequestGuard {
    fn drop(&mut self) {
        if self.complete {
            return;
        }
        if let Ok(mut pending) = self.shared.pending() {
            pending.remove(&self.request_id);
        }
        self.shared
            .invalidate("MCP stdio request was cancelled before its response");
    }
}

async fn read_pump(shared: Arc<StdioShared>, mut stdout: BufReader<tokio::process::ChildStdout>) {
    let result = read_messages(&shared, &mut stdout).await;
    let reason = match result {
        Ok(()) => "MCP server closed stdout".to_owned(),
        Err(error) => error.to_string(),
    };
    shared.invalidate(&reason);
}

async fn read_messages(
    shared: &StdioShared,
    stdout: &mut BufReader<tokio::process::ChildStdout>,
) -> Result<(), IntegrationError> {
    loop {
        let mut line = String::new();
        let read =
            stdout
                .read_line(&mut line)
                .await
                .map_err(|error| IntegrationError::McpError {
                    reason: format!("MCP stdio read failed: {error}"),
                })?;
        if read == 0 {
            return Ok(());
        }
        let message: serde_json::Value =
            serde_json::from_str(line.trim()).map_err(|error| IntegrationError::McpError {
                reason: format!("invalid JSON-RPC response: {error}"),
            })?;
        match shared.protocol.inspect(&message)? {
            InboundMessage::Consumed => {}
            InboundMessage::Reply(reply) => {
                let payload =
                    serde_json::to_string(&reply).map_err(|error| IntegrationError::McpError {
                        reason: format!("failed to serialize MCP client response: {error}"),
                    })?;
                shared.write(&payload).await?;
            }
            InboundMessage::Response => route_response(shared, message)?,
        }
    }
}

fn route_response(
    shared: &StdioShared,
    message: serde_json::Value,
) -> Result<(), IntegrationError> {
    let response: JsonRpcResponse =
        serde_json::from_value(message).map_err(|error| IntegrationError::McpError {
            reason: format!("invalid JSON-RPC response: {error}"),
        })?;
    let request_id = response
        .id
        .as_ref()
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| IntegrationError::McpError {
            reason: "MCP stdio response did not contain a numeric request id".to_owned(),
        })?;
    let sender =
        shared
            .pending()?
            .remove(&request_id)
            .ok_or_else(|| IntegrationError::McpError {
                reason: "MCP stdio response did not match an active request".to_owned(),
            })?;
    sender
        .send(Ok(response))
        .map_err(|_response| IntegrationError::McpError {
            reason: "MCP stdio response receiver was unavailable".to_owned(),
        })
}

fn channel_error() -> IntegrationError {
    IntegrationError::McpError {
        reason: "MCP stdio connection is no longer usable".to_owned(),
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
        if self.shared.invalidated.load(Ordering::Acquire) {
            return Err(channel_error());
        }
        let receiver = self.shared.register_request(request_id)?;
        let mut guard = RequestGuard::new(Arc::clone(&self.shared), request_id);
        self.shared.write(&payload).await?;
        let response = tokio::time::timeout(MCP_REQUEST_TIMEOUT, receiver)
            .await
            .map_err(|_elapsed| IntegrationError::McpError {
                reason: "MCP request timed out; the stdio connection was closed".to_owned(),
            })?
            .map_err(|_closed| channel_error())?
            .map_err(|reason| IntegrationError::McpError { reason })?;
        guard.finish();
        Ok(response)
    }

    async fn notify(&self, payload: String) -> Result<(), IntegrationError> {
        self.shared.write(&payload).await
    }

    fn supports_protocol_version(&self, version: &str) -> bool {
        matches!(
            version,
            "2025-11-25" | "2025-06-18" | "2025-03-26" | "2024-11-05"
        )
    }

    async fn invalidate(&self) {
        self.shared
            .invalidate("MCP stdio connection was invalidated");
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        self.reader.abort();
        self.shared.invalidate("MCP stdio connection was closed");
    }
}

#[cfg(test)]
#[path = "mcp_stdio_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "mcp_stdio_protocol_tests.rs"]
mod protocol_tests;
