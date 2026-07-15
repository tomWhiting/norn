use std::error::Error;
use std::io;

use super::state::AuthorityView;
use super::{evaluate_p1, evaluate_with_fixture_authorities};
use crate::baseline::{
    LegacyGovernance, OriginLedger, P1_BASE_COMMIT, P1_BASE_TREE,
    P1_GENERATED_INCLUDE_TECHNICAL_IDENTITY, RepositoryBaselineFacts,
};
use crate::config::RepositoryPolicy;
use crate::facts::analyze_facts;
use crate::finding::EvidenceTraceabilityIssue;
use crate::phase_lock::{CampaignPhase, P1AuthorityError};
use crate::redaction::RedactionRegistry;
use crate::rust::modules::GeneratedIncludeRegistry;
use crate::version::{ANALYZER_VERSION, DIGEST_VERSION};
use crate::writers::{
    WRITER_ANALYZER_VERSION, WRITER_SCHEMA_VERSION, WriterFamilyRegistry, builtin_sink_registry,
};
use crate::{
    AuthorityIssue, CompleteCurrentSnapshot, CurrentFactIssue, EntryKind, GitObjectId,
    InvalidPolicy, OwnedSnapshot, P1BaseSnapshot, P1EvaluationInput, PolicyAuthority,
    PolicyInvalidReason, PolicyState, RepositoryPath, SnapshotEntry, digest_bytes,
};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const MANIFEST: &[u8] =
    b"[workspace]\n[package]\nname = \"fixture\"\nedition = \"2024\"\nbuild = false\n";
const SOURCE: &[u8] = b"pub fn stable_value() -> u8 { 7 }\n";
const TRYBUILD_MANIFEST: &[u8] = concat!(
    "[workspace]\n",
    "[package]\nname = \"fixture\"\nedition = \"2024\"\nbuild = false\n",
    "[dev-dependencies]\ntrybuild = \"1\"\n",
)
.as_bytes();
const TRYBUILD_LOCK: &[u8] = br#"
version = 4
[[package]]
name = "fixture"
version = "0.0.0"
dependencies = ["trybuild"]
[[package]]
name = "trybuild"
version = "1.0.117"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "0710d4dfbeae4f9c390baa784c49858a7468fa433f3fe5d0ec5ebef651cf59f9"
"#;

struct FixtureAuthorities {
    policy: RepositoryPolicy,
    generated: GeneratedIncludeRegistry,
    origin: OriginLedger,
    governance: LegacyGovernance,
    writers: WriterFamilyRegistry,
    redaction: RedactionRegistry,
}

impl FixtureAuthorities {
    fn from_base(base: &OwnedSnapshot) -> TestResult<Self> {
        let policy = policy()?;
        let generated = GeneratedIncludeRegistry::empty();
        let origin = origin(base, &generated, &policy)?;
        let governance = empty_governance()?;
        let writers = empty_writer_families()?;
        let redaction = RedactionRegistry::new(Vec::new(), Vec::new())?;
        Ok(Self {
            policy,
            generated,
            origin,
            governance,
            writers,
            redaction,
        })
    }

    const fn view(&self) -> AuthorityView<'_> {
        AuthorityView {
            repository_policy: &self.policy,
            generated_includes: &self.generated,
            origin: &self.origin,
            governance: &self.governance,
            writer_families: &self.writers,
            redaction: &self.redaction,
            active_phase: CampaignPhase::P1,
        }
    }
}

#[test]
fn state_triad_is_explicit_and_marker_absence_is_narrow() -> TestResult {
    assert_eq!(
        evaluate_candidate(OwnedSnapshot::empty())?,
        PolicyState::Absent
    );

    let invalid_marker = snapshot(&[("policy/phase-lock.json", EntryKind::Regular, b"{}")])?;
    let invalid = evaluate_candidate(invalid_marker)?;
    assert!(matches!(
        invalid,
        PolicyState::Invalid(ref state)
            if matches!(
                state.reason(),
                PolicyInvalidReason::Authority {
                    authority: Some(PolicyAuthority::PhaseLock),
                    issue: AuthorityIssue::Invalid,
                }
            )
    ));

    let current = valid_snapshot(false)?;
    let authorities = FixtureAuthorities::from_base(&current)?;
    assert!(matches!(
        evaluate_with_fixture_authorities(&current, authorities.view()),
        PolicyState::Ready(_)
    ));
    Ok(())
}

#[test]
fn ready_report_is_order_independent_and_canonically_sorted() -> TestResult {
    let left = valid_snapshot(false)?;
    let right = valid_snapshot(true)?;
    let authorities = FixtureAuthorities::from_base(&left)?;
    let left = ready(evaluate_with_fixture_authorities(&left, authorities.view()))?;
    let right = ready(evaluate_with_fixture_authorities(
        &right,
        authorities.view(),
    ))?;

    assert_eq!(left, right);
    assert!(left.findings().windows(2).all(|pair| pair[0] <= pair[1]));
    assert_eq!(serde_json::to_vec(&left)?, serde_json::to_vec(&right)?);
    Ok(())
}

#[test]
fn current_fact_failure_is_invalid_instead_of_an_empty_report() -> TestResult {
    let base = valid_snapshot(false)?;
    let authorities = FixtureAuthorities::from_base(&base)?;
    let malformed = snapshot(&[("Cargo.toml", EntryKind::Regular, b"not toml")])?;

    let state = evaluate_with_fixture_authorities(&malformed, authorities.view());
    assert!(matches!(
        state,
        PolicyState::Invalid(ref invalid)
            if matches!(
                invalid.reason(),
                PolicyInvalidReason::CurrentFacts {
                    issue: CurrentFactIssue::Cargo,
                    ..
                }
            )
    ));
    Ok(())
}

#[test]
fn compile_fixture_provenance_drift_is_never_ready() -> TestResult {
    let base = compile_fixture_snapshot("compile_fail", false, false)?;
    let authorities = FixtureAuthorities::from_base(&base)?;

    for current in [
        compile_fixture_snapshot("pass", false, false)?,
        compile_fixture_snapshot("compile_fail", true, false)?,
    ] {
        let state = evaluate_with_fixture_authorities(&current, authorities.view());
        assert!(matches!(
            state,
            PolicyState::Invalid(ref invalid)
                if invalid.reason() == &PolicyInvalidReason::CompileTestFixtureDrift
        ));
    }

    let deleted = compile_fixture_snapshot("compile_fail", false, true)?;
    assert!(matches!(
        evaluate_with_fixture_authorities(&deleted, authorities.view()),
        PolicyState::Invalid(ref invalid)
            if matches!(
                invalid.reason(),
                PolicyInvalidReason::CurrentFacts {
                    issue: CurrentFactIssue::Modules,
                    ..
                }
            )
    ));
    Ok(())
}

#[test]
fn a_present_marker_cannot_downgrade_to_absent() -> TestResult {
    let marker = snapshot(&[("policy/phase-lock.json", EntryKind::Symlink, b"elsewhere")])?;
    let state = evaluate_candidate(marker)?;
    assert!(matches!(
        state,
        PolicyState::Invalid(ref invalid)
            if matches!(
                invalid.reason(),
                PolicyInvalidReason::Authority {
                    authority: Some(PolicyAuthority::PhaseLock),
                    issue: AuthorityIssue::NotRegular,
                }
            )
    ));
    Ok(())
}

#[test]
fn ready_join_emits_module_loc_debt_writer_and_redaction_findings() -> TestResult {
    let base = valid_snapshot(false)?;
    let authorities = FixtureAuthorities::from_base(&base)?;
    let mut oversized = String::new();
    for ordinal in 0..501_u16 {
        oversized.push_str("pub const VALUE_");
        oversized.push_str(&ordinal.to_string());
        oversized.push_str(": u16 = ");
        oversized.push_str(&ordinal.to_string());
        oversized.push_str(";\n");
    }
    oversized.push_str("pub fn prohibited() { ");
    oversized.push_str(&["pan", "ic!();"].concat());
    oversized.push_str(" }\n");
    let rich = owned_snapshot(vec![
        ("Cargo.toml", EntryKind::Regular, MANIFEST.to_vec()),
        (
            "docs/reviews/evidence/p1/unregistered.json",
            EntryKind::Regular,
            b"{}".to_vec(),
        ),
        (
            "src/lib.rs",
            EntryKind::Regular,
            b"mod oversized;\nmod policy;\n".to_vec(),
        ),
        (
            "src/oversized.rs",
            EntryKind::Regular,
            oversized.into_bytes(),
        ),
        (
            "src/policy/mod.rs",
            EntryKind::Regular,
            b"pub fn logic_is_not_a_declaration() {}\n".to_vec(),
        ),
    ])?;
    let report = ready(evaluate_with_fixture_authorities(&rich, authorities.view()))?;
    let codes = report
        .findings()
        .iter()
        .map(crate::Finding::code)
        .collect::<Vec<_>>();
    assert!(codes.contains(&crate::FindingCode::ModuleShape));
    assert!(codes.contains(&crate::FindingCode::ProductionLocExceeded));
    assert!(codes.contains(&crate::FindingCode::ProhibitedDebt));
    assert!(codes.contains(&crate::FindingCode::UnknownWriterSink));
    assert!(codes.contains(&crate::FindingCode::EvidenceRedaction));
    Ok(())
}

#[test]
fn ready_join_reports_production_items_hidden_as_tests() -> TestResult {
    let base = valid_snapshot(false)?;
    let authorities = FixtureAuthorities::from_base(&base)?;
    let hidden = snapshot(&[
        ("Cargo.toml", EntryKind::Regular, MANIFEST),
        (
            "src/lib.rs",
            EntryKind::Regular,
            b"#[cfg(test)]\npub fn stable_value() -> u8 { 7 }\n",
        ),
    ])?;
    let report = ready(evaluate_with_fixture_authorities(
        &hidden,
        authorities.view(),
    ))?;
    assert!(
        report
            .findings()
            .iter()
            .any(|finding| finding.code() == crate::FindingCode::ProductionHiddenAsTest)
    );
    Ok(())
}

fn ready(state: PolicyState) -> TestResult<crate::PolicyReport> {
    match state {
        PolicyState::Ready(report) => Ok(report),
        PolicyState::Absent | PolicyState::Invalid(_) => {
            Err(io::Error::other("fixture evaluation was not ready").into())
        }
    }
}

fn valid_snapshot(reverse: bool) -> TestResult<OwnedSnapshot> {
    let entries = if reverse {
        vec![
            ("src/lib.rs", EntryKind::Regular, SOURCE.to_vec()),
            ("Cargo.toml", EntryKind::Regular, MANIFEST.to_vec()),
        ]
    } else {
        vec![
            ("Cargo.toml", EntryKind::Regular, MANIFEST.to_vec()),
            ("src/lib.rs", EntryKind::Regular, SOURCE.to_vec()),
        ]
    };
    owned_snapshot(entries)
}

fn compile_fixture_snapshot(
    method: &str,
    include_extra: bool,
    omit_selected: bool,
) -> TestResult<OwnedSnapshot> {
    let selector = if include_extra {
        format!("cases.{method}(\"tests/ui/*.rs\");")
    } else {
        format!("cases.{method}(\"tests/ui/case.rs\");")
    };
    let harness =
        format!("#[test]\nfn ui() {{\nlet cases = trybuild::TestCases::new();\n{selector}\n}}\n");
    let mut entries = vec![
        ("Cargo.toml", EntryKind::Regular, TRYBUILD_MANIFEST.to_vec()),
        ("Cargo.lock", EntryKind::Regular, TRYBUILD_LOCK.to_vec()),
        ("src/lib.rs", EntryKind::Regular, Vec::new()),
        ("tests/harness.rs", EntryKind::Regular, harness.into_bytes()),
    ];
    if !omit_selected {
        entries.push((
            "tests/ui/case.rs",
            EntryKind::Regular,
            b"fn main() {}\n".to_vec(),
        ));
    }
    if include_extra {
        entries.push((
            "tests/ui/extra.rs",
            EntryKind::Regular,
            b"fn main() {}\n".to_vec(),
        ));
    }
    owned_snapshot(entries)
}

fn snapshot(entries: &[(&str, EntryKind, &[u8])]) -> TestResult<OwnedSnapshot> {
    owned_snapshot(
        entries
            .iter()
            .map(|(path, kind, bytes)| (*path, *kind, bytes.to_vec()))
            .collect(),
    )
}

fn owned_snapshot(entries: Vec<(&str, EntryKind, Vec<u8>)>) -> TestResult<OwnedSnapshot> {
    let entries = entries
        .into_iter()
        .map(|(path, kind, bytes)| {
            Ok((
                RepositoryPath::parse(path)?,
                SnapshotEntry::new(kind, bytes),
            ))
        })
        .collect::<Result<Vec<_>, crate::RepositoryPathError>>()?;
    Ok(OwnedSnapshot::try_from_entries(entries)?)
}

fn policy() -> TestResult<RepositoryPolicy> {
    let document = format!(
        "schema_version = 1\n\n[algorithms]\nanalyzer = \"{ANALYZER_VERSION}\"\ndigest = \"{DIGEST_VERSION}\"\n\n[production_loc]\nentrypoint_max = 200\nother_rust_max = 500\n"
    );
    Ok(RepositoryPolicy::decode(document.as_bytes())?)
}

fn origin(
    base: &OwnedSnapshot,
    generated: &GeneratedIncludeRegistry,
    policy: &RepositoryPolicy,
) -> TestResult<OriginLedger> {
    let facts = analyze_facts(base, generated);
    let baseline = RepositoryBaselineFacts::try_from_repository(&facts)?;
    let document = serde_json::json!({
        "schema_version": 1,
        "algorithms": {
            "analyzer": ANALYZER_VERSION,
            "digest": DIGEST_VERSION,
        },
        "base": {
            "commit": P1_BASE_COMMIT,
            "tree": P1_BASE_TREE,
        },
        "digests": {
            "repository_policy": policy.normalized_digest()?,
            "source_inventory": baseline.source_inventory_digest(),
            "generated_include_registry": P1_GENERATED_INCLUDE_TECHNICAL_IDENTITY,
        },
        "source_inventory": baseline.source_inventory(),
        "compile_test_fixtures": baseline.compile_test_fixtures(),
        "production_files": baseline.production_files(),
        "item_groups": baseline.item_groups(),
        "prohibited_debt": baseline.prohibited_debt(),
        "writer_operations": baseline.writer_operations(),
    });
    Ok(OriginLedger::decode_p1(&serde_json::to_vec(&document)?)?)
}

fn empty_governance() -> TestResult<LegacyGovernance> {
    let document = format!(
        "schema_version = 1\nloc_exceptions = []\ndebt_exceptions = []\n\n[algorithms]\nanalyzer = \"{ANALYZER_VERSION}\"\ndigest = \"{DIGEST_VERSION}\"\n"
    );
    Ok(LegacyGovernance::decode(document.as_bytes())?)
}

fn empty_writer_families() -> TestResult<WriterFamilyRegistry> {
    let sink = builtin_sink_registry()?.digest();
    let resolutions = digest_bytes(b"fixture-writer-resolutions");
    let document = format!(
        "schema_version = {WRITER_SCHEMA_VERSION}\nsink_registry = \"{sink}\"\nwriter_resolutions = \"{resolutions}\"\nclassifications = []\n\n[algorithms]\nwriter = \"{WRITER_ANALYZER_VERSION}\"\ndigest = \"{DIGEST_VERSION}\"\n\n[vocabulary]\nfamilies = []\nshared_primitives = []\ncleanup_reviews = []\nfalse_positive_reviews = []\n"
    );
    Ok(WriterFamilyRegistry::decode_p1(document.as_bytes())?)
}

#[test]
fn invalid_state_does_not_retain_snapshot_or_private_value_identity() -> TestResult {
    let private_value = ["norn", "-private-prompt-", "sentinel"].concat();
    let current = snapshot(&[(
        "policy/phase-lock.json",
        EntryKind::Regular,
        private_value.as_bytes(),
    )])?;
    let snapshot_identity = current.canonical_identity().to_string();
    let value_identity = digest_bytes(private_value.as_bytes()).to_string();
    let state = evaluate_candidate(current)?;
    assert!(matches!(state, PolicyState::Invalid(_)));

    let serialized = serde_json::to_vec(&state)?;
    let rendered = format!("{state:?}");
    for prohibited in [
        private_value.as_bytes(),
        snapshot_identity.as_bytes(),
        value_identity.as_bytes(),
    ] {
        assert!(
            !serialized
                .windows(prohibited.len())
                .any(|window| window == prohibited)
        );
        assert!(
            !rendered
                .as_bytes()
                .windows(prohibited.len())
                .any(|window| window == prohibited)
        );
    }
    Ok(())
}

#[test]
fn structured_traceability_state_preserves_only_closed_issue_and_count() -> TestResult {
    let private_path = ["docs/reviews/evidence/p1/", "private-fixture.json"].concat();
    let private_fixture_id = ["private", "-fixture-id"].concat();
    let private_source = ["private", " source finding text"].concat();
    let private_bytes = ["private", " response bytes"].concat();
    let snapshot_digest = digest_bytes(private_path.as_bytes()).to_string();
    let value_digest = digest_bytes(private_bytes.as_bytes()).to_string();

    for (issue, count) in [
        (EvidenceTraceabilityIssue::FindingMissing, 2_u64),
        (EvidenceTraceabilityIssue::SourceMismatch, 3_u64),
        (EvidenceTraceabilityIssue::EvidenceMissing, 5_u64),
    ] {
        let error = P1AuthorityError::EvidenceTraceability { issue, count };
        let invalid = InvalidPolicy::authority(&error);
        assert!(matches!(
            invalid.reason(),
            PolicyInvalidReason::Authority {
                authority: Some(PolicyAuthority::ResponsesContract),
                issue: AuthorityIssue::EvidenceTraceability {
                    issue: observed_issue,
                    count: observed_count,
                },
            } if *observed_issue == issue && *observed_count == count
        ));

        let state = PolicyState::Invalid(invalid);
        let serialized = serde_json::to_vec(&state)?;
        let rendered = format!("{state:?}");
        for prohibited in [
            private_path.as_bytes(),
            private_fixture_id.as_bytes(),
            private_source.as_bytes(),
            private_bytes.as_bytes(),
            snapshot_digest.as_bytes(),
            value_digest.as_bytes(),
        ] {
            assert!(
                !serialized
                    .windows(prohibited.len())
                    .any(|window| window == prohibited)
            );
            assert!(
                !rendered
                    .as_bytes()
                    .windows(prohibited.len())
                    .any(|window| window == prohibited)
            );
        }
    }
    Ok(())
}

fn evaluate_candidate(current: OwnedSnapshot) -> TestResult<PolicyState> {
    let current = CompleteCurrentSnapshot::from_complete_snapshot(current);
    let commit = GitObjectId::parse(P1_BASE_COMMIT)?;
    let tree = GitObjectId::parse(P1_BASE_TREE)?;
    let base = P1BaseSnapshot::try_from_git_tree(commit, tree, std::iter::empty())?;
    Ok(evaluate_p1(P1EvaluationInput::new(&current, &base)))
}
