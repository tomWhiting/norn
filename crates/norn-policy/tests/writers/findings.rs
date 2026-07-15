use std::collections::BTreeMap;

use norn_policy::finding::{RepositoryFinding, UnknownWriterIssue, WriterClassificationIssue};
use norn_policy::writers::{
    ClassificationIssue, FlowClass, OperationKind, SinkOrigin, SinkRegistry, SinkSpec,
    UnknownSinkReason, WriterFindingError, WriterRole, analyze_writers, builtin_sink_registry,
    canonical_writer_findings,
};

use super::support::{TestResult, source};

#[test]
fn every_unknown_reason_has_one_stable_finding_issue() -> TestResult {
    let cases = [
        (
            UnknownSinkReason::AmbiguousAlias,
            UnknownWriterIssue::AmbiguousAlias,
            "ambiguous_alias",
        ),
        (
            UnknownSinkReason::UnresolvedAlias,
            UnknownWriterIssue::UnresolvedAlias,
            "unresolved_alias",
        ),
        (
            UnknownSinkReason::WildcardImport,
            UnknownWriterIssue::WildcardImport,
            "wildcard_import",
        ),
        (
            UnknownSinkReason::DynamicReceiver,
            UnknownWriterIssue::DynamicReceiver,
            "dynamic_receiver",
        ),
        (
            UnknownSinkReason::GenericReceiver,
            UnknownWriterIssue::GenericReceiver,
            "generic_receiver",
        ),
        (
            UnknownSinkReason::MacroTokenCandidate,
            UnknownWriterIssue::MacroTokenCandidate,
            "macro_token_candidate",
        ),
        (
            UnknownSinkReason::MacroDefinitionCandidate,
            UnknownWriterIssue::MacroDefinitionCandidate,
            "macro_definition_candidate",
        ),
        (
            UnknownSinkReason::KnownNamespaceCandidate,
            UnknownWriterIssue::KnownNamespaceCandidate,
            "known_namespace_candidate",
        ),
        (
            UnknownSinkReason::CallableEscape,
            UnknownWriterIssue::CallableEscape,
            "callable_escape",
        ),
        (
            UnknownSinkReason::AuthorityArgument,
            UnknownWriterIssue::AuthorityArgument,
            "authority_argument",
        ),
        (
            UnknownSinkReason::AuthorityMethod,
            UnknownWriterIssue::AuthorityMethod,
            "authority_method",
        ),
        (
            UnknownSinkReason::AuthorityStorage,
            UnknownWriterIssue::AuthorityStorage,
            "authority_storage",
        ),
        (
            UnknownSinkReason::AuthorityReturn,
            UnknownWriterIssue::AuthorityReturn,
            "authority_return",
        ),
        (
            UnknownSinkReason::NewWrapperCandidate,
            UnknownWriterIssue::NewWrapperCandidate,
            "new_wrapper_candidate",
        ),
        (
            UnknownSinkReason::DefinitionMismatch,
            UnknownWriterIssue::DefinitionMismatch,
            "definition_mismatch",
        ),
    ];

    assert_eq!(cases.len(), 15);
    for (reason, expected, token) in cases {
        let issue = UnknownWriterIssue::from(reason);
        assert_eq!(issue, expected);
        assert_eq!(serde_json::to_value(issue)?, token);
    }
    assert_eq!(
        serde_json::to_value(UnknownWriterIssue::UnobservedRequiredSink)?,
        "unobserved_required_sink"
    );
    Ok(())
}

#[test]
fn every_classification_issue_has_one_stable_finding_issue() -> TestResult {
    let registry = builtin_sink_registry()?;
    let inventory = analyze_writers(
        &[source(
            "crates/sample/src/lib.rs",
            "fn run() { std::fs::write(\"artifact\", b\"value\"); }",
        )?],
        &registry,
    )?;
    let Some(operation) = inventory
        .operations()
        .first()
        .map(norn_policy::writers::WriterOperation::id)
    else {
        return Err(std::io::Error::other("writer fixture produced no operation").into());
    };
    let cases = [
        (ClassificationIssue::Missing { operation }, "missing"),
        (ClassificationIssue::Duplicate { operation }, "duplicate"),
        (ClassificationIssue::Stale { operation }, "stale"),
        (
            ClassificationIssue::SharedEdges { operation },
            "shared_edges",
        ),
    ];

    assert_eq!(cases.len(), 4);
    for (issue, token) in cases {
        let converted = WriterClassificationIssue::from(issue);
        let encoded = serde_json::to_value(converted)?;
        assert_eq!(encoded["issue"], token);
        assert_eq!(encoded["operation"], operation.digest().to_hex());
    }
    Ok(())
}

#[test]
fn canonical_unknown_and_unobserved_rows_become_non_disclosing_findings() -> TestResult {
    const SENTINEL: &str = "never-leak-private-writer-source-sentinel";
    let path = "crates/sample/src/lib.rs";
    let text = format!(
        "fn run<T: std::io::Write>(mut writer: T) {{ let value = b\"{SENTINEL}\"; writer.write_all(value); }}"
    );
    let registry = builtin_sink_registry()?;
    let inventory = analyze_writers(&[source(path, &text)?], &registry)?;
    assert!(!inventory.candidates().is_empty());
    assert!(!inventory.unobserved_required_sinks().is_empty());

    let findings = canonical_writer_findings(&inventory)?;
    assert_eq!(
        findings.len(),
        inventory.candidates().len() + inventory.unobserved_required_sinks().len()
    );

    let definitions = registry
        .specs()
        .iter()
        .filter_map(|spec| {
            spec.definition()
                .map(|definition| (spec.id().as_str(), definition.source()))
        })
        .collect::<BTreeMap<_, _>>();
    let mut expected_definition_paths = BTreeMap::new();
    for sink in inventory.unobserved_required_sinks() {
        let Some(source) = definitions.get(sink.as_str()) else {
            return Err(std::io::Error::other("required sink has no definition fixture").into());
        };
        *expected_definition_paths
            .entry(source.as_str())
            .or_insert(0_usize) += 1;
    }
    let mut actual_definition_paths = BTreeMap::new();
    for finding in &findings {
        let Some(RepositoryFinding::UnknownWriterSink { issue, .. }) = finding.repository_details()
        else {
            return Err(std::io::Error::other("unexpected writer finding shape").into());
        };
        if *issue == UnknownWriterIssue::UnobservedRequiredSink {
            assert!(finding.span().is_none());
            let Some(path) = finding.path() else {
                return Err(std::io::Error::other("unobserved sink has no authority path").into());
            };
            *actual_definition_paths
                .entry(path.as_str())
                .or_insert(0_usize) += 1;
        } else {
            assert_eq!(
                finding.path().map(norn_policy::RepositoryPath::as_str),
                Some(path)
            );
            assert!(finding.span().is_some());
        }
    }
    assert_eq!(actual_definition_paths, expected_definition_paths);

    let encoded = serde_json::to_string(&findings)?;
    let debug = format!("{findings:?}");
    assert!(!encoded.contains(SENTINEL));
    assert!(!debug.contains(SENTINEL));
    assert!(!encoded.contains("source_snippet"));
    Ok(())
}

#[test]
fn canonical_conversion_rejects_an_unreviewed_registry_identity() -> TestResult {
    let schema_version = builtin_sink_registry()?.schema_version();
    let spec = SinkSpec::function(
        "fixture.write",
        "fixture::write",
        OperationKind::Write,
        WriterRole::HandleMutation,
        FlowClass::None,
        SinkOrigin::Reviewed,
    )?;
    let registry = SinkRegistry::try_new(schema_version, vec![spec])?;
    let inventory = analyze_writers(&[], &registry)?;

    assert!(matches!(
        canonical_writer_findings(&inventory),
        Err(WriterFindingError::RegistryIdentity)
    ));
    Ok(())
}
