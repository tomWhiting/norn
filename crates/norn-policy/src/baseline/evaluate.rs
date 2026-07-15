//! Pure legacy-exception comparison against immutable origin facts.

mod errors;
mod input;

pub use errors::{LegacyEvaluationError, LocCeilingsError};
pub use input::{CurrentRepositoryFacts, LocCeilings};

use std::collections::{BTreeMap, BTreeSet};

use super::governance::{LegacyGovernance, LegacyGovernanceEntry, LegacyState};
use super::items::compare_item_groups;
use super::model::{DebtOriginFact, OriginId, OriginLedger};
use super::production::ProductionFileFact;
use crate::path::RepositoryPath;
use crate::phase_lock::CampaignPhase;
use serde::Serialize;

/// Origin exception family.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacyKind {
    /// A production file above its applicable LOC limit.
    ProductionLoc,
    /// One prohibited-debt multiset occurrence.
    ProhibitedDebt,
    /// A stable production item group.
    ProductionItem,
}

/// Stable closed legacy-policy issue.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacyIssueCode {
    /// A current over-limit file has no immutable origin exception.
    NewLocException,
    /// An active legacy file changed while remaining over limit.
    LocChanged,
    /// An active legacy file reached or passed its due phase.
    LocOverdue,
    /// A persistently resolved LOC exception became active again.
    LocReactivated,
    /// A current debt occurrence has no immutable origin exception.
    NewDebtException,
    /// Production content changed under an active legacy suppression.
    DebtProductionChanged,
    /// An active legacy debt occurrence reached or passed its due phase.
    DebtOverdue,
    /// A persistently resolved debt occurrence reappeared.
    DebtReactivated,
    /// Current facts resolved an exception without recording that transition.
    ResolutionNotRecorded,
    /// Production occurrences moved behind a test-only predicate.
    ProductionHiddenAsTest,
}

/// One deterministic legacy comparison issue containing no source prose.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct LegacyIssue {
    code: LegacyIssueCode,
    kind: LegacyKind,
    origin_id: OriginId,
    path: RepositoryPath,
    hidden_count: Option<u32>,
}

impl LegacyIssue {
    /// Return the closed issue code.
    #[must_use]
    pub const fn code(&self) -> LegacyIssueCode {
        self.code
    }

    /// Return the affected immutable or newly derived origin identity.
    #[must_use]
    pub const fn origin_id(&self) -> OriginId {
        self.origin_id
    }

    /// Return the affected exception family.
    #[must_use]
    pub const fn kind(&self) -> LegacyKind {
        self.kind
    }

    /// Return the affected repository path.
    #[must_use]
    pub const fn path(&self) -> &RepositoryPath {
        &self.path
    }

    /// Return the reclassified occurrence count for item-hiding issues.
    #[must_use]
    pub const fn hidden_count(&self) -> Option<u32> {
        self.hidden_count
    }
}

/// Derived current disposition for one immutable exception.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct LegacyDisposition {
    origin_id: OriginId,
    kind: LegacyKind,
    state: LegacyState,
}

impl LegacyDisposition {
    /// Return the immutable exception identity.
    #[must_use]
    pub const fn origin_id(&self) -> OriginId {
        self.origin_id
    }

    /// Return the immutable exception family.
    #[must_use]
    pub const fn kind(&self) -> LegacyKind {
        self.kind
    }

    /// Return the current derived state.
    #[must_use]
    pub const fn state(&self) -> LegacyState {
        self.state
    }
}

/// Complete sorted result of pure legacy evaluation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LegacyEvaluation {
    issues: Vec<LegacyIssue>,
    dispositions: Vec<LegacyDisposition>,
}

impl LegacyEvaluation {
    /// Borrow every issue in deterministic order.
    #[must_use]
    pub fn issues(&self) -> &[LegacyIssue] {
        &self.issues
    }

    /// Borrow every immutable exception's current disposition.
    #[must_use]
    pub fn dispositions(&self) -> &[LegacyDisposition] {
        &self.dispositions
    }
}

/// Compare current immutable facts with origin and reviewed governance.
///
/// # Errors
///
/// Rejects governance that does not exactly cover immutable legacy exceptions
/// before evaluating current legacy state.
pub fn evaluate_legacy(
    current: &CurrentRepositoryFacts,
    origin: &OriginLedger,
    governance: &LegacyGovernance,
    limits: LocCeilings,
    active_phase: CampaignPhase,
) -> Result<LegacyEvaluation, LegacyEvaluationError> {
    governance
        .validate_against(origin, LocCeilings::p1_baseline())
        .map_err(LegacyEvaluationError::Governance)?;

    let mut issues = Vec::new();
    let mut dispositions = Vec::new();
    evaluate_loc(
        current,
        origin,
        governance,
        limits,
        active_phase,
        &mut issues,
        &mut dispositions,
    )?;
    evaluate_debt(
        current,
        origin,
        governance,
        active_phase,
        &mut issues,
        &mut dispositions,
    )?;
    evaluate_item_groups(current, origin, &mut issues)?;
    issues.sort();
    dispositions.sort();
    Ok(LegacyEvaluation {
        issues,
        dispositions,
    })
}

fn evaluate_item_groups(
    current: &CurrentRepositoryFacts,
    origin: &OriginLedger,
    issues: &mut Vec<LegacyIssue>,
) -> Result<(), LegacyEvaluationError> {
    let findings = compare_item_groups(origin.item_groups(), current.item_groups())
        .map_err(LegacyEvaluationError::ItemComparison)?;
    for finding in findings {
        issues.push(LegacyIssue {
            code: LegacyIssueCode::ProductionHiddenAsTest,
            kind: LegacyKind::ProductionItem,
            origin_id: finding.origin_id(),
            path: finding.path().clone(),
            hidden_count: Some(finding.hidden_count()),
        });
    }
    Ok(())
}

fn evaluate_loc(
    current: &CurrentRepositoryFacts,
    origin: &OriginLedger,
    governance: &LegacyGovernance,
    limits: LocCeilings,
    active_phase: CampaignPhase,
    issues: &mut Vec<LegacyIssue>,
    dispositions: &mut Vec<LegacyDisposition>,
) -> Result<(), LegacyEvaluationError> {
    let current_by_path: BTreeMap<&str, &ProductionFileFact> = current
        .production_files
        .iter()
        .map(|fact| (fact.path().as_str(), fact))
        .collect();
    let origin_by_id: BTreeMap<OriginId, &ProductionFileFact> = origin
        .production_files()
        .iter()
        .map(|fact| (fact.origin_id(), fact))
        .collect();
    let governance_by_id = governance.loc_map();
    let legacy_paths: BTreeSet<&str> = governance_by_id
        .keys()
        .filter_map(|id| origin_by_id.get(id).map(|fact| fact.path().as_str()))
        .collect();

    for (origin_id, entry) in governance_by_id {
        let origin_fact = origin_by_id
            .get(&origin_id)
            .copied()
            .ok_or(LegacyEvaluationError::OriginReference)?;
        let current_fact = current_by_path.get(origin_fact.path().as_str()).copied();
        evaluate_one_loc(
            origin_fact,
            current_fact,
            entry,
            limits,
            active_phase,
            issues,
            dispositions,
        );
    }
    for fact in &current.production_files {
        if limits.exceeded(fact) && !legacy_paths.contains(fact.path().as_str()) {
            issues.push(issue(
                LegacyIssueCode::NewLocException,
                LegacyKind::ProductionLoc,
                fact.origin_id(),
                fact.path(),
            ));
        }
    }
    Ok(())
}

fn evaluate_one_loc(
    origin: &ProductionFileFact,
    current: Option<&ProductionFileFact>,
    governance: &LegacyGovernanceEntry,
    limits: LocCeilings,
    active_phase: CampaignPhase,
    issues: &mut Vec<LegacyIssue>,
    dispositions: &mut Vec<LegacyDisposition>,
) {
    let still_over = current.is_some_and(|fact| limits.exceeded(fact));
    let state = if still_over {
        LegacyState::Active
    } else {
        LegacyState::Resolved
    };
    dispositions.push(LegacyDisposition {
        origin_id: origin.origin_id(),
        kind: LegacyKind::ProductionLoc,
        state,
    });

    if !still_over {
        if governance.state() == LegacyState::Active {
            issues.push(issue(
                LegacyIssueCode::ResolutionNotRecorded,
                LegacyKind::ProductionLoc,
                origin.origin_id(),
                origin.path(),
            ));
        }
        return;
    }
    if governance.state() == LegacyState::Resolved {
        issues.push(issue(
            LegacyIssueCode::LocReactivated,
            LegacyKind::ProductionLoc,
            origin.origin_id(),
            origin.path(),
        ));
        return;
    }

    let unchanged = current.is_some_and(|fact| {
        fact.origin_id() == origin.origin_id()
            && fact.production_loc() == origin.production_loc()
            && fact.projection_hash() == origin.projection_hash()
    });
    if !unchanged {
        issues.push(issue(
            LegacyIssueCode::LocChanged,
            LegacyKind::ProductionLoc,
            origin.origin_id(),
            origin.path(),
        ));
    } else if active_phase >= governance.due_phase() {
        issues.push(issue(
            LegacyIssueCode::LocOverdue,
            LegacyKind::ProductionLoc,
            origin.origin_id(),
            origin.path(),
        ));
    }
}

fn evaluate_debt(
    current: &CurrentRepositoryFacts,
    origin: &OriginLedger,
    governance: &LegacyGovernance,
    active_phase: CampaignPhase,
    issues: &mut Vec<LegacyIssue>,
    dispositions: &mut Vec<LegacyDisposition>,
) -> Result<(), LegacyEvaluationError> {
    let current_ids: BTreeSet<OriginId> = current
        .prohibited_debt
        .iter()
        .map(DebtOriginFact::origin_id)
        .collect();
    let origin_by_id: BTreeMap<OriginId, &DebtOriginFact> = origin
        .prohibited_debt()
        .iter()
        .map(|fact| (fact.origin_id(), fact))
        .collect();
    let origin_ids: BTreeSet<OriginId> = origin_by_id.keys().copied().collect();
    let governance_by_id = governance.debt_map();
    let current_production: BTreeMap<&str, &ProductionFileFact> = current
        .production_files
        .iter()
        .map(|fact| (fact.path().as_str(), fact))
        .collect();
    let origin_production: BTreeMap<&str, &ProductionFileFact> = origin
        .production_files()
        .iter()
        .map(|fact| (fact.path().as_str(), fact))
        .collect();

    for (origin_id, entry) in governance_by_id {
        let fact = origin_by_id
            .get(&origin_id)
            .copied()
            .ok_or(LegacyEvaluationError::OriginReference)?;
        let present = current_ids.contains(&origin_id);
        dispositions.push(LegacyDisposition {
            origin_id,
            kind: LegacyKind::ProhibitedDebt,
            state: if present {
                LegacyState::Active
            } else {
                LegacyState::Resolved
            },
        });
        evaluate_one_debt(
            fact,
            present,
            entry,
            active_phase,
            &origin_production,
            &current_production,
            issues,
        );
    }
    for fact in &current.prohibited_debt {
        if !origin_ids.contains(&fact.origin_id()) {
            issues.push(issue(
                LegacyIssueCode::NewDebtException,
                LegacyKind::ProhibitedDebt,
                fact.origin_id(),
                fact.path(),
            ));
        }
    }
    Ok(())
}

fn evaluate_one_debt(
    fact: &DebtOriginFact,
    present: bool,
    governance: &LegacyGovernanceEntry,
    active_phase: CampaignPhase,
    origin_production: &BTreeMap<&str, &ProductionFileFact>,
    current_production: &BTreeMap<&str, &ProductionFileFact>,
    issues: &mut Vec<LegacyIssue>,
) {
    if !present {
        if governance.state() == LegacyState::Active {
            issues.push(issue(
                LegacyIssueCode::ResolutionNotRecorded,
                LegacyKind::ProhibitedDebt,
                fact.origin_id(),
                fact.path(),
            ));
        }
        return;
    }
    if governance.state() == LegacyState::Resolved {
        issues.push(issue(
            LegacyIssueCode::DebtReactivated,
            LegacyKind::ProhibitedDebt,
            fact.origin_id(),
            fact.path(),
        ));
        return;
    }

    let changed_production = origin_production
        .get(fact.path().as_str())
        .is_some_and(|origin| {
            current_production
                .get(fact.path().as_str())
                .is_none_or(|current| {
                    current.origin_id() != origin.origin_id()
                        || current.production_loc() != origin.production_loc()
                        || current.projection_hash() != origin.projection_hash()
                })
        });
    if changed_production {
        issues.push(issue(
            LegacyIssueCode::DebtProductionChanged,
            LegacyKind::ProhibitedDebt,
            fact.origin_id(),
            fact.path(),
        ));
    } else if active_phase >= governance.due_phase() {
        issues.push(issue(
            LegacyIssueCode::DebtOverdue,
            LegacyKind::ProhibitedDebt,
            fact.origin_id(),
            fact.path(),
        ));
    }
}

fn issue(
    code: LegacyIssueCode,
    kind: LegacyKind,
    origin_id: OriginId,
    path: &RepositoryPath,
) -> LegacyIssue {
    LegacyIssue {
        code,
        kind,
        origin_id,
        path: path.clone(),
        hidden_count: None,
    }
}
