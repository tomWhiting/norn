//! Extension system — Meridian extension protocol bridge.
//!
//! Extensions are external processes that publish tool definitions through
//! a manifest. Norn loads a manifest, optionally `connect()`s to the
//! extension (handshake by transport), and registers every advertised tool
//! as an [`ExtensionProxyTool`] that forwards calls to the remote.
//!
//! Two transports are supported:
//!
//! - **HTTP**: each tool call is a POST to `<base_url>/tools/<name>` with
//!   the model arguments as the JSON body.
//! - **Stdio**: a JSON request `{ "tool": <name>, "arguments": <args> }` is
//!   written to the subprocess stdin and a single JSON line is read back.

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

use crate::error::{IntegrationError, ToolError};
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::{Tool, ToolOutput};

const EXTENSION_TIMEOUT: Duration = Duration::from_secs(30);

/// Transport an extension is reached over.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExtTransport {
    /// HTTP base URL.
    Http {
        /// Base URL — tool calls hit `<base_url>/tools/<name>`.
        base_url: String,
    },
    /// Subprocess command line, executed via `sh -c`.
    Stdio {
        /// Command line to spawn.
        command: String,
    },
}

/// Tool definition declared in an extension manifest.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ExtToolDef {
    /// Tool name as it will appear in Norn's registry.
    pub name: String,
    /// Description shown to the model.
    pub description: String,
    /// JSON Schema for tool arguments.
    pub input_schema: serde_json::Value,
}

/// Top-level extension manifest. Mirrors the shape of Meridian extension
/// protocol manifests, but only the fields Norn consumes are captured.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ExtensionManifest {
    /// Logical name of the extension.
    pub name: String,
    /// Manifest schema version (free-form string).
    pub version: String,
    /// Tools the extension publishes.
    pub tools: Vec<ExtToolDef>,
    /// Transport used to reach the extension.
    pub transport: ExtTransport,
}

/// Registry of loaded extensions. Constructed empty and populated via
/// [`ExtensionRegistry::load_manifest`] / [`ExtensionRegistry::connect`].
/// Sharing across agents happens through `Arc<ExtensionRegistry>`.
pub struct ExtensionRegistry {
    extensions: Mutex<Vec<Arc<ConnectedExtension>>>,
    /// Per-agent working directory used when spawning stdio extensions.
    /// When unset, extensions inherit the process CWD (legacy behaviour).
    working_dir: Option<crate::tool::context::SharedWorkingDir>,
}

struct ConnectedExtension {
    manifest: ExtensionManifest,
    state: ExtensionState,
}

enum ExtensionState {
    Http {
        client: reqwest::Client,
    },
    Stdio {
        // Wrapped in async Mutex so calls serialise on the single stdin/out.
        // Boxed to reduce enum size — StdioIo is much larger than the Http
        // variant's reqwest::Client handle.
        io: Box<Mutex<StdioIo>>,
    },
}

struct StdioIo {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Drop for StdioIo {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

impl ExtensionRegistry {
    /// Construct an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            extensions: Mutex::new(Vec::new()),
            working_dir: None,
        }
    }

    /// Install the agent's shared working directory. Stdio extensions
    /// connected after this call will spawn with this as their child's
    /// CWD.
    #[must_use]
    pub fn with_working_dir(mut self, working_dir: crate::tool::context::SharedWorkingDir) -> Self {
        self.working_dir = Some(working_dir);
        self
    }

    /// Load a manifest from a JSON file. The manifest is validated for
    /// well-formedness only — connection is performed by
    /// [`Self::connect`].
    ///
    /// # Errors
    ///
    /// Returns [`IntegrationError`] when the file is unreadable or the
    /// manifest fails to parse.
    pub fn load_manifest(path: &Path) -> Result<ExtensionManifest, IntegrationError> {
        let text = std::fs::read_to_string(path).map_err(|e| IntegrationError::McpError {
            reason: format!("failed to read manifest at {}: {e}", path.display()),
        })?;
        serde_json::from_str(&text).map_err(|e| IntegrationError::McpError {
            reason: format!("invalid manifest at {}: {e}", path.display()),
        })
    }

    /// Connect to the extension described by `manifest`. For HTTP, this
    /// builds a reqwest client; for stdio, it spawns the subprocess. The
    /// returned `Arc<ExtensionRegistry>` retains ownership of the
    /// connection.
    ///
    /// # Errors
    ///
    /// Returns [`IntegrationError`] when the transport setup fails.
    pub async fn connect(&self, manifest: ExtensionManifest) -> Result<(), IntegrationError> {
        let state = match &manifest.transport {
            ExtTransport::Http { .. } => {
                let client = reqwest::Client::builder()
                    .timeout(EXTENSION_TIMEOUT)
                    .build()
                    .map_err(|e| IntegrationError::McpError {
                        reason: format!("failed to build HTTP client: {e}"),
                    })?;
                ExtensionState::Http { client }
            }
            ExtTransport::Stdio { command } => {
                let mut cmd = Command::new("sh");
                cmd.arg("-c")
                    .arg(command)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .kill_on_drop(true);
                if let Some(ref wd) = self.working_dir {
                    cmd.current_dir(wd.get());
                }
                let mut child = cmd.spawn().map_err(|e| IntegrationError::McpError {
                    reason: format!("failed to spawn extension '{}': {e}", manifest.name),
                })?;
                let stdin = child
                    .stdin
                    .take()
                    .ok_or_else(|| IntegrationError::McpError {
                        reason: "extension stdin handle missing".to_owned(),
                    })?;
                let stdout = child
                    .stdout
                    .take()
                    .ok_or_else(|| IntegrationError::McpError {
                        reason: "extension stdout handle missing".to_owned(),
                    })?;
                ExtensionState::Stdio {
                    io: Box::new(Mutex::new(StdioIo {
                        child,
                        stdin,
                        stdout: BufReader::new(stdout),
                    })),
                }
            }
        };

        let extension = Arc::new(ConnectedExtension { manifest, state });
        self.extensions.lock().await.push(extension);
        Ok(())
    }

    /// Number of connected extensions.
    pub async fn len(&self) -> usize {
        self.extensions.lock().await.len()
    }

    /// True when no extensions are connected.
    pub async fn is_empty(&self) -> bool {
        self.extensions.lock().await.is_empty()
    }

    /// Build [`ExtensionProxyTool`] instances for every advertised tool of
    /// every connected extension. The list is ordered by extension
    /// connection order.
    pub async fn proxy_tools(self: &Arc<Self>) -> Vec<ExtensionProxyTool> {
        let mut tools = Vec::new();
        let extensions = self.extensions.lock().await.clone();
        for ext in extensions {
            for def in &ext.manifest.tools {
                tools.push(ExtensionProxyTool {
                    def: def.clone(),
                    extension: Arc::clone(&ext),
                });
            }
        }
        tools
    }
}

impl Default for ExtensionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Tool implementation that forwards `execute` calls to a remote extension.
pub struct ExtensionProxyTool {
    def: ExtToolDef,
    extension: Arc<ConnectedExtension>,
}

#[async_trait]
impl Tool for ExtensionProxyTool {
    fn name(&self) -> &str {
        &self.def.name
    }

    fn description(&self) -> &str {
        &self.def.description
    }

    fn input_schema(&self) -> serde_json::Value {
        self.def.input_schema.clone()
    }

    fn effect(&self) -> ToolEffect {
        match self.extension.state {
            ExtensionState::Http { .. } => ToolEffect::Network,
            ExtensionState::Stdio { .. } => ToolEffect::Process,
        }
    }

    async fn execute(
        &self,
        envelope: &ToolEnvelope,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let response = match &self.extension.state {
            ExtensionState::Http { client } => {
                let base = match &self.extension.manifest.transport {
                    ExtTransport::Http { base_url } => base_url.trim_end_matches('/').to_owned(),
                    ExtTransport::Stdio { .. } => {
                        return Err(ToolError::ExecutionFailed {
                            reason: "transport/state mismatch".to_owned(),
                        });
                    }
                };
                let url = format!("{}/tools/{}", base, self.def.name);
                client
                    .post(url)
                    .header("Content-Type", "application/json")
                    .json(&envelope.model_args)
                    .send()
                    .await
                    .map_err(|e| ToolError::ExecutionFailed {
                        reason: format!("extension HTTP call failed: {e}"),
                    })?
                    .text()
                    .await
                    .map_err(|e| ToolError::ExecutionFailed {
                        reason: format!("extension HTTP read failed: {e}"),
                    })?
            }
            ExtensionState::Stdio { io } => {
                let payload = serde_json::json!({
                    "tool": self.def.name,
                    "arguments": envelope.model_args,
                });
                let payload_str =
                    serde_json::to_string(&payload).map_err(|e| ToolError::ExecutionFailed {
                        reason: format!("serialize extension payload: {e}"),
                    })?;

                let mut guard = io.lock().await;
                guard
                    .stdin
                    .write_all(payload_str.as_bytes())
                    .await
                    .map_err(|e| ToolError::ExecutionFailed {
                        reason: format!("extension stdin write: {e}"),
                    })?;
                guard
                    .stdin
                    .write_all(b"\n")
                    .await
                    .map_err(|e| ToolError::ExecutionFailed {
                        reason: format!("extension stdin write: {e}"),
                    })?;
                guard
                    .stdin
                    .flush()
                    .await
                    .map_err(|e| ToolError::ExecutionFailed {
                        reason: format!("extension stdin flush: {e}"),
                    })?;
                let mut line = String::new();
                guard.stdout.read_line(&mut line).await.map_err(|e| {
                    ToolError::ExecutionFailed {
                        reason: format!("extension stdout read: {e}"),
                    }
                })?;
                line
            }
        };

        let value: serde_json::Value = serde_json::from_str(response.trim())
            .unwrap_or_else(|_| serde_json::json!({ "text": response.trim() }));
        Ok(ToolOutput::success(value))
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::no_effect_underscore_binding,
    clippy::useless_vec,
    clippy::missing_const_for_fn,
    clippy::duration_suboptimal_units,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::unnecessary_trailing_comma,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants
)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn load_manifest_parses_well_formed_json() {
        let tmp = NamedTempFile::new().unwrap();
        let manifest_json = serde_json::json!({
            "name": "demo",
            "version": "0.1.0",
            "tools": [
                {
                    "name": "hello",
                    "description": "say hi",
                    "input_schema": {"type": "object"}
                }
            ],
            "transport": {"type": "http", "base_url": "http://localhost:7777"}
        })
        .to_string();
        std::fs::write(tmp.path(), &manifest_json).unwrap();

        let manifest = ExtensionRegistry::load_manifest(tmp.path()).unwrap();
        assert_eq!(manifest.name, "demo");
        assert_eq!(manifest.tools.len(), 1);
        assert_eq!(manifest.tools[0].name, "hello");
        match manifest.transport {
            ExtTransport::Http { base_url } => assert_eq!(base_url, "http://localhost:7777"),
            ExtTransport::Stdio { .. } => panic!("expected http transport"),
        }
    }

    #[test]
    fn load_manifest_rejects_invalid_json() {
        let tmp = NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "not json").unwrap();
        let err = ExtensionRegistry::load_manifest(tmp.path()).unwrap_err();
        match err {
            IntegrationError::McpError { reason } => assert!(reason.contains("invalid manifest")),
            other => panic!("expected McpError, got {other:?}"),
        }
    }

    // R7 acceptance: create a mock manifest, load it, verify tools discoverable.
    #[tokio::test]
    async fn discover_tools_from_manifest() {
        let manifest = ExtensionManifest {
            name: "demo".to_owned(),
            version: "0.1.0".to_owned(),
            tools: vec![
                ExtToolDef {
                    name: "alpha".to_owned(),
                    description: "first".to_owned(),
                    input_schema: serde_json::json!({"type": "object"}),
                },
                ExtToolDef {
                    name: "beta".to_owned(),
                    description: "second".to_owned(),
                    input_schema: serde_json::json!({"type": "object"}),
                },
            ],
            transport: ExtTransport::Http {
                base_url: "http://localhost:7777".to_owned(),
            },
        };

        let registry = Arc::new(ExtensionRegistry::new());
        registry.connect(manifest).await.unwrap();
        assert_eq!(registry.len().await, 1);
        let tools = registry.proxy_tools().await;
        assert_eq!(tools.len(), 2);
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(names.contains(&"alpha"));
        assert!(names.contains(&"beta"));
    }

    #[tokio::test]
    async fn extension_proxy_tool_implements_tool() {
        // Build a real ExtensionProxyTool via the registry and coerce it.
        let manifest = ExtensionManifest {
            name: "demo".to_owned(),
            version: "0.1.0".to_owned(),
            tools: vec![ExtToolDef {
                name: "alpha".to_owned(),
                description: "first".to_owned(),
                input_schema: serde_json::json!({"type": "object"}),
            }],
            transport: ExtTransport::Http {
                base_url: "http://localhost:0".to_owned(),
            },
        };
        let registry = Arc::new(ExtensionRegistry::new());
        registry.connect(manifest).await.unwrap();
        let tools = registry.proxy_tools().await;
        let _boxed: Box<dyn Tool + Send + Sync> = Box::new(tools.into_iter().next().unwrap());
    }
}
