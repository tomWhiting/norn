//! Provenance of rule content loaded into the runtime.

use serde::{Deserialize, Serialize};

use crate::system_prompt::authority::PromptSource;

/// Authority-bearing origin of a rule.
///
/// This metadata is derived by the API or discovery boundary and deliberately
/// does not live on [`Rule`](crate::rules::types::Rule), so rule-file content
/// cannot select its own authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleOrigin {
    /// A rule supplied through a trusted programmatic or user-level surface.
    Operator,
    /// A rule discovered inside the active workspace.
    Workspace,
}

impl RuleOrigin {
    /// Derive provenance from the scanner's exact source-directory index.
    #[must_use]
    pub(crate) fn from_discovery_directory(
        directory_index: usize,
        workspace_directory_indexes: &[usize],
    ) -> Self {
        if workspace_directory_indexes.contains(&directory_index) {
            Self::Workspace
        } else {
            Self::Operator
        }
    }

    /// Convert provenance to the shared prompt-source authority model.
    #[must_use]
    pub const fn prompt_source(self) -> PromptSource {
        match self {
            Self::Operator => PromptSource::OperatorRule,
            Self::Workspace => PromptSource::WorkspaceRule,
        }
    }
}
