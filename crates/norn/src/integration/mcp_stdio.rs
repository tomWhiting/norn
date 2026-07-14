//! Stdio transport for the MCP client.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard};

use async_trait::async_trait;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, oneshot};

use super::mcp_client::{JsonRpcResponse, Transport};
use super::mcp_protocol::{ClientProtocolState, InboundMessage};
use super::mcp_transport_bounds::{read_bounded_stdio_line, with_request_timeout};
use crate::error::IntegrationError;

#[path = "mcp_stdio_request.rs"]
mod request_support;
use request_support::RequestGuard;

#[path = "mcp_stdio_stderr.rs"]
mod stderr_support;
use stderr_support::{StderrObservation, StderrSummary, drain_stderr};

pub(super) struct StdioTransport {
    shared: Arc<StdioShared>,
}

struct StdioShared {
    child: StdMutex<Child>,
    stdin: Mutex<ChildStdin>,
    pending: StdMutex<HashMap<u64, PendingResponse>>,
    pumps: StdMutex<PumpHandles>,
    invalidated: AtomicBool,
    protocol: Arc<ClientProtocolState>,
    max_inbound_message_bytes: usize,
    request_timeout_ms: Option<u64>,
    stderr_observation: Arc<StderrObservation>,
    _permit: crate::resource::DescriptorPermit,
}

#[derive(Default)]
struct PumpHandles {
    stdout: Option<tokio::task::AbortHandle>,
    stderr: Option<tokio::task::AbortHandle>,
}

type PendingResponse = oneshot::Sender<Result<JsonRpcResponse, String>>;

#[derive(Clone, Copy)]
struct StdioConnectionOptions {
    max_inbound_message_bytes: usize,
    request_timeout_ms: Option<u64>,
}

impl StdioConnectionOptions {
    const fn new(max_inbound_message_bytes: usize, request_timeout_ms: Option<u64>) -> Self {
        Self {
            max_inbound_message_bytes,
            request_timeout_ms,
        }
    }
}

impl StdioTransport {
    pub(super) fn spawn(
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
        working_dir: Option<&Path>,
        protocol: Arc<ClientProtocolState>,
        max_inbound_message_bytes: usize,
        request_timeout_ms: Option<u64>,
    ) -> Result<Self, IntegrationError> {
        let governor = crate::resource::DescriptorGovernor::global().map_err(|error| {
            IntegrationError::McpError {
                reason: format!("MCP descriptor admission unavailable: {error}"),
            }
        })?;
        Self::spawn_with_governor(
            command,
            args,
            env,
            working_dir,
            protocol,
            StdioConnectionOptions::new(max_inbound_message_bytes, request_timeout_ms),
            &governor,
        )
    }

    fn spawn_with_governor(
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
        working_dir: Option<&Path>,
        protocol: Arc<ClientProtocolState>,
        options: StdioConnectionOptions,
        governor: &crate::resource::DescriptorGovernor,
    ) -> Result<Self, IntegrationError> {
        let mut launch_permit = governor
            .try_acquire(crate::resource::THREE_PIPE_SPAWN_PEAK)
            .map_err(|error| IntegrationError::McpError {
                reason: format!("MCP descriptor admission failed: {error}"),
            })?;
        let mut retained_permit = launch_permit
            .split(crate::resource::THREE_PIPE_RETAINED)
            .ok_or_else(|| IntegrationError::McpError {
                reason: "MCP launch admission did not contain three retained pipes".to_owned(),
            })?;
        let stderr_permit = retained_permit
            .split(1)
            .ok_or_else(|| IntegrationError::McpError {
                reason: "MCP launch admission could not isolate the stderr pipe".to_owned(),
            })?;
        let mut process = Command::new(command);
        process
            .args(args)
            .envs(env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
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
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| IntegrationError::McpError {
                reason: "stderr handle missing on spawned MCP server".to_owned(),
            })?;
        let child_id = child.id();
        let stderr_observation = Arc::new(StderrObservation::default());
        let shared = Arc::new(StdioShared {
            child: StdMutex::new(child),
            stdin: Mutex::new(stdin),
            pending: StdMutex::new(HashMap::new()),
            pumps: StdMutex::new(PumpHandles::default()),
            invalidated: AtomicBool::new(false),
            protocol,
            max_inbound_message_bytes: options.max_inbound_message_bytes,
            request_timeout_ms: options.request_timeout_ms,
            stderr_observation: Arc::clone(&stderr_observation),
            _permit: retained_permit,
        });
        let task_shared = Arc::clone(&shared);
        let stdout_reader = tokio::spawn(async move {
            read_pump(task_shared, BufReader::new(stdout)).await;
        })
        .abort_handle();
        let stderr_reader = tokio::spawn(drain_stderr(
            BufReader::new(stderr),
            stderr_permit,
            child_id,
            stderr_observation,
        ))
        .abort_handle();
        shared.install_pumps(stdout_reader, stderr_reader)?;
        Ok(Self { shared })
    }
}

impl StdioShared {
    fn pending(&self) -> Result<MutexGuard<'_, HashMap<u64, PendingResponse>>, IntegrationError> {
        self.pending
            .lock()
            .map_err(|_poisoned| self.channel_error())
    }

    fn child(&self) -> Result<MutexGuard<'_, Child>, IntegrationError> {
        self.child.lock().map_err(|_poisoned| self.channel_error())
    }

    fn install_pumps(
        &self,
        stdout: tokio::task::AbortHandle,
        stderr: tokio::task::AbortHandle,
    ) -> Result<(), IntegrationError> {
        let Ok(mut pumps) = self.pumps.lock() else {
            self.stderr_observation.interrupt();
            stdout.abort();
            stderr.abort();
            return Err(self.channel_error());
        };
        if self.invalidated.load(Ordering::Acquire) {
            self.stderr_observation.interrupt();
            stdout.abort();
            stderr.abort();
            return Err(self.channel_error());
        }
        pumps.stdout = Some(stdout);
        pumps.stderr = Some(stderr);
        Ok(())
    }

    fn abort_pumps(&self) {
        self.stderr_observation.interrupt();
        let mut pumps = match self.pumps.lock() {
            Ok(pumps) => pumps,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(stdout) = pumps.stdout.take() {
            stdout.abort();
        }
        if let Some(stderr) = pumps.stderr.take() {
            stderr.abort();
        }
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
            return Err(self.channel_error());
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
        let was_invalidated = self.invalidated.swap(true, Ordering::AcqRel);
        self.abort_pumps();
        if was_invalidated {
            return;
        }
        let stderr_summary = self.stderr_observation.snapshot();
        let failure = format!("{reason}; {stderr_summary}");
        trace_invalidation(stderr_summary);
        if let Ok(mut child) = self.child() {
            let _kill_result = child.start_kill();
        }
        if let Ok(mut pending) = self.pending() {
            for (_id, sender) in pending.drain() {
                let _send_result = sender.send(Err(failure.clone()));
            }
        }
    }

    fn channel_error(&self) -> IntegrationError {
        IntegrationError::McpError {
            reason: format!(
                "MCP stdio connection is no longer usable; {}",
                self.stderr_observation.snapshot()
            ),
        }
    }
}

fn trace_invalidation(stderr_summary: StderrSummary) {
    tracing::debug!(summary = %stderr_summary, "MCP stdio connection invalidated");
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
    let mut line = Vec::new();
    loop {
        if !read_bounded_stdio_line(stdout, &mut line, shared.max_inbound_message_bytes).await? {
            return Ok(());
        }
        let message: serde_json::Value =
            serde_json::from_slice(&line).map_err(|error| IntegrationError::McpError {
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
            return Err(self.shared.channel_error());
        }
        let receiver = self.shared.register_request(request_id)?;
        let mut guard = RequestGuard::new(Arc::clone(&self.shared), request_id);
        let exchange = async {
            self.shared.write(&payload).await?;
            receiver
                .await
                .map_err(|_closed| self.shared.channel_error())?
                .map_err(|reason| IntegrationError::McpError { reason })
        };
        let response =
            with_request_timeout("stdio", self.shared.request_timeout_ms, exchange).await?;
        guard.finish();
        Ok(response)
    }

    async fn notify(&self, payload: String) -> Result<(), IntegrationError> {
        with_request_timeout(
            "stdio",
            self.shared.request_timeout_ms,
            self.shared.write(&payload),
        )
        .await
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

    fn is_live(&self) -> bool {
        !self.shared.invalidated.load(Ordering::Acquire)
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        self.shared.invalidate("MCP stdio connection was closed");
    }
}

#[cfg(test)]
#[path = "mcp_stdio_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "mcp_stdio_protocol_tests.rs"]
mod protocol_tests;
