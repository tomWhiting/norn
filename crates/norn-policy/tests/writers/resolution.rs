mod support;

use norn_policy::digest_bytes;
use norn_policy::writers::{
    UnknownSinkReason, WriterCandidateForm, WriterResolution, WriterResolutionAuthority,
    WriterResolutionAuthorityError, WriterResolutionCoverage, WriterResolutionCoverageError,
    WriterResolutionDisposition, WriterToken,
};
use support::*;

#[test]
fn candidate_identity_excludes_span_but_binds_semantic_fields() -> TestResult {
    let first = candidate(
        "src/first.rs",
        10,
        20,
        "write",
        UnknownSinkReason::DynamicReceiver,
        WriterCandidateForm::MethodCall,
        0,
    )?;
    let moved = candidate(
        "src/first.rs",
        80,
        90,
        "write",
        UnknownSinkReason::DynamicReceiver,
        WriterCandidateForm::MethodCall,
        0,
    )?;
    assert_eq!(first.id(), moved.id());
    assert_ne!(first.span(), moved.span());

    let changed_form = candidate(
        "src/first.rs",
        10,
        20,
        "write",
        UnknownSinkReason::DynamicReceiver,
        WriterCandidateForm::FunctionCall,
        0,
    )?;
    let changed_reason = candidate(
        "src/first.rs",
        10,
        20,
        "write",
        UnknownSinkReason::GenericReceiver,
        WriterCandidateForm::MethodCall,
        0,
    )?;
    assert_ne!(first.id(), changed_form.id());
    assert_ne!(first.id(), changed_reason.id());
    Ok(())
}

#[test]
fn coverage_is_the_exact_span_independent_semantic_union() -> TestResult {
    let base = candidate(
        "src/base.rs",
        1,
        2,
        "write",
        UnknownSinkReason::DynamicReceiver,
        WriterCandidateForm::MethodCall,
        0,
    )?;
    let moved = candidate(
        "src/base.rs",
        40,
        50,
        "write",
        UnknownSinkReason::DynamicReceiver,
        WriterCandidateForm::MethodCall,
        0,
    )?;
    let current = candidate(
        "src/current.rs",
        3,
        4,
        "flush",
        UnknownSinkReason::AuthorityMethod,
        WriterCandidateForm::MethodCall,
        0,
    )?;
    let forward = WriterResolutionCoverage::for_snapshots(
        std::slice::from_ref(&base),
        &[moved, current.clone()],
    )?;
    let reverse = WriterResolutionCoverage::for_snapshots(&[current, base], &[])?;
    assert_eq!(forward.len(), 2);
    assert_eq!(forward.review_inventory(), reverse.review_inventory());
    Ok(())
}

#[test]
fn coverage_rejects_duplicate_snapshot_candidates() -> TestResult {
    let candidate = candidate(
        "src/lib.rs",
        1,
        2,
        "write",
        UnknownSinkReason::DynamicReceiver,
        WriterCandidateForm::MethodCall,
        0,
    )?;
    let error =
        WriterResolutionCoverage::for_snapshots(&[candidate.clone(), candidate.clone()], &[]);
    assert!(matches!(
        error,
        Err(WriterResolutionCoverageError::Duplicate { .. })
    ));
    Ok(())
}

#[test]
fn authority_round_trips_exact_sink_and_non_writer_resolutions() -> TestResult {
    let registry = registry()?;
    let (base, current) = candidate_pair()?;
    let mut resolutions = vec![
        WriterResolution::resolved_sink(base.id(), WriterToken::parse("standard.write")?),
        WriterResolution::reviewed_non_writer(
            current.id(),
            WriterToken::parse("buffer.formatting")?,
        ),
    ];
    resolutions.sort_by_key(WriterResolution::candidate);
    let authority = WriterResolutionAuthority::author_p1(
        std::slice::from_ref(&base),
        std::slice::from_ref(&current),
        &registry,
        vec![WriterToken::parse("buffer.formatting")?],
        resolutions,
    )?;
    let encoded = authority.encode_p1()?;
    let decoded = decode(&encoded, &base, &current, &registry)?;
    assert_eq!(authority, decoded);
    assert_eq!(authority.normalized_digest()?, decoded.normalized_digest()?);
    let resolution = decoded
        .resolution_for(current.id())
        .ok_or("missing validated resolution")?;
    assert!(matches!(
        resolution.disposition(),
        WriterResolutionDisposition::ReviewedNonWriter { review }
            if review.as_str() == "buffer.formatting"
    ));
    Ok(())
}

#[test]
fn authority_rejects_algorithm_registry_and_inventory_drift() -> TestResult {
    let registry = registry()?;
    let (base, current) = candidate_pair()?;
    let bytes = authority_bytes(
        &base,
        &current,
        &registry,
        Vec::new(),
        exact_sink_rows(&base, &current),
    )?;

    let writer = set_nested_string(&bytes, "algorithms", "writer", "norn-writers-stale")?;
    assert!(matches!(
        decode(&writer, &base, &current, &registry),
        Err(WriterResolutionAuthorityError::WriterAlgorithm)
    ));
    let digest = set_nested_string(&bytes, "algorithms", "digest", "norn-digest-stale")?;
    assert!(matches!(
        decode(&digest, &base, &current, &registry),
        Err(WriterResolutionAuthorityError::DigestAlgorithm)
    ));
    let registry_drift = set_top_digest(&bytes, "sink_registry", digest_bytes(b"other"))?;
    assert!(matches!(
        decode(&registry_drift, &base, &current, &registry),
        Err(WriterResolutionAuthorityError::SinkRegistry)
    ));
    let inventory = set_top_digest(&bytes, "review_inventory", digest_bytes(b"other"))?;
    assert!(matches!(
        decode(&inventory, &base, &current, &registry),
        Err(WriterResolutionAuthorityError::ReviewInventory)
    ));
    Ok(())
}

#[test]
fn authority_rejects_missing_stale_duplicate_and_colliding_rows() -> TestResult {
    let registry = registry()?;
    let (base, current) = candidate_pair()?;

    let missing = authority_bytes(
        &base,
        &current,
        &registry,
        Vec::new(),
        vec![sink_row(base.id(), "standard.write")],
    )?;
    assert!(matches!(
        decode(&missing, &base, &current, &registry),
        Err(WriterResolutionAuthorityError::MissingResolution { .. })
    ));

    let stale = candidate(
        "src/stale.rs",
        1,
        2,
        "write",
        UnknownSinkReason::DynamicReceiver,
        WriterCandidateForm::MethodCall,
        0,
    )?;
    let stale_rows = sorted_rows(vec![
        sink_row(base.id(), "standard.write"),
        sink_row(current.id(), "standard.write"),
        sink_row(stale.id(), "standard.write"),
    ]);
    let stale_bytes = authority_bytes(&base, &current, &registry, Vec::new(), stale_rows)?;
    assert!(matches!(
        decode(&stale_bytes, &base, &current, &registry),
        Err(WriterResolutionAuthorityError::StaleResolution { .. })
    ));

    let mut duplicate_rows = exact_sink_rows(&base, &current);
    duplicate_rows.push(sink_row(current.id(), "standard.write"));
    let duplicate = authority_bytes(
        &base,
        &current,
        &registry,
        Vec::new(),
        sorted_rows(duplicate_rows),
    )?;
    assert!(matches!(
        decode(&duplicate, &base, &current, &registry),
        Err(WriterResolutionAuthorityError::DuplicateResolution { .. })
    ));

    let collision_rows = sorted_rows(vec![
        sink_row(base.id(), "standard.write"),
        review_row(base.id(), "reviewed.base"),
        sink_row(current.id(), "standard.write"),
    ]);
    let collision = authority_bytes(
        &base,
        &current,
        &registry,
        vec!["reviewed.base"],
        collision_rows,
    )?;
    assert!(matches!(
        decode(&collision, &base, &current, &registry),
        Err(WriterResolutionAuthorityError::ResolutionCollision { .. })
    ));
    Ok(())
}

#[test]
fn authority_requires_strict_rows_and_exact_review_vocabulary() -> TestResult {
    let registry = registry()?;
    let (base, current) = candidate_pair()?;
    let mut rows = exact_sink_rows(&base, &current);
    rows.reverse();
    let unordered = authority_bytes(&base, &current, &registry, Vec::new(), rows)?;
    assert!(matches!(
        decode(&unordered, &base, &current, &registry),
        Err(WriterResolutionAuthorityError::ResolutionOrder)
    ));

    let review_rows = sorted_rows(vec![
        review_row(base.id(), "review.a"),
        review_row(current.id(), "review.b"),
    ]);
    let review_order = authority_bytes(
        &base,
        &current,
        &registry,
        vec!["review.b", "review.a"],
        review_rows,
    )?;
    assert!(matches!(
        decode(&review_order, &base, &current, &registry),
        Err(WriterResolutionAuthorityError::ReviewOrder)
    ));

    let duplicate_review = authority_bytes(
        &base,
        &current,
        &registry,
        vec!["review.a", "review.a", "review.b"],
        sorted_rows(vec![
            review_row(base.id(), "review.a"),
            review_row(current.id(), "review.b"),
        ]),
    )?;
    assert!(matches!(
        decode(&duplicate_review, &base, &current, &registry),
        Err(WriterResolutionAuthorityError::DuplicateReview)
    ));

    let missing_review = authority_bytes(
        &base,
        &current,
        &registry,
        Vec::new(),
        sorted_rows(vec![
            review_row(base.id(), "review.base"),
            sink_row(current.id(), "standard.write"),
        ]),
    )?;
    assert!(matches!(
        decode(&missing_review, &base, &current, &registry),
        Err(WriterResolutionAuthorityError::MissingReview { .. })
    ));

    let stale_review = authority_bytes(
        &base,
        &current,
        &registry,
        vec!["review.unused"],
        exact_sink_rows(&base, &current),
    )?;
    assert!(matches!(
        decode(&stale_review, &base, &current, &registry),
        Err(WriterResolutionAuthorityError::StaleReview)
    ));
    Ok(())
}

#[test]
fn authority_rejects_unknown_sinks_and_open_or_duplicate_toml() -> TestResult {
    let registry = registry()?;
    let (base, current) = candidate_pair()?;
    let unknown = authority_bytes(
        &base,
        &current,
        &registry,
        Vec::new(),
        sorted_rows(vec![
            sink_row(base.id(), "missing.sink"),
            sink_row(current.id(), "standard.write"),
        ]),
    )?;
    assert!(matches!(
        decode(&unknown, &base, &current, &registry),
        Err(WriterResolutionAuthorityError::UnknownSink { .. })
    ));

    let valid = authority_bytes(
        &base,
        &current,
        &registry,
        Vec::new(),
        exact_sink_rows(&base, &current),
    )?;
    let text = String::from_utf8(valid)?;
    let duplicate = format!("schema_version = 1\n{text}");
    assert!(matches!(
        decode(duplicate.as_bytes(), &base, &current, &registry),
        Err(WriterResolutionAuthorityError::Toml(_))
    ));
    let unknown_field = format!("unexpected = true\n{text}");
    assert!(matches!(
        decode(unknown_field.as_bytes(), &base, &current, &registry),
        Err(WriterResolutionAuthorityError::Toml(_))
    ));
    Ok(())
}
