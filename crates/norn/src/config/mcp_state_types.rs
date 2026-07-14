//! Types for provenance-preserving live MCP configuration state.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::{McpDefinitionFingerprint, McpServerSettings};

/// One independently inspectable MCP configuration layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum McpConfigLayer {
    /// User settings under the trusted Norn home.
    User,
    /// Shared project settings in `.norn/settings.json`.
    SharedProject,
    /// Workspace-local settings in `.norn/settings.local.json`.
    WorkspaceLocal,
    /// User-owned project-local settings under the trusted Norn home.
    PrivateLocal,
    /// Explicit command-line definitions.
    Cli,
    /// Ephemeral changes owned by the running session.
    Session,
}

impl McpConfigLayer {
    /// Layers in increasing precedence order.
    pub const PRECEDENCE: [Self; 6] = [
        Self::User,
        Self::SharedProject,
        Self::WorkspaceLocal,
        Self::PrivateLocal,
        Self::Cli,
        Self::Session,
    ];

    /// Whether this layer is controlled by project files.
    #[must_use]
    pub const fn is_project_controlled(self) -> bool {
        matches!(self, Self::SharedProject | Self::WorkspaceLocal)
    }
}

/// A session-layer value, including control tombstones.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpSessionEntry {
    /// A complete session-owned server definition.
    Definition(McpServerSettings),
    /// A disable tombstone over the current, dynamically resolved lower winner.
    DisabledInherited,
    /// A disabled session-owned definition retained for later re-enablement.
    DisabledDefinition(McpServerSettings),
}

/// One entry in a server's complete provenance chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpLayerEntry {
    /// A complete server definition at this layer.
    Definition {
        /// The layer containing the definition.
        layer: McpConfigLayer,
        /// The unmerged definition.
        definition: McpServerSettings,
    },
    /// A session tombstone over the dynamically resolved lower winner.
    DisabledInherited,
    /// A session tombstone retaining a session-owned definition.
    DisabledDefinition(McpServerSettings),
}

impl McpLayerEntry {
    /// Source layer of this chain entry.
    #[must_use]
    pub const fn layer(&self) -> McpConfigLayer {
        match self {
            Self::Definition { layer, .. } => *layer,
            Self::DisabledInherited | Self::DisabledDefinition(_) => McpConfigLayer::Session,
        }
    }
}

/// One effective, whole-entry MCP server definition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectiveMcpServer {
    name: String,
    source: McpConfigLayer,
    definition: McpServerSettings,
    fingerprint: McpDefinitionFingerprint,
}

impl EffectiveMcpServer {
    pub(crate) fn new(
        name: String,
        source: McpConfigLayer,
        definition: McpServerSettings,
        fingerprint: McpDefinitionFingerprint,
    ) -> Self {
        Self {
            name,
            source,
            definition,
            fingerprint,
        }
    }

    /// Logical configured server name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Winning source layer.
    #[must_use]
    pub const fn source(&self) -> McpConfigLayer {
        self.source
    }

    /// Complete winning definition.
    #[must_use]
    pub const fn definition(&self) -> &McpServerSettings {
        &self.definition
    }

    /// Whether this server is enabled.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.definition.enabled.unwrap_or(true)
    }

    /// Stable fingerprint of the winning definition.
    #[must_use]
    pub const fn fingerprint(&self) -> &McpDefinitionFingerprint {
        &self.fingerprint
    }
}

/// Complete resolution details for one logical server name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpServerInspection {
    name: String,
    chain: Vec<McpLayerEntry>,
    effective: Option<EffectiveMcpServer>,
}

impl McpServerInspection {
    pub(crate) fn new(
        name: String,
        chain: Vec<McpLayerEntry>,
        effective: Option<EffectiveMcpServer>,
    ) -> Self {
        Self {
            name,
            chain,
            effective,
        }
    }

    /// Logical server name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Every defined layer, in increasing precedence order.
    #[must_use]
    pub fn chain(&self) -> &[McpLayerEntry] {
        &self.chain
    }

    /// Effective definition, absent when no layer defines this name.
    #[must_use]
    pub const fn effective(&self) -> Option<&EffectiveMcpServer> {
        self.effective.as_ref()
    }
}

/// Persistent settings scope that may receive an MCP-only patch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpPersistentScope {
    /// Trusted user settings.
    User,
    /// Shared project settings.
    SharedProject,
    /// Workspace-local project settings.
    WorkspaceLocal,
    /// Trusted private project-local settings.
    PrivateLocal,
}

impl McpPersistentScope {
    /// Whether a persisted definition remains subject to project approval.
    #[must_use]
    pub const fn requires_project_approval(self) -> bool {
        matches!(self, Self::SharedProject | Self::WorkspaceLocal)
    }
}

/// One MCP-only persistent document mutation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpPersistentMutation {
    /// Insert or wholly replace a named definition.
    Upsert {
        /// Logical server name.
        name: String,
        /// Complete replacement definition.
        definition: McpServerSettings,
    },
    /// Remove a named definition from this persistent layer.
    Remove {
        /// Logical server name.
        name: String,
    },
    /// Set only the named definition's `enabled` field.
    SetEnabled {
        /// Logical server name.
        name: String,
        /// New enabled state.
        enabled: bool,
    },
}

/// Result of an MCP-only persistent mutation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpPersistentChange {
    scope: McpPersistentScope,
    path: PathBuf,
    changed: bool,
}

impl McpPersistentChange {
    pub(crate) fn new(scope: McpPersistentScope, path: PathBuf, changed: bool) -> Self {
        Self {
            scope,
            path,
            changed,
        }
    }

    /// Mutated persistent scope.
    #[must_use]
    pub const fn scope(&self) -> McpPersistentScope {
        self.scope
    }

    /// Settings document that received the patch.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether serialized document content changed.
    #[must_use]
    pub const fn changed(&self) -> bool {
        self.changed
    }

    /// Whether the resulting layer remains subject to explicit approval.
    #[must_use]
    pub const fn requires_project_approval(&self) -> bool {
        self.scope.requires_project_approval()
    }
}

/// Immutable effective server map produced from one state generation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct McpConfigSnapshot {
    servers: BTreeMap<String, EffectiveMcpServer>,
}

impl McpConfigSnapshot {
    pub(crate) fn new(servers: BTreeMap<String, EffectiveMcpServer>) -> Self {
        Self { servers }
    }

    /// Effective servers in deterministic name order.
    pub fn iter(&self) -> impl Iterator<Item = &EffectiveMcpServer> {
        self.servers.values()
    }

    /// Look up one effective server.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&EffectiveMcpServer> {
        self.servers.get(name)
    }

    /// Number of effective names.
    #[must_use]
    pub fn len(&self) -> usize {
        self.servers.len()
    }

    /// Whether the snapshot has no effective names.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }
}
