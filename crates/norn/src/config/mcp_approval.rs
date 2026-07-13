//! Remembered approval for shared-project MCP definitions.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::{McpConfigSource, ResolvedMcpServer};
use crate::error::ConfigError;
use crate::resource::PrivateLineLog;

const APPROVAL_LOG: &str = "project-approvals.jsonl";

/// Activation state shown to startup and `norn mcp list` surfaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpApprovalState {
    /// Direct operator scopes need no second approval.
    NotRequired,
    /// This exact shared-project definition has been approved.
    Approved,
    /// The project definition is new or changed and remains inactive.
    Pending,
}

/// Append-only remembered approval ledger under the user-level Norn root.
pub struct McpApprovalStore {
    log: PrivateLineLog,
}

impl McpApprovalStore {
    /// Open the standard `$NORN_HOME/mcp` approval ledger.
    pub fn open() -> Result<Self, ConfigError> {
        let root = super::paths::norn_dir().ok_or_else(|| ConfigError::InvalidConfig {
            reason: "cannot resolve the user-level Norn directory for MCP approvals".to_owned(),
        })?;
        Self::at_root(&root)
    }

    /// Open an approval ledger below an explicit absolute Norn root.
    pub fn at_root(root: &Path) -> Result<Self, ConfigError> {
        let path = root.join("mcp").join(APPROVAL_LOG);
        let log = PrivateLineLog::new(&path).map_err(|error| storage_error("open", &error))?;
        Ok(Self { log })
    }

    /// Resolve the effective server's approval state.
    pub fn state(
        &self,
        project_root: &Path,
        server: &ResolvedMcpServer,
    ) -> Result<McpApprovalState, ConfigError> {
        if server.source() != McpConfigSource::Project {
            return Ok(McpApprovalState::NotRequired);
        }
        let key = approval_key(project_root, server.name())?;
        let current = self.load()?;
        if current
            .get(&key)
            .is_some_and(|fingerprint| fingerprint == server.fingerprint().as_str())
        {
            Ok(McpApprovalState::Approved)
        } else {
            Ok(McpApprovalState::Pending)
        }
    }

    /// Remember approval for one effective shared-project definition.
    pub fn approve(
        &self,
        project_root: &Path,
        server: &ResolvedMcpServer,
    ) -> Result<(), ConfigError> {
        if server.source() != McpConfigSource::Project {
            return Err(ConfigError::InvalidConfig {
                reason: format!(
                    "mcp server '{}' is not supplied by shared project settings",
                    server.name(),
                ),
            });
        }
        self.append(&ApprovalEvent::Approve {
            project: project_id(project_root)?,
            server: server.name().to_owned(),
            fingerprint: server.fingerprint().as_str().to_owned(),
            approved_at_ms: now_ms()?,
        })
    }

    /// Revoke any remembered approval for a project/server name.
    pub fn revoke(&self, project_root: &Path, server: &str) -> Result<(), ConfigError> {
        self.append(&ApprovalEvent::Revoke {
            project: project_id(project_root)?,
            server: server.to_owned(),
            revoked_at_ms: now_ms()?,
        })
    }

    fn load(&self) -> Result<BTreeMap<ApprovalKey, String>, ConfigError> {
        let contents = match self.log.read_to_string() {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(error) => return Err(storage_error("read", &error)),
        };
        let mut current = BTreeMap::new();
        for (index, line) in contents.lines().enumerate() {
            let event: ApprovalEvent =
                serde_json::from_str(line).map_err(|error| ConfigError::InvalidConfig {
                    reason: format!(
                        "MCP approval ledger record {} is invalid: {error}",
                        index + 1,
                    ),
                })?;
            match event {
                ApprovalEvent::Approve {
                    project,
                    server,
                    fingerprint,
                    ..
                } => {
                    current.insert(ApprovalKey { project, server }, fingerprint);
                }
                ApprovalEvent::Revoke {
                    project, server, ..
                } => {
                    current.remove(&ApprovalKey { project, server });
                }
            }
        }
        Ok(current)
    }

    fn append(&self, event: &ApprovalEvent) -> Result<(), ConfigError> {
        let line = serde_json::to_string(&event).map_err(|error| ConfigError::InvalidConfig {
            reason: format!("failed to serialize MCP approval: {error}"),
        })?;
        self.log
            .append_line(&line)
            .map_err(|error| storage_error("write", &error))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ApprovalKey {
    project: String,
    server: String,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum ApprovalEvent {
    Approve {
        project: String,
        server: String,
        fingerprint: String,
        approved_at_ms: u64,
    },
    Revoke {
        project: String,
        server: String,
        revoked_at_ms: u64,
    },
}

fn approval_key(project_root: &Path, server: &str) -> Result<ApprovalKey, ConfigError> {
    Ok(ApprovalKey {
        project: project_id(project_root)?,
        server: server.to_owned(),
    })
}

fn project_id(project_root: &Path) -> Result<String, ConfigError> {
    let canonical = project_root
        .canonicalize()
        .map_err(|error| ConfigError::InvalidConfig {
            reason: format!("failed to canonicalize the MCP project root: {error}"),
        })?;
    canonical
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| ConfigError::InvalidConfig {
            reason: "MCP project roots must be valid UTF-8".to_owned(),
        })
}

fn now_ms() -> Result<u64, ConfigError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| ConfigError::InvalidConfig {
            reason: format!("system clock cannot timestamp MCP approval: {error}"),
        })
        .and_then(|duration| {
            u64::try_from(duration.as_millis()).map_err(|error| ConfigError::InvalidConfig {
                reason: format!("MCP approval timestamp is out of range: {error}"),
            })
        })
}

fn storage_error(operation: &str, error: &io::Error) -> ConfigError {
    ConfigError::InvalidConfig {
        reason: format!("failed to {operation} the MCP approval ledger: {error}"),
    }
}

impl std::fmt::Debug for McpApprovalStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpApprovalStore")
            .field("path", &self.log.path())
            .finish()
    }
}

#[cfg(test)]
#[path = "mcp_approval_tests.rs"]
mod tests;
