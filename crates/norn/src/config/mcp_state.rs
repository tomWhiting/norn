//! Stateful, reloadable MCP configuration without transport side effects.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::mcp::{fingerprint, validate_one};
use super::mcp_state_types::{
    EffectiveMcpServer, McpConfigLayer, McpConfigSnapshot, McpLayerEntry, McpPersistentChange,
    McpPersistentMutation, McpPersistentScope, McpServerInspection, McpSessionEntry,
};
use super::{McpServerSettings, NornSettings};
use crate::error::{ConfigError, NornError};

/// Complete MCP definitions keyed by logical server name.
pub type McpDefinitions = BTreeMap<String, McpServerSettings>;

/// Reloadable MCP configuration state retaining every source layer.
#[derive(Clone, Debug)]
pub struct McpConfigState {
    project_root: PathBuf,
    user: McpDefinitions,
    shared_project: McpDefinitions,
    workspace_local: McpDefinitions,
    private_local: McpDefinitions,
    cli: McpDefinitions,
    session: BTreeMap<String, McpSessionEntry>,
}

impl McpConfigState {
    /// Load all persistent layers while retaining the supplied CLI definitions.
    pub fn load(cwd: &Path, cli: McpDefinitions) -> Result<Self, NornError> {
        let project_root = cwd.canonicalize().map_err(|error| {
            NornError::Config(ConfigError::InvalidConfig {
                reason: format!("failed to resolve the MCP settings workspace root: {error}"),
            })
        })?;
        validate_definitions(&cli)?;
        let disk = load_disk_layers(&project_root)?;
        Ok(Self {
            project_root,
            user: disk.user,
            shared_project: disk.shared_project,
            workspace_local: disk.workspace_local,
            private_local: disk.private_local,
            cli,
            session: BTreeMap::new(),
        })
    }

    #[cfg(test)]
    pub(crate) fn from_layers(
        project_root: PathBuf,
        persistent: [McpDefinitions; 4],
        cli: McpDefinitions,
    ) -> Result<Self, ConfigError> {
        let [user, shared_project, workspace_local, private_local] = persistent;
        for definitions in [
            &user,
            &shared_project,
            &workspace_local,
            &private_local,
            &cli,
        ] {
            validate_definitions(definitions)?;
        }
        Ok(Self {
            project_root,
            user,
            shared_project,
            workspace_local,
            private_local,
            cli,
            session: BTreeMap::new(),
        })
    }

    /// Canonical project root used for project-scoped paths.
    #[must_use]
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    /// Raw complete definitions for a non-session layer.
    #[must_use]
    pub fn definitions(&self, layer: McpConfigLayer) -> Option<&McpDefinitions> {
        match layer {
            McpConfigLayer::User => Some(&self.user),
            McpConfigLayer::SharedProject => Some(&self.shared_project),
            McpConfigLayer::WorkspaceLocal => Some(&self.workspace_local),
            McpConfigLayer::PrivateLocal => Some(&self.private_local),
            McpConfigLayer::Cli => Some(&self.cli),
            McpConfigLayer::Session => None,
        }
    }

    /// Raw session definitions and tombstones.
    #[must_use]
    pub const fn session_entries(&self) -> &BTreeMap<String, McpSessionEntry> {
        &self.session
    }

    /// Replace only the CLI layer, preserving persistent and session layers.
    pub fn replace_cli(&mut self, cli: McpDefinitions) -> Result<bool, ConfigError> {
        validate_definitions(&cli)?;
        let changed = self.cli != cli;
        self.cli = cli;
        Ok(changed)
    }

    /// Reload only disk-backed layers, preserving CLI and session state.
    pub fn reload_disk(&mut self) -> Result<bool, NornError> {
        let disk = load_disk_layers(&self.project_root)?;
        let changed = self.user != disk.user
            || self.shared_project != disk.shared_project
            || self.workspace_local != disk.workspace_local
            || self.private_local != disk.private_local;
        self.user = disk.user;
        self.shared_project = disk.shared_project;
        self.workspace_local = disk.workspace_local;
        self.private_local = disk.private_local;
        Ok(changed)
    }

    /// Add or wholly replace one ephemeral session definition.
    pub fn session_add(
        &mut self,
        name: String,
        definition: McpServerSettings,
    ) -> Result<bool, ConfigError> {
        validate_one(&name, &definition)?;
        let value = McpSessionEntry::Definition(definition);
        let changed = self.session.get(&name) != Some(&value);
        self.session.insert(name, value);
        Ok(changed)
    }

    /// Remove only the session entry, revealing the next lower definition.
    pub fn session_remove(&mut self, name: &str) -> Result<bool, ConfigError> {
        validate_name_for_tombstone(name)?;
        Ok(self.session.remove(name).is_some())
    }

    /// Disable the current complete definition through a session tombstone.
    pub fn session_disable(&mut self, name: &str) -> Result<bool, ConfigError> {
        let value = match self.session.get(name) {
            Some(McpSessionEntry::Definition(definition)) => {
                McpSessionEntry::DisabledDefinition(definition.clone())
            }
            Some(McpSessionEntry::DisabledInherited | McpSessionEntry::DisabledDefinition(_)) => {
                return Ok(false);
            }
            None if self.highest_non_session(name).is_some() => McpSessionEntry::DisabledInherited,
            None => return Err(missing_server(name, "disable")),
        };
        let changed = self.session.get(name) != Some(&value);
        self.session.insert(name.to_owned(), value);
        Ok(changed)
    }

    /// Enable a definition, restoring a disabled session entry.
    pub fn session_enable(&mut self, name: &str) -> Result<bool, ConfigError> {
        let current = self.session.get(name).cloned();
        let mut definition = match current.as_ref() {
            Some(
                McpSessionEntry::Definition(definition)
                | McpSessionEntry::DisabledDefinition(definition),
            ) => definition.clone(),
            Some(McpSessionEntry::DisabledInherited) => {
                self.session.remove(name);
                return Ok(true);
            }
            None => return Err(missing_session_entry(name, "enable")),
        };
        definition.enabled = Some(true);
        validate_one(name, &definition)?;
        let value = McpSessionEntry::Definition(definition);
        let changed = current.as_ref() != Some(&value);
        self.session.insert(name.to_owned(), value);
        Ok(changed)
    }

    /// Inspect every layer for one name, including shadowed project entries.
    pub fn inspect(&self, name: &str) -> Result<McpServerInspection, ConfigError> {
        let mut chain = Vec::new();
        for layer in McpConfigLayer::PRECEDENCE {
            match layer {
                McpConfigLayer::Session => append_session_chain(&mut chain, self.session.get(name)),
                _ => {
                    if let Some(definition) = self
                        .definitions(layer)
                        .and_then(|definitions| definitions.get(name))
                    {
                        chain.push(McpLayerEntry::Definition {
                            layer,
                            definition: definition.clone(),
                        });
                    }
                }
            }
        }
        let effective = self.resolve_one(name)?;
        Ok(McpServerInspection::new(name.to_owned(), chain, effective))
    }

    /// Resolve an immutable whole-entry snapshot for a later runtime generation.
    pub fn snapshot(&self) -> Result<McpConfigSnapshot, ConfigError> {
        let mut names = BTreeSet::new();
        for definitions in [
            &self.user,
            &self.shared_project,
            &self.workspace_local,
            &self.private_local,
            &self.cli,
        ] {
            names.extend(definitions.keys().cloned());
        }
        names.extend(self.session.keys().cloned());
        let mut servers = BTreeMap::new();
        for name in names {
            if let Some(server) = self.resolve_one(&name)? {
                servers.insert(name, server);
            }
        }
        Ok(McpConfigSnapshot::new(servers))
    }

    /// Apply an MCP-only persistent mutation and reload disk-backed layers.
    pub fn persist(
        &mut self,
        scope: McpPersistentScope,
        mutation: &McpPersistentMutation,
    ) -> Result<McpPersistentChange, NornError> {
        let change = super::mcp_patch::persist_mcp_mutation(&self.project_root, scope, mutation)?;
        self.reload_disk()?;
        Ok(change)
    }

    fn resolve_one(&self, name: &str) -> Result<Option<EffectiveMcpServer>, ConfigError> {
        if let Some(session) = self.session.get(name) {
            return match session {
                McpSessionEntry::Definition(definition) => {
                    effective(name, McpConfigLayer::Session, definition).map(Some)
                }
                McpSessionEntry::DisabledDefinition(definition) => {
                    effective_disabled(name, definition).map(Some)
                }
                McpSessionEntry::DisabledInherited => self
                    .highest_non_session(name)
                    .map(|definition| effective_disabled(name, definition))
                    .transpose(),
            };
        }
        for layer in McpConfigLayer::PRECEDENCE.into_iter().rev().skip(1) {
            if let Some(definition) = self
                .definitions(layer)
                .and_then(|definitions| definitions.get(name))
            {
                return effective(name, layer, definition).map(Some);
            }
        }
        Ok(None)
    }

    fn highest_non_session(&self, name: &str) -> Option<&McpServerSettings> {
        McpConfigLayer::PRECEDENCE
            .into_iter()
            .rev()
            .skip(1)
            .find_map(|layer| self.definitions(layer).and_then(|values| values.get(name)))
    }
}

struct DiskLayers {
    user: McpDefinitions,
    shared_project: McpDefinitions,
    workspace_local: McpDefinitions,
    private_local: McpDefinitions,
}

fn load_disk_layers(project_root: &Path) -> Result<DiskLayers, NornError> {
    let layers = super::loader::load_settings_at_launch_root(project_root)?;
    super::validate_working_directory_authority(&layers.user, &layers.project, &layers.local)?;
    let private_local = super::mcp_local::load_project_local_mcp_settings(project_root)?;
    let disk = DiskLayers {
        user: definitions_from(layers.user),
        shared_project: definitions_from(layers.project),
        workspace_local: definitions_from(layers.local),
        private_local: definitions_from(private_local),
    };
    for definitions in [
        &disk.user,
        &disk.shared_project,
        &disk.workspace_local,
        &disk.private_local,
    ] {
        validate_definitions(definitions)?;
    }
    Ok(disk)
}

fn definitions_from(settings: NornSettings) -> McpDefinitions {
    settings.mcp_servers.unwrap_or_default()
}

fn validate_definitions(definitions: &McpDefinitions) -> Result<(), ConfigError> {
    for (name, definition) in definitions {
        validate_one(name, definition)?;
    }
    Ok(())
}

fn validate_name_for_tombstone(name: &str) -> Result<(), ConfigError> {
    validate_one(
        name,
        &McpServerSettings {
            enabled: Some(false),
            ..McpServerSettings::default()
        },
    )
}

fn effective(
    name: &str,
    source: McpConfigLayer,
    definition: &McpServerSettings,
) -> Result<EffectiveMcpServer, ConfigError> {
    validate_one(name, definition)?;
    Ok(EffectiveMcpServer::new(
        name.to_owned(),
        source,
        definition.clone(),
        fingerprint(name, definition)?,
    ))
}

fn effective_disabled(
    name: &str,
    definition: &McpServerSettings,
) -> Result<EffectiveMcpServer, ConfigError> {
    let mut disabled = definition.clone();
    disabled.enabled = Some(false);
    effective(name, McpConfigLayer::Session, &disabled)
}

fn append_session_chain(chain: &mut Vec<McpLayerEntry>, value: Option<&McpSessionEntry>) {
    match value {
        Some(McpSessionEntry::Definition(definition)) => chain.push(McpLayerEntry::Definition {
            layer: McpConfigLayer::Session,
            definition: definition.clone(),
        }),
        Some(McpSessionEntry::DisabledInherited) => {
            chain.push(McpLayerEntry::DisabledInherited);
        }
        Some(McpSessionEntry::DisabledDefinition(definition)) => {
            chain.push(McpLayerEntry::DisabledDefinition(definition.clone()));
        }
        None => {}
    }
}

fn missing_server(name: &str, operation: &str) -> ConfigError {
    ConfigError::InvalidConfig {
        reason: format!("cannot {operation} unknown mcp server '{name}'"),
    }
}

fn missing_session_entry(name: &str, operation: &str) -> ConfigError {
    ConfigError::InvalidConfig {
        reason: format!("cannot {operation} mcp server '{name}' without a session entry"),
    }
}

#[cfg(test)]
#[path = "mcp_state_tests.rs"]
mod tests;
