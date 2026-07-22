//! Prompt authority derived from the provenance of each instruction fragment.

use crate::provider::request::MessageRole;

/// Provider-neutral authority carried by a prompt fragment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PromptAuthority {
    /// Product-owned policy sent with the provider's highest authority.
    System,
    /// Trusted operator or runtime guidance below product policy.
    Developer,
    /// Repository-controlled or human-authored input.
    User,
}

impl PromptAuthority {
    /// Stable discriminator used by prompt-plan fingerprints and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Developer => "developer",
            Self::User => "user",
        }
    }
}

impl From<PromptAuthority> for MessageRole {
    fn from(authority: PromptAuthority) -> Self {
        match authority {
            PromptAuthority::System => Self::System,
            PromptAuthority::Developer => Self::Developer,
            PromptAuthority::User => Self::User,
        }
    }
}

/// Provenance of an instruction fragment.
///
/// Authority is deliberately derived by [`Self::authority`]. Callers cannot
/// attach an independent role that disagrees with the source.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PromptSource {
    /// Norn's compiled product policy and runtime contract.
    ProductPolicy,
    /// Instructions from an explicit or user-level profile.
    OperatorProfile,
    /// Instructions from a profile discovered inside the workspace.
    WorkspaceProfile,
    /// Explicit `system_prompt` / `append_system_prompt` operator input.
    OperatorOverride,
    /// User-level `NORN.md` instructions.
    UserContextFile,
    /// Project-root `NORN.md` instructions.
    ProjectContextFile,
    /// Compiled guidance describing how the skill catalog is used.
    SkillCatalogPolicy,
    /// Skill metadata loaded from caller-trusted paths.
    OperatorSkillCatalog,
    /// Skill metadata loaded from inside the active workspace.
    WorkspaceSkillCatalog,
    /// Compiled child and fork policy owned by the Norn runtime.
    ChildAgentPolicy,
    /// Request-local runtime guidance such as tool and environment context.
    ManagedRuntimeContext,
    /// Human-authored task, steering, or delegation request.
    UserRequest,
    /// Rule content loaded from a trusted user-level source.
    OperatorRule,
    /// Rule content loaded from the active workspace.
    WorkspaceRule,
}

impl PromptSource {
    /// Authority assigned to this source.
    #[must_use]
    pub const fn authority(self) -> PromptAuthority {
        match self {
            Self::ProductPolicy
            | Self::SkillCatalogPolicy
            | Self::ChildAgentPolicy
            | Self::ManagedRuntimeContext => PromptAuthority::System,
            Self::OperatorProfile
            | Self::OperatorOverride
            | Self::UserContextFile
            | Self::OperatorSkillCatalog
            | Self::OperatorRule => PromptAuthority::Developer,
            Self::WorkspaceProfile
            | Self::ProjectContextFile
            | Self::WorkspaceSkillCatalog
            | Self::UserRequest
            | Self::WorkspaceRule => PromptAuthority::User,
        }
    }

    /// Stable discriminator used by prompt-plan fingerprints and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProductPolicy => "product_policy",
            Self::OperatorProfile => "operator_profile",
            Self::WorkspaceProfile => "workspace_profile",
            Self::OperatorOverride => "operator_override",
            Self::UserContextFile => "user_context_file",
            Self::ProjectContextFile => "project_context_file",
            Self::SkillCatalogPolicy => "skill_catalog_policy",
            Self::OperatorSkillCatalog => "operator_skill_catalog",
            Self::WorkspaceSkillCatalog => "workspace_skill_catalog",
            Self::ChildAgentPolicy => "child_agent_policy",
            Self::ManagedRuntimeContext => "managed_runtime_context",
            Self::UserRequest => "user_request",
            Self::OperatorRule => "operator_rule",
            Self::WorkspaceRule => "workspace_rule",
        }
    }

    pub(crate) const fn stable_order(self) -> u8 {
        match self {
            Self::ProductPolicy => 0,
            Self::ChildAgentPolicy => 1,
            Self::OperatorProfile => 10,
            Self::WorkspaceProfile => 11,
            Self::OperatorOverride => 20,
            Self::UserContextFile => 30,
            Self::ProjectContextFile => 40,
            Self::SkillCatalogPolicy => 50,
            Self::OperatorSkillCatalog => 51,
            Self::WorkspaceSkillCatalog => 52,
            Self::OperatorRule => 60,
            Self::WorkspaceRule => 61,
            Self::ManagedRuntimeContext => 70,
            Self::UserRequest => 80,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PromptAuthority, PromptSource};

    #[test]
    fn source_exhaustively_derives_authority() {
        let cases = [
            (PromptSource::ProductPolicy, PromptAuthority::System),
            (PromptSource::ChildAgentPolicy, PromptAuthority::System),
            (PromptSource::OperatorProfile, PromptAuthority::Developer),
            (PromptSource::WorkspaceProfile, PromptAuthority::User),
            (PromptSource::OperatorOverride, PromptAuthority::Developer),
            (PromptSource::UserContextFile, PromptAuthority::Developer),
            (PromptSource::ProjectContextFile, PromptAuthority::User),
            (PromptSource::SkillCatalogPolicy, PromptAuthority::System),
            (
                PromptSource::OperatorSkillCatalog,
                PromptAuthority::Developer,
            ),
            (PromptSource::WorkspaceSkillCatalog, PromptAuthority::User),
            (PromptSource::ManagedRuntimeContext, PromptAuthority::System),
            (PromptSource::UserRequest, PromptAuthority::User),
            (PromptSource::OperatorRule, PromptAuthority::Developer),
            (PromptSource::WorkspaceRule, PromptAuthority::User),
        ];

        for (source, expected) in cases {
            assert_eq!(source.authority(), expected, "source={source:?}");
        }
    }
}
