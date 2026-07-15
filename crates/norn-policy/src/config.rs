//! Closed repository-policy configuration and normalized authority digest.

use std::str::Utf8Error;

use serde::Deserialize;
use serde_json::{Map, Number, Value};
use thiserror::Error;

use crate::digest::{CanonicalJsonError, Digest, digest_json};
use crate::version::{ANALYZER_VERSION, DIGEST_VERSION, POLICY_SCHEMA_VERSION};

/// Largest permitted production LOC for a Rust entrypoint.
pub const BUILTIN_ENTRYPOINT_LOC_MAX: u32 = 200;

/// Largest permitted production LOC for any other Rust source file.
pub const BUILTIN_OTHER_RUST_LOC_MAX: u32 = 500;

/// The non-downgradable result mode for every repository rule.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnforcementMode {
    /// A finding rejects the repository state or staged mutation.
    Hard,
}

/// Rule families that every valid repository policy requires.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum RuleFamily {
    /// Discover Cargo targets and production-reachable Rust source strictly.
    ProductionReachability,
    /// Validate registered generated source includes.
    GeneratedIncludes,
    /// Enforce cfg-aware production LOC ceilings.
    ProductionLoc,
    /// Restrict production `mod.rs` files to declarations and visible re-exports.
    ModuleShape,
    /// Reject new or changed prohibited constructs.
    ProhibitedDebt,
    /// Preserve production item/projection identity against test-only hiding.
    ProductionProjection,
    /// Enforce immutable origin facts and reviewed governance.
    OriginGovernance,
    /// Discover and classify every possible filesystem writer.
    WriterInventory,
    /// Validate retained artifacts against closed redaction schemas.
    EvidenceRedaction,
    /// Require complete finding-to-evidence traceability.
    EvidenceTraceability,
}

impl RuleFamily {
    /// Return the stable normalized policy identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProductionReachability => "production_reachability",
            Self::GeneratedIncludes => "generated_includes",
            Self::ProductionLoc => "production_loc",
            Self::ModuleShape => "module_shape",
            Self::ProhibitedDebt => "prohibited_debt",
            Self::ProductionProjection => "production_projection",
            Self::OriginGovernance => "origin_governance",
            Self::WriterInventory => "writer_inventory",
            Self::EvidenceRedaction => "evidence_redaction",
            Self::EvidenceTraceability => "evidence_traceability",
        }
    }
}

const REQUIRED_RULE_FAMILIES: [RuleFamily; 10] = [
    RuleFamily::ProductionReachability,
    RuleFamily::GeneratedIncludes,
    RuleFamily::ProductionLoc,
    RuleFamily::ModuleShape,
    RuleFamily::ProhibitedDebt,
    RuleFamily::ProductionProjection,
    RuleFamily::OriginGovernance,
    RuleFamily::WriterInventory,
    RuleFamily::EvidenceRedaction,
    RuleFamily::EvidenceTraceability,
];

/// Validated cfg-aware production LOC ceilings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProductionLocLimits {
    entrypoint_max: u32,
    other_rust_max: u32,
}

impl ProductionLocLimits {
    /// Return the `lib.rs` and `main.rs` production LOC ceiling.
    #[must_use]
    pub const fn entrypoint_max(self) -> u32 {
        self.entrypoint_max
    }

    /// Return the ceiling for every other production Rust file.
    #[must_use]
    pub const fn other_rust_max(self) -> u32 {
        self.other_rust_max
    }
}

/// A strict, validated `policy/repository.toml` document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepositoryPolicy {
    production_loc: ProductionLocLimits,
}

impl RepositoryPolicy {
    pub(crate) const fn p1_baseline() -> Self {
        Self {
            production_loc: ProductionLocLimits {
                entrypoint_max: BUILTIN_ENTRYPOINT_LOC_MAX,
                other_rust_max: BUILTIN_OTHER_RUST_LOC_MAX,
            },
        }
    }

    /// Decode owned bytes and validate the complete repository policy.
    ///
    /// # Errors
    ///
    /// Rejects non-UTF-8 or invalid TOML, duplicate and unknown fields,
    /// unsupported identities, zero limits, and limits looser than the built-in
    /// hard maxima.
    pub fn decode(bytes: &[u8]) -> Result<Self, RepositoryPolicyError> {
        let document = std::str::from_utf8(bytes).map_err(RepositoryPolicyError::Utf8)?;
        let parsed: PolicyDocument =
            toml::from_str(document).map_err(RepositoryPolicyError::Toml)?;
        parsed.validate()
    }

    /// Return the closed document schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        POLICY_SCHEMA_VERSION
    }

    /// Return the exact analyzer implementation identity.
    #[must_use]
    pub const fn analyzer_version(&self) -> &'static str {
        ANALYZER_VERSION
    }

    /// Return the exact canonical digest implementation identity.
    #[must_use]
    pub const fn digest_version(&self) -> &'static str {
        DIGEST_VERSION
    }

    /// Return the non-downgradable enforcement mode.
    #[must_use]
    pub const fn enforcement_mode(&self) -> EnforcementMode {
        EnforcementMode::Hard
    }

    /// Return every required built-in rule family in normalized order.
    #[must_use]
    pub const fn required_rule_families(&self) -> &'static [RuleFamily] {
        &REQUIRED_RULE_FAMILIES
    }

    /// Return validated cfg-aware production LOC ceilings.
    #[must_use]
    pub const fn production_loc(&self) -> ProductionLocLimits {
        self.production_loc
    }

    /// Hash the normalized hard-policy document using canonical JSON.
    ///
    /// TOML ordering, comments, and whitespace are intentionally absent from
    /// this authority digest. The normalized value also binds the compiled hard
    /// mode and complete required-family inventory, neither of which the input
    /// document can override.
    ///
    /// # Errors
    ///
    /// Returns an error only if canonical encoding of the closed normalized
    /// value fails.
    pub fn normalized_digest(&self) -> Result<Digest, CanonicalJsonError> {
        digest_json(&self.normalized_value())
    }

    fn normalized_value(&self) -> Value {
        let mut algorithms = Map::new();
        algorithms.insert(
            "analyzer".to_owned(),
            Value::String(ANALYZER_VERSION.to_owned()),
        );
        algorithms.insert(
            "digest".to_owned(),
            Value::String(DIGEST_VERSION.to_owned()),
        );

        let mut production_loc = Map::new();
        production_loc.insert(
            "entrypoint_max".to_owned(),
            Value::Number(Number::from(self.production_loc.entrypoint_max)),
        );
        production_loc.insert(
            "other_rust_max".to_owned(),
            Value::Number(Number::from(self.production_loc.other_rust_max)),
        );

        let required_rule_families = REQUIRED_RULE_FAMILIES
            .iter()
            .map(|family| Value::String(family.as_str().to_owned()))
            .collect();

        let mut normalized = Map::new();
        normalized.insert("algorithms".to_owned(), Value::Object(algorithms));
        normalized.insert("enforcement".to_owned(), Value::String("hard".to_owned()));
        normalized.insert("production_loc".to_owned(), Value::Object(production_loc));
        normalized.insert(
            "required_rule_families".to_owned(),
            Value::Array(required_rule_families),
        );
        normalized.insert(
            "schema_version".to_owned(),
            Value::Number(Number::from(POLICY_SCHEMA_VERSION)),
        );
        Value::Object(normalized)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyDocument {
    schema_version: u32,
    algorithms: AlgorithmDocument,
    production_loc: ProductionLocDocument,
}

impl PolicyDocument {
    fn validate(self) -> Result<RepositoryPolicy, RepositoryPolicyError> {
        if self.schema_version != POLICY_SCHEMA_VERSION {
            return Err(RepositoryPolicyError::SchemaVersion {
                actual: self.schema_version,
            });
        }
        if self.algorithms.analyzer != ANALYZER_VERSION {
            return Err(RepositoryPolicyError::AnalyzerVersion);
        }
        if self.algorithms.digest != DIGEST_VERSION {
            return Err(RepositoryPolicyError::DigestVersion);
        }

        validate_limit(
            LocLimitKind::Entrypoint,
            self.production_loc.entrypoint_max,
            BUILTIN_ENTRYPOINT_LOC_MAX,
        )?;
        validate_limit(
            LocLimitKind::OtherRust,
            self.production_loc.other_rust_max,
            BUILTIN_OTHER_RUST_LOC_MAX,
        )?;

        Ok(RepositoryPolicy {
            production_loc: ProductionLocLimits {
                entrypoint_max: self.production_loc.entrypoint_max,
                other_rust_max: self.production_loc.other_rust_max,
            },
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AlgorithmDocument {
    analyzer: String,
    digest: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProductionLocDocument {
    entrypoint_max: u32,
    other_rust_max: u32,
}

/// Closed production LOC classes admitted by repository policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocLimitKind {
    /// `lib.rs` and `main.rs`.
    Entrypoint,
    /// Every other production Rust source file.
    OtherRust,
}

/// Strict repository-policy decoding or validation failure.
#[derive(Debug, Error)]
pub enum RepositoryPolicyError {
    /// The owned document bytes are not UTF-8.
    #[error("repository policy is not UTF-8")]
    Utf8(#[source] Utf8Error),
    /// TOML syntax, duplicate fields, unknown fields, or field types are invalid.
    #[error("repository policy is not valid closed-schema TOML")]
    Toml(#[source] toml::de::Error),
    /// The document schema is unsupported.
    #[error("repository policy schema version {actual} is unsupported")]
    SchemaVersion {
        /// Observed schema version.
        actual: u32,
    },
    /// The analyzer identity differs from the compiled implementation.
    #[error("repository policy analyzer identity does not match this evaluator")]
    AnalyzerVersion,
    /// The canonical digest identity differs from the compiled implementation.
    #[error("repository policy digest identity does not match this evaluator")]
    DigestVersion,
    /// A numeric limit would disable its rule family.
    #[error("repository policy {kind:?} production LOC maximum must be positive")]
    ZeroLimit {
        /// Limit class containing zero.
        kind: LocLimitKind,
    },
    /// A numeric limit is looser than the compiled hard maximum.
    #[error(
        "repository policy {kind:?} production LOC maximum {actual} exceeds built-in {maximum}"
    )]
    LimitLoosened {
        /// Limit class that was loosened.
        kind: LocLimitKind,
        /// Requested maximum.
        actual: u32,
        /// Compiled hard maximum.
        maximum: u32,
    },
}

fn validate_limit(
    kind: LocLimitKind,
    actual: u32,
    maximum: u32,
) -> Result<(), RepositoryPolicyError> {
    if actual == 0 {
        return Err(RepositoryPolicyError::ZeroLimit { kind });
    }
    if actual > maximum {
        return Err(RepositoryPolicyError::LimitLoosened {
            kind,
            actual,
            maximum,
        });
    }
    Ok(())
}
