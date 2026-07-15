use std::collections::BTreeSet;

use norn_policy::writers::{SinkDiscovery, UnknownSinkReason, WriterCandidateForm};

use super::support::{TestResult, analyze};

#[test]
fn unrelated_qualified_and_local_terminal_names_are_not_candidates() -> TestResult {
    let inventory = analyze(
        "crates/sample/src/benign_terminals.rs",
        r#"
struct Fixture;

impl Fixture {
    fn new() -> Self { Self }

    fn rebuild() -> Self {
        Self::new()
    }
}

fn write() {}

fn run() {
    let _ = Vec::new();
    let _ = String::new();
    let _ = Fixture::rebuild();
    write();
}
"#,
    )?;
    assert!(
        inventory.candidates().is_empty(),
        "benign terminal-name collisions became writer candidates: {:?}",
        inventory.candidates()
    );
    Ok(())
}

#[test]
fn future_standard_filesystem_apis_remain_typed_unknowns() -> TestResult {
    let inventory = analyze(
        "crates/sample/src/future_std_fs.rs",
        r#"
fn run() {
    std::fs::future_global_mutation("alpha");
    std::fs::File::future_associated_mutation("beta");
}
"#,
    )?;
    for candidate in ["future_global_mutation", "future_associated_mutation"] {
        assert!(inventory.candidates().iter().any(|unknown| {
            unknown.reason() == UnknownSinkReason::KnownNamespaceCandidate
                && unknown.candidate().as_str() == candidate
        }));
    }
    Ok(())
}

#[test]
fn filesystem_support_types_are_not_writer_candidates() -> TestResult {
    let inventory = analyze(
        "crates/sample/src/support_types.rs",
        r#"
use std::os::unix::fs::PermissionsExt;

fn run() {
    let _ = std::fs::Permissions::from_mode(0o600);
    let flags = rustix::fs::OFlags::empty()
        | rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::CLOEXEC;
    let _ = flags.contains(rustix::fs::OFlags::NOFOLLOW);
}
"#,
    )?;
    assert!(
        inventory.candidates().is_empty(),
        "filesystem support APIs became writer candidates: {:?}",
        inventory.candidates()
    );
    Ok(())
}

#[test]
fn valid_source_identifiers_have_no_arbitrary_candidate_length_limit() -> TestResult {
    let name = format!("writer_{}", "a".repeat(256));
    let source = format!("fn run(file: std::fs::File) {{ file.{name}(); }}\n");
    let inventory = analyze("crates/sample/src/long_identifier.rs", &source)?;
    assert!(inventory.candidates().iter().any(|unknown| {
        unknown.reason() == UnknownSinkReason::AuthorityMethod
            && unknown.candidate().as_str() == name
    }));
    Ok(())
}

#[test]
fn bare_unresolved_registered_terminal_remains_typed() -> TestResult {
    let inventory = analyze(
        "crates/sample/src/bare_unresolved.rs",
        "fn run() { write(\"alpha\", b\"x\"); }",
    )?;
    assert!(inventory.candidates().iter().any(|unknown| {
        unknown.reason() == UnknownSinkReason::UnresolvedAlias
            && unknown.candidate().as_str() == "write"
    }));
    Ok(())
}

#[test]
fn unresolved_dynamic_generic_macro_and_wrapper_candidates_are_typed() -> TestResult {
    let inventory = analyze(
        "crates/sample/src/unknown.rs",
        r#"
use std::fs::*;

fn wildcard() {
    write("alpha", b"x");
}

fn generic<W: std::io::Write>(writer: &mut W) {
    writer.write_all(b"x");
}

fn dynamic(thing: Unknown) {
    thing.flush();
}

fn macro_body() {
    quote! { file.write_all(bytes) };
}

fn custom() -> std::fs::File {
    std::fs::File::create("alpha")
}
"#,
    )?;
    let reasons: BTreeSet<UnknownSinkReason> = inventory
        .candidates()
        .iter()
        .map(norn_policy::writers::WriterCandidate::reason)
        .collect();
    assert!(reasons.contains(&UnknownSinkReason::WildcardImport));
    assert!(reasons.contains(&UnknownSinkReason::GenericReceiver));
    assert!(reasons.contains(&UnknownSinkReason::DynamicReceiver));
    assert!(reasons.contains(&UnknownSinkReason::MacroTokenCandidate));
    assert!(reasons.contains(&UnknownSinkReason::NewWrapperCandidate));
    for (reason, form) in [
        (
            UnknownSinkReason::WildcardImport,
            WriterCandidateForm::FunctionCall,
        ),
        (
            UnknownSinkReason::GenericReceiver,
            WriterCandidateForm::MethodCall,
        ),
        (
            UnknownSinkReason::DynamicReceiver,
            WriterCandidateForm::MethodCall,
        ),
        (
            UnknownSinkReason::MacroTokenCandidate,
            WriterCandidateForm::MacroToken,
        ),
        (
            UnknownSinkReason::NewWrapperCandidate,
            WriterCandidateForm::WrapperDefinition,
        ),
    ] {
        assert!(
            inventory
                .candidates()
                .iter()
                .any(|candidate| candidate.reason() == reason && candidate.form() == form)
        );
    }
    Ok(())
}

#[test]
fn registered_write_macro_is_an_operation() -> TestResult {
    let inventory = analyze(
        "crates/sample/src/macro.rs",
        r#"
fn render(output: &mut std::fs::File) {
    write!(output, "fixed");
}
"#,
    )?;
    assert!(inventory.operations().iter().any(|operation| {
        operation.sink().as_str() == "std.macro.write"
            && operation.discovery() == SinkDiscovery::MacroInvocation
    }));
    assert!(inventory.candidates().is_empty());
    Ok(())
}

#[test]
fn unregistered_macros_cannot_capture_tracked_authority_or_callables() -> TestResult {
    let inventory = analyze(
        "crates/sample/src/macro_escape.rs",
        r#"
use std::fs::write as persist;

fn run(file: std::fs::File) {
    let writer = std::fs::write;
    quote! {
        file.write_all(b"data");
        writer("first", b"data");
        persist("second", b"data");
    };
}
"#,
    )?;
    assert!(inventory.candidates().iter().any(|unknown| {
        unknown.reason() == UnknownSinkReason::AuthorityArgument
            && unknown.candidate().as_str() == "quote"
            && unknown.form() == WriterCandidateForm::AuthorityEscape
    }));
    assert_eq!(
        inventory
            .candidates()
            .iter()
            .filter(|unknown| {
                unknown.reason() == UnknownSinkReason::CallableEscape
                    && unknown.candidate().as_str() == "std.fs.write"
                    && unknown.form() == WriterCandidateForm::CallableEscape
            })
            .count(),
        2
    );
    Ok(())
}

#[test]
fn unregistered_macro_definitions_cannot_hide_imported_writer_aliases() -> TestResult {
    let inventory = analyze(
        "crates/sample/src/macro_definition_escape.rs",
        r#"
use std::fs::write as persist;

macro_rules! hidden_writer {
    () => { persist("artifact", b"data") };
}
"#,
    )?;
    assert!(inventory.candidates().iter().any(|unknown| {
        unknown.reason() == UnknownSinkReason::CallableEscape
            && unknown.candidate().as_str() == "std.fs.write"
            && unknown.form() == WriterCandidateForm::CallableEscape
    }));
    Ok(())
}
