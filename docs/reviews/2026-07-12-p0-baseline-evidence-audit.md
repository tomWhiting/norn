# P0 baseline-evidence audit

**Date:** 2026-07-12
**Phase base:** `41ea210`
**Status:** historical audit complete; owner disposition required
**Method:** inspect every commit from `41ea210..78d982b`, its parent, and the
introduction history of the decisive P0 tests; no worktree was mutated

## Question

Gate B requires each confirmed-defect regression to fail for the documented
reason on the reviewed baseline. The original handoff had candidate passes and
source analysis but no durable per-finding red-state matrix. This audit asks
whether Git history can now supply that missing chronology without inventing
evidence.

## Finding

With one exception, it cannot. The original and corrective P0 commits introduce
production fixes and their decisive tests in the same commit:

- `cd91c39`: workspace/config/command authority fixes and tests.
- `4fbc716`: credential redaction, redirect/backend fixes and tests.
- `0f110d5`: initial private persistence fixes and tests.
- `864b473`: descriptor-relative private-artifact confinement and tests.
- `30e5126`: configuration/provider API sealing and tests.
- `9406df8`: terminal diagnostic fixes and tests.
- `735db41`: private fetched-artifact implementation and tests.
- `9f35cfd`: fetched-content verbatim preservation and its regression.
- `1f947e5`: descriptor-capacity correction and tests.
- `2c6b50c`: private TUI history and tests.

Their parents do not contain the decisive tests. Applying a current test patch
to an old commit could be useful retrospective reconstruction, but it would not
make that patch a historical baseline test. Reverse-applying production hunks
at the current head would be counterfactual mutation evidence. Neither can be
recorded as the chronology Gate B requires.

Test-only commits do not fill the gap. `34ca22e` pins already-safe 401/429 and
loop delivery behavior. `37c806a` adds external compile-fail checks for an
authority restriction already in production. Their parents are green for the
property being pinned, not defect-red baselines.

## Native red-green evidence

The macOS descriptor-relative create correction is the one mechanically native
historical red-green case:

| State | Commit/evidence |
|---|---|
| Pre-P0/base behavior | The convergence test predates P0 and is green at `41ea210`, whose path-based open does not exercise the Darwin `openat(O_CREAT)` defect. |
| Defect-red corrective baseline | At `cdaae83`, after descriptor-relative migration and before the retry, Gate D reproduced `open_or_resume_concurrent_same_id_converges_on_one_session` failing 6/10 isolated runs; subsequent retained review evidence reproduced 19/20 failures. |
| Corrected green | `c25e841` plus the checked-in runner records 50/50 convergence, primitive same-name create, exclusive-create, and persistent-failure distributions. Independent review added 15/15 and higher-contention confirmation. |

When disk capacity permits, the historical distribution can be rerun in a
detached `cdaae83` worktree. The evidence command must first list the exact test
so zero matched tests cannot masquerade as success, then run at least 50 fresh
processes and retain every exit:

```sh
cargo test -p norn \
  session::manager::tests::open_or_resume_concurrent_same_id_converges_on_one_session \
  -- --exact --list

# Repeat the same exact test in 50 fresh cargo-test processes and retain the
# complete pass/fail distribution. Do not record only the final invocation.
```

## Stronger non-red baseline proof

- At `41ea210`,
  `provider/openai/provider.rs::catalog_backend_tracks_actual_connection`
  positively characterizes the defective `BACKEND-01` behavior: an explicitly
  spelled canonical OAuth URL is treated as non-ChatGPT. This is executable
  baseline characterization, not a failing regression.
- The source review retains exact data-flow/source proof for the other original
  findings: repository-controlled authority reaches endpoint/environment/
  process fields, credential structures expose raw `Debug`, and artifact
  writers use ambient path/mode/link semantics.
- The final P0 traceability record maps those baseline source proofs to exact
  candidate regressions without relabeling the proof as a historical test run.

## Optional retrospective reconstruction

A small number of tests can be transplanted honestly if both the test-only
patch and its hash are retained and the result is labelled **retrospective**:

- Apply only `fetched_content_preserves_leading_frontmatter_verbatim` to
  `735db41`; the historical `strip_frontmatter` implementation should fail it.
- Apply only the SEC-05 public-surface compile checks to `41ea210`, including a
  `test-utils` build; the then-feature-shippable constructors should violate the
  boundary.

Many current tests depend on types, validation seams, or private filesystem
primitives introduced by their production fix. Backporting those dependencies
would cease to be a test-only reconstruction. Reconstruction can strengthen a
few rows but cannot satisfy universal historical Gate B.

## Required disposition

Two P0-only process exceptions are therefore required for an honest phase
verdict:

1. The already documented Gate A exception for decisions/evidence agreement
   that were not recorded before implementation.
2. A Gate B exception accepting retained source/characterization proof plus
   candidate regressions where the repository lacks native pre-fix executable
   states. The native openat red-green evidence remains required and is not
   waived.

These exceptions acknowledge missing historical process evidence; they waive
no implementation, test, lint, LOC, security-review, or final Gate C result.
P1 must enforce prospective baseline capture so later phases cannot use this
exception as precedent.

## Reproduction prerequisites

If the owner requests the optional worktree reconstruction, first provide at
least 15 GiB of free build capacity. Each detached worktree must record its
commit, clean status, toolchain, isolated `CARGO_TARGET_DIR`, exact test list,
test-only patch hash when applicable, every invocation result, and distribution.
The current host has insufficient capacity, so no partial build from this audit
is presented as evidence.
