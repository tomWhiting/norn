use norn_policy::writers::{
    DefinitionSpec, FlowClass, OperationKind, ReceiverConstraint, SinkOrigin, SinkRegistry,
    SinkSpec, UnknownSinkReason, WriterRole, analyze_writers,
};

use super::support::{TestResult, source};

const DEFINITION_PATH: &str = "crates/sample/src/fixture.rs";
const DEFINITION: &str = "fn make_writer() -> std::fs::File { std::fs::File::create(\"out\") }";

#[test]
fn exact_definition_and_qualified_call_propagate_return_authority() -> TestResult {
    let registry = registry()?;
    let definition = source(
        DEFINITION_PATH,
        "fn make_writer() -> std::fs::File { std::fs::File::create(\"out\") }",
    )?;
    let caller = source(
        "crates/sample/src/caller.rs",
        r#"
fn run() {
    let mut file = crate::fixture::make_writer();
    file.write_all(b"data");
}
"#,
    )?;
    let inventory = analyze_writers(&[definition, caller], &registry)?;
    assert!(inventory.unobserved_required_sinks().is_empty());
    assert!(inventory.candidates().is_empty());
    assert!(
        inventory
            .operations()
            .iter()
            .any(|operation| { operation.sink().as_str() == "fixture.make_writer" })
    );
    assert!(
        inventory
            .operations()
            .iter()
            .any(|operation| { operation.sink().as_str() == "io.handle.write_all" })
    );
    Ok(())
}

#[test]
fn deletion_rename_signature_drift_and_duplicate_definitions_fail_closed() -> TestResult {
    let registry = registry()?;
    for text in [
        "fn unrelated() {}",
        "fn renamed() -> std::fs::File { std::fs::File::create(\"out\") }",
    ] {
        let inventory = analyze_writers(&[source(DEFINITION_PATH, text)?], &registry)?;
        assert_eq!(inventory.unobserved_required_sinks().len(), 1);
    }

    for text in [
        "fn make_writer(path: &str) -> std::fs::File { std::fs::File::create(path) }",
        "fn make_writer() -> std::fs::File { std::fs::File::create(\"changed\") }",
        r#"
fn make_writer() -> std::fs::File { std::fs::File::create("one") }
fn make_writer() -> std::fs::File { std::fs::File::create("two") }
"#,
    ] {
        let inventory = analyze_writers(&[source(DEFINITION_PATH, text)?], &registry)?;
        assert_eq!(inventory.unobserved_required_sinks().len(), 1);
        assert!(
            inventory
                .candidates()
                .iter()
                .any(|unknown| { unknown.reason() == UnknownSinkReason::DefinitionMismatch })
        );
    }
    Ok(())
}

#[test]
fn trailing_parameter_comma_is_exact_definition_authority() -> TestResult {
    let spec = SinkSpec::project_function(
        "fixture.make_writer",
        "crate::fixture::make_writer",
        DefinitionSpec::from_function_source(
            DEFINITION_PATH,
            "make_writer",
            "fn make_writer(path: &str,) -> std::fs::File { loop {} }",
        )?,
        OperationKind::Open,
        WriterRole::RootOpen,
        FlowClass::WritableHandle,
    )?;
    let registry = SinkRegistry::try_new(1, vec![spec])?;
    let exact = source(
        DEFINITION_PATH,
        "fn make_writer(\n    path: &str,\n) -> std::fs::File { loop {} }",
    )?;
    let exact_inventory = analyze_writers(&[exact], &registry)?;
    assert!(exact_inventory.unobserved_required_sinks().is_empty());
    assert!(exact_inventory.candidates().is_empty());

    let drifted = source(
        DEFINITION_PATH,
        "fn make_writer(\n    path: &str\n) -> std::fs::File { loop {} }",
    )?;
    let drifted_inventory = analyze_writers(&[drifted], &registry)?;
    assert_eq!(drifted_inventory.unobserved_required_sinks().len(), 1);
    assert!(
        drifted_inventory
            .candidates()
            .iter()
            .any(|unknown| { unknown.reason() == UnknownSinkReason::DefinitionMismatch })
    );
    Ok(())
}

#[test]
fn wrapper_calls_cannot_satisfy_missing_definition_authority() -> TestResult {
    let registry = registry()?;
    let caller = source(
        "crates/sample/src/caller.rs",
        "fn run() { let handle = crate::fixture::make_writer(); drop(handle); }",
    )?;
    let inventory = analyze_writers(&[caller], &registry)?;
    assert!(
        inventory
            .operations()
            .iter()
            .any(|operation| { operation.sink().as_str() == "fixture.make_writer" })
    );
    assert_eq!(inventory.unobserved_required_sinks().len(), 1);
    Ok(())
}

#[test]
fn same_name_decoy_in_another_module_is_not_the_reviewed_wrapper() -> TestResult {
    let registry = registry()?;
    let definition = source(
        DEFINITION_PATH,
        "fn make_writer() -> std::fs::File { std::fs::File::create(\"out\") }",
    )?;
    let decoy = source(
        "crates/sample/src/decoy.rs",
        r#"
fn make_writer() -> std::fs::File { std::fs::File::create("decoy") }
fn run() { let handle = make_writer(); drop(handle); }
"#,
    )?;
    let inventory = analyze_writers(&[definition, decoy], &registry)?;
    assert!(inventory.unobserved_required_sinks().is_empty());
    assert!(inventory.candidates().iter().any(|unknown| {
        unknown.reason() == UnknownSinkReason::NewWrapperCandidate
            && unknown.candidate().as_str() == "make_writer"
    }));
    assert!(!inventory.candidates().iter().any(|unknown| {
        unknown.reason() == UnknownSinkReason::UnresolvedAlias
            && unknown.candidate().as_str() == "make_writer"
    }));
    assert_eq!(
        inventory
            .operations()
            .iter()
            .filter(|operation| operation.sink().as_str() == "fixture.make_writer")
            .count(),
        0
    );
    Ok(())
}

#[test]
fn inline_module_decoy_in_the_definition_source_is_not_the_reviewed_item() -> TestResult {
    let registry = registry()?;
    let source = source(
        DEFINITION_PATH,
        r#"
fn make_writer() -> std::fs::File { std::fs::File::create("out") }

mod decoy {
    fn make_writer() -> std::fs::File { std::fs::File::create("decoy") }
    fn run() {
        let handle = make_writer();
        drop(handle);
    }
}
"#,
    )?;
    let inventory = analyze_writers(&[source], &registry)?;
    assert!(inventory.unobserved_required_sinks().is_empty());
    assert!(inventory.candidates().iter().any(|unknown| {
        unknown.reason() == UnknownSinkReason::NewWrapperCandidate
            && unknown.candidate().as_str() == "make_writer"
    }));
    assert!(!inventory.candidates().iter().any(|unknown| {
        unknown.reason() == UnknownSinkReason::UnresolvedAlias
            && unknown.candidate().as_str() == "make_writer"
    }));
    assert_eq!(
        inventory
            .operations()
            .iter()
            .filter(|operation| operation.sink().as_str() == "fixture.make_writer")
            .count(),
        0
    );
    Ok(())
}

#[test]
fn exact_method_definition_propagates_its_registered_return_authority() -> TestResult {
    let registry = method_registry()?;
    let source = source(
        DEFINITION_PATH,
        r#"
struct PrivateRoot;

impl PrivateRoot {
    fn make_writer(&self) -> std::fs::File {
        std::fs::File::create("reviewed")
    }
}

fn run(root: &PrivateRoot) {
    let mut handle = root.make_writer();
    handle.write_all(b"data");
}
"#,
    )?;
    let inventory = analyze_writers(&[source], &registry)?;
    assert!(inventory.unobserved_required_sinks().is_empty());
    assert!(inventory.candidates().is_empty());
    for sink in ["fixture.method.make_writer", "io.handle.write_all"] {
        assert!(
            inventory
                .operations()
                .iter()
                .any(|operation| operation.sink().as_str() == sink)
        );
    }
    Ok(())
}

#[test]
fn same_named_project_methods_require_exact_receiver_definitions() -> TestResult {
    let registry = colliding_method_registry()?;
    let source = source(
        DEFINITION_PATH,
        r"
struct FirstRoot;
struct SecondRoot;
struct UnregisteredPrivateRoot;

impl FirstRoot {
    fn open() -> Self { Self }
    fn publish(&self) {}
}

impl SecondRoot {
    fn open() -> Self { Self }
    fn publish(&self) {}
}

impl UnregisteredPrivateRoot {
    fn publish(&self) {}
}

fn run(unregistered: &UnregisteredPrivateRoot) {
    let first = FirstRoot::open();
    let second = SecondRoot::open();
    first.publish();
    second.publish();
    unregistered.publish();
}
",
    )?;
    let inventory = analyze_writers(&[source], &registry)?;
    assert!(inventory.unobserved_required_sinks().is_empty());
    for sink in ["fixture.first.publish", "fixture.second.publish"] {
        assert_eq!(
            inventory
                .operations()
                .iter()
                .filter(|operation| operation.sink().as_str() == sink)
                .count(),
            1
        );
    }
    assert!(inventory.candidates().iter().any(|unknown| {
        unknown.reason() == UnknownSinkReason::DynamicReceiver
            && unknown.candidate().as_str() == "publish"
    }));
    Ok(())
}

#[test]
fn unqualified_receiver_authority_does_not_cross_source_boundaries() -> TestResult {
    let registry = colliding_method_registry()?;
    let definitions = source(
        DEFINITION_PATH,
        r"
struct FirstRoot;
struct SecondRoot;

impl FirstRoot {
    fn open() -> Self { Self }
    fn publish(&self) {}
}

impl SecondRoot {
    fn open() -> Self { Self }
    fn publish(&self) {}
}
",
    )?;
    let decoy = source(
        "crates/sample/src/decoy.rs",
        r"
struct FirstRoot;
fn run(root: &FirstRoot) { root.publish(); }
",
    )?;
    let inventory = analyze_writers(&[definitions, decoy], &registry)?;
    assert!(inventory.unobserved_required_sinks().is_empty());
    assert!(inventory.candidates().iter().any(|unknown| {
        unknown.reason() == UnknownSinkReason::DynamicReceiver
            && unknown.candidate().as_str() == "publish"
    }));
    assert!(inventory.operations().iter().all(|operation| {
        operation.path().as_str() != "crates/sample/src/decoy.rs"
            || !operation.sink().as_str().ends_with(".publish")
    }));
    Ok(())
}

fn registry() -> Result<SinkRegistry, Box<dyn std::error::Error>> {
    let specs = vec![
        SinkSpec::function(
            "std.file.create",
            "std::fs::File::create",
            OperationKind::Create,
            WriterRole::RootOpen,
            FlowClass::WritableHandle,
            SinkOrigin::Standard,
        )?,
        SinkSpec::method(
            "io.handle.write_all",
            "write_all",
            ReceiverConstraint::WritableHandle,
            OperationKind::Write,
            WriterRole::HandleMutation,
            FlowClass::None,
            SinkOrigin::Standard,
        )?,
        SinkSpec::project_function(
            "fixture.make_writer",
            "crate::fixture::make_writer",
            DefinitionSpec::from_function_source(DEFINITION_PATH, "make_writer", DEFINITION)?,
            OperationKind::Open,
            WriterRole::RootOpen,
            FlowClass::WritableHandle,
        )?,
    ];
    Ok(SinkRegistry::try_new(1, specs)?)
}

fn method_registry() -> Result<SinkRegistry, Box<dyn std::error::Error>> {
    let mut specs = registry()?
        .specs()
        .iter()
        .filter(|spec| spec.definition().is_none())
        .cloned()
        .collect::<Vec<_>>();
    specs.push(SinkSpec::project_method(
        "fixture.method.make_writer",
        "make_writer",
        ReceiverConstraint::RootAuthority,
        DefinitionSpec::from_function_source(
            DEFINITION_PATH,
            "PrivateRoot::make_writer",
            r#"fn make_writer(&self) -> std::fs::File {
                std::fs::File::create("reviewed")
            }"#,
        )?,
        OperationKind::Open,
        WriterRole::RootOpen,
        FlowClass::WritableHandle,
    )?);
    Ok(SinkRegistry::try_new(1, specs)?)
}

fn colliding_method_registry() -> Result<SinkRegistry, Box<dyn std::error::Error>> {
    let mut specs = Vec::new();
    for (open_id, method_id, receiver) in [
        ("fixture.first.open", "fixture.first.publish", "FirstRoot"),
        (
            "fixture.second.open",
            "fixture.second.publish",
            "SecondRoot",
        ),
    ] {
        specs.push(SinkSpec::project_function(
            open_id,
            &format!("{receiver}::open"),
            DefinitionSpec::from_function_source(
                DEFINITION_PATH,
                &format!("{receiver}::open"),
                "fn open() -> Self { Self }",
            )?,
            OperationKind::Open,
            WriterRole::RootOpen,
            FlowClass::RootAuthority,
        )?);
        specs.push(SinkSpec::project_method(
            method_id,
            "publish",
            ReceiverConstraint::RootAuthority,
            DefinitionSpec::from_function_source(
                DEFINITION_PATH,
                &format!("{receiver}::publish"),
                "fn publish(&self) {}",
            )?,
            OperationKind::Write,
            WriterRole::Publication,
            FlowClass::None,
        )?);
    }
    Ok(SinkRegistry::try_new(1, specs)?)
}
