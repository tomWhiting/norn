# Stable toolchain reconciliation — 2026-07-14

**Status:** Ready for independent review. This record covers the compatibility
cleanup after merging `origin/main`; it does not grant Gate D acceptance.

**Range:** `271ab46..d391f0c`

## Reason

The remote stable-toolchain change landed while the live MCP slice was being
packaged. The merge itself changed only `Cargo.lock` and
`rust-toolchain.toml`, but strict Clippy exposed test-module lint names that
are unavailable in older stable Clippy and five ordinary stable diagnostics.

No suppression was added. The cleanup deletes the two obsolete names from 65
existing test-module allowance lists, removes one duplicated test-only cfg,
uses `clone_into` for three existing string assignments, and marks
`PowerShell` as a code identifier in documentation.

## Verification

- `cargo +1.94.0 clippy -p norn -p norn-cli -p norn-tui --all-targets --
  -D warnings`: pass.
- `cargo +1.94.0 test -p norn -p norn-cli -p norn-tui --all-targets
  --quiet`: pass. The major targets include 3,299 Norn unit tests, 457 CLI
  tests, 678 TUI tests, and 17 PTY tests.
- `cargo fmt --all -- --check`: pass.
- `2026-07-14-stable-clippy-policy.json` covers all 66 changed Rust files:
  zero production files over 500 lines, zero thin-entrypoint violations, and
  zero added matches for unwrap, expect, panic, todo, unimplemented, lint
  allowances, ignored tests, empty cfg bypasses, lint CLI suppressions, or
  debt markers.

## Toolchain note

This machine's `stable` alias currently resolves to Clippy 1.92.0. That
version reports `large_stack_arrays` against the compiler-generated Norn
lib-test harness at the synthetic span `crates/norn/src/lib.rs:1:1` after the
harness aggregates roughly 3,300 tests. There is no source span or repository
array to fix. Stable 1.94.0 does not produce the false positive and passes the
same strict all-target command. No threshold change, crate-level allowance, or
command-line lint suppression was introduced.
