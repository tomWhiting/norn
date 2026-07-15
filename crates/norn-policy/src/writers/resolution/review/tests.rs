use crate::digest::digest_bytes;
use crate::finding::ByteSpan;
use crate::path::RepositoryPath;
use crate::writers::{
    SinkRegistry, UnknownSinkReason, WRITER_SCHEMA_VERSION, WriterCandidate, WriterCandidateForm,
    WriterCandidateSemantics, WriterResolutionReviewInventory,
    WriterResolutionReviewInventoryError, WriterToken,
};

#[test]
fn cross_snapshot_semantic_collision_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let base = candidate("src/base.rs", "write")?;
    let different = candidate("src/current.rs", "flush")?;
    let forged = different.with_forged_id_for_collision_test(base.id());
    let registry = SinkRegistry::try_new(WRITER_SCHEMA_VERSION, Vec::new())?;

    assert!(matches!(
        WriterResolutionReviewInventory::author_p1(&[base], &[forged], &registry),
        Err(WriterResolutionReviewInventoryError::CandidateCollision { .. })
    ));
    Ok(())
}

fn candidate(path: &str, token: &str) -> Result<WriterCandidate, Box<dyn std::error::Error>> {
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
        ByteSpan::new(1, 2)?,
        semantics,
        0,
    ))
}
