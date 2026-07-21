# P5 D3 H1 residual correction handoff

**Date:** 2026-07-21

**Candidate branch:** `codex/p5-d3-correction`

**Controlling confirmation:**
[`2026-07-21-p5-d3-gate-d-correction-confirmation.md`](2026-07-21-p5-d3-gate-d-correction-confirmation.md)
at `0dc2035e177773583c4d1298761d67c52a647567`

**Requested scope:** narrow same-reviewer confirmation of H1-a, H1-b, and
H1-c only

**Acceptance boundary:** D3 was already returned `READY` as an implementation
candidate at `0dc2035`. This handoff does not request a repeat D3 panel, owner
acceptance, merge approval, or whole-P5 acceptance.

## Exact correction target

The controlling confirmation closed R1/R2 and elected H2/H3/H4, returned D3
`READY`, and retained three nonblocking H1 residuals. This correction addresses
only those residuals.

- Review base: `0dc2035e177773583c4d1298761d67c52a647567`.
- Product correction: `e96ee64d906eeb5365702ac223026d8db0d50e10`.
- Exact runner/source commit:
  `c8619aec1ab065ddd14bd5cd2cd574bc95e087c3`.
- Exact source tree: `e7f676a1a6389f307d6892ce3ae763de5436682e`.
- Retained-evidence commit:
  `a79af89ae5209d47d871880fea5d900720eb6f6d`.

The NUL-delimited `0dc2035..c8619ae` inventory contains 13 paths: the 12
product/test paths in `e96ee64` plus the source-bound evidence runner. Its
SHA-256 is
`200cfec964ffe9d49d964bb7d925540e6690162d4770f795e3f18dc4addd9bdb`.
The historical handoff and confirmation are not modified by this correction.

## H1-a: signed zero is distinguished

The prior commitment projection deliberately normalized JSON `-0.0` and
`0.0` to the same bytes. The correction removes that special case. Numeric
values now contribute their `serde_json::Number` rendering to the existing
type-tagged and length-framed digest, so the two signed-zero representations
produce different commitments.

`canonical_commitment_sorts_nested_objects_and_distinguishes_signed_zero`
proves all three relevant properties:

- nested object key order remains canonical;
- otherwise-identical `-0.0` and `0.0` groups have different digests; and
- changing the signed zero under an already sealed boundary fails both
  new-publication and complete provenance validation.

This closes the identified signed-zero collision. It does not change the
documented boundary that the digest is an integrity commitment rather than an
authentication mechanism.

## H1-b: managed single and batch appends are gated

Both `EventStore::append` and `EventStore::append_batch` now validate the
transition from the store's current suffix to the requested event or batch
before either a sink call or an in-memory mutation:

- sink-backed stores hold the sink mutex as append-order authority, inspect the
  current in-memory suffix, validate framing and duplicate IDs, release the
  state read guard, then call the sink and finally publish to memory;
- sinkless stores hold one state write guard across validation, duplicate-ID
  checking, and the eventual push; and
- custom sinks reached through `EventStore` therefore receive no rejected
  single event or batch.

The shared transition validator first validates complete response-publication
groups wholly inside the request. It then examines only the trailing part of
the existing timeline that could still be completed. A response group has at
most four rows, so an incomplete but completable group must begin within the
last three existing rows. The scan and existing-suffix clone are therefore
bounded O(1), rather than rescanning an entire growing timeline on each append.

The regressions cover:

- single legacy and V1 boundary appends through sinkless and counting custom
  stores, with zero sink calls and zero memory mutation;
- direct and response-audio legacy prefixes at every cut through sinkless and
  custom-store batch routes, with the original memory preserved;
- a suffix-only single append against a preloaded legacy boundary; and
- ordinary non-publication single events continuing to reach a custom sink.

Existing rejection tests that intentionally construct malformed history now
use `append_unvalidated_for_test`. That helper is compiled only under
`#[cfg(test)]`, is crate-private, refuses every sink-backed store, and retains
duplicate-ID rejection. It is an explicit invalid-fixture mechanism, not a
production escape from the new gate.

## H1-c: durable legacy orphan completion is refused

The concrete Norn JSONL writers now reject an existing incomplete legacy
`ResponseStatePublication` group before accepting a suffix:

- `JsonlSink::persist` performs the check after its strict read and before
  adding or writing a previously absent singleton;
- the multi-row JSONL batch path performs it after the strict read and before
  retry-prefix calculation, counter mutation, or writes; and
- the same concrete checks run when that JSONL sink targets a registered
  timeline, under the registered target's locking and identity authority.

The check is deliberately limited to incomplete legacy groups. An interrupted
V1 group may still complete only by an exact retry whose existing committed
boundary verifies the complete candidate. A complete legacy group remains
readable and may precede a newly committed V1 group; the correction does not
rewrite or reject valid historical data.

The direct JSONL regression writes every nonempty incomplete prefix of both the
three-row direct and four-row response-audio legacy forms. It then attempts the
remaining suffix and proves the file bytes and `EventStore` memory are
unchanged. The same cut matrix is exercised through a registered `JsonlSink`.
The registered sink opens the deliberately raw fixture successfully; its
subsequent append is what fails without writing. No claim is made that opening
the sink itself rejects the fixture.

## Mutation binding

Each mutation was applied independently, its exact named regression was run,
and the production guard was restored before final verification:

- restoring the old signed-zero normalization made
  `canonical_commitment_sorts_nested_objects_and_distinguishes_signed_zero`
  fail **0/1**;
- removing both `EventStore::append` transition checks made
  `custom_store_single_append_rejects_legacy_orphan_completion` fail **0/1**;
- removing both `append_batch` transition checks made
  `custom_store_rejects_suffix_only_legacy_orphan_completion` fail **0/1**;
- removing the multi-row JSONL durable-orphan check made
  `jsonl_sink_rejects_suffix_only_legacy_orphan_completion_without_writes`
  fail **0/1**; and
- separately removing the JSONL singleton check made that same exact test fail
  **0/1**, through its one-row remaining-suffix cases.

These results bind H1-a, the managed single and batch gates, and both concrete
JSONL persistence branches rather than merely observing final success output.

## Source-bound evidence

The retained record is
[`evidence/p5-d3/2026-07-21-p5-d3-h1-residual-correction-evidence.json`](evidence/p5-d3/2026-07-21-p5-d3-h1-residual-correction-evidence.json),
committed at `a79af89`. Its SHA-256 is
`4e02c796ff79758eeb78caebeb93a3f8c11afc9614e2c0278729e1428268e04e`.

The record binds:

- source `c8619aec1ab065ddd14bd5cd2cd574bc95e087c3` and tree
  `e7f676a1a6389f307d6892ce3ae763de5436682e`;
- runner
  `docs/reviews/evidence/p5-d3/run_p5_d3_evidence.py`, SHA-256
  `57a17055c4c6907d335e4971df20202c1a1c88a4c3c1f272eb128d24b333f0df`;
- the 13-path inventory and hash stated above; and
- 998 NUL-delimited committed Rust manifest records, SHA-256
  `fc846815acdf965189a5e2d2a9fedde05a39cbb9f985d34e40b1a1be5551afa2`.

The runner records **70/70** successful observations with zero failures:

- independent same-process handle publication: **20/20**;
- independent process publication: **20/20**; and
- 30 exact H1, compatibility, retry, strict-reader, fork, and D3 sentinels:
  **30/30**.

Final verification at the corrected source also reports:

- default-feature `norn` library suite: **4,214/4,214**;
- all-feature `norn` library test binary: **4,219/4,219**;
- `cargo test --locked --workspace --all-targets --all-features
  --no-fail-fast --quiet`: **5,591/5,591** tests across the listed test
  binaries;
- workspace doctests: **8/8**;
- strict workspace/all-target/all-feature Clippy with `-D warnings`: pass without
  a lint suppression;
- `cargo fmt --all -- --check`: pass;
- `git diff --check`: pass.

Cargo used the repository's ordinary `target/` directory, not a temporary
target. Every Rust path in the correction inventory remains below 500 physical
lines at the exact source; the maximum is 494 lines.

## Honest scope

The corrected guarantee is exact but intentionally bounded:

- Norn's publisher and appends routed through `EventStore` enforce the complete
  H1 transition before the relevant mutation. The concrete direct and
  registered JSONL paths additionally enforce H1-c against extending a durable
  legacy orphan.
- The public `PersistenceSink` trait cannot prevent an embedder from calling
  its own sink implementation directly. This handoff makes no H1 claim over
  such calls; the guarantee applies when a custom sink is reached through
  `EventStore`.
- `EventStore::with_sink_and_events` documents its supplied events as already
  persisted history. The constructor trusts that preload and does not perform
  a full historical validation sweep. Managed open/resume paths validate their
  durable history separately, but arbitrary embedder-supplied preloads remain
  outside this claim.
- The bounded transition scan prevents minting or completing a publication
  through managed appends; it is not presented as a general validator for an
  arbitrarily malformed trusted preload.
- Multi-row persistence remains interruption-recoverable rather than
  physically atomic. Exact-prefix retry and fail-closed reopen semantics are
  unchanged.

D8 role authority, owner acceptance and merge of D3, whole-P5 acceptance, P2
acceptance, WebSocket work, the broader conversation-state matrices, the
pre-existing non-driven headless exit-class follow-up, and the mandatory D7/P9
authenticated live-wire gate remain outside this narrow confirmation.

## Narrow confirmation request

The same reviewer should confirm only that:

1. H1-a now distinguishes signed zero and the mutation result binds that
   behavior.
2. H1-b rejects invalid single and batch transitions before sink or memory
   mutation for sinkless and custom `EventStore` routes.
3. H1-c rejects every tested direct/audio legacy prefix through singleton,
   multi-row, direct JSONL, and registered writer routes without writes.
4. Complete legacy history remains compatible and exact V1 retry semantics are
   unchanged.
5. The trailing transition scan is bounded by the four-row format invariant
   and introduces no history-length-dependent append regression.
6. The retained hashes, 13-path inventory, 998-record Rust manifest, 70/70
   observations, and final gates reproduce at the exact source.
7. No statement above extends the guarantee to direct embedder-owned sink calls
   or unvalidated trusted preloads.

The requested verdict is **H1-a CLOSED; H1-b CLOSED; H1-c CLOSED** or one
specific remaining defect. D3's existing `READY` implementation-candidate
verdict at `0dc2035` should otherwise remain unchanged; this narrow review is
not an owner-acceptance or whole-P5 verdict.
