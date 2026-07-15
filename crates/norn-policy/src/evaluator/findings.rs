//! Lossless conversion from joined canonical facts to stable hard findings.

use thiserror::Error;

use super::state::AuthorityView;
use crate::baseline::{
    CurrentRepositoryFacts, DebtOriginFact, LegacyEvaluation, LegacyGovernance, LegacyIssue,
    LegacyIssueCode, LegacyKind as BaselineLegacyKind, LocCeilings, OriginLedger,
};
use crate::debt::{
    DebtConstructKind as CanonicalDebtConstruct, DebtOccurrence,
    DebtTargetKind as CanonicalDebtTarget,
};
use crate::facts::RepositoryFacts;
use crate::finding::{
    ByteSpan, DebtConstructKind, DebtTargetKind, Finding, FindingPhase, LegacyChangeIssue,
    LegacyKind, ModuleShapeIssue, RepositoryFinding,
};
use crate::phase_lock::CampaignPhase;
use crate::redaction::validate_retained_artifacts;
use crate::rust::ModuleShapeKind;
use crate::writers::classify::validate_classifications_for_operations;
use crate::writers::{
    ClassificationIssue, WriterInventory, WriterOperationId, canonical_writer_findings,
};
use crate::{OwnedSnapshot, RepositoryPath};

pub(super) fn canonical_findings(
    snapshot: &OwnedSnapshot,
    facts: &RepositoryFacts,
    current: &CurrentRepositoryFacts,
    legacy: &LegacyEvaluation,
    limits: LocCeilings,
    authorities: AuthorityView<'_>,
) -> Result<Vec<Finding>, FindingBuildError> {
    let mut findings = Vec::new();
    append_module_shape(facts, &mut findings)?;
    append_legacy(
        LegacyContext {
            facts,
            current,
            legacy,
            limits,
            origin: authorities.origin,
            governance: authorities.governance,
        },
        &mut findings,
    )?;
    append_writers(
        facts,
        authorities.origin,
        authorities.writer_families,
        &mut findings,
    )?;
    findings.extend(
        validate_retained_artifacts(authorities.redaction, snapshot)
            .into_iter()
            .map(crate::redaction::RedactionViolation::into_finding),
    );
    findings.sort();
    Ok(findings)
}

#[derive(Clone, Copy)]
struct LegacyContext<'a> {
    facts: &'a RepositoryFacts,
    current: &'a CurrentRepositoryFacts,
    legacy: &'a LegacyEvaluation,
    limits: LocCeilings,
    origin: &'a OriginLedger,
    governance: &'a LegacyGovernance,
}

fn append_module_shape(
    facts: &RepositoryFacts,
    findings: &mut Vec<Finding>,
) -> Result<(), FindingBuildError> {
    for file in facts.production_files() {
        for violation in &file.module_shape {
            findings.push(Finding::repository(
                file.path.clone(),
                Some(span_from_usize(violation.start, violation.end)?),
                RepositoryFinding::ModuleShape {
                    construct_kind: module_shape_issue(violation.kind),
                },
            ));
        }
    }
    Ok(())
}

fn append_legacy(
    context: LegacyContext<'_>,
    findings: &mut Vec<Finding>,
) -> Result<(), FindingBuildError> {
    for issue in context.legacy.issues() {
        let finding = match issue.code() {
            LegacyIssueCode::NewLocException => {
                new_loc_finding(context.current, issue, context.limits)?
            }
            LegacyIssueCode::NewDebtException => new_debt_finding(context.facts, issue)?,
            LegacyIssueCode::ProductionHiddenAsTest => Finding::repository(
                issue.path().clone(),
                None,
                RepositoryFinding::ProductionHiddenAsTest {
                    fingerprint: issue.origin_id().digest(),
                    count: u64::from(
                        issue
                            .hidden_count()
                            .ok_or(FindingBuildError::LegacyReference)?,
                    ),
                },
            ),
            LegacyIssueCode::LocOverdue | LegacyIssueCode::DebtOverdue => Finding::repository(
                issue.path().clone(),
                None,
                RepositoryFinding::LegacyExceptionOverdue {
                    origin: issue.origin_id().digest(),
                    kind: legacy_kind(issue.kind()),
                    due_phase: finding_phase(governance_due(context.governance, issue)?),
                },
            ),
            LegacyIssueCode::LocChanged
            | LegacyIssueCode::LocReactivated
            | LegacyIssueCode::DebtProductionChanged
            | LegacyIssueCode::DebtReactivated
            | LegacyIssueCode::ResolutionNotRecorded => Finding::repository(
                issue.path().clone(),
                None,
                RepositoryFinding::LegacyExceptionChanged {
                    origin: issue.origin_id().digest(),
                    kind: legacy_kind(issue.kind()),
                    issue: legacy_change(issue.code())?,
                },
            ),
        };
        findings.push(finding);
    }
    if context
        .legacy
        .issues()
        .iter()
        .any(|issue| !origin_contains_issue(context.origin, issue))
    {
        return Err(FindingBuildError::LegacyReference);
    }
    Ok(())
}

fn new_loc_finding(
    current: &CurrentRepositoryFacts,
    issue: &LegacyIssue,
    limits: LocCeilings,
) -> Result<Finding, FindingBuildError> {
    let fact = current
        .production_files()
        .iter()
        .find(|fact| fact.origin_id() == issue.origin_id())
        .ok_or(FindingBuildError::LegacyReference)?;
    Ok(Finding::repository(
        fact.path().clone(),
        None,
        RepositoryFinding::ProductionLocExceeded {
            actual: u64::from(fact.production_loc()),
            limit: u64::from(limits.limit_for(fact)),
        },
    ))
}

fn new_debt_finding(
    facts: &RepositoryFacts,
    issue: &LegacyIssue,
) -> Result<Finding, FindingBuildError> {
    let occurrence = facts
        .debt()
        .iter()
        .find(|debt| DebtOriginFact::from(*debt).origin_id() == issue.origin_id())
        .ok_or(FindingBuildError::LegacyReference)?;
    Ok(debt_finding(occurrence))
}

fn debt_finding(occurrence: &DebtOccurrence) -> Finding {
    Finding::repository(
        occurrence.path().clone(),
        Some(occurrence.span()),
        RepositoryFinding::ProhibitedDebt {
            target_kind: debt_target(occurrence.target().kind()),
            construct_kind: debt_construct(occurrence.construct()),
            fingerprint: occurrence.fingerprint(),
        },
    )
}

fn append_writers(
    facts: &RepositoryFacts,
    origin: &OriginLedger,
    families: &crate::writers::WriterFamilyRegistry,
    findings: &mut Vec<Finding>,
) -> Result<(), FindingBuildError> {
    let inventory = facts.writers().ok_or(FindingBuildError::WriterInventory)?;
    let Ok(writer_findings) = canonical_writer_findings(inventory) else {
        return Err(FindingBuildError::WriterFindings);
    };
    findings.extend(writer_findings);
    let operations = inventory
        .operations()
        .iter()
        .map(crate::writers::WriterOperation::id)
        .chain(
            origin
                .writer_operations()
                .iter()
                .map(|operation| WriterOperationId::new(operation.operation_id())),
        );
    for issue in validate_classifications_for_operations(operations, families.classifications()) {
        let operation = classification_operation(&issue);
        let (path, span) = writer_location(inventory, origin, operation)?;
        findings.push(Finding::repository(
            path,
            Some(span),
            RepositoryFinding::WriterClassification {
                issue: issue.into(),
            },
        ));
    }
    Ok(())
}

fn writer_location(
    inventory: &WriterInventory,
    origin: &OriginLedger,
    operation: WriterOperationId,
) -> Result<(RepositoryPath, ByteSpan), FindingBuildError> {
    if let Some(current) = inventory
        .operations()
        .iter()
        .find(|candidate| candidate.id() == operation)
    {
        return Ok((current.path().clone(), current.span()));
    }
    let operation = operation.digest();
    let prior = origin
        .writer_operations()
        .iter()
        .find(|candidate| candidate.operation_id() == operation)
        .ok_or(FindingBuildError::WriterLocation)?;
    let (start, end) = prior.span();
    Ok((prior.path().clone(), ByteSpan::new(start, end)?))
}

const fn classification_operation(issue: &ClassificationIssue) -> WriterOperationId {
    match issue {
        ClassificationIssue::Missing { operation }
        | ClassificationIssue::Duplicate { operation }
        | ClassificationIssue::Stale { operation }
        | ClassificationIssue::SharedEdges { operation } => *operation,
    }
}

fn governance_due(
    governance: &LegacyGovernance,
    issue: &LegacyIssue,
) -> Result<CampaignPhase, FindingBuildError> {
    let entries = match issue.kind() {
        BaselineLegacyKind::ProductionLoc => governance.loc_exceptions(),
        BaselineLegacyKind::ProhibitedDebt => governance.debt_exceptions(),
        BaselineLegacyKind::ProductionItem => return Err(FindingBuildError::LegacyReference),
    };
    entries
        .iter()
        .find(|entry| entry.origin_id() == issue.origin_id())
        .map(crate::baseline::LegacyGovernanceEntry::due_phase)
        .ok_or(FindingBuildError::LegacyReference)
}

fn origin_contains_issue(origin: &OriginLedger, issue: &LegacyIssue) -> bool {
    match issue.code() {
        LegacyIssueCode::NewLocException | LegacyIssueCode::NewDebtException => true,
        LegacyIssueCode::ProductionHiddenAsTest => origin
            .item_groups()
            .iter()
            .any(|fact| fact.origin_id() == issue.origin_id()),
        LegacyIssueCode::LocChanged
        | LegacyIssueCode::LocOverdue
        | LegacyIssueCode::LocReactivated => origin
            .production_files()
            .iter()
            .any(|fact| fact.origin_id() == issue.origin_id()),
        LegacyIssueCode::DebtProductionChanged
        | LegacyIssueCode::DebtOverdue
        | LegacyIssueCode::DebtReactivated
        | LegacyIssueCode::ResolutionNotRecorded => match issue.kind() {
            BaselineLegacyKind::ProductionLoc => origin
                .production_files()
                .iter()
                .any(|fact| fact.origin_id() == issue.origin_id()),
            BaselineLegacyKind::ProhibitedDebt => origin
                .prohibited_debt()
                .iter()
                .any(|fact| fact.origin_id() == issue.origin_id()),
            BaselineLegacyKind::ProductionItem => false,
        },
    }
}

const fn module_shape_issue(kind: ModuleShapeKind) -> ModuleShapeIssue {
    match kind {
        ModuleShapeKind::InlineModule => ModuleShapeIssue::InlineModule,
        ModuleShapeKind::PrivateUse => ModuleShapeIssue::PrivateUse,
        ModuleShapeKind::OtherItem => ModuleShapeIssue::OtherItem,
        ModuleShapeKind::UnattachedAttribute => ModuleShapeIssue::UnattachedAttribute,
    }
}

const fn legacy_kind(kind: BaselineLegacyKind) -> LegacyKind {
    match kind {
        BaselineLegacyKind::ProductionLoc => LegacyKind::ProductionLoc,
        BaselineLegacyKind::ProhibitedDebt => LegacyKind::ProhibitedDebt,
        BaselineLegacyKind::ProductionItem => LegacyKind::ProductionItem,
    }
}

const fn legacy_change(code: LegacyIssueCode) -> Result<LegacyChangeIssue, FindingBuildError> {
    match code {
        LegacyIssueCode::LocChanged => Ok(LegacyChangeIssue::LocChanged),
        LegacyIssueCode::LocReactivated => Ok(LegacyChangeIssue::LocReactivated),
        LegacyIssueCode::DebtProductionChanged => Ok(LegacyChangeIssue::DebtProductionChanged),
        LegacyIssueCode::DebtReactivated => Ok(LegacyChangeIssue::DebtReactivated),
        LegacyIssueCode::ResolutionNotRecorded => Ok(LegacyChangeIssue::ResolutionNotRecorded),
        LegacyIssueCode::NewLocException => Ok(LegacyChangeIssue::NewLocException),
        LegacyIssueCode::NewDebtException => Ok(LegacyChangeIssue::NewDebtException),
        LegacyIssueCode::LocOverdue
        | LegacyIssueCode::DebtOverdue
        | LegacyIssueCode::ProductionHiddenAsTest => Err(FindingBuildError::LegacyReference),
    }
}

const fn finding_phase(phase: CampaignPhase) -> FindingPhase {
    match phase {
        CampaignPhase::P1 => FindingPhase::P1,
        CampaignPhase::P2 => FindingPhase::P2,
        CampaignPhase::P3 => FindingPhase::P3,
        CampaignPhase::P4 => FindingPhase::P4,
        CampaignPhase::P5 => FindingPhase::P5,
        CampaignPhase::P6 => FindingPhase::P6,
        CampaignPhase::P7 => FindingPhase::P7,
        CampaignPhase::P8 => FindingPhase::P8,
        CampaignPhase::P9 => FindingPhase::P9,
    }
}

const fn debt_target(kind: CanonicalDebtTarget) -> DebtTargetKind {
    match kind {
        CanonicalDebtTarget::Library => DebtTargetKind::Library,
        CanonicalDebtTarget::ProcMacro => DebtTargetKind::ProcMacro,
        CanonicalDebtTarget::Binary => DebtTargetKind::Binary,
        CanonicalDebtTarget::Example => DebtTargetKind::Example,
        CanonicalDebtTarget::BuildScript => DebtTargetKind::BuildScript,
        CanonicalDebtTarget::IntegrationTest => DebtTargetKind::IntegrationTest,
        CanonicalDebtTarget::Benchmark => DebtTargetKind::Benchmark,
    }
}

const fn debt_construct(kind: CanonicalDebtConstruct) -> DebtConstructKind {
    match kind {
        CanonicalDebtConstruct::AllowAttribute => DebtConstructKind::AllowAttribute,
        CanonicalDebtConstruct::ExpectAttribute => DebtConstructKind::ExpectAttribute,
        CanonicalDebtConstruct::IgnoreAttribute => DebtConstructKind::IgnoreAttribute,
        CanonicalDebtConstruct::ImpossibleCfg => DebtConstructKind::ImpossibleCfg,
        CanonicalDebtConstruct::UnderscoreBinding => DebtConstructKind::UnderscoreBinding,
        CanonicalDebtConstruct::UnwrapCall => DebtConstructKind::UnwrapCall,
        CanonicalDebtConstruct::UnwrapErrCall => DebtConstructKind::UnwrapErrCall,
        CanonicalDebtConstruct::ExpectCall => DebtConstructKind::ExpectCall,
        CanonicalDebtConstruct::ExpectErrCall => DebtConstructKind::ExpectErrCall,
        CanonicalDebtConstruct::PanicMacro => DebtConstructKind::PanicMacro,
        CanonicalDebtConstruct::TodoMacro => DebtConstructKind::TodoMacro,
        CanonicalDebtConstruct::UnimplementedMacro => DebtConstructKind::UnimplementedMacro,
        CanonicalDebtConstruct::UnreachableMacro => DebtConstructKind::UnreachableMacro,
        CanonicalDebtConstruct::TodoMarker => DebtConstructKind::TodoMarker,
        CanonicalDebtConstruct::FixmeMarker => DebtConstructKind::FixmeMarker,
        CanonicalDebtConstruct::HackMarker => DebtConstructKind::HackMarker,
    }
}

fn span_from_usize(start: usize, end: usize) -> Result<ByteSpan, FindingBuildError> {
    let Ok(start) = u64::try_from(start) else {
        return Err(FindingBuildError::Span);
    };
    let Ok(end) = u64::try_from(end) else {
        return Err(FindingBuildError::Span);
    };
    ByteSpan::new(start, end).map_err(Into::into)
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(super) enum FindingBuildError {
    #[error("canonical writer inventory is unavailable")]
    WriterInventory,
    #[error("canonical writer findings could not be constructed")]
    WriterFindings,
    #[error("writer classification has no current or origin location")]
    WriterLocation,
    #[error("legacy evaluation references no complete current or origin fact")]
    LegacyReference,
    #[error("a finding span cannot be represented")]
    Span,
}

impl From<crate::finding::ByteSpanError> for FindingBuildError {
    fn from(_: crate::finding::ByteSpanError) -> Self {
        Self::Span
    }
}
