# P0 G-1 integration-fence correction

**Source review:** `2026-07-13-p0-status-report.md` at `51b83ea`  
**Scope:** status-report finding G-1 only  
**Whole-phase status:** P0 remains open

## Correction

D1D moved empty `--extension` URI validation from `builder_from_cli` to
`resolve_invocation`, the shared production boundary used by both print and TUI
drivers. The integration test still called the old layer and therefore failed
deterministically even though both production paths retained the hard error.

`empty_extension_uri_is_argument_error` now drives `resolve_invocation`
directly. Its positive counterpart first resolves the invocation and then
passes the resulting profile, settings, and applied configuration through
`builder_from_cli` and `AgentBuilder::build`. This pins both ownership of the
validation and continuation through assembly without duplicating production
validation in the builder.

## Verification

- `cargo test -p norn-cli --test assembly_flag_wiring`: 18 passed, zero failed.
- `cargo test -p norn-cli --tests`: 501 passed across the library and eight
  integration binaries, zero failed and zero ignored.
- `cargo fmt --all --check`: pass.
- `cargo clippy -p norn-cli --all-targets -- -D warnings`: pass.
- `git diff --check`: pass.
- Exact-range policy report for `51b83ea..880891f`: one changed Rust file,
  entirely test-only; zero added unwrap, expect, panic, suppression,
  ignored-test, unresolved-marker, `todo!`, or `unimplemented!` matches; zero
  production over-500 or thin-entrypoint violations. Raw evidence is retained
  in `evidence/2026-07-14-p0-g1-policy.json`.

The production diff is empty. This correction adds no production LOC and makes
no workspace-wide or whole-phase gate claim.

## Process rule

Every future round must run `cargo test -p <crate> --tests` for each touched
crate. Focused and `--lib` suites remain useful diagnostics but cannot replace
that integration fence. Concurrency-sensitive evidence retains its separate
loop and distribution requirements.

## Remaining P0 gates

- Independent acceptance of the D1E weighted-admission candidate, including
  permit lifetimes and the owner decision on transient headroom and excluded
  one-shot filesystem operations.
- Independent acceptance of the D1D MCP startup candidate.
- Owner dispositions for the P0-only Gate A retrospective exception and Gate B
  baseline-evidence exception.
- Final Gate C evidence at the completed candidate, followed by a fresh
  whole-phase Gate D review.

The MCP product ruling in `DECISIONS-2026-07.md` section 10 is now explicitly
attributed to Tom. Live MCP mutation remains a separately tracked, unimplemented
slice.
