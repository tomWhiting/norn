//! Monotonic governance transition validation.

use thiserror::Error;

use super::{GovernanceTable, LegacyGovernance, LegacyGovernanceEntry, LegacyState};

/// Validate that governance only tightens between reviewed revisions.
///
/// # Errors
///
/// Rejects added/removed exception identities, changed reviewed metadata, later
/// due phases, or a resolved exception becoming active again.
pub(crate) fn validate_governance_tightening(
    previous: &LegacyGovernance,
    current: &LegacyGovernance,
) -> Result<(), GovernanceTransitionError> {
    validate_transition_table(
        GovernanceTable::Loc,
        &previous.loc_exceptions,
        &current.loc_exceptions,
    )?;
    validate_transition_table(
        GovernanceTable::Debt,
        &previous.debt_exceptions,
        &current.debt_exceptions,
    )
}

fn validate_transition_table(
    table: GovernanceTable,
    previous: &[LegacyGovernanceEntry],
    current: &[LegacyGovernanceEntry],
) -> Result<(), GovernanceTransitionError> {
    if previous.len() != current.len()
        || previous
            .iter()
            .zip(current)
            .any(|(left, right)| left.origin_id != right.origin_id)
    {
        return Err(GovernanceTransitionError::IdentitySet { table });
    }
    for (left, right) in previous.iter().zip(current) {
        if left.owner != right.owner || left.remediation_record != right.remediation_record {
            return Err(GovernanceTransitionError::ReviewedMetadataChanged { table });
        }
        if right.due_phase > left.due_phase {
            return Err(GovernanceTransitionError::DueMovedLater { table });
        }
        if left.state == LegacyState::Resolved && right.state == LegacyState::Active {
            return Err(GovernanceTransitionError::Reactivated);
        }
    }
    Ok(())
}

/// Governance monotonicity failure.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum GovernanceTransitionError {
    /// A legacy exception identity was added or removed.
    #[error("governance identity set changed for {table}")]
    IdentitySet {
        /// Changed table.
        table: GovernanceTable,
    },
    /// A reviewed owner or remediation-record token changed.
    #[error("reviewed governance metadata changed for {table}")]
    ReviewedMetadataChanged {
        /// Changed table.
        table: GovernanceTable,
    },
    /// A due phase moved later.
    #[error("governance due phase moved later for {table}")]
    DueMovedLater {
        /// Loosened table.
        table: GovernanceTable,
    },
    /// A resolved exception was reactivated.
    #[error("resolved governance was reactivated")]
    Reactivated,
}

#[cfg(test)]
mod tests {
    use super::{GovernanceTransitionError, validate_governance_tightening};
    use crate::baseline::{GovernanceTable, LegacyGovernance};

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    #[test]
    fn allows_tightening_but_rejects_later_due_and_reactivation() -> TestResult {
        let previous = governance("active", "P5")?;
        let tightened = governance("resolved", "P4")?;
        validate_governance_tightening(&previous, &tightened)?;

        let later = governance("active", "P6")?;
        assert!(matches!(
            validate_governance_tightening(&previous, &later),
            Err(GovernanceTransitionError::DueMovedLater { .. })
        ));

        let reopened = governance("active", "P4")?;
        let error = validate_governance_tightening(&tightened, &reopened);
        assert!(matches!(error, Err(GovernanceTransitionError::Reactivated)));
        let Err(error) = error else {
            return Err("reactivated governance unexpectedly passed".into());
        };
        assert!(!error.to_string().contains(&"0".repeat(64)));
        Ok(())
    }

    #[test]
    fn rejects_a_changed_identity_set() -> TestResult {
        let previous = governance("active", "P5")?;
        let current = LegacyGovernance::decode(
            br#"schema_version = 1
loc_exceptions = []
debt_exceptions = []

[algorithms]
analyzer = "norn-policy-1"
digest = "norn-sha256-canonical-json-1"
"#,
        )?;

        assert!(matches!(
            validate_governance_tightening(&previous, &current),
            Err(GovernanceTransitionError::IdentitySet { .. })
        ));
        Ok(())
    }

    #[test]
    fn rejects_changed_owner_or_remediation_metadata() -> TestResult {
        let previous = governance_with_metadata("policy-team", "loc-001")?;
        for current in [
            governance_with_metadata("other-team", "loc-001")?,
            governance_with_metadata("policy-team", "loc-002")?,
        ] {
            assert!(matches!(
                validate_governance_tightening(&previous, &current),
                Err(GovernanceTransitionError::ReviewedMetadataChanged {
                    table: GovernanceTable::Loc,
                })
            ));
        }
        Ok(())
    }

    fn governance(state: &str, due_phase: &str) -> TestResult<LegacyGovernance> {
        governance_document(state, due_phase, "policy-team", "loc-001")
    }

    fn governance_with_metadata(
        owner: &str,
        remediation_record: &str,
    ) -> TestResult<LegacyGovernance> {
        governance_document("active", "P5", owner, remediation_record)
    }

    fn governance_document(
        state: &str,
        due_phase: &str,
        owner: &str,
        remediation_record: &str,
    ) -> TestResult<LegacyGovernance> {
        let document = format!(
            r#"schema_version = 1
debt_exceptions = []

[algorithms]
analyzer = "norn-policy-1"
digest = "norn-sha256-canonical-json-1"

[[loc_exceptions]]
origin_id = "{}"
owner = "{owner}"
due_phase = "{due_phase}"
remediation_record = "{remediation_record}"
state = "{state}"
"#,
            "0".repeat(64),
        );
        Ok(LegacyGovernance::decode(document.as_bytes())?)
    }
}
