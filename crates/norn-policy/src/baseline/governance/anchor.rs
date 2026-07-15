//! Opaque reviewed-governance anchor acquisition.

use std::fmt;

use thiserror::Error;

use super::transition::validate_governance_tightening;
use super::{GovernanceTransitionError, LegacyGovernance};
use crate::baseline::{LocCeilings, OriginLedger};
use crate::digest::Digest;

/// Candidate identity of the closed empty P1 governance document.
///
/// This value exists only to keep structural foundation tests compilable. It is
/// not a reviewed non-empty authority and must be replaced with the generated,
/// independently reviewed checked-tree anchor before any commit or `Ready`
/// claim.
pub const P1_GOVERNANCE_ANCHOR_IDENTITY: Digest = Digest::from_bytes([
    0xe7, 0xfb, 0xb7, 0x4c, 0xe8, 0x86, 0x3b, 0xee, 0x99, 0x95, 0x72, 0xd1, 0xcc, 0x5a, 0xb9, 0xe8,
    0x66, 0x8d, 0xda, 0xc0, 0x98, 0x65, 0x1b, 0xea, 0x49, 0x53, 0x23, 0xfb, 0xff, 0x8d, 0x06, 0x1e,
]);

/// Opaque governance value pinned to the compiled reviewed identity.
pub struct ReviewedGovernanceAnchor {
    governance: LegacyGovernance,
    identity: Digest,
}

impl ReviewedGovernanceAnchor {
    /// Acquire the fixed reviewed anchor and link it to the immutable origin.
    ///
    /// # Errors
    ///
    /// Rejects malformed governance, normalization failure, an identity other
    /// than the compiled reviewed anchor, or incomplete immutable-origin
    /// coverage. Source bytes and semantic contents are never retained in an
    /// error.
    pub fn acquire(bytes: &[u8], origin: &OriginLedger) -> Result<Self, GovernanceAnchorError> {
        let Ok(governance) = LegacyGovernance::decode(bytes) else {
            return Err(GovernanceAnchorError::Invalid);
        };
        let Ok(identity) = governance.normalized_digest() else {
            return Err(GovernanceAnchorError::Normalization);
        };
        if identity != P1_GOVERNANCE_ANCHOR_IDENTITY {
            return Err(GovernanceAnchorError::Identity);
        }
        if governance
            .validate_against(origin, LocCeilings::p1_baseline())
            .is_err()
        {
            return Err(GovernanceAnchorError::OriginLink);
        }
        Ok(Self {
            governance,
            identity,
        })
    }

    /// Return the normalized identity proven during acquisition.
    #[must_use]
    pub const fn identity(&self) -> Digest {
        self.identity
    }

    /// Require current governance to tighten this reviewed anchor.
    ///
    /// # Errors
    ///
    /// Rejects an identity-set change, a later due phase, or reactivation.
    pub fn validate_successor(
        &self,
        current: &LegacyGovernance,
    ) -> Result<(), GovernanceTransitionError> {
        validate_governance_tightening(&self.governance, current)
    }
}

impl fmt::Debug for ReviewedGovernanceAnchor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReviewedGovernanceAnchor")
            .finish_non_exhaustive()
    }
}

/// Closed, non-disclosing reviewed-anchor acquisition failure.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum GovernanceAnchorError {
    /// The fixed document did not decode as strict governance.
    #[error("reviewed governance anchor is invalid")]
    Invalid,
    /// Its normalized identity could not be computed.
    #[error("reviewed governance anchor could not be normalized")]
    Normalization,
    /// It was not the compiled independently reviewed identity.
    #[error("reviewed governance anchor identity does not match")]
    Identity,
    /// It did not exactly cover the immutable legacy origin.
    #[error("reviewed governance anchor does not match the immutable origin")]
    OriginLink,
}
