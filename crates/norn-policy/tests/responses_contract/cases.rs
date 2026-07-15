use std::error::Error;
use std::io;

use norn_policy::finding::EvidenceTraceabilityIssue;
use norn_policy::{ResponsesContractAuthority, ResponsesContractError};

use super::support::{CONTRACT_PINS, Corpus, PUBLIC_REQUEST, TRACEABILITY, TestResult};

#[test]
fn acquires_every_transitive_authority_from_one_snapshot() -> TestResult<()> {
    let corpus = Corpus::valid()?;
    let forward = ResponsesContractAuthority::acquire(&corpus.snapshot()?)?;
    let reversed = ResponsesContractAuthority::acquire(&corpus.reversed_snapshot()?)?;

    assert_eq!(forward.public_fixture_count(), 26);
    assert_eq!(forward.codex_fixture_count(), 13);
    assert_eq!(forward.governed_file_count(), 52);
    assert_eq!(forward.digest(), reversed.digest());
    Ok(())
}

#[test]
fn aggregate_identity_changes_with_valid_governed_content() -> TestResult<()> {
    let mut corpus = Corpus::valid()?;
    let before = ResponsesContractAuthority::acquire(&corpus.snapshot()?)?;
    corpus.revise_matrix_text()?;
    let after = ResponsesContractAuthority::acquire(&corpus.snapshot()?)?;

    assert_ne!(before.digest(), after.digest());
    Ok(())
}

#[test]
fn rejects_duplicate_control_members_before_schema_decode() -> TestResult<()> {
    let mut corpus = Corpus::valid()?;
    let bytes = std::str::from_utf8(corpus.bytes(CONTRACT_PINS)?)?;
    let duplicate = bytes.replacen(
        "\"schema_version\": 1",
        "\"schema_version\": 1, \"schema_version\": 1",
        1,
    );
    if duplicate == bytes {
        return Err(io::Error::other("duplicate-key mutation did not change the fixture").into());
    }
    corpus.replace(CONTRACT_PINS, duplicate.into_bytes());

    assert!(matches!(
        ResponsesContractAuthority::acquire(&corpus.snapshot()?),
        Err(ResponsesContractError::Json)
    ));
    Ok(())
}

#[test]
fn rejects_any_unlisted_entry_beneath_the_fixture_root() -> TestResult<()> {
    let mut corpus = Corpus::valid()?;
    corpus.insert(
        "crates/norn/testdata/openai_responses/public/requests/unlisted.json",
        b"{}".to_vec(),
    );

    assert!(matches!(
        ResponsesContractAuthority::acquire(&corpus.snapshot()?),
        Err(ResponsesContractError::UndeclaredFixture)
    ));
    Ok(())
}

#[test]
fn rejects_any_unlisted_entry_beneath_the_public_contract_root() -> TestResult<()> {
    let mut corpus = Corpus::valid()?;
    corpus.insert(
        "policy/contracts/openai-responses-v1/unlisted.json",
        b"{}".to_vec(),
    );

    assert!(matches!(
        ResponsesContractAuthority::acquire(&corpus.snapshot()?),
        Err(ResponsesContractError::UndeclaredPublicContract)
    ));
    Ok(())
}

#[test]
fn rejects_dialect_crossing_even_when_index_hashes_are_updated() -> TestResult<()> {
    let mut corpus = Corpus::valid()?;
    corpus.cross_public_fixture_dialect()?;

    assert!(matches!(
        ResponsesContractAuthority::acquire(&corpus.snapshot()?),
        Err(ResponsesContractError::FixtureSchema)
    ));
    Ok(())
}

#[test]
fn rejects_changed_fixture_bytes_before_envelope_acceptance() -> TestResult<()> {
    let mut corpus = Corpus::valid()?;
    corpus.replace(PUBLIC_REQUEST, b"{}".to_vec());

    assert!(matches!(
        ResponsesContractAuthority::acquire(&corpus.snapshot()?),
        Err(ResponsesContractError::LengthMismatch)
    ));
    Ok(())
}

#[test]
fn rejects_equal_length_fixture_digest_drift() -> TestResult<()> {
    let mut corpus = Corpus::valid()?;
    let mut changed = corpus.bytes(PUBLIC_REQUEST)?.to_vec();
    let Some(first) = changed.first_mut() else {
        return Err(std::io::Error::other("test request is empty").into());
    };
    *first = b'[';
    corpus.replace(PUBLIC_REQUEST, changed);

    assert!(matches!(
        ResponsesContractAuthority::acquire(&corpus.snapshot()?),
        Err(ResponsesContractError::DigestMismatch)
    ));
    Ok(())
}

#[test]
fn rejects_duplicate_members_inside_a_rehashed_fixture() -> TestResult<()> {
    let mut corpus = Corpus::valid()?;
    let original = std::str::from_utf8(corpus.bytes(PUBLIC_REQUEST)?)?;
    let duplicate = original.replacen(
        "\"schema_version\": 1",
        "\"schema_version\": 1,\n  \"schema_version\": 1",
        1,
    );
    corpus.replace_public_request_and_repin(duplicate.into_bytes())?;

    assert!(matches!(
        ResponsesContractAuthority::acquire(&corpus.snapshot()?),
        Err(ResponsesContractError::FixtureSchema)
    ));
    Ok(())
}

#[test]
fn rejects_observed_finding_absent_from_traceability() -> TestResult<()> {
    const SENTINEL: &str = "UNKNOWN-TRACE-001";
    let mut corpus = Corpus::valid()?;
    corpus.replace_public_finding(SENTINEL)?;

    let error = acquire_error(&corpus)?;
    assert!(matches!(
        &error,
        ResponsesContractError::EvidenceTraceability {
            issue: EvidenceTraceabilityIssue::FindingMissing,
            count: 1,
        }
    ));
    assert_non_disclosing(&error, &[SENTINEL]);
    Ok(())
}

#[test]
fn rejects_unexpected_observed_traceability_mapping() -> TestResult<()> {
    let mut corpus = Corpus::valid()?;
    corpus.replace_public_finding("EVT-03")?;

    assert!(matches!(
        ResponsesContractAuthority::acquire(&corpus.snapshot()?),
        Err(ResponsesContractError::EvidenceTraceability {
            issue: EvidenceTraceabilityIssue::SourceMismatch,
            count: 1,
        })
    ));
    Ok(())
}

#[test]
fn rejects_planned_traceability_absent_after_consistent_repin() -> TestResult<()> {
    let mut corpus = Corpus::valid()?;
    corpus.remove_planned_public_fixture()?;

    assert!(matches!(
        ResponsesContractAuthority::acquire(&corpus.snapshot()?),
        Err(ResponsesContractError::EvidenceTraceability {
            issue: EvidenceTraceabilityIssue::EvidenceMissing,
            count: 1,
        })
    ));
    Ok(())
}

#[test]
fn rejects_unratified_official_source_reference() -> TestResult<()> {
    let mut corpus = Corpus::valid()?;
    corpus.replace_public_source("https://example.invalid/responses")?;

    assert!(matches!(
        ResponsesContractAuthority::acquire(&corpus.snapshot()?),
        Err(ResponsesContractError::FixtureSchema)
    ));
    Ok(())
}

#[test]
fn rejects_traceability_registry_drift() -> TestResult<()> {
    let mut corpus = Corpus::valid()?;
    let mut changed = corpus.bytes(TRACEABILITY)?.to_vec();
    changed.extend_from_slice(b"\n");
    corpus.replace(TRACEABILITY, changed);

    assert!(matches!(
        ResponsesContractAuthority::acquire(&corpus.snapshot()?),
        Err(ResponsesContractError::TraceabilitySchema)
    ));
    Ok(())
}

#[test]
fn rejects_missing_declared_extraction_output() -> TestResult<()> {
    let mut corpus = Corpus::valid()?;
    corpus.remove("policy/contracts/openai-responses-v1/sse-events.json");

    assert!(matches!(
        ResponsesContractAuthority::acquire(&corpus.snapshot()?),
        Err(ResponsesContractError::Missing)
    ));
    Ok(())
}

#[test]
fn rejects_ratified_codex_pin_drift() -> TestResult<()> {
    let mut corpus = Corpus::valid()?;
    let bytes = corpus.bytes(CONTRACT_PINS)?.to_vec();
    let changed = String::from_utf8(bytes)?.replacen(
        "0396f99cf1a27fc87dd12d23403b25e840b6ecbd",
        "1396f99cf1a27fc87dd12d23403b25e840b6ecbd",
        1,
    );
    corpus.replace(CONTRACT_PINS, changed.into_bytes());

    assert!(matches!(
        ResponsesContractAuthority::acquire(&corpus.snapshot()?),
        Err(ResponsesContractError::Schema)
    ));
    Ok(())
}

#[test]
fn undeclared_path_is_absent_from_all_error_renderings() -> TestResult<()> {
    const SENTINEL: &str = "undeclared-path-sentinel";
    let mut corpus = Corpus::valid()?;
    corpus.insert(
        "crates/norn/testdata/openai_responses/public/requests/undeclared-path-sentinel.json",
        b"{}".to_vec(),
    );

    let error = acquire_error(&corpus)?;
    assert!(matches!(&error, ResponsesContractError::UndeclaredFixture));
    assert_non_disclosing(&error, &[SENTINEL]);
    Ok(())
}

#[test]
fn declared_path_and_fixture_id_are_absent_from_error_renderings() -> TestResult<()> {
    const ID_SENTINEL: &str = "aaa-hostile-fixture-id-sentinel";
    const PATH_SENTINEL: &str = "declared-path-sentinel";
    let mut corpus = Corpus::valid()?;
    corpus.declare_hostile_public_fixture(
        ID_SENTINEL,
        "crates/norn/testdata/openai_responses/public/requests/declared-path-sentinel.json",
    )?;

    let error = acquire_error(&corpus)?;
    assert!(matches!(&error, ResponsesContractError::Missing));
    assert_non_disclosing(&error, &[ID_SENTINEL, PATH_SENTINEL]);
    Ok(())
}

#[test]
fn malformed_bytes_are_absent_from_error_renderings() -> TestResult<()> {
    const SENTINEL: &str = "hostile-malformed-sentinel";
    let mut corpus = Corpus::valid()?;
    corpus.replace(CONTRACT_PINS, br#"{"hostile-malformed-sentinel":"#.to_vec());

    let error = acquire_error(&corpus)?;
    assert!(matches!(&error, ResponsesContractError::Json));
    assert_non_disclosing(&error, &[SENTINEL]);
    Ok(())
}

fn acquire_error(corpus: &Corpus) -> TestResult<ResponsesContractError> {
    match ResponsesContractAuthority::acquire(&corpus.snapshot()?) {
        Ok(_) => Err(io::Error::other("test corpus unexpectedly acquired").into()),
        Err(error) => Ok(error),
    }
}

fn assert_non_disclosing(error: &ResponsesContractError, sentinels: &[&str]) {
    let display = error.to_string();
    let debug = format!("{error:?}");
    for sentinel in sentinels {
        assert!(!display.contains(sentinel));
        assert!(!debug.contains(sentinel));
    }

    let mut source = error.source();
    while let Some(current) = source {
        let source_display = current.to_string();
        let source_debug = format!("{current:?}");
        for sentinel in sentinels {
            assert!(!source_display.contains(sentinel));
            assert!(!source_debug.contains(sentinel));
        }
        source = current.source();
    }
}
