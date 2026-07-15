use std::collections::BTreeSet;

use norn_policy::writers::UnknownSinkReason;

use super::support::{TestResult, analyze};

#[test]
fn standard_mutation_matrix_covers_copy_symlink_and_write_fmt() -> TestResult {
    let inventory = analyze(
        "crates/sample/src/matrix.rs",
        r#"
use std::os::unix::fs as unix_fs;

fn mutate(file: &mut std::fs::File) {
    std::fs::copy("from", "to");
    tokio::fs::copy("from", "to");
    unix_fs::symlink("from", "to");
    std::os::windows::fs::symlink_file("from", "to");
    std::os::windows::fs::symlink_dir("from", "to");
    tokio::fs::symlink("from", "to");
    file.write_fmt(format_args!("fixed"));
}
"#,
    )?;
    let sinks: BTreeSet<&str> = inventory
        .operations()
        .iter()
        .map(|operation| operation.sink().as_str())
        .collect();
    for sink in [
        "std.fs.copy",
        "tokio.fs.copy",
        "std.fs.symlink.unix",
        "std.fs.symlink.windows_file",
        "std.fs.symlink.windows_dir",
        "tokio.fs.symlink",
        "io.handle.write_fmt",
    ] {
        assert!(sinks.contains(sink));
    }
    assert!(inventory.candidates().is_empty());
    Ok(())
}

#[test]
fn unknown_std_namespace_calls_fail_closed_while_reviewed_reads_do_not() -> TestResult {
    let inventory = analyze(
        "crates/sample/src/future_std.rs",
        r#"
fn inspect() {
    std::fs::read("input");
    std::fs::future_mutation("output");
}
"#,
    )?;
    assert!(inventory.operations().is_empty());
    assert_eq!(inventory.candidates().len(), 1);
    assert_eq!(
        inventory.candidates()[0].reason(),
        UnknownSinkReason::KnownNamespaceCandidate
    );
    assert_eq!(
        inventory.candidates()[0].candidate().as_str(),
        "future_mutation"
    );
    assert!(!inventory.is_registry_complete());
    Ok(())
}

#[test]
fn parenthesized_and_locally_bound_writer_callables_are_not_hidden() -> TestResult {
    let inventory = analyze(
        "crates/sample/src/callable.rs",
        r#"
fn consume<T>(value: T) {}

fn mutate() {
    ((std::fs::write))("direct", b"x");
    let writer = (std::fs::write);
    writer("bound", b"x");
    let second = writer;
    (second)("second", b"x");
    consume(std::fs::copy);
}
"#,
    )?;
    assert_eq!(
        inventory
            .operations()
            .iter()
            .filter(|operation| operation.sink().as_str() == "std.fs.write")
            .count(),
        3
    );
    assert!(inventory.candidates().iter().any(|unknown| {
        unknown.reason() == UnknownSinkReason::CallableEscape
            && unknown.candidate().as_str() == "std.fs.copy"
    }));
    Ok(())
}

#[test]
fn writer_call_results_are_not_reinterpreted_as_callable_paths() -> TestResult {
    let inventory = analyze(
        "crates/sample/src/call_result.rs",
        r#"
fn mutate() {
    let result = std::fs::write("artifact", b"data");
    drop(result);
}
"#,
    )?;
    assert_eq!(inventory.operations().len(), 1);
    assert!(inventory.candidates().is_empty());
    Ok(())
}

#[test]
fn macro_rules_expansion_bodies_are_typed_unknowns() -> TestResult {
    let inventory = analyze(
        "crates/sample/src/defined_macro.rs",
        r"
macro_rules! persist {
    ($from:expr, $to:expr) => {
        std::fs::copy($from, $to)
    };
}
",
    )?;
    assert!(inventory.candidates().iter().any(|unknown| {
        unknown.reason() == UnknownSinkReason::MacroDefinitionCandidate
            && unknown.candidate().as_str() == "copy"
    }));
    assert!(!inventory.is_registry_complete());
    Ok(())
}
