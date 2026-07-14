//! Provenance-preserving MCP configuration resolution.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::loader::{LoadedSettings, load_settings_at_launch_root};
use super::{
    McpServerSettings, NornSettings, merge_settings, validate_settings,
    validate_working_directory_authority,
};
use crate::error::{ConfigError, NornError};
use crate::integration::{McpClientConfig, McpTransport};

/// Settings source that supplied the effective server definition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpConfigSource {
    /// User-level `~/.norn/settings.json`.
    User,
    /// Shared project `.norn/settings.json`.
    Project,
    /// Private project settings under the user-owned Norn directory.
    Local,
    /// Explicit command-line definition.
    Cli,
    /// Ephemeral live-session definition.
    Session,
}

/// Stable identity of one normalized operational server definition.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct McpDefinitionFingerprint(pub(crate) String);

impl McpDefinitionFingerprint {
    /// Lowercase SHA-256 digest with a versioned domain separator.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One effective server with its winning configuration source attached.
#[derive(Clone)]
pub struct ResolvedMcpServer {
    pub(crate) name: String,
    pub(crate) source: McpConfigSource,
    pub(crate) definition: McpServerSettings,
    pub(crate) fingerprint: McpDefinitionFingerprint,
}

impl ResolvedMcpServer {
    /// Logical server name used to qualify discovered tools.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Winning settings layer.
    #[must_use]
    pub const fn source(&self) -> McpConfigSource {
        self.source
    }

    /// Whether this effective definition should be connected.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.definition.enabled.unwrap_or(true)
    }

    /// Normalized definition identity used by remembered project approval.
    #[must_use]
    pub const fn fingerprint(&self) -> &McpDefinitionFingerprint {
        &self.fingerprint
    }

    /// Effective typed definition. Its `Debug` implementation is redacted.
    #[must_use]
    pub const fn definition(&self) -> &McpServerSettings {
        &self.definition
    }

    /// Convert this resolved entry into a transport config. Disabled entries
    /// return `None` and remain visible to status/listing surfaces.
    pub fn client_config(
        &self,
        working_dir: &Path,
    ) -> Result<Option<McpClientConfig>, ConfigError> {
        if !self.enabled() {
            return Ok(None);
        }
        let transport = if let Some(command) = self.definition.command.as_ref() {
            McpTransport::Stdio {
                command: command.clone(),
                args: self.definition.args.clone().unwrap_or_default(),
            }
        } else if let Some(url) = self.definition.url.as_ref() {
            McpTransport::Http { url: url.clone() }
        } else {
            return Err(ConfigError::InvalidConfig {
                reason: format!("mcp server '{}' has no active transport", self.name),
            });
        };
        Ok(Some(McpClientConfig {
            name: self.name.clone(),
            transport,
            env: map_to_hash(self.definition.env.as_ref()),
            headers: map_to_hash(self.definition.headers.as_ref()),
            working_dir: Some(working_dir.to_path_buf()),
        }))
    }
}

impl fmt::Debug for ResolvedMcpServer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedMcpServer")
            .field("name", &self.name)
            .field("source", &self.source)
            .field("definition", &self.definition)
            .field("fingerprint", &self.fingerprint)
            .finish()
    }
}

/// Effective server map after all five scopes are overlaid by name.
#[derive(Clone, Debug, Default)]
pub struct ResolvedMcpServers {
    servers: BTreeMap<String, ResolvedMcpServer>,
}

impl ResolvedMcpServers {
    /// Iterate in deterministic server-name order, including disabled entries.
    pub fn iter(&self) -> impl Iterator<Item = &ResolvedMcpServer> {
        self.servers.values()
    }

    /// Look up one effective server.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&ResolvedMcpServer> {
        self.servers.get(name)
    }

    /// Number of effective names, including disabled entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.servers.len()
    }

    /// Whether no server name is configured at any scope.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }

    fn definitions(&self) -> Option<BTreeMap<String, McpServerSettings>> {
        (!self.servers.is_empty()).then(|| {
            self.servers
                .iter()
                .map(|(name, server)| (name.clone(), server.definition.clone()))
                .collect()
        })
    }
}

/// Direct runtime scopes overlaid above on-disk settings.
#[derive(Clone, Debug, Default)]
pub struct McpRuntimeOverrides {
    /// Explicit command-line definitions.
    pub cli: BTreeMap<String, McpServerSettings>,
    /// Ephemeral definitions changed during the current session.
    pub session: BTreeMap<String, McpServerSettings>,
}

/// Merged settings plus provenance-preserving MCP resolution.
#[derive(Clone, Debug)]
pub struct ResolvedSettings {
    /// Canonical project root used for project-scoped approvals and roots.
    pub project_root: PathBuf,
    /// Effective general settings, including the effective MCP map.
    pub settings: NornSettings,
    /// Effective MCP entries with winning-source provenance.
    pub mcp_servers: ResolvedMcpServers,
}

/// Load, validate, merge, and resolve settings for one canonical project.
pub fn load_resolved_settings(
    cwd: &Path,
    overrides: &McpRuntimeOverrides,
) -> Result<ResolvedSettings, NornError> {
    let project_root = cwd.canonicalize().map_err(|error| {
        NornError::Config(ConfigError::InvalidConfig {
            reason: format!("failed to resolve the settings workspace root: {error}"),
        })
    })?;
    load_resolved_settings_at_launch_root(&project_root, overrides)
}

pub(crate) fn load_resolved_settings_at_launch_root(
    project_root: &Path,
    overrides: &McpRuntimeOverrides,
) -> Result<ResolvedSettings, NornError> {
    let mut layers = load_settings_at_launch_root(project_root)?;
    validate_working_directory_authority(&layers.user, &layers.project, &layers.local)?;
    let private_local = super::mcp_local::load_project_local_mcp_settings(project_root)?;
    let mcp_servers = resolve_layers(&layers, &private_local, overrides)?;
    let mut cli_layer = NornSettings {
        mcp_servers: (!overrides.cli.is_empty()).then(|| overrides.cli.clone()),
        ..NornSettings::default()
    };
    let mut merged = merge_settings(
        &mut layers.user,
        &mut layers.project,
        &mut layers.local,
        &mut cli_layer,
    );
    merged.mcp_servers = mcp_servers.definitions();
    validate_settings(&merged)?;
    Ok(ResolvedSettings {
        project_root: project_root.to_path_buf(),
        settings: merged,
        mcp_servers,
    })
}

fn resolve_layers(
    layers: &LoadedSettings,
    private_local: &NornSettings,
    overrides: &McpRuntimeOverrides,
) -> Result<ResolvedMcpServers, ConfigError> {
    let mut effective = BTreeMap::new();
    overlay(
        &mut effective,
        layers.user.mcp_servers.as_ref(),
        McpConfigSource::User,
    )?;
    overlay(
        &mut effective,
        layers.project.mcp_servers.as_ref(),
        McpConfigSource::Project,
    )?;
    overlay(
        &mut effective,
        layers.local.mcp_servers.as_ref(),
        McpConfigSource::Project,
    )?;
    overlay(
        &mut effective,
        private_local.mcp_servers.as_ref(),
        McpConfigSource::Local,
    )?;
    overlay_map(&mut effective, &overrides.cli, McpConfigSource::Cli)?;
    overlay_map(&mut effective, &overrides.session, McpConfigSource::Session)?;
    Ok(ResolvedMcpServers { servers: effective })
}

fn overlay(
    effective: &mut BTreeMap<String, ResolvedMcpServer>,
    definitions: Option<&BTreeMap<String, McpServerSettings>>,
    source: McpConfigSource,
) -> Result<(), ConfigError> {
    if let Some(definitions) = definitions {
        overlay_map(effective, definitions, source)?;
    }
    Ok(())
}

fn overlay_map(
    effective: &mut BTreeMap<String, ResolvedMcpServer>,
    definitions: &BTreeMap<String, McpServerSettings>,
    source: McpConfigSource,
) -> Result<(), ConfigError> {
    for (name, definition) in definitions {
        validate_one(name, definition)?;
        effective.insert(
            name.clone(),
            ResolvedMcpServer {
                name: name.clone(),
                source,
                fingerprint: fingerprint(name, definition)?,
                definition: definition.clone(),
            },
        );
    }
    Ok(())
}

fn map_to_hash(values: Option<&BTreeMap<String, String>>) -> HashMap<String, String> {
    values
        .into_iter()
        .flat_map(BTreeMap::iter)
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

pub(crate) fn validate_one(name: &str, definition: &McpServerSettings) -> Result<(), ConfigError> {
    let settings = NornSettings {
        mcp_servers: Some(BTreeMap::from([(name.to_owned(), definition.clone())])),
        ..NornSettings::default()
    };
    validate_settings(&settings)
}

#[derive(Serialize)]
struct NormalizedDefinition<'a> {
    domain: &'static str,
    name: &'a str,
    enabled: bool,
    transport: &'static str,
    command: Option<&'a str>,
    args: &'a [String],
    url: Option<String>,
    env: &'a BTreeMap<String, String>,
    headers: BTreeMap<String, &'a str>,
}

pub(crate) fn fingerprint(
    name: &str,
    definition: &McpServerSettings,
) -> Result<McpDefinitionFingerprint, ConfigError> {
    let empty_args = Vec::new();
    let empty_map = BTreeMap::new();
    let transport = match definition.transport.as_deref() {
        Some("stdio") => "stdio",
        Some("http") => "http",
        None if definition.command.is_some() => "stdio",
        None if definition.url.is_some() => "http",
        _ if definition.enabled == Some(false) => "disabled",
        _ => "invalid",
    };
    let url = definition
        .url
        .as_deref()
        .map(url::Url::parse)
        .transpose()
        .map_err(|_parse_error| ConfigError::InvalidConfig {
            reason: format!("mcp server '{name}' has an invalid URL"),
        })?
        .map(|parsed| parsed.to_string());
    let headers = definition
        .headers
        .as_ref()
        .unwrap_or(&empty_map)
        .iter()
        .map(|(key, value)| (key.to_ascii_lowercase(), value.as_str()))
        .collect();
    let normalized = NormalizedDefinition {
        domain: "norn-mcp-definition-v1",
        name,
        enabled: definition.enabled.unwrap_or(true),
        transport,
        command: definition.command.as_deref(),
        args: definition.args.as_deref().unwrap_or(&empty_args),
        url,
        env: definition.env.as_ref().unwrap_or(&empty_map),
        headers,
    };
    let encoded = serde_json::to_vec(&normalized).map_err(|error| ConfigError::InvalidConfig {
        reason: format!("failed to normalize mcp server '{name}': {error}"),
    })?;
    let digest = Sha256::digest(encoded);
    Ok(McpDefinitionFingerprint(format!("{digest:x}")))
}

#[cfg(test)]
#[path = "mcp_tests.rs"]
mod tests;
