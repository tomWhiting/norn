use norn_policy::digest::digest_bytes;
use norn_policy::finding::ByteSpan;
use norn_policy::path::RepositoryPath;
use norn_policy::version::DIGEST_VERSION;
use norn_policy::writers::{
    SinkRegistry, UnknownSinkReason, WRITER_ANALYZER_VERSION,
    WRITER_RESOLUTION_REVIEW_INVENTORY_PATH, WRITER_RESOLUTION_REVIEW_SCHEMA_VERSION,
    WRITER_SCHEMA_VERSION, WriterCandidate, WriterCandidateForm, WriterCandidateSemantics,
    WriterResolutionCoverage, WriterResolutionReviewInventory,
    WriterResolutionReviewInventoryError, WriterToken,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn review_rows_are_candidate_sorted_and_encoding_is_deterministic() -> TestResult {
    let registry = registry()?;
    let first = candidate("src/z.rs", 10, 20, "write", 0)?;
    let second = candidate("src/a.rs", 30, 40, "flush", 0)?;
    let forward = WriterResolutionReviewInventory::author_p1(
        &[first.clone(), second.clone()],
        &[],
        &registry,
    )?;
    let reverse = WriterResolutionReviewInventory::author_p1(&[second, first], &[], &registry)?;

    assert!(
        forward
            .rows()
            .windows(2)
            .all(|pair| pair[0].candidate_id() < pair[1].candidate_id())
    );
    assert_eq!(forward, reverse);
    assert_eq!(forward.encode_p1_pretty()?, reverse.encode_p1_pretty()?);
    assert_eq!(forward.canonical_identity()?, reverse.canonical_identity()?);
    assert_eq!(
        forward.schema_version(),
        WRITER_RESOLUTION_REVIEW_SCHEMA_VERSION
    );
    assert_eq!(
        WRITER_RESOLUTION_REVIEW_INVENTORY_PATH,
        "docs/reviews/evidence/p1/writer-resolution-inventory.json"
    );
    Ok(())
}

#[test]
fn span_only_movement_collapses_to_one_row_with_both_spans() -> TestResult {
    let registry = registry()?;
    let base = candidate("src/lib.rs", 1, 5, "write", 0)?;
    let current = candidate("src/lib.rs", 101, 105, "write", 0)?;
    assert_eq!(base.id(), current.id());

    let inventory = WriterResolutionReviewInventory::author_p1(
        std::slice::from_ref(&base),
        std::slice::from_ref(&current),
        &registry,
    )?;
    let coverage = WriterResolutionCoverage::for_snapshots(
        std::slice::from_ref(&base),
        std::slice::from_ref(&current),
    )?;
    assert_eq!(inventory.rows().len(), 1);
    let row = inventory.rows().first().ok_or("missing review row")?;
    assert_eq!(row.candidate_id(), base.id());
    assert_eq!(row.base_span(), Some(ByteSpan::new(1, 5)?));
    assert_eq!(row.current_span(), Some(ByteSpan::new(101, 105)?));
    assert_eq!(inventory.review_inventory(), coverage.review_inventory());

    let unchanged = WriterResolutionReviewInventory::author_p1(
        std::slice::from_ref(&base),
        std::slice::from_ref(&base),
        &registry,
    )?;
    assert_eq!(inventory.review_inventory(), unchanged.review_inventory());
    assert_ne!(
        inventory.canonical_identity()?,
        unchanged.canonical_identity()?
    );
    Ok(())
}

#[test]
fn review_row_contains_only_closed_semantics_and_optional_spans() -> TestResult {
    let registry = registry()?;
    let current = candidate("src/current.rs", 7, 9, "flush", 3)?;
    let inventory =
        WriterResolutionReviewInventory::author_p1(&[], std::slice::from_ref(&current), &registry)?;
    let row = inventory
        .rows()
        .first()
        .ok_or("missing current review row")?;

    assert_eq!(row.path().as_str(), "src/current.rs");
    assert_eq!(row.candidate().as_str(), "flush");
    assert_eq!(row.reason(), UnknownSinkReason::DynamicReceiver);
    assert_eq!(row.form(), WriterCandidateForm::MethodCall);
    assert_eq!(row.ordinal(), 3);
    assert_eq!(row.base_span(), None);
    assert_eq!(row.current_span(), Some(ByteSpan::new(7, 9)?));
    assert_eq!(inventory.sink_registry(), registry.digest());
    Ok(())
}

#[test]
fn duplicate_candidate_within_one_snapshot_is_rejected() -> TestResult {
    let registry = registry()?;
    let candidate = candidate("src/lib.rs", 1, 2, "write", 0)?;
    assert!(matches!(
        WriterResolutionReviewInventory::author_p1(&[candidate.clone(), candidate], &[], &registry,),
        Err(WriterResolutionReviewInventoryError::DuplicateCandidate { .. })
    ));
    Ok(())
}

#[test]
fn pretty_json_has_exact_metadata_and_one_trailing_newline() -> TestResult {
    let registry = registry()?;
    let candidate = candidate("src/lib.rs", 1, 2, "write", 0)?;
    let inventory = WriterResolutionReviewInventory::author_p1(&[candidate], &[], &registry)?;
    let encoded = inventory.encode_p1_pretty()?;
    assert_eq!(encoded.last(), Some(&b'\n'));
    assert!(!encoded.ends_with(b"\n\n"));

    let document: serde_json::Value = serde_json::from_slice(&encoded)?;
    assert_eq!(
        document["schema_version"],
        WRITER_RESOLUTION_REVIEW_SCHEMA_VERSION
    );
    assert_eq!(document["algorithms"]["writer"], WRITER_ANALYZER_VERSION);
    assert_eq!(document["algorithms"]["digest"], DIGEST_VERSION);
    assert_eq!(document["sink_registry"], registry.digest().to_hex());
    assert_eq!(document["rows"].as_array().map(Vec::len), Some(1));
    Ok(())
}

fn registry() -> Result<SinkRegistry, Box<dyn std::error::Error>> {
    Ok(SinkRegistry::try_new(WRITER_SCHEMA_VERSION, Vec::new())?)
}

fn candidate(
    path: &str,
    start: u64,
    end: u64,
    token: &str,
    ordinal: u32,
) -> Result<WriterCandidate, Box<dyn std::error::Error>> {
    let path = RepositoryPath::parse(path)?;
    let semantics = WriterCandidateSemantics::new(
        digest_bytes(format!("item:{path}").as_bytes()),
        digest_bytes(format!("call:{token}").as_bytes()),
        WriterToken::parse(token)?,
        UnknownSinkReason::DynamicReceiver,
        WriterCandidateForm::MethodCall,
    );
    Ok(WriterCandidate::new(
        path,
        ByteSpan::new(start, end)?,
        semantics,
        ordinal,
    ))
}
