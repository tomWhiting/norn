use std::collections::BTreeSet;

use norn_policy::writers::{OperationKind, UnknownSinkReason};

use super::support::{TestResult, analyze};

#[test]
fn raw_identifiers_comments_and_aliases_preserve_writer_semantics() -> TestResult {
    let inventory = analyze(
        "crates/sample/src/raw.rs",
        r#"
use std::fs::r#write as persist;
use std::r#write as r#emit;

fn run(file: &mut std::fs::File) {
    std::fs::/* structural comment */r#write("direct", b"one");
    persist("alias", b"two");
    file.r#write_all(b"three");
    r#emit!(file, "four");
}
"#,
    )?;
    let sinks: Vec<&str> = inventory
        .operations()
        .iter()
        .map(|operation| operation.sink().as_str())
        .collect();
    assert_eq!(
        sinks
            .iter()
            .copied()
            .filter(|sink| *sink == "std.fs.write")
            .count(),
        2
    );
    assert!(sinks.contains(&"io.handle.write_all"));
    assert!(sinks.contains(&"std.macro.write.qualified"));
    assert!(inventory.candidates().is_empty());
    Ok(())
}

#[test]
fn reviewed_builder_modes_preserve_standard_and_tokio_authority() -> TestResult {
    let inventory = analyze(
        "crates/sample/src/options.rs",
        r#"
fn run() {
    let standard = std::fs::OpenOptions::new();
    let standard_file = standard
        .read(true)
        .mode(0o600)
        .custom_flags(0)
        .open("standard");
    std::mem::drop(standard_file);

    let asynchronous = tokio::fs::OpenOptions::new();
    let asynchronous_file = asynchronous
        .read(true)
        .mode(0o600)
        .custom_flags(0)
        .open("asynchronous");
    std::mem::drop(asynchronous_file);
}
"#,
    )?;
    let open_count = inventory
        .operations()
        .iter()
        .filter(|operation| operation.kind() == OperationKind::Open)
        .count();
    assert!(open_count >= 4);
    for (sink, kind) in [
        ("std.open_options.mode", OperationKind::Permissions),
        ("std.open_options.custom_flags", OperationKind::Open),
        ("tokio.open_options.mode", OperationKind::Permissions),
        ("tokio.open_options.custom_flags", OperationKind::Open),
    ] {
        let operation = inventory
            .operations()
            .iter()
            .find(|operation| operation.sink().as_str() == sink);
        assert!(operation.is_some_and(|operation| operation.kind() == kind));
    }
    assert!(inventory.candidates().is_empty());
    Ok(())
}

#[test]
fn authority_adapters_require_exact_standard_provenance() -> TestResult {
    let rejected = analyze(
        "crates/sample/src/provenance.rs",
        r"
fn drop<T>(value: T) -> T { value }

fn rejected(
    first: std::fs::File,
    second: std::fs::File,
    third: std::fs::File,
    fourth: std::fs::File,
    fifth: std::fs::File,
    sixth: std::fs::File,
) {
    let boxed = Box::new(first);
    let returned = drop(second);
    Arc::new(third);
    Rc::new(fourth);
    BufWriter::new(fifth);
    Some(sixth);
    Ok::<std::fs::File, ()>(sixth);
    third.as_ref();
    third.as_mut();
    third.borrow();
    third.borrow_mut();
    third.deref();
    third.deref_mut();
    std::mem::drop((boxed, returned));
}
",
    )?;
    assert!(rejected.candidates().iter().any(|unknown| {
        unknown.reason() == UnknownSinkReason::AuthorityArgument
            && unknown.candidate().as_str() == "new"
    }));
    assert!(rejected.candidates().iter().any(|unknown| {
        unknown.reason() == UnknownSinkReason::AuthorityArgument
            && unknown.candidate().as_str() == "drop"
    }));
    for candidate in ["some", "ok"] {
        assert!(rejected.candidates().iter().any(|unknown| {
            unknown.reason() == UnknownSinkReason::AuthorityArgument
                && unknown.candidate().as_str() == candidate
        }));
    }
    for candidate in [
        "as_ref",
        "as_mut",
        "borrow",
        "borrow_mut",
        "deref",
        "deref_mut",
    ] {
        assert!(rejected.candidates().iter().any(|unknown| {
            unknown.reason() == UnknownSinkReason::AuthorityMethod
                && unknown.candidate().as_str() == candidate
        }));
    }

    let accepted = analyze(
        "crates/sample/src/exact_provenance.rs",
        r"
use std::boxed::Box as StdBox;
use std::mem::drop as release;

fn accepted(first: std::fs::File, second: std::fs::File) {
    let direct = std::boxed::Box::new(first);
    let imported = StdBox::new(second);
    std::mem::drop(direct);
    release(imported);
}
",
    )?;
    assert!(accepted.candidates().is_empty());
    Ok(())
}

#[test]
fn opaque_return_types_cannot_hide_unregistered_authority() -> TestResult {
    let inventory = analyze(
        "crates/sample/src/opaque_return.rs",
        r"
type Package = std::fs::File;

fn package(file: std::fs::File) -> Package {
    file
}
",
    )?;
    assert!(inventory.candidates().iter().any(|unknown| {
        unknown.reason() == UnknownSinkReason::NewWrapperCandidate
            && unknown.candidate().as_str() == "package"
    }));
    Ok(())
}

#[test]
fn borrowed_moved_boxed_returned_and_method_authority_fail_closed() -> TestResult {
    let inventory = analyze(
        "crates/sample/src/authority.rs",
        r"
fn consume<T>(value: T) {}

fn explicit(file: std::fs::File) -> std::fs::File {
    return file;
}

fn implicit(file: std::fs::File) -> Box<std::fs::File> {
    Box::new(file)
}

fn run(first: std::fs::File, second: std::fs::File, third: std::fs::File) {
    consume(&first);
    consume(second);
    consume(std::boxed::Box::new(third));
    first.future_method();
    let aggregate = (first,);
    drop(aggregate);
}
",
    )?;
    let reasons: BTreeSet<UnknownSinkReason> = inventory
        .candidates()
        .iter()
        .map(norn_policy::writers::WriterCandidate::reason)
        .collect();
    for reason in [
        UnknownSinkReason::AuthorityArgument,
        UnknownSinkReason::AuthorityMethod,
        UnknownSinkReason::AuthorityStorage,
        UnknownSinkReason::AuthorityReturn,
        UnknownSinkReason::NewWrapperCandidate,
    ] {
        assert!(reasons.contains(&reason));
    }
    Ok(())
}

#[test]
fn aggregate_conditional_block_const_static_and_returned_callables_fail_closed() -> TestResult {
    let inventory = analyze(
        "crates/sample/src/callable_forms.rs",
        r"
struct Bundle<T> { writer: T }

const CONSTANT_WRITER: fn(&str, &[u8]) -> std::io::Result<()> = std::fs::write;
static STATIC_WRITER: fn(&str, &[u8]) -> std::io::Result<()> = std::fs::write;

fn explicit() -> fn(&str, &[u8]) -> std::io::Result<()> {
    return std::fs::write;
}

fn conditional(flag: bool) -> fn(&str, &[u8]) -> std::io::Result<()> {
    if flag { std::fs::write } else { std::fs::write }
}

fn run() {
    let tuple = (std::fs::write,);
    let record = Bundle { writer: std::fs::write };
    let block = { std::fs::write };
    drop((tuple, record, block, CONSTANT_WRITER, STATIC_WRITER));
}
",
    )?;
    let callable_escapes = inventory
        .candidates()
        .iter()
        .filter(|unknown| unknown.reason() == UnknownSinkReason::CallableEscape)
        .count();
    assert!(callable_escapes >= 8);
    assert!(inventory.candidates().iter().all(|unknown| {
        unknown.reason() != UnknownSinkReason::CallableEscape
            || unknown.candidate().as_str() == "std.fs.write"
    }));
    Ok(())
}

#[test]
fn unregistered_rustix_tempfile_and_macro_raw_ids_are_typed_unknowns() -> TestResult {
    let inventory = analyze(
        "crates/sample/src/ecosystems.rs",
        r#"
fn run() {
    rustix::fs::future_mutation("path");
    tempfile::future_mutation("path");
}

macro_rules! persist {
    () => { std::fs::r#write("path", b"data") };
}
"#,
    )?;
    assert_eq!(
        inventory
            .candidates()
            .iter()
            .filter(|unknown| unknown.reason() == UnknownSinkReason::KnownNamespaceCandidate)
            .count(),
        2
    );
    assert!(inventory.candidates().iter().any(|unknown| {
        unknown.reason() == UnknownSinkReason::MacroDefinitionCandidate
            && unknown.candidate().as_str() == "write"
    }));
    Ok(())
}
