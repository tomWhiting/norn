# Feature ask for chiron `diagnostics`: cfg(test)-span subtraction

**From:** Sable Nightwick (norn reviewer seat)
**Date:** 2026-08-09
**Context:** norn `CONVENTIONS.toml` split, landed `ff30f70`. Full rationale
and measurements: `docs/reviews/2026-08-08-conventions-wip-review.md`.
**Consumer pin:** norn pins `diagnostics` at chiron rev `25161bc` (workspace
`Cargo.toml`); adopting this feature means a deliberate rev bump there.

## The ask, in one sentence

An opt-in, per-pattern way to **discard matches whose span lies inside
`#[cfg(test)]`-gated code**, so a rule can block on production occurrences
of a pattern that is simultaneously sanctioned in test code.

## Why norn needs it (measured, not hypothetical)

norn's house law bans `unwrap`/`expect`/`panic!`/`#[allow]` in production
code but **explicitly permits them inside `#[cfg(test)]` items** (the
test-code exception in `CLAUDE.md`). The conventions pattern engine scans
the **whole file** on every mutation, and norn's tests overwhelmingly live
*inside* source files as `mod tests`. Measured on norn's tree: 175 files
carry `unwrap`/`expect` and 179 carry `#[allow(` — essentially all of it
sanctioned test code, production being clean by construction (the workspace
lints already deny it at the gate).

Consequence: six of norn's nine convention rules run at `advise` instead of
`block`, because blocking them would reject edits to most test-bearing
files. The rules that should carry the hardest feedback are exactly the
ones that can't.

Configuration cannot express the carve-out:

- **Path scoping fails** — the tests are inline `mod tests`, not `tests/`
  directories, so no glob separates them.
- **The tree-sitter query language fails** — it cannot negate on an
  ancestor ("match X only when no enclosing item carries `#[cfg(test)]`").

So this is engine work, by elimination.

## Sketch (yours to redesign)

1. Per language, a second query identifying **test-gated regions**: any
   item whose attributes contain `cfg(test)` — `mod_item`, `fn_item`,
   `struct_item`, etc. with `#[cfg(test)]`, plus a file-level
   `#![cfg(test)]` inner attribute marking the whole file.
2. At execution, **subtract**: drop any pattern match whose byte range is
   contained in a test-gated region. Byte-range subtraction works for both
   the AST matcher and the regex matcher (regex matches carry offsets too —
   `TODO`-marker rules inside test fixture strings are half of norn's
   false-positive surface).
3. Config surface: a per-pattern flag (spelling yours —
   `scope = "production"` / `exclude_test_spans = true`). **Opt-in**, so
   existing configurations keep their semantics unchanged.

## Honest edges worth ruling on early

- `#[cfg_attr(test, ...)]` and custom cfg predicates (`cfg(any(test,
  fuzzing))`): v1 could reasonably handle bare `cfg(test)` only, as long as
  the limitation is stated — a partial carve-out that reads as complete
  would recreate the defect class this whole exercise removes.
- Doc comments and string literals mentioning banned tokens are a separate
  false-positive class (norn has both) — out of scope here; the AST matcher
  already avoids them, only regex patterns are exposed.

## What norn does the day this ships

Flip `allow-attr`, `expect-attr`, `fallible-shortcuts`, `panic-macros`,
`silent-var-rename`, and `todo-markers` from `advise` to `block` with the
new flag set, restoring the full hard-feedback design of the original
`codex/conventions-wip` prototype — whose test scaffolding (routing
assertions, red-proofs) is already in place to pin the change.
