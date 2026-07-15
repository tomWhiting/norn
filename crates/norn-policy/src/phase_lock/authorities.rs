//! Snapshot-owned P1 authority acquisition.

mod error;

pub use error::{P1AuthorityError, P1AuthorityKind};

use crate::baseline::{
    GovernanceAnchorError, LegacyGovernance, LocCeilings, OriginLedger, ReviewedGovernanceAnchor,
    generated_registry_technical_identity,
};
use crate::config::RepositoryPolicy;
use crate::digest::digest_bytes;
use crate::evaluation_input::P1EvaluationInput;
use crate::redaction::RedactionRegistry;
use crate::responses_contract::ResponsesContractAuthority;
use crate::rust::modules::GeneratedIncludeRegistry;
use crate::snapshot::{EntryKind, OwnedSnapshot};
use crate::strict_json::decode_strict_json;
use crate::writers::WriterFamilyRegistry;
use crate::{Digest, RepositoryPath};

use super::PhaseLock;

const PHASE_LOCK_PATH: &str = "policy/phase-lock.json";
const REPOSITORY_POLICY_PATH: &str = "policy/repository.toml";
const GENERATED_INCLUDES_PATH: &str = "policy/generated-includes.json";
const ORIGIN_PATH: &str = "policy/origin/p1-computed.json";
const GOVERNANCE_PATH: &str = "policy/governance/legacy.toml";
const GOVERNANCE_ANCHOR_PATH: &str = "policy/governance/p1-reviewed.toml";
const WRITER_FAMILIES_PATH: &str = "policy/writer-families.toml";
const REDACTION_REGISTRY_PATH: &str = "policy/redaction-registry.json";
const SOURCE_FINDINGS_PATH: &str = "docs/reviews/evidence/p1/finding-traceability.jsonl";

/// Opaque proof that every P1 authority came from one current snapshot and the
/// immutable, exact P1 base projection.
pub struct ReadyP1Authorities {
    lock: PhaseLock,
    repository_policy: RepositoryPolicy,
    generated_includes: GeneratedIncludeRegistry,
    origin: OriginLedger,
    governance: LegacyGovernance,
    writer_families: WriterFamilyRegistry,
    responses: ResponsesContractAuthority,
    redaction: RedactionRegistry,
}

impl ReadyP1Authorities {
    /// Acquire all P1 authorities from fixed regular-file paths and bind them
    /// transitively to the exact base reconstruction.
    ///
    /// # Errors
    ///
    /// Rejects a missing or non-regular authority, an invalid closed document,
    /// any lock mismatch, an origin that differs from exact-base
    /// reconstruction, incomplete governance or writer-family coverage, an
    /// invalid Responses corpus, or any retained-artifact violation.
    pub fn acquire(input: P1EvaluationInput<'_>) -> Result<Self, P1AuthorityError> {
        let current = input.current().snapshot();
        let Ok(lock) = PhaseLock::decode_p1(required_bytes(
            current,
            P1AuthorityKind::PhaseLock,
            PHASE_LOCK_PATH,
        )?) else {
            return Err(P1AuthorityError::Invalid(P1AuthorityKind::PhaseLock));
        };

        let generated_bytes = required_bytes(
            current,
            P1AuthorityKind::GeneratedIncludes,
            GENERATED_INCLUDES_PATH,
        )?;
        let Ok(generated_includes): Result<GeneratedIncludeRegistry, _> =
            decode_strict_json(generated_bytes)
        else {
            return Err(P1AuthorityError::Invalid(
                P1AuthorityKind::GeneratedIncludes,
            ));
        };
        let Some(base) = input.base() else {
            return Err(P1AuthorityError::ExactBase);
        };
        let Ok(exact_base) = crate::baseline::ExactP1Base::acquire(base, &generated_includes)
        else {
            return Err(P1AuthorityError::ExactBase);
        };
        let Ok(generated_identity) = generated_registry_technical_identity(&generated_includes)
        else {
            return Err(P1AuthorityError::GeneratedIncludeIdentity);
        };
        if lock.digests().generated_include_registry != generated_identity {
            return Err(P1AuthorityError::Digest(P1AuthorityKind::GeneratedIncludes));
        }

        let Ok(repository_policy) = RepositoryPolicy::decode(required_bytes(
            current,
            P1AuthorityKind::RepositoryPolicy,
            REPOSITORY_POLICY_PATH,
        )?) else {
            return Err(P1AuthorityError::Invalid(P1AuthorityKind::RepositoryPolicy));
        };
        let Ok(policy_digest) = repository_policy.normalized_digest() else {
            return Err(P1AuthorityError::Normalization(
                P1AuthorityKind::RepositoryPolicy,
            ));
        };
        require_digest(
            lock.digests().repository_policy,
            policy_digest,
            P1AuthorityKind::RepositoryPolicy,
        )?;

        let Ok(expected_origin) = OriginLedger::generate_p1(&exact_base) else {
            return Err(P1AuthorityError::OriginReconstruction);
        };
        let Ok(origin) = OriginLedger::decode_p1(required_bytes(
            current,
            P1AuthorityKind::Origin,
            ORIGIN_PATH,
        )?) else {
            return Err(P1AuthorityError::Invalid(P1AuthorityKind::Origin));
        };
        if origin != expected_origin {
            return Err(P1AuthorityError::OriginMismatch);
        }
        let Ok(origin_digest) = origin.normalized_digest() else {
            return Err(P1AuthorityError::Normalization(P1AuthorityKind::Origin));
        };
        require_digest(
            lock.digests().origin,
            origin_digest,
            P1AuthorityKind::Origin,
        )?;

        let Ok(governance) = LegacyGovernance::decode(required_bytes(
            current,
            P1AuthorityKind::Governance,
            GOVERNANCE_PATH,
        )?) else {
            return Err(P1AuthorityError::Invalid(P1AuthorityKind::Governance));
        };
        let Ok(governance_digest) = governance.normalized_digest() else {
            return Err(P1AuthorityError::Normalization(P1AuthorityKind::Governance));
        };
        require_digest(
            lock.digests().governance,
            governance_digest,
            P1AuthorityKind::Governance,
        )?;
        let baseline_limits = LocCeilings::p1_baseline();
        if governance
            .validate_against(&origin, baseline_limits)
            .is_err()
        {
            return Err(P1AuthorityError::GovernanceLink);
        }
        let anchor = ReviewedGovernanceAnchor::acquire(
            required_bytes(
                current,
                P1AuthorityKind::GovernanceAnchor,
                GOVERNANCE_ANCHOR_PATH,
            )?,
            &origin,
        )
        .map_err(anchor_error)?;
        require_digest(
            lock.digests().governance_anchor,
            anchor.identity(),
            P1AuthorityKind::GovernanceAnchor,
        )?;
        if anchor.validate_successor(&governance).is_err() {
            return Err(P1AuthorityError::GovernanceTransition);
        }

        let Ok(writer_families) = WriterFamilyRegistry::decode_p1(required_bytes(
            current,
            P1AuthorityKind::WriterFamilies,
            WRITER_FAMILIES_PATH,
        )?) else {
            return Err(P1AuthorityError::Invalid(P1AuthorityKind::WriterFamilies));
        };
        let Ok(writer_digest) = writer_families.normalized_digest() else {
            return Err(P1AuthorityError::Normalization(
                P1AuthorityKind::WriterFamilies,
            ));
        };
        require_digest(
            lock.digests().writer_families,
            writer_digest,
            P1AuthorityKind::WriterFamilies,
        )?;
        if !writer_families.validate_against_origin(&origin).is_empty() {
            return Err(P1AuthorityError::WriterFamilyLink);
        }

        let responses =
            ResponsesContractAuthority::acquire(current).map_err(P1AuthorityError::from)?;
        require_digest(
            lock.digests().contract_manifest,
            responses.digest(),
            P1AuthorityKind::ResponsesContract,
        )?;

        let Ok(redaction) = RedactionRegistry::decode_p1(required_bytes(
            current,
            P1AuthorityKind::RedactionRegistry,
            REDACTION_REGISTRY_PATH,
        )?) else {
            return Err(P1AuthorityError::Invalid(
                P1AuthorityKind::RedactionRegistry,
            ));
        };
        require_digest(
            lock.digests().evidence_schemas,
            redaction.digest(),
            P1AuthorityKind::RedactionRegistry,
        )?;
        let source_findings = required_bytes(
            current,
            P1AuthorityKind::SourceFindings,
            SOURCE_FINDINGS_PATH,
        )?;
        require_digest(
            lock.digests().source_findings,
            digest_bytes(source_findings),
            P1AuthorityKind::SourceFindings,
        )?;

        let gate_entrypoint = required_bytes(
            current,
            P1AuthorityKind::GateEntrypoint,
            lock.gate().entrypoint_path.as_str(),
        )?;
        require_digest(
            lock.gate().entrypoint_sha256,
            digest_bytes(gate_entrypoint),
            P1AuthorityKind::GateEntrypoint,
        )?;
        let gate_manifest = required_bytes(
            current,
            P1AuthorityKind::GateManifest,
            lock.gate().command_manifest_path.as_str(),
        )?;
        require_digest(
            lock.gate().command_manifest_sha256,
            digest_bytes(gate_manifest),
            P1AuthorityKind::GateManifest,
        )?;

        Ok(Self {
            lock,
            repository_policy,
            generated_includes,
            origin,
            governance,
            writer_families,
            responses,
            redaction,
        })
    }

    /// Borrow the validated phase lock.
    #[must_use]
    pub const fn lock(&self) -> &PhaseLock {
        &self.lock
    }

    /// Borrow the hard repository policy.
    #[must_use]
    pub const fn repository_policy(&self) -> &RepositoryPolicy {
        &self.repository_policy
    }

    /// Borrow the exact generated-include authority.
    #[must_use]
    pub const fn generated_includes(&self) -> &GeneratedIncludeRegistry {
        &self.generated_includes
    }

    /// Borrow the exact reconstructed origin ledger.
    #[must_use]
    pub const fn origin(&self) -> &OriginLedger {
        &self.origin
    }

    /// Borrow reviewed legacy governance.
    #[must_use]
    pub const fn governance(&self) -> &LegacyGovernance {
        &self.governance
    }

    /// Borrow the complete writer-family authority.
    #[must_use]
    pub const fn writer_families(&self) -> &WriterFamilyRegistry {
        &self.writer_families
    }

    /// Borrow the complete public-plus-Codex contract authority.
    #[must_use]
    pub const fn responses(&self) -> &ResponsesContractAuthority {
        &self.responses
    }

    /// Borrow the complete retained-artifact authority.
    #[must_use]
    pub const fn redaction(&self) -> &RedactionRegistry {
        &self.redaction
    }
}

fn required_bytes<'a>(
    snapshot: &'a OwnedSnapshot,
    authority: P1AuthorityKind,
    path: &str,
) -> Result<&'a [u8], P1AuthorityError> {
    let Ok(path) = RepositoryPath::parse(path) else {
        return Err(P1AuthorityError::CompiledPath);
    };
    let Some(entry) = snapshot.get(&path) else {
        return Err(P1AuthorityError::Missing(authority));
    };
    if entry.kind() != EntryKind::Regular {
        return Err(P1AuthorityError::NotRegular(authority));
    }
    Ok(entry.bytes())
}

fn require_digest(
    expected: Digest,
    actual: Digest,
    authority: P1AuthorityKind,
) -> Result<(), P1AuthorityError> {
    if expected != actual {
        return Err(P1AuthorityError::Digest(authority));
    }
    Ok(())
}

const fn anchor_error(error: GovernanceAnchorError) -> P1AuthorityError {
    match error {
        GovernanceAnchorError::Invalid => {
            P1AuthorityError::Invalid(P1AuthorityKind::GovernanceAnchor)
        }
        GovernanceAnchorError::Normalization => {
            P1AuthorityError::Normalization(P1AuthorityKind::GovernanceAnchor)
        }
        GovernanceAnchorError::Identity => {
            P1AuthorityError::Digest(P1AuthorityKind::GovernanceAnchor)
        }
        GovernanceAnchorError::OriginLink => P1AuthorityError::GovernanceAnchorLink,
    }
}
