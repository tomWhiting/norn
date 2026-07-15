use norn_policy::writers::{UnknownSinkReason, analyze_writers, builtin_sink_registry};

use super::support::{TestResult, analyze, source};

#[test]
fn sibling_and_block_imports_remain_lexically_isolated() -> TestResult {
    let siblings = analyze(
        "crates/sample/src/scopes.rs",
        r#"
mod first {
    use std::fs::write as persist;
    fn run() { persist("first", b"one"); }
}

mod second {
    use std::fs::rename as persist;
    fn run() { persist("second", "third"); }
}
"#,
    )?;
    for sink in ["std.fs.write", "std.fs.rename"] {
        assert!(
            siblings
                .operations()
                .iter()
                .any(|operation| operation.sink().as_str() == sink)
        );
    }
    assert!(siblings.candidates().is_empty());

    let block = analyze(
        "crates/sample/src/block.rs",
        r#"
fn run() {
    {
        use std::fs::write as persist;
        persist("inside", b"one");
    }
    persist("outside", b"two");
}
"#,
    )?;
    assert_eq!(
        block
            .operations()
            .iter()
            .filter(|operation| operation.sink().as_str() == "std.fs.write")
            .count(),
        1
    );
    Ok(())
}

#[test]
fn inner_bindings_do_not_replace_outer_authority() -> TestResult {
    let inventory = analyze(
        "crates/sample/src/shadow.rs",
        r"
fn run(file: std::fs::File) {
    {
        let file = 1;
        drop(file);
    }
    file.sync_all();
}
",
    )?;
    assert!(
        inventory
            .operations()
            .iter()
            .any(|operation| operation.sink().as_str() == "io.handle.sync_all")
    );
    assert!(inventory.candidates().is_empty());
    Ok(())
}

#[test]
fn closure_and_callable_parameter_shadows_do_not_inherit_outer_provenance() -> TestResult {
    let inventory = analyze(
        "crates/sample/src/parameter_shadow.rs",
        r"
struct MyFile;

fn finish(file: &mut std::fs::File) { file.sync_all(); }

fn run(finish: fn(std::fs::File), file: std::fs::File) {
    let inspect = |file: MyFile| file.sync_all();
    inspect(MyFile);
    finish(file);
}
",
    )?;
    assert!(inventory.candidates().iter().any(|unknown| {
        unknown.reason() == UnknownSinkReason::DynamicReceiver
            && unknown.candidate().as_str() == "sync_all"
    }));
    assert!(inventory.candidates().iter().any(|unknown| {
        unknown.reason() == UnknownSinkReason::AuthorityArgument
            && unknown.candidate().as_str() == "finish"
    }));
    assert_eq!(
        inventory
            .operations()
            .iter()
            .filter(|operation| operation.sink().as_str() == "io.handle.sync_all")
            .count(),
        1
    );
    Ok(())
}

#[test]
fn conditional_callable_assignment_joins_conservatively() -> TestResult {
    let inventory = analyze(
        "crates/sample/src/join.rs",
        r#"
fn benign(_: &str, _: &[u8]) {}

fn run(condition: bool) {
    let mut persist = std::fs::write;
    if condition {
        persist = benign;
    }
    persist("target", b"data");
}
"#,
    )?;
    assert!(inventory.candidates().iter().any(|unknown| {
        unknown.reason() == UnknownSinkReason::AmbiguousAlias
            && unknown.candidate().as_str() == "persist"
    }));
    Ok(())
}

#[test]
fn conditional_authority_assignment_joins_conservatively() -> TestResult {
    let inventory = analyze(
        "crates/sample/src/flow_join.rs",
        r"
fn replacement() -> std::fs::File { loop {} }

fn run(condition: bool, mut file: std::fs::File) {
    if condition {
        file = replacement();
    }
    file.sync_all();
}
",
    )?;
    assert!(inventory.candidates().iter().any(|unknown| {
        unknown.reason() == UnknownSinkReason::AuthorityMethod
            && unknown.candidate().as_str() == "sync_all"
    }));
    assert!(
        inventory
            .operations()
            .iter()
            .all(|operation| operation.sink().as_str() != "io.handle.sync_all")
    );
    Ok(())
}

#[test]
fn exact_cross_file_function_proof_contains_authority() -> TestResult {
    let implementation = source(
        "crates/sample/src/finish.rs",
        r"
pub fn finish(file: &mut std::fs::File) {
    file.set_len(4);
    file.sync_all();
}
",
    )?;
    let caller = source(
        "crates/sample/src/lib.rs",
        r#"
use crate::finish::finish;

fn run() {
    let mut file = std::fs::File::create("target");
    finish(&mut file);
}
"#,
    )?;
    let inventory = analyze_writers(&[implementation, caller], &builtin_sink_registry()?)?;
    assert!(inventory.candidates().iter().all(|unknown| {
        unknown.reason() != UnknownSinkReason::AuthorityArgument
            || unknown.candidate().as_str() != "finish"
    }));
    for sink in ["io.handle.set_len", "io.handle.sync_all"] {
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
fn escaping_and_generic_local_functions_remain_fail_closed() -> TestResult {
    let inventory = analyze(
        "crates/sample/src/unproved.rs",
        r#"
fn consume<T>(_: T) {}

fn escape(file: std::fs::File) {
    consume(file);
}

fn generic<T>(file: &mut std::fs::File, _: T) {
    file.sync_all();
}

fn run() {
    let first = std::fs::File::create("first");
    escape(first);
    let mut second = std::fs::File::create("second");
    generic(&mut second, ());
}
"#,
    )?;
    for candidate in ["escape", "generic"] {
        let observed = inventory
            .candidates()
            .iter()
            .map(|unknown| (unknown.reason(), unknown.candidate().as_str()))
            .collect::<Vec<_>>();
        assert!(
            inventory.candidates().iter().any(|unknown| {
                unknown.reason() == UnknownSinkReason::AuthorityArgument
                    && unknown.candidate().as_str() == candidate
            }),
            "missing fail-closed authority argument for {candidate}: {observed:?}"
        );
    }
    Ok(())
}

#[test]
fn sibling_same_name_functions_resolve_by_exact_module() -> TestResult {
    let inventory = analyze(
        "crates/sample/src/modules.rs",
        r#"
mod first {
    fn finish(file: &mut std::fs::File) { file.sync_all(); }
    fn run() {
        let mut file = std::fs::File::create("first");
        finish(&mut file);
    }
}

mod second {
    fn finish(file: &mut std::fs::File) { file.set_len(4); }
    fn run() {
        let mut file = std::fs::File::create("second");
        finish(&mut file);
    }
}
"#,
    )?;
    assert!(inventory.candidates().iter().all(|unknown| {
        unknown.reason() != UnknownSinkReason::AuthorityArgument
            || unknown.candidate().as_str() != "finish"
    }));
    Ok(())
}

#[test]
fn same_function_paths_in_other_crates_do_not_create_ambiguity() -> TestResult {
    let first = source(
        "crates/first/src/lib.rs",
        r#"
fn finish(file: &mut std::fs::File) { file.sync_all(); }
fn run() {
    let mut file = std::fs::File::create("first");
    finish(&mut file);
}
"#,
    )?;
    let second = source(
        "crates/second/src/lib.rs",
        r#"
fn finish(file: &mut std::fs::File) { file.set_len(4); }
fn run() {
    let mut file = std::fs::File::create("second");
    finish(&mut file);
}
"#,
    )?;
    let inventory = analyze_writers(&[first, second], &builtin_sink_registry()?)?;
    assert!(inventory.candidates().iter().all(|unknown| {
        unknown.reason() != UnknownSinkReason::AuthorityArgument
            || unknown.candidate().as_str() != "finish"
    }));
    Ok(())
}

#[test]
fn type_names_that_only_contain_file_do_not_gain_authority() -> TestResult {
    let inventory = analyze(
        "crates/sample/src/decoy.rs",
        r"
struct MyFile;

fn run(file: MyFile) {
    file.sync_all();
}
",
    )?;
    assert!(inventory.candidates().iter().any(|unknown| {
        unknown.reason() == UnknownSinkReason::DynamicReceiver
            && unknown.candidate().as_str() == "sync_all"
    }));
    assert!(
        inventory
            .operations()
            .iter()
            .all(|operation| operation.sink().as_str() != "io.handle.sync_all")
    );
    Ok(())
}

#[test]
fn writer_analysis_uses_heap_traversal_at_twenty_thousand_levels() -> TestResult {
    const DEPTH: usize = 20_000;

    let mut authority = "(".repeat(DEPTH);
    authority.push_str("file");
    authority.extend(std::iter::repeat_n(')', DEPTH));
    let mut callable = "(".repeat(DEPTH);
    callable.push_str("std::fs::write");
    callable.extend(std::iter::repeat_n(')', DEPTH));
    let text =
        format!("fn run(file: std::fs::File) {{ consume({authority}); consume({callable}); }}");
    let inventory = analyze("crates/sample/src/deep.rs", &text)?;
    assert!(inventory.candidates().iter().any(|unknown| {
        unknown.reason() == UnknownSinkReason::AuthorityArgument
            && unknown.candidate().as_str() == "consume"
    }));
    assert!(inventory.candidates().iter().any(|unknown| {
        unknown.reason() == UnknownSinkReason::CallableEscape
            && unknown.candidate().as_str() == "std.fs.write"
    }));
    Ok(())
}
