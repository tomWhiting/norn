use std::collections::BTreeSet;

use norn_policy::writers::{UnknownSinkReason, analyze_writers, builtin_sink_registry};

use super::support::{TestResult, source};

#[test]
fn grouped_chains_substitute_function_and_type_prefixes() -> TestResult {
    let facade = source(
        "crates/sample/src/facade.rs",
        r#"
pub use std::fs::{
    self as disk,
    File as StoredFile,
    OpenOptions as StoredOptions,
};
"#,
    )?;
    let bridge = source(
        "crates/sample/src/bridge.rs",
        r#"
pub(crate) use crate::facade::{
    disk as filesystem,
    StoredFile as DiskFile,
    StoredOptions as DiskOptions,
};
"#,
    )?;
    let caller = source(
        "crates/sample/src/caller.rs",
        r#"
use crate::bridge::{DiskFile, DiskOptions, filesystem};

fn finish(file: DiskFile) {
    file.sync_all();
}

fn run() {
    let file = DiskFile::create("first");
    let options = DiskOptions::new();
    options.create(true).truncate(true).open("second");
    filesystem::write("third", b"data");
    finish(file);
}
"#,
    )?;
    let inventory = analyze_writers(&[caller, bridge, facade], &builtin_sink_registry()?)?;
    let sinks: BTreeSet<&str> = inventory
        .operations()
        .iter()
        .map(|operation| operation.sink().as_str())
        .collect();
    for expected in [
        "std.file.create",
        "std.open_options.new",
        "std.open_options.create",
        "std.open_options.truncate",
        "std.open_options.open",
        "std.fs.write",
        "io.handle.sync_all",
    ] {
        assert!(
            sinks.contains(expected),
            "missing re-exported sink {expected}"
        );
    }
    assert!(
        inventory.candidates().is_empty(),
        "unexpected unknowns: {:?}",
        inventory.candidates()
    );
    Ok(())
}

#[test]
fn restricted_visibility_and_crate_partitions_are_exact() -> TestResult {
    let first_facade = source(
        "crates/first/src/facade.rs",
        "pub(super) use std::fs::write as persist;",
    )?;
    let first_caller = source(
        "crates/first/src/lib.rs",
        "fn run() { crate::facade::persist(\"first\", b\"one\"); }",
    )?;
    let second_facade = source(
        "crates/second/src/facade.rs",
        "pub(in crate) use std::fs::rename as persist;",
    )?;
    let second_caller = source(
        "crates/second/src/lib.rs",
        "fn run() { crate::facade::persist(\"second\", \"third\"); }",
    )?;
    let inventory = analyze_writers(
        &[second_caller, first_facade, first_caller, second_facade],
        &builtin_sink_registry()?,
    )?;
    let sinks: BTreeSet<&str> = inventory
        .operations()
        .iter()
        .map(|operation| operation.sink().as_str())
        .collect();
    assert!(sinks.contains("std.fs.write"));
    assert!(sinks.contains("std.fs.rename"));
    assert!(inventory.candidates().is_empty());
    Ok(())
}

#[test]
fn duplicate_and_cyclic_aliases_fail_closed() -> TestResult {
    let first = source(
        "crates/sample/src/facade.rs",
        "pub use std::fs::write as persist;",
    )?;
    let duplicate = source(
        "crates/sample/src/facade/mod.rs",
        "pub use std::fs::rename as persist;",
    )?;
    let caller = source(
        "crates/sample/src/caller.rs",
        "fn run() { crate::facade::persist(\"one\", b\"two\"); }",
    )?;
    let ambiguous = analyze_writers(&[first, duplicate, caller], &builtin_sink_registry()?)?;
    assert!(ambiguous.candidates().iter().any(|unknown| {
        unknown.reason() == UnknownSinkReason::AmbiguousAlias
            && unknown.candidate().as_str() == "persist"
    }));

    let first = source(
        "crates/cycle/src/first.rs",
        "pub use crate::second::persist;",
    )?;
    let second = source(
        "crates/cycle/src/second.rs",
        "pub use crate::first::persist;",
    )?;
    let caller = source(
        "crates/cycle/src/lib.rs",
        "fn run() { crate::first::persist(\"one\", b\"two\"); }",
    )?;
    let cycle = analyze_writers(&[first, second, caller], &builtin_sink_registry()?)?;
    assert!(cycle.candidates().iter().any(|unknown| {
        unknown.reason() == UnknownSinkReason::AmbiguousAlias
            && unknown.candidate().as_str() == "persist"
    }));
    Ok(())
}

#[test]
fn writer_globs_fail_closed_without_terminal_guessing() -> TestResult {
    let facade = source("crates/glob/src/facade.rs", "pub use std::fs::*;")?;
    let caller = source(
        "crates/glob/src/lib.rs",
        "fn run() { crate::facade::write(\"one\", b\"two\"); }",
    )?;
    let inventory = analyze_writers(&[facade, caller], &builtin_sink_registry()?)?;
    assert!(inventory.candidates().iter().any(|unknown| {
        unknown.reason() == UnknownSinkReason::WildcardImport
            && unknown.candidate().as_str() == "write"
    }));
    assert!(inventory.operations().is_empty());
    Ok(())
}

#[test]
fn private_and_test_only_imports_do_not_enter_the_export_graph() -> TestResult {
    let facade = source(
        "crates/sample/src/facade.rs",
        r#"
use std::fs::write as local_write;

#[cfg(test)]
pub use std::fs::write as test_write;

fn local() {
    local_write("local", b"data");
}
"#,
    )?;
    let caller = source(
        "crates/sample/src/lib.rs",
        r#"
fn run() {
    crate::facade::local_write("private", b"data");
    crate::facade::test_write("test", b"data");
}
"#,
    )?;
    let inventory = analyze_writers(&[facade, caller], &builtin_sink_registry()?)?;
    assert_eq!(
        inventory
            .operations()
            .iter()
            .filter(|operation| operation.sink().as_str() == "std.fs.write")
            .count(),
        1
    );
    assert!(
        inventory
            .operations()
            .iter()
            .all(|operation| operation.path().as_str() != "crates/sample/src/lib.rs")
    );
    Ok(())
}

#[test]
fn renamed_callable_escape_is_not_silently_omitted() -> TestResult {
    let facade = source(
        "crates/sample/src/facade.rs",
        "pub use std::fs::write as persist;",
    )?;
    let caller = source(
        "crates/sample/src/lib.rs",
        r#"
fn consume<T>(_: T) {}
fn run() { consume(crate::facade::persist); }
"#,
    )?;
    let inventory = analyze_writers(&[facade, caller], &builtin_sink_registry()?)?;
    assert!(inventory.candidates().iter().any(|unknown| {
        unknown.reason() == UnknownSinkReason::CallableEscape
            && unknown.candidate().as_str() == "std.fs.write"
    }));
    Ok(())
}
