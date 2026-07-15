use std::collections::BTreeSet;

use norn_policy::writers::{
    DefinitionSpec, FlowClass, OperationKind, SinkRegistry, SinkSpec, WriterRole, analyze_writers,
    builtin_sink_registry,
};

use super::support::{TestResult, analyze, analyze_with, source};

#[test]
fn imports_aliases_and_local_handle_references_resolve() -> TestResult {
    let inventory = analyze(
        "crates/sample/src/lib.rs",
        r#"
use std::fs as disk;
use std::fs::File as FsFile;

fn run() -> std::io::Result<()> {
    let mut file = FsFile::create("alpha")?;
    file.write_all(b"one")?;
    let alias = &mut file;
    alias.flush()?;
    disk::rename("alpha", "beta")?;
    Ok(())
}
"#,
    )?;
    let sinks: BTreeSet<&str> = inventory
        .operations()
        .iter()
        .map(|operation| operation.sink().as_str())
        .collect();
    assert!(sinks.contains("std.file.create"));
    assert!(sinks.contains("io.handle.write_all"));
    assert!(sinks.contains("io.handle.flush"));
    assert!(sinks.contains("std.fs.rename"));
    assert!(
        inventory.candidates().is_empty(),
        "unexpected unknowns: {:?}",
        inventory.candidates()
    );
    Ok(())
}

#[test]
fn renamed_facade_reexport_is_resolved_exactly() -> TestResult {
    let facade = source(
        "crates/sample/src/facade.rs",
        "pub use std::fs::write as save;",
    )?;
    let caller_path = "crates/sample/src/caller.rs";
    let caller = source(
        caller_path,
        r#"
use crate::facade::save as store;

fn run() {
    store("artifact", b"data");
}
"#,
    )?;
    let inventory = analyze_writers(&[facade, caller], &builtin_sink_registry()?)?;
    assert!(
        inventory.operations().iter().any(|operation| {
            operation.path().as_str() == caller_path && operation.sink().as_str() == "std.fs.write"
        }),
        "renamed writer facade was not resolved: {:?}",
        inventory.candidates()
    );
    assert!(inventory.candidates().is_empty());
    Ok(())
}

#[test]
fn registered_wrapper_propagates_a_writable_handle() -> TestResult {
    let mut specs: Vec<SinkSpec> = builtin_sink_registry()?
        .specs()
        .iter()
        .filter(|spec| spec.definition().is_none())
        .cloned()
        .collect();
    specs.push(SinkSpec::project_function(
        "fixture.make_writer",
        "make_writer",
        DefinitionSpec::from_function_source(
            "crates/sample/src/wrapper.rs",
            "make_writer",
            r#"fn make_writer() -> std::fs::File {
                std::fs::File::create("alpha")
            }"#,
        )?,
        OperationKind::Open,
        WriterRole::RootOpen,
        FlowClass::WritableHandle,
    )?);
    let registry = SinkRegistry::try_new(1, specs)?;
    let inventory = analyze_with(
        "crates/sample/src/wrapper.rs",
        r#"
fn make_writer() -> std::fs::File {
    std::fs::File::create("alpha")
}

fn run() {
    let mut file = make_writer();
    file.write_all(b"one");
}
"#,
        &registry,
    )?;
    assert!(
        inventory
            .operations()
            .iter()
            .any(|operation| operation.sink().as_str() == "fixture.make_writer")
    );
    assert!(
        inventory
            .operations()
            .iter()
            .any(|operation| operation.sink().as_str() == "io.handle.write_all")
    );
    assert!(inventory.candidates().is_empty());
    assert!(inventory.unobserved_required_sinks().is_empty());
    Ok(())
}

#[test]
fn builder_chains_and_concrete_parameters_propagate() -> TestResult {
    let inventory = analyze(
        "crates/sample/src/options.rs",
        r#"
fn finish(file: &mut std::fs::File) {
    file.set_len(4);
    file.sync_all();
}

fn run() {
    let options = std::fs::OpenOptions::new();
    let mut file = options.create(true).truncate(true).open("alpha");
    finish(&mut file);
}
"#,
    )?;
    let kinds: BTreeSet<OperationKind> = inventory
        .operations()
        .iter()
        .map(norn_policy::writers::WriterOperation::kind)
        .collect();
    assert!(kinds.contains(&OperationKind::Create));
    assert!(kinds.contains(&OperationKind::Truncate));
    assert!(kinds.contains(&OperationKind::Open));
    assert!(kinds.contains(&OperationKind::SetLength));
    assert!(kinds.contains(&OperationKind::Sync));
    assert!(
        inventory.candidates().is_empty(),
        "unexpected unknowns: {:?}",
        inventory.candidates()
    );
    Ok(())
}

#[test]
fn test_only_writer_calls_are_excluded() -> TestResult {
    let source = source(
        "crates/sample/src/cfg.rs",
        r#"
#[cfg(test)]
fn hidden() {
    std::fs::write("hidden", b"x");
}

fn visible() {
    std::fs::write("visible", b"x");
}
"#,
    )?;
    let inventory = analyze_writers(&[source], &builtin_sink_registry()?)?;
    assert_eq!(inventory.operations().len(), 1);
    assert!(inventory.candidates().is_empty());
    Ok(())
}
