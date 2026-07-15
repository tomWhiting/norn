use std::collections::BTreeSet;

use norn_policy::writers::{
    DefinitionSpec, FlowClass, OperationKind, RegistryError, SinkOrigin, SinkRegistry, SinkSpec,
    WriterRole, analyze_writers, builtin_sink_registry,
};

use super::support::{TestResult, source};

#[test]
fn builtins_cover_required_ecosystems_and_operation_classes() -> TestResult {
    let registry = builtin_sink_registry()?;
    registry.validate()?;
    let origins: BTreeSet<SinkOrigin> = registry
        .specs()
        .iter()
        .map(norn_policy::writers::SinkSpec::origin)
        .collect();
    for origin in [
        SinkOrigin::Standard,
        SinkOrigin::Tokio,
        SinkOrigin::Rustix,
        SinkOrigin::Tempfile,
        SinkOrigin::ProjectWrapper,
    ] {
        assert!(origins.contains(&origin));
    }
    let kinds: BTreeSet<OperationKind> = registry
        .specs()
        .iter()
        .map(norn_policy::writers::SinkSpec::kind)
        .collect();
    for kind in [
        OperationKind::Open,
        OperationKind::Create,
        OperationKind::Truncate,
        OperationKind::Append,
        OperationKind::Write,
        OperationKind::SetLength,
        OperationKind::Permissions,
        OperationKind::Flush,
        OperationKind::Sync,
        OperationKind::Persist,
        OperationKind::Rename,
        OperationKind::Link,
        OperationKind::Remove,
    ] {
        assert!(kinds.contains(&kind));
    }
    let project_specs: Vec<&SinkSpec> = registry
        .specs()
        .iter()
        .filter(|spec| spec.origin() == SinkOrigin::ProjectWrapper)
        .collect();
    assert!(!project_specs.is_empty());
    assert!(project_specs.iter().all(|spec| spec.definition().is_some()));
    assert!(
        registry
            .specs()
            .iter()
            .all(|spec| spec.id().as_str() != "project.acquire_private_fs")
    );
    Ok(())
}

#[test]
fn duplicate_ids_and_selectors_are_rejected() -> TestResult {
    let first = SinkSpec::function(
        "fixture.first",
        "fixture::write",
        OperationKind::Write,
        WriterRole::HandleMutation,
        FlowClass::None,
        SinkOrigin::Reviewed,
    )?;
    let duplicate_id = SinkSpec::function(
        "fixture.first",
        "fixture::other",
        OperationKind::Write,
        WriterRole::HandleMutation,
        FlowClass::None,
        SinkOrigin::Reviewed,
    )?;
    assert!(matches!(
        SinkRegistry::try_new(1, vec![first.clone(), duplicate_id]),
        Err(RegistryError::DuplicateId)
    ));
    let duplicate_selector = SinkSpec::function(
        "fixture.second",
        "fixture::write",
        OperationKind::Write,
        WriterRole::HandleMutation,
        FlowClass::None,
        SinkOrigin::Reviewed,
    )?;
    assert!(matches!(
        SinkRegistry::try_new(1, vec![first, duplicate_selector]),
        Err(RegistryError::DuplicateSelector)
    ));
    Ok(())
}

#[test]
fn required_wrapper_entries_report_stale_coverage() -> TestResult {
    let required = SinkSpec::project_function(
        "fixture.required",
        "required_writer",
        DefinitionSpec::from_function_source(
            "crates/sample/src/required.rs",
            "required_writer",
            "fn required_writer() {}",
        )?,
        OperationKind::Write,
        WriterRole::Publication,
        FlowClass::None,
    )?;
    let registry = SinkRegistry::try_new(1, vec![required])?;
    let source = source("crates/sample/src/empty.rs", "fn empty() {}")?;
    let inventory = analyze_writers(&[source], &registry)?;
    assert_eq!(inventory.unobserved_required_sinks().len(), 1);
    assert!(!inventory.is_registry_complete());
    Ok(())
}

#[test]
fn inventories_bind_exact_sorted_sources_and_registry_semantics() -> TestResult {
    let first = source("crates/sample/src/b.rs", "fn value() -> u8 { 1 }")?;
    let second = source("crates/sample/src/a.rs", "fn value() -> u8 { 2 }")?;
    let changed = source("crates/sample/src/a.rs", "fn value() -> u8 { 3 }")?;
    let builtin = builtin_sink_registry()?;
    let forward = analyze_writers(&[first.clone(), second.clone()], &builtin)?;
    let reverse = analyze_writers(&[second, first.clone()], &builtin)?;
    let changed = analyze_writers(&[changed, first], &builtin)?;
    let empty = SinkRegistry::try_new(1, Vec::new())?;
    let extension = analyze_writers(&[], &empty)?;

    assert_eq!(forward.sources(), reverse.sources());
    assert_eq!(
        forward
            .sources()
            .iter()
            .map(|identity| identity.path().as_str())
            .collect::<Vec<_>>(),
        ["crates/sample/src/a.rs", "crates/sample/src/b.rs"]
    );
    assert_ne!(
        forward.source_inventory_digest(),
        changed.source_inventory_digest()
    );
    assert_eq!(forward.registry_digest(), builtin.digest());
    assert_ne!(forward.registry_digest(), extension.registry_digest());
    assert!(extension.is_registry_complete());
    Ok(())
}

#[test]
fn registry_digest_binds_semantics_without_binding_declaration_order() -> TestResult {
    let first = SinkSpec::function(
        "fixture.first",
        "fixture::first",
        OperationKind::Write,
        WriterRole::HandleMutation,
        FlowClass::None,
        SinkOrigin::Reviewed,
    )?;
    let second = SinkSpec::function(
        "fixture.second",
        "fixture::second",
        OperationKind::Rename,
        WriterRole::Publication,
        FlowClass::None,
        SinkOrigin::Reviewed,
    )?;
    let changed = SinkSpec::function(
        "fixture.first",
        "fixture::first",
        OperationKind::Write,
        WriterRole::Publication,
        FlowClass::None,
        SinkOrigin::Reviewed,
    )?;
    let forward = SinkRegistry::try_new(1, vec![first.clone(), second.clone()])?;
    let reverse = SinkRegistry::try_new(1, vec![second.clone(), first])?;
    let changed = SinkRegistry::try_new(1, vec![changed, second])?;

    assert_eq!(forward.digest(), reverse.digest());
    assert_ne!(forward.digest(), changed.digest());
    Ok(())
}

#[test]
fn registry_digest_and_uniqueness_bind_exact_project_definitions() -> TestResult {
    let first_definition = DefinitionSpec::from_function_source(
        "crates/sample/src/wrapper.rs",
        "make_writer",
        "fn make_writer() -> std::fs::File { std::fs::File::create(\"one\") }",
    )?;
    let changed_definition = DefinitionSpec::from_function_source(
        "crates/sample/src/wrapper.rs",
        "make_writer",
        "fn make_writer() -> std::fs::File { std::fs::File::create(\"two\") }",
    )?;
    let first = SinkSpec::project_function(
        "fixture.first",
        "crate::wrapper::make_writer",
        first_definition.clone(),
        OperationKind::Open,
        WriterRole::RootOpen,
        FlowClass::WritableHandle,
    )?;
    let changed = SinkSpec::project_function(
        "fixture.first",
        "crate::wrapper::make_writer",
        changed_definition,
        OperationKind::Open,
        WriterRole::RootOpen,
        FlowClass::WritableHandle,
    )?;
    assert_ne!(
        SinkRegistry::try_new(1, vec![first.clone()])?.digest(),
        SinkRegistry::try_new(1, vec![changed])?.digest()
    );

    let duplicate_definition = SinkSpec::project_function(
        "fixture.second",
        "crate::alias::make_writer",
        first_definition,
        OperationKind::Open,
        WriterRole::RootOpen,
        FlowClass::WritableHandle,
    )?;
    assert!(matches!(
        SinkRegistry::try_new(1, vec![first, duplicate_definition]),
        Err(RegistryError::DuplicateDefinition)
    ));
    Ok(())
}

#[test]
fn definition_authority_is_exactly_one_complete_function_without_trailing_syntax() {
    for definition in [
        "fn make_writer()",
        "fn make_writer() {} const EXTRA: usize = 1;",
        "fn make_writer() {} fn decoy()",
    ] {
        assert!(matches!(
            DefinitionSpec::from_function_source(
                "crates/sample/src/wrapper.rs",
                "make_writer",
                definition,
            ),
            Err(RegistryError::DefinitionAuthority)
        ));
    }
    assert!(matches!(
        DefinitionSpec::from_function_source(
            "crates/sample/src/wrapper.rs",
            "PrivateRoot::make_writer",
            "fn make_writer(&self) {} fn decoy(&self) {}",
        ),
        Err(RegistryError::DefinitionAuthority)
    ));
}
