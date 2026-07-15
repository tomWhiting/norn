//! Exact P1 analysis-snapshot and generated-include authority.

use serde::Serialize;
use sha2::{Digest as _, Sha256};
use thiserror::Error;

use super::reconstruct::{BaselineFactsError, RepositoryBaselineFacts};
use crate::digest::{CanonicalJsonError, Digest, canonical_json_bytes};
use crate::evaluation_input::P1BaseSnapshot;
use crate::facts::SourceInventoryEntry;
use crate::facts::analyze_facts;
use crate::rust::modules::{
    CompileTestFixtureFact, GeneratedIncludeRegistration, GeneratedIncludeRegistry,
    HashedSourceInput, ModuleTargetIdentity, SourceSpan,
};
use crate::snapshot::OwnedSnapshot;

const GENERATED_REGISTRY_TECHNICAL_IDENTITY_DOMAIN: &[u8] =
    b"norn-policy-p1-generated-include-technical-registry-1";

/// Semantic projection of the exact P1 Git tree used by policy analysis.
///
/// The separately pinned Git tree remains authoritative for Git modes. This
/// identity intentionally maps both ordinary and executable blobs to
/// [`crate::EntryKind::Regular`], matching [`OwnedSnapshot`] analysis semantics.
pub const P1_BASE_ANALYSIS_SNAPSHOT_IDENTITY: Digest = Digest::from_bytes([
    0xa8, 0xee, 0xc4, 0x78, 0x76, 0xb2, 0x00, 0xb7, 0x90, 0xe1, 0xf6, 0x12, 0x73, 0x8f, 0xc4, 0x2e,
    0xe8, 0x03, 0x44, 0x68, 0x7f, 0x4e, 0xa4, 0x98, 0xd5, 0x6e, 0xfc, 0xb2, 0x08, 0xa2, 0xc7, 0x6c,
]);

/// Exact technical identity of the sole P1 generated-include registration.
pub const P1_GENERATED_INCLUDE_TECHNICAL_IDENTITY: Digest = Digest::from_bytes([
    0x52, 0x72, 0xfe, 0x6d, 0x23, 0x41, 0x9c, 0x9b, 0xb4, 0x28, 0x92, 0xc5, 0x5a, 0x62, 0x5b, 0xb8,
    0xb3, 0xf0, 0x04, 0x90, 0xf9, 0x21, 0x4d, 0x19, 0x48, 0x1c, 0x45, 0x92, 0x3a, 0x7a, 0x2e, 0x65,
]);

/// Opaque proof that canonical facts came from every exact P1 authority input.
pub struct ExactP1Base {
    analysis_snapshot_identity: Digest,
    generated_registry_technical_identity: Digest,
    facts: RepositoryBaselineFacts,
}

impl ExactP1Base {
    /// Validate exact P1 authorities and derive the complete canonical fact graph.
    ///
    /// Snapshot identity is checked before analysis. The generated-include
    /// registry's executable fields are bound as complete canonical JSON,
    /// including ordering. Callers cannot supply reconstructed facts.
    ///
    /// # Errors
    ///
    /// Rejects any snapshot or registry drift, or a canonical fact graph that
    /// cannot be reconstructed completely.
    pub fn acquire(
        base: &P1BaseSnapshot,
        generated: &GeneratedIncludeRegistry,
    ) -> Result<Self, ExactP1BaseError> {
        if base.validate_p1_identity().is_err() {
            return Err(ExactP1BaseError::GitIdentity);
        }
        Self::acquire_with_authorities(
            base.snapshot(),
            generated,
            P1_BASE_ANALYSIS_SNAPSHOT_IDENTITY,
            P1_GENERATED_INCLUDE_TECHNICAL_IDENTITY,
        )
    }

    fn acquire_with_authorities(
        snapshot: &OwnedSnapshot,
        generated: &GeneratedIncludeRegistry,
        expected_snapshot: Digest,
        expected_registry: Digest,
    ) -> Result<Self, ExactP1BaseError> {
        let analysis_snapshot_identity = snapshot.canonical_identity();
        if analysis_snapshot_identity != expected_snapshot {
            return Err(ExactP1BaseError::AnalysisSnapshotIdentity);
        }
        let generated_registry_technical_identity =
            generated_registry_technical_identity(generated)?;
        if generated_registry_technical_identity != expected_registry {
            return Err(ExactP1BaseError::GeneratedRegistryIdentity);
        }

        let repository = analyze_facts(snapshot, generated);
        let facts = RepositoryBaselineFacts::try_from_repository(&repository)
            .map_err(ExactP1BaseError::BaselineFacts)?;
        let Some(writers) = repository.writers() else {
            return Err(ExactP1BaseError::WriterIncomplete);
        };
        if !writers.is_registry_complete() {
            return Err(ExactP1BaseError::WriterIncomplete);
        }
        Ok(Self {
            analysis_snapshot_identity,
            generated_registry_technical_identity,
            facts,
        })
    }

    /// Return the verified semantic snapshot identity.
    #[must_use]
    pub const fn analysis_snapshot_identity(&self) -> Digest {
        self.analysis_snapshot_identity
    }

    /// Return the verified generated-include technical registry identity.
    #[must_use]
    pub const fn generated_registry_technical_identity(&self) -> Digest {
        self.generated_registry_technical_identity
    }

    /// Return the exact number of classified source rows bound into the proof.
    #[must_use]
    pub const fn source_count(&self) -> usize {
        self.facts.source_count()
    }

    /// Borrow every exact classified-source row from the immutable base.
    #[must_use]
    pub fn source_inventory(&self) -> &[SourceInventoryEntry] {
        self.facts.source_inventory()
    }

    /// Borrow every exact compile-test fixture row from the immutable base.
    #[must_use]
    pub fn compile_test_fixtures(&self) -> &[CompileTestFixtureFact] {
        self.facts.compile_test_fixtures()
    }

    pub(super) const fn facts(&self) -> &RepositoryBaselineFacts {
        &self.facts
    }
}

/// Hash every generated-include technical field as canonical, domain-separated JSON.
///
/// # Errors
///
/// Returns an error if registry serialization or canonical JSON encoding fails.
pub fn generated_registry_technical_identity(
    registry: &GeneratedIncludeRegistry,
) -> Result<Digest, GeneratedRegistryIdentityError> {
    let technical = TechnicalRegistry::from_registry(registry);
    let value =
        serde_json::to_value(technical).map_err(GeneratedRegistryIdentityError::Serialization)?;
    let canonical =
        canonical_json_bytes(&value).map_err(GeneratedRegistryIdentityError::Canonical)?;
    Ok(framed_identity(
        GENERATED_REGISTRY_TECHNICAL_IDENTITY_DOMAIN,
        &canonical,
    ))
}

#[derive(Serialize)]
struct TechnicalRegistry<'a> {
    schema_version: u32,
    entries: Vec<TechnicalRegistration<'a>>,
}

impl<'a> TechnicalRegistry<'a> {
    fn from_registry(registry: &'a GeneratedIncludeRegistry) -> Self {
        let GeneratedIncludeRegistry {
            schema_version,
            entries,
        } = registry;
        Self {
            schema_version: *schema_version,
            entries: entries
                .iter()
                .map(TechnicalRegistration::from_registration)
                .collect(),
        }
    }
}

#[derive(Serialize)]
struct TechnicalRegistration<'a> {
    source: &'a crate::RepositoryPath,
    callsite: &'a SourceSpan,
    enclosing_item: &'a SourceSpan,
    invocation_digest: &'a Digest,
    target: &'a ModuleTargetIdentity,
    generator: &'a HashedSourceInput,
    inputs: &'a [HashedSourceInput],
    output_basename: &'a str,
}

impl<'a> TechnicalRegistration<'a> {
    fn from_registration(registration: &'a GeneratedIncludeRegistration) -> Self {
        let GeneratedIncludeRegistration {
            source,
            callsite,
            enclosing_item,
            invocation_digest,
            target,
            generator,
            inputs,
            output_basename,
        } = registration;
        Self {
            source,
            callsite,
            enclosing_item,
            invocation_digest,
            target,
            generator,
            inputs,
            output_basename,
        }
    }
}

fn framed_identity(domain: &[u8], value: &[u8]) -> Digest {
    let mut hasher = Sha256::new();
    append_field(&mut hasher, domain);
    append_field(&mut hasher, value);
    Digest::from_bytes(hasher.finalize().into())
}

fn append_field(hasher: &mut Sha256, value: &[u8]) {
    let native = value.len().to_be_bytes();
    hasher.update(&[0_u8; 16][native.len()..]);
    hasher.update(native);
    hasher.update(value);
}

/// Failure to establish the exact P1 base authority.
#[derive(Debug, Error)]
pub enum ExactP1BaseError {
    /// Observed Git commit, tree, path, mode, or blob authority differs from P1.
    #[error("Git observation does not match the exact P1 base authority")]
    GitIdentity,
    /// The complete analysis snapshot differs from the ratified P1 projection.
    #[error("analysis snapshot does not match the exact P1 base projection")]
    AnalysisSnapshotIdentity,
    /// The complete generated-include registry differs from its ratified value.
    #[error("generated-include registry does not match the exact P1 authority")]
    GeneratedRegistryIdentity,
    /// Exact-base facts were incomplete or internally inconsistent.
    #[error("exact P1 snapshot could not produce complete baseline facts")]
    BaselineFacts(#[source] BaselineFactsError),
    /// The immutable origin snapshot has unknown or unobserved writer sinks.
    #[error("exact P1 snapshot has an incomplete writer inventory")]
    WriterIncomplete,
    /// The generated registry could not be encoded for identity comparison.
    #[error(transparent)]
    GeneratedRegistryIdentityEncoding(#[from] GeneratedRegistryIdentityError),
}

/// Failure to encode a complete generated-include registry identity.
#[derive(Debug, Error)]
pub enum GeneratedRegistryIdentityError {
    /// Serde could not represent the closed registry as JSON.
    #[error("generated-include registry serialization failed")]
    Serialization(#[source] serde_json::Error),
    /// Canonical JSON rejected the serialized registry.
    #[error("generated-include registry canonical encoding failed")]
    Canonical(#[source] CanonicalJsonError),
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::{
        ExactP1Base, ExactP1BaseError, GENERATED_REGISTRY_TECHNICAL_IDENTITY_DOMAIN,
        framed_identity, generated_registry_technical_identity,
    };
    use crate::path::RepositoryPath;
    use crate::rust::modules::GeneratedIncludeRegistry;
    use crate::snapshot::{EntryKind, OwnedSnapshot, SnapshotEntry};

    #[test]
    fn registry_identity_framing_is_domain_separated() {
        assert_ne!(
            framed_identity(GENERATED_REGISTRY_TECHNICAL_IDENTITY_DOMAIN, b"{}"),
            framed_identity(b"norn-policy-other-authority-1", b"{}")
        );
        assert_ne!(
            framed_identity(GENERATED_REGISTRY_TECHNICAL_IDENTITY_DOMAIN, b"{}"),
            framed_identity(GENERATED_REGISTRY_TECHNICAL_IDENTITY_DOMAIN, b"[]")
        );
    }

    #[test]
    fn opaque_acquisition_checks_identity_before_fact_reconstruction() -> Result<(), Box<dyn Error>>
    {
        const MANIFEST: &[u8] =
            b"[workspace]\n[package]\nname = \"sample\"\nedition = \"2024\"\nbuild = false\n";
        const SOURCE: &[u8] = b"pub fn value() -> u8 { 7 }\n";
        let snapshot = fixture_snapshot(&[
            ("Cargo.toml", EntryKind::Regular, MANIFEST),
            ("src/lib.rs", EntryKind::Regular, SOURCE),
        ])?;
        let registry = GeneratedIncludeRegistry::empty();
        let registry_identity = generated_registry_technical_identity(&registry)?;
        let snapshot_identity = snapshot.canonical_identity();
        assert!(matches!(
            ExactP1Base::acquire_with_authorities(
                &snapshot,
                &registry,
                snapshot_identity,
                registry_identity,
            ),
            Err(ExactP1BaseError::WriterIncomplete)
        ));

        let reordered = fixture_snapshot(&[
            ("src/lib.rs", EntryKind::Regular, SOURCE),
            ("Cargo.toml", EntryKind::Regular, MANIFEST),
        ])?;
        assert!(matches!(
            ExactP1Base::acquire_with_authorities(
                &reordered,
                &registry,
                snapshot_identity,
                registry_identity,
            ),
            Err(ExactP1BaseError::WriterIncomplete)
        ));

        let variants = [
            OwnedSnapshot::empty(),
            fixture_snapshot(&[("Cargo.toml", EntryKind::Regular, MANIFEST)])?,
            fixture_snapshot(&[
                ("Cargo.toml", EntryKind::Regular, MANIFEST),
                ("src/lib.rs", EntryKind::Regular, SOURCE),
                ("README.md", EntryKind::Regular, b"extra"),
            ])?,
            fixture_snapshot(&[
                ("Cargo.toml", EntryKind::Regular, MANIFEST),
                ("src/lib.rs", EntryKind::Symlink, SOURCE),
            ])?,
            fixture_snapshot(&[
                ("Cargo.toml", EntryKind::Regular, MANIFEST),
                (
                    "src/lib.rs",
                    EntryKind::Regular,
                    b"pub fn value() -> u8 { 8 }\n",
                ),
            ])?,
            fixture_snapshot(&[
                ("Cargo.toml", EntryKind::Regular, MANIFEST),
                ("src/main.rs", EntryKind::Regular, SOURCE),
            ])?,
        ];
        for variant in variants {
            assert!(matches!(
                ExactP1Base::acquire_with_authorities(
                    &variant,
                    &registry,
                    snapshot_identity,
                    registry_identity,
                ),
                Err(ExactP1BaseError::AnalysisSnapshotIdentity)
            ));
        }
        Ok(())
    }

    fn fixture_snapshot(
        entries: &[(&str, EntryKind, &[u8])],
    ) -> Result<OwnedSnapshot, Box<dyn Error>> {
        let entries = entries
            .iter()
            .map(|(path, kind, bytes)| {
                Ok((
                    RepositoryPath::parse(*path)?,
                    SnapshotEntry::copy_from_slice(*kind, bytes),
                ))
            })
            .collect::<Result<Vec<_>, crate::RepositoryPathError>>()?;
        Ok(OwnedSnapshot::try_from_entries(entries)?)
    }
}
