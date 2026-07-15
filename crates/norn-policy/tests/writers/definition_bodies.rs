use norn_policy::writers::{
    DefinitionSpec, FlowClass, OperationKind, SinkRegistry, SinkSpec, UnknownSinkReason,
    WriterCandidateForm, WriterRole, analyze_writers, builtin_sink_registry,
};

use super::support::{TestResult, source};

const DEFINITION_PATH: &str = "crates/sample/src/wrapper.rs";
const REVIEWED_DEFINITION: &str = "fn make_writer() -> usize { let value = 1; value }";

#[test]
fn same_signature_with_changed_body_fails_closed() -> TestResult {
    let registry = project_registry(REVIEWED_DEFINITION)?;
    let changed = source(
        DEFINITION_PATH,
        "fn make_writer() -> usize { let value = 2; value }",
    )?;
    let inventory = analyze_writers(&[changed], &registry)?;

    assert_eq!(inventory.unobserved_required_sinks().len(), 1);
    assert!(inventory.candidates().iter().any(|unknown| {
        unknown.reason() == UnknownSinkReason::DefinitionMismatch
            && unknown.candidate().as_str() == "fixture.make_writer"
            && unknown.form() == WriterCandidateForm::WrapperDefinition
    }));
    Ok(())
}

#[test]
fn comments_and_whitespace_preserve_full_definition_authority() -> TestResult {
    let registry = project_registry(REVIEWED_DEFINITION)?;
    let reformatted = source(
        DEFINITION_PATH,
        r#"
fn make_writer ( ) -> usize
{
    // This comment is not part of the normalized implementation.
    let value = 1 /* nor is this one */ ;
    value
}
"#,
    )?;
    let inventory = analyze_writers(&[reformatted], &registry)?;

    assert!(inventory.unobserved_required_sinks().is_empty());
    assert!(
        inventory
            .candidates()
            .iter()
            .all(|unknown| unknown.reason() != UnknownSinkReason::DefinitionMismatch)
    );
    Ok(())
}

#[test]
fn builtin_definition_pins_match_checked_in_project_sources() -> TestResult {
    let registry = builtin_sink_registry()?;
    let sources = [
        source(
            "crates/norn/src/util/private_fs.rs",
            include_str!("../../../norn/src/util/private_fs.rs"),
        )?,
        source(
            "crates/norn/src/session/persistence/io.rs",
            include_str!("../../../norn/src/session/persistence/io.rs"),
        )?,
        source(
            "crates/norn/src/session/persistence/index.rs",
            include_str!("../../../norn/src/session/persistence/index.rs"),
        )?,
        source(
            "crates/norn/src/tools/task/disk/storage.rs",
            include_str!("../../../norn/src/tools/task/disk/storage.rs"),
        )?,
    ];
    let inventory = analyze_writers(&sources, &registry)?;
    let missing = registry
        .specs()
        .iter()
        .filter(|spec| spec.definition().is_some())
        .filter(|spec| inventory.unobserved_required_sinks().contains(spec.id()))
        .map(|spec| spec.id().as_str())
        .collect::<Vec<_>>();

    assert!(
        missing.is_empty(),
        "stale built-in definition pins: {missing:?}"
    );
    Ok(())
}

fn project_registry(definition: &str) -> Result<SinkRegistry, Box<dyn std::error::Error>> {
    let spec = SinkSpec::project_function(
        "fixture.make_writer",
        "make_writer",
        DefinitionSpec::from_function_source(DEFINITION_PATH, "make_writer", definition)?,
        OperationKind::Open,
        WriterRole::RootOpen,
        FlowClass::None,
    )?;
    Ok(SinkRegistry::try_new(1, vec![spec])?)
}
