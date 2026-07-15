//! One fail-closed P1 evaluation over complete owned snapshots.

mod findings;
mod state;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod writer_tests;

pub use state::{
    AuthorityIssue, CurrentFactIssue, InvalidPolicy, PolicyAuthority, PolicyInvalidReason,
    PolicyReport, PolicyState,
};

use crate::baseline::{CurrentRepositoryFacts, LocCeilings, evaluate_legacy};
use crate::facts::analyze_facts;
use crate::phase_lock::ReadyP1Authorities;
use crate::{OwnedSnapshot, P1EvaluationInput};

use self::findings::canonical_findings;
use self::state::{AuthorityView, invalid_current_facts};

/// Evaluate the complete current repository against the exact P1 base tree.
///
/// This is the only public semantic entrypoint. It reads no filesystem, Git,
/// process, environment, or network state. `Absent` is returned only when the
/// fixed phase-lock marker is missing. Once that marker exists, every authority
/// or fact failure is a persistent `Invalid` state rather than an advisory
/// fallback.
#[must_use]
pub fn evaluate_p1(input: P1EvaluationInput<'_>) -> PolicyState {
    let current_role = input.current();
    if !current_role.marker_observed() {
        return PolicyState::Absent;
    }
    let authorities = match ReadyP1Authorities::acquire(input) {
        Ok(authorities) => authorities,
        Err(error) => return PolicyState::Invalid(InvalidPolicy::authority(&error)),
    };
    evaluate_ready(
        current_role.snapshot(),
        AuthorityView::from_ready(&authorities),
    )
}

fn evaluate_ready(current: &OwnedSnapshot, authorities: AuthorityView<'_>) -> PolicyState {
    let facts = analyze_facts(current, authorities.generated_includes);
    if let Err(error) = facts.validate_integrity() {
        return PolicyState::Invalid(invalid_current_facts(error, facts.failures()));
    }
    if facts.compile_test_fixtures() != authorities.origin.compile_test_fixtures() {
        return PolicyState::Invalid(InvalidPolicy::new(
            PolicyInvalidReason::CompileTestFixtureDrift,
        ));
    }
    let Ok(current_facts) = CurrentRepositoryFacts::try_from_repository(&facts) else {
        return PolicyState::Invalid(InvalidPolicy::new(PolicyInvalidReason::CurrentProjection));
    };
    let policy_limits = authorities.repository_policy.production_loc();
    let Ok(limits) = LocCeilings::new(
        policy_limits.entrypoint_max(),
        policy_limits.other_rust_max(),
    ) else {
        return PolicyState::Invalid(InvalidPolicy::new(PolicyInvalidReason::PolicyCeilings));
    };
    let Ok(legacy) = evaluate_legacy(
        &current_facts,
        authorities.origin,
        authorities.governance,
        limits,
        authorities.active_phase,
    ) else {
        return PolicyState::Invalid(InvalidPolicy::new(PolicyInvalidReason::LegacyEvaluation));
    };
    let Ok(findings) = canonical_findings(
        current,
        &facts,
        &current_facts,
        &legacy,
        limits,
        authorities,
    ) else {
        return PolicyState::Invalid(InvalidPolicy::new(PolicyInvalidReason::FindingConstruction));
    };
    PolicyState::Ready(PolicyReport::new(
        current_facts.source_inventory_digest(),
        findings,
        legacy.dispositions().to_vec(),
    ))
}

#[cfg(test)]
fn evaluate_with_fixture_authorities(
    current: &OwnedSnapshot,
    authorities: AuthorityView<'_>,
) -> PolicyState {
    evaluate_ready(current, authorities)
}
