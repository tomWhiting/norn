//! Prompt authority derived from the provenance of each instruction fragment.

use crate::provider::request::MessageRole;

/// Provider-neutral authority carried by a prompt fragment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PromptAuthority {
    /// Product-owned policy sent with the provider's highest authority.
    System,
    /// Trusted stable operator guidance below product policy.
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
    /// Explicit base policy supplied by a legacy library embedder.
    EmbedderPolicy,
    /// Compiled child-agent lifecycle policy.
    ChildAgentPolicy,
    /// Compiled fork identity and reintegration policy.
    ForkAgentPolicy,
    /// Prompt text compiled into a built-in Norn variant.
    BuiltinVariant,
    /// Compiled guidance describing how the skill catalog is used.
    SkillCatalogPolicy,
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
    /// Skill metadata loaded from caller-trusted paths.
    OperatorSkillCatalog,
    /// Skill metadata loaded from inside the active workspace.
    WorkspaceSkillCatalog,
    /// Prompt text supplied through configured variant settings.
    ConfiguredVariant,
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
            | Self::EmbedderPolicy
            | Self::ForkAgentPolicy
            | Self::BuiltinVariant
            | Self::SkillCatalogPolicy
            | Self::ChildAgentPolicy => PromptAuthority::System,
            Self::OperatorProfile
            | Self::OperatorOverride
            | Self::UserContextFile
            | Self::OperatorSkillCatalog
            | Self::OperatorRule => PromptAuthority::Developer,
            Self::WorkspaceProfile
            | Self::ProjectContextFile
            | Self::WorkspaceSkillCatalog
            | Self::ConfiguredVariant
            | Self::UserRequest
            | Self::WorkspaceRule => PromptAuthority::User,
        }
    }

    /// Stable discriminator used by prompt-plan fingerprints and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProductPolicy => "product_policy",
            Self::EmbedderPolicy => "embedder_policy",
            Self::ChildAgentPolicy => "child_agent_policy",
            Self::ForkAgentPolicy => "fork_agent_policy",
            Self::BuiltinVariant => "builtin_variant",
            Self::SkillCatalogPolicy => "skill_catalog_policy",
            Self::OperatorProfile => "operator_profile",
            Self::WorkspaceProfile => "workspace_profile",
            Self::OperatorOverride => "operator_override",
            Self::UserContextFile => "user_context_file",
            Self::ProjectContextFile => "project_context_file",
            Self::OperatorSkillCatalog => "operator_skill_catalog",
            Self::WorkspaceSkillCatalog => "workspace_skill_catalog",
            Self::ConfiguredVariant => "configured_variant",
            Self::UserRequest => "user_request",
            Self::OperatorRule => "operator_rule",
            Self::WorkspaceRule => "workspace_rule",
        }
    }

    pub(crate) const fn stable_order(self) -> u8 {
        match self {
            Self::ProductPolicy => 0,
            Self::EmbedderPolicy => 1,
            Self::ChildAgentPolicy => 2,
            Self::ForkAgentPolicy => 3,
            Self::BuiltinVariant => 4,
            Self::SkillCatalogPolicy => 5,
            Self::OperatorProfile => 20,
            Self::OperatorOverride => 21,
            Self::UserContextFile => 22,
            Self::OperatorSkillCatalog => 23,
            Self::OperatorRule => 24,
            Self::WorkspaceProfile => 40,
            Self::ConfiguredVariant => 41,
            Self::ProjectContextFile => 42,
            Self::WorkspaceSkillCatalog => 43,
            Self::WorkspaceRule => 44,
            Self::UserRequest => 50,
        }
    }
}

/// Explicit wire projection for volatile, request-local Norn policy.
///
/// Environment, collaboration, and provider-surface framing can change every
/// request and are not stable [`PromptSource`] values. Trusted prompt-command
/// output is tracked separately at Developer authority. Stateless transports
/// lower both channels into the documented compatibility tail.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManagedContextProjection {
    /// Internal System marker lifted by the Responses adapter into top-level
    /// `instructions`. The wire field itself has no message-role discriminator.
    ThreadedResponsesInstructions,
    /// Stateless compatibility message at the trailing Developer position.
    StatelessDeveloperTail,
}

impl ManagedContextProjection {
    /// Provider-neutral role used to build the selected wire projection.
    #[must_use]
    pub const fn role(self) -> MessageRole {
        match self {
            Self::ThreadedResponsesInstructions => MessageRole::System,
            Self::StatelessDeveloperTail => MessageRole::Developer,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ManagedContextProjection, PromptAuthority, PromptSource};

    #[test]
    fn source_exhaustively_derives_authority() {
        let cases = [
            (PromptSource::ProductPolicy, PromptAuthority::System),
            (PromptSource::EmbedderPolicy, PromptAuthority::System),
            (PromptSource::ChildAgentPolicy, PromptAuthority::System),
            (PromptSource::ForkAgentPolicy, PromptAuthority::System),
            (PromptSource::BuiltinVariant, PromptAuthority::System),
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
            (PromptSource::ConfiguredVariant, PromptAuthority::User),
            (PromptSource::UserRequest, PromptAuthority::User),
            (PromptSource::OperatorRule, PromptAuthority::Developer),
            (PromptSource::WorkspaceRule, PromptAuthority::User),
        ];

        for (source, expected) in cases {
            assert_eq!(source.authority(), expected, "source={source:?}");
        }
    }

    #[test]
    fn managed_context_projection_is_transport_explicit() {
        assert_eq!(
            ManagedContextProjection::ThreadedResponsesInstructions.role(),
            crate::provider::request::MessageRole::System,
        );
        assert_eq!(
            ManagedContextProjection::StatelessDeveloperTail.role(),
            crate::provider::request::MessageRole::Developer,
        );
    }
}
