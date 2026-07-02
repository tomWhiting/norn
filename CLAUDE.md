# CLAUDE.md

## Coding Standards

This codebase runs mission-critical infrastructure for financial, legal, and healthcare settings.

- **No lazy code.** Every implementation complete and robust. No partial implementations, no deferred work, no "good enough for now."
- **No silent failures.** Every error handled, logged, or propagated. No swallowed `Result`s, no empty catch blocks, no bare `continue` on `Err`.
- **No shortcuts.** Handle all edge cases. Validate at boundaries. Test failure paths, not just happy paths.
- **No god files.** Nothing over 500 lines of code (excluding tests, comments, whitespace). Break it into modules.
- **Modular structure enforced.** `mod.rs` contains only `pub mod` declarations and re-exports. Logic goes in named files. `lib.rs`/`main.rs` are thin entry points.
- **Production ready.** All code deployable immediately. Would you trust this with patient records?
- **NO ARBITRARY LIMITS / NO ARBITRARY DEFAULTS.** The sin is *invented* values, not defaults as such (owner clarification 2026-07-03). Never make up caps, rate limits, or "sensible defaults" for configurable values (scheduler threads, timeouts, retry policy, poll intervals) — a number you invented is arbitrary. Defaults are fine when they are **factual** (e.g. a model's context window from the generated model catalog) or **owner-ruled** (a value the owner explicitly chose, recorded in the decisions docs). Everything stays configurable; explicit config always wins over any default. When a value is needed and no factual/ruled source exists, discuss it with the owner — don't guess, and don't leave the feature silently disabled either.
- **NO BACKWARDS COMPATIBILITY** during this build — no compat shims, no zombie code, no `#[deprecated]` markers. Replace, don't add alongside.

## Linting

Strict clippy lints in the workspace `Cargo.toml`: `unsafe_code = "deny"`, `missing_docs = "warn"`, pedantic enabled, warnings on `unwrap_used`/`expect_used`/`panic`/`todo`.

```
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

Both must pass clean before any commit. If clippy fires, **fix the code**. `#[allow(...)]`, `#[expect(...)]`, `#[deny(...)]`, `#[ignore]` on tests, `_var` renames, and `#[cfg(any())]` dead-code hiding are all bypasses, not fixes. Tests that need a runtime gate it at runtime (read an env var, emit a `tracing::info!` skip line, return `Ok(())`) — never `#[ignore]`.

**Test-code exception (the only one):** `#[allow(...)]` is permitted on items that are themselves inside `#[cfg(test)]` (test modules, test helpers, trybuild fixtures) — e.g. `clippy::unwrap_used` in assertions. It is never permitted on production items, including production items that happen to be exercised only by tests.

## Error Handling

- `thiserror` for library errors (domain-specific types). `anyhow` only in the binary (`aion-server`) for top-level reporting.
- Never `.unwrap()` or `.expect()` in library code. Mutex/lock poison always handled explicitly and mapped to a typed error.

## Code Review

When work is ready, have it reviewed by a rigorous sub-agent on the **Fable** model — never a lighter model. Give the reviewer the brief, the original intent, and the relevant files, and let them explore beyond that. There is no such thing as a minor issue: everything is dealt with, nothing deferred, nothing skipped. **Standard:** would you trust this code with patient records, financial transactions, or legal documents? If not, it's not ready.

## The Brief Workflow

Implementation is driven by the per-cluster briefs under `docs/design/<cluster>/briefs/`. Each brief is a unit of work with numbered requirements (R1..Rn), EARS-style specs, concrete acceptance criteria, file paths, and checklist/story cross-references. Dispatch foundation-first: **AC → AP/AS → AE → AD/AT → AF/AN → AW/AR/AL/AU**. If you edit any brief JSON, re-render the markdown and re-run `check-coverage.py` (scripts under the meridian design-system) before landing. The brief files are authoritative; if a `design.json` `structure` annotation ever disagrees with a brief, trust the brief.
