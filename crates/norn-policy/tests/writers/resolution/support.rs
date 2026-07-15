use serde::Serialize;

use norn_policy::digest::{Digest, digest_bytes};
use norn_policy::finding::ByteSpan;
use norn_policy::path::RepositoryPath;
use norn_policy::writers::{
    FlowClass, OperationKind, SinkOrigin, SinkRegistry, SinkSpec, UnknownSinkReason,
    WRITER_ANALYZER_VERSION, WRITER_RESOLUTION_SCHEMA_VERSION, WRITER_SCHEMA_VERSION,
    WriterCandidate, WriterCandidateForm, WriterCandidateId, WriterCandidateSemantics,
    WriterResolutionAuthority, WriterResolutionAuthorityError, WriterResolutionCoverage,
    WriterRole, WriterToken,
};

pub(super) type TestResult = Result<(), Box<dyn std::error::Error>>;

pub(super) fn registry() -> Result<SinkRegistry, Box<dyn std::error::Error>> {
    let sink = SinkSpec::function(
        "standard.write",
        "std::fs::write",
        OperationKind::Write,
        WriterRole::HandleMutation,
        FlowClass::None,
        SinkOrigin::Standard,
    )?;
    Ok(SinkRegistry::try_new(WRITER_SCHEMA_VERSION, vec![sink])?)
}

pub(super) fn candidate_pair()
-> Result<(WriterCandidate, WriterCandidate), Box<dyn std::error::Error>> {
    Ok((
        candidate(
            "src/base.rs",
            1,
            2,
            "write",
            UnknownSinkReason::DynamicReceiver,
            WriterCandidateForm::MethodCall,
            0,
        )?,
        candidate(
            "src/current.rs",
            3,
            4,
            "format",
            UnknownSinkReason::UnresolvedAlias,
            WriterCandidateForm::FunctionCall,
            0,
        )?,
    ))
}

pub(super) fn candidate(
    path: &str,
    start: u64,
    end: u64,
    token: &str,
    reason: UnknownSinkReason,
    form: WriterCandidateForm,
    ordinal: u32,
) -> Result<WriterCandidate, Box<dyn std::error::Error>> {
    let path = RepositoryPath::parse(path)?;
    let span = ByteSpan::new(start, end)?;
    let semantics = WriterCandidateSemantics::new(
        digest_bytes(format!("item:{path}").as_bytes()),
        digest_bytes(format!("call:{token}").as_bytes()),
        WriterToken::parse(token)?,
        reason,
        form,
    );
    Ok(WriterCandidate::new(path, span, semantics, ordinal))
}

#[derive(Clone, Serialize)]
pub(super) struct TestResolution<'a> {
    candidate: WriterCandidateId,
    disposition: TestDisposition<'a>,
}

#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum TestDisposition<'a> {
    ResolvedSink { sink: &'a str },
    ReviewedNonWriter { review: &'a str },
}

#[derive(Serialize)]
struct TestAlgorithms<'a> {
    writer: &'a str,
    digest: &'a str,
}

#[derive(Serialize)]
struct TestDocument<'a> {
    schema_version: u32,
    algorithms: TestAlgorithms<'a>,
    sink_registry: Digest,
    review_inventory: Digest,
    non_writer_reviews: Vec<&'a str>,
    resolutions: Vec<TestResolution<'a>>,
}

pub(super) fn exact_sink_rows<'a>(
    base: &WriterCandidate,
    current: &WriterCandidate,
) -> Vec<TestResolution<'a>> {
    sorted_rows(vec![
        sink_row(base.id(), "standard.write"),
        sink_row(current.id(), "standard.write"),
    ])
}

pub(super) fn sorted_rows(rows: Vec<TestResolution<'_>>) -> Vec<TestResolution<'_>> {
    let mut rows = rows;
    rows.sort_by_key(|row| row.candidate);
    rows
}

pub(super) fn sink_row(candidate: WriterCandidateId, sink: &str) -> TestResolution<'_> {
    TestResolution {
        candidate,
        disposition: TestDisposition::ResolvedSink { sink },
    }
}

pub(super) fn review_row(candidate: WriterCandidateId, review: &str) -> TestResolution<'_> {
    TestResolution {
        candidate,
        disposition: TestDisposition::ReviewedNonWriter { review },
    }
}

pub(super) fn authority_bytes(
    base: &WriterCandidate,
    current: &WriterCandidate,
    registry: &SinkRegistry,
    reviews: Vec<&str>,
    resolutions: Vec<TestResolution<'_>>,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let coverage = WriterResolutionCoverage::for_snapshots(
        std::slice::from_ref(base),
        std::slice::from_ref(current),
    )?;
    let document = TestDocument {
        schema_version: WRITER_RESOLUTION_SCHEMA_VERSION,
        algorithms: TestAlgorithms {
            writer: WRITER_ANALYZER_VERSION,
            digest: norn_policy::version::DIGEST_VERSION,
        },
        sink_registry: registry.digest(),
        review_inventory: coverage.review_inventory(),
        non_writer_reviews: reviews,
        resolutions,
    };
    Ok(toml::to_string(&document)?.into_bytes())
}

pub(super) fn decode(
    bytes: &[u8],
    base: &WriterCandidate,
    current: &WriterCandidate,
    registry: &SinkRegistry,
) -> Result<WriterResolutionAuthority, WriterResolutionAuthorityError> {
    WriterResolutionAuthority::decode_p1(
        bytes,
        std::slice::from_ref(base),
        std::slice::from_ref(current),
        registry,
    )
}

pub(super) fn set_nested_string(
    bytes: &[u8],
    table: &str,
    field: &str,
    value: &str,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let text = std::str::from_utf8(bytes)?;
    let mut document: toml::Value = toml::from_str(text)?;
    let nested = document
        .get_mut(table)
        .and_then(toml::Value::as_table_mut)
        .ok_or("missing test table")?;
    nested.insert(field.to_owned(), toml::Value::String(value.to_owned()));
    Ok(toml::to_string(&document)?.into_bytes())
}

pub(super) fn set_top_digest(
    bytes: &[u8],
    field: &str,
    value: Digest,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let text = std::str::from_utf8(bytes)?;
    let mut document: toml::Value = toml::from_str(text)?;
    let root = document.as_table_mut().ok_or("missing test root")?;
    root.insert(field.to_owned(), toml::Value::String(value.to_hex()));
    Ok(toml::to_string(&document)?.into_bytes())
}
