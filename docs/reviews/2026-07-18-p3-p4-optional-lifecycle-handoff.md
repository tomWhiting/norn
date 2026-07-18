# P3/P4 optional-shape and lifecycle review handoff

**Date:** 2026-07-18
**Review source:** `624540d..56fd4dd`
**Candidate source:** `56fd4dd626af0c66954a51932fc05395f3023622`
**Candidate tree:** `6f0f6dce8d8a1a92c35d18ce085b00b8f1515e77`
**Requested verdict:** independent review of this evidence candidate; this is not
P3 or P4 acceptance.

## Scope and outcome

This range closes the finite optional-shape/lifecycle evidence item under owner
decision 17. It does not change production behavior. The Rust delta is confined
to test-only modules and `cfg(test)` wiring.

The candidate establishes two deliberately separate claims:

1. The official output-item contract contains 28 public variants and 274
   contextual optional or nullable property occurrences, producing 659 legal
   absent/null/present state assertions.
2. Behavioral lifecycle evidence uses seven named equivalence classes across
   ten named surfaces. It does not execute a `659 x surfaces` Cartesian product.

The lifecycle matrix has 70 explicit cells: 45 covered cells and 25 reasoned
`not_applicable` cells. Every covered cell names source tests; every excluded
cell states why the class cannot validly traverse that surface.

## Candidate contents

- The content-hashed official contract artifact is
  `docs/reviews/evidence/p3-p4/2026-07-18-response-optional-shape-contract.json`.
  Its SHA-256 is
  `7ea54c502732510a8c7e55a2acbfd481b92ed267ded34ce747af79216883dea3`.
- The referenced official schema section has 2,103,673 characters,
  2,103,677 UTF-8 bytes, and SHA-256
  `d414cb294fadb4b56185f6507fe57a092dfb10e888776b24aa866be89d3182ea`.
- The machine-readable lifecycle artifact is
  `docs/reviews/evidence/p3-p4/2026-07-18-response-lifecycle-surface-inventory.json`.
  Its SHA-256 is
  `561f9cc099d31a9d6c0e7ac40f28cd477db8058e7d71a7b400ccabe7f3ad11f0`.
- The source-bound runner is
  `docs/reviews/evidence/p3-p4/run_response_optional_lifecycle_gate.sh`.
  Its source SHA-256 is
  `af001f32af59514aa857c56970f42effc5e16c11159bbe647442fd83d8cb42a2`.
- The retained result is
  `docs/reviews/evidence/p3-p4/2026-07-18-response-optional-lifecycle-gate-56fd4dd.json`.
  Its SHA-256 is
  `848e44f34ed075065a7a5370c811dad68daa15139f72e0dd534edd826f3936f0`.

The successful corpora contain 48 live-safe items, 52 historical public items,
and 53 historical items after adding one deliberately opaque future item. The
real ForkTool inherited corpus is also 53 items. Unsupported executable forms
remain failure-only; they are not inserted into a successful corpus to inflate
coverage.

## Lifecycle surfaces

The ten surfaces are authoritative schema, live reconciliation, strict
persistence/reload, `store:false` replay, persistent spawn, library context
filter, persistent in-root ForkTool, ownership-changing top-level fork,
response-audio sidecar, and failure boundary.

The seven classes are populated public success, minimal public success, accepted
nested-union success, opaque historical data, wire failure, common stream-event
envelope, and response-audio sidecar.

The inventory resolves 63 surface references to 44 unique source tests. The
strict opaque tests use `FsyncPerEvent` and compare the complete 53-entry
`ResponseTranscriptItem` vector, including stream provenance, after reload and
ownership-changing fork. `store:false` replay then proves exact provider JSON
while omitting only the non-provider envelope coordinates.

The persistent ForkTool test proves the full inherited public-plus-opaque vector
is contiguous, proves the seeded opaque item occurs exactly once, and separately
proves the exact public-item projection followed only by the fork's structured
result.

Unknown live item/event evidence is causal but intentionally split at the
component boundary: mapper tests prove raw retention and typed classification;
loop tests prove the typed failure cannot publish an ordinary assistant or tool
turn. Neither loop fixture alone is described as mapper evidence.

## Retained gate

The source-bound gate passed with zero failures:

- Rust 1.94 formatting and `git diff --check`: pass.
- Strict workspace, all-target, all-feature Clippy with `-D warnings`: pass.
- `norn --lib --all-features`: 4,011 passed, zero failed.
- Official contract and lifecycle-inventory structural verification: pass.
- Added-line policy: zero new allow/expect/ignore suppressions, unwraps, expects,
  panics, TODOs, or unimplemented markers.
- Focused lifecycle observations: 120 passed, zero failed across 44 tests.
- Strict opaque reload: 20/20.
- Ownership-changing manager fork: 20/20.
- Persistent spawn: 20/20.
- Real persistent ForkTool: 20/20.

The gate used `/Users/tom/Developer/ablative/norn/target`. It rejected temporary
or worktree-local build directories and recorded `os_temp_used: false`.

All changed production prefixes are below 500 lines except the pre-existing
668-line prefix in `fork_tool.rs`. That exact prefix is 668 lines at both the
review-range base and source commit and has the same SHA-256; this range changes only
its existing test module. The gate fails if any over-limit production prefix is
new or differs from the review-range base.

## Corrections made before freezing

The internal adversarial pass caught and corrected the following evidence
defects before source freeze:

- An invalid `"completed"` witness for the optional reasoning-summary-part
  status was replaced by the only legal present value, `"incomplete"`.
- Unknown-output loop evidence was paired with a direct real-mapper causal test.
- Permissive per-type counts were replaced by exact 28-entry count arrays.
- Strict opaque tests were moved from `Flush` to `FsyncPerEvent` and now compare
  exact provenance as well as raw JSON.
- ForkTool replay now rejects an extra duplicated opaque item.
- The twelve schema-valid but unsupported nested computer/patch variants and the
  required-null shell carrier are attached to live and failure-boundary anchors.
- The runner now resolves linked worktrees through Git's common directory and
  builds only in the primary repository target.

## Honest boundaries

- The 659 values are official schema state assertions, not 659 behavioral test
  executions and not 659 executions through every lifecycle surface.
- The official schema section bytes and extraction generator are not checked in.
  Regeneration requires refetching the official schema and first matching the
  recorded section hash. The retained gate verifies the artifact hash and its
  internal invariants offline; it does not claim offline regeneration.
- The common stream-event envelope is lossless live input, not a transcript
  record. Only reconciled items or response-audio sidecars persist.
- Required-null execution in the minimal corpus covers the directly applicable
  paths. Required-null occurrences beneath omitted optional or empty nested
  containers are covered by populated/nested fixtures; no claim says all 14
  contextual occurrences execute in the minimal fixture.
- This range does not resolve the pending same-reviewer M-1/F-2 correction
  confirmation, P1/P2 acceptance, the retrospective P3/P4 phase-base decision,
  the final full-range phase gates, or separate P3 then P4 acceptance reviews.

## Review request

Please attack the finite-inventory arithmetic, the seven-by-ten applicability
matrix, exact anchor resolution, unsupported-versus-success corpus boundary,
opaque provenance and duplicate behavior, event-envelope claim boundary,
source/runner binding, and the unchanged-over-limit production-prefix rule.
Return a verdict for this candidate only; do not infer whole-phase acceptance.
