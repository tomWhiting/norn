//! Compile-fail suite: pins the derive's spanned diagnostics — missing field
//! docs, flatten misuse, unsupported types, tuple variants, generics,
//! external tagging, non-String map keys, unknown `#[tool_args]` keys,
//! unknown `rename_all` rules, and ambiguous untagged unit variants — to the
//! exact rustc output recorded in the `tests/ui/*.stderr` fixtures.

#[test]
fn ui() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui/*.rs");
}
