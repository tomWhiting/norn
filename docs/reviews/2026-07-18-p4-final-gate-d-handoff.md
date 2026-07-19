# P4 final Gate D handoff

**Date:** 2026-07-18

**Status:** READY FOR INDEPENDENT P4 REVIEW; not P4 acceptance

**Subsequent result:** Review `80f0e36` returned `NOT READY`. This document
remains the historical `7f47218` submission. Corrected production source
`ab26632`, evidence head `180759f`, regenerated artifacts, and narrowed EVT-06
semantics are recorded in
[`2026-07-19-p4-final-gate-d-correction-handoff.md`](2026-07-19-p4-final-gate-d-correction-handoff.md).
Nothing below records P4 acceptance.

**Phase base:** `a90b730091bccaeaa03ba98c3b31425e40e32dac`

**Frozen source:** `7f47218d8629d55a09577348d6b1a57a78f2aecf`

**Frozen tree:** `b8b042f61b8d921b4cb27496d5a72b8d56b8bb0c`

**Exact review range:** `a90b730091bccaeaa03ba98c3b31425e40e32dac..7f47218d8629d55a09577348d6b1a57a78f2aecf`

**P3 prerequisite handoff:**
[`2026-07-18-p3-final-gate-d-handoff.md`](2026-07-18-p3-final-gate-d-handoff.md)

**P3 prerequisite result:**
[`2026-07-18-p3-final-gate-d-review.md`](2026-07-18-p3-final-gate-d-review.md) — `READY` at `06be7c7`

**Tracking plan:**
[`RESPONSES-API-REMEDIATION-PLAN.md`](../RESPONSES-API-REMEDIATION-PLAN.md)

## Sequencing and verdict requested

P3's independent `READY` prerequisite is satisfied by `06be7c7`. Owner-approved
D15 makes deterministic public/Codex fixtures sufficient for the P4 gate and
assigns the mandatory authenticated real-wire test to D7/P9 before overall
integrated acceptance. Return one separate P4-only `READY` or `NOT READY`
verdict for `STATE-01` and `EVT-01` through `EVT-07`. Do not infer P4 acceptance
from the common machine bundle or from P3's verdict.

The source range contains interleaved P3/P4 implementation, so this prepared
package uses one common source-bound machine bundle. D15 resolves the
live-fixture scope without claiming that the live fixture ran. The outcomes,
reviewer seats, and verdicts remain separate. Any production-source correction
after `7f47218` invalidates
the freeze and requires regenerated evidence.

## P4 outcome under review

Completed output items drive the P3 canonical ordered transcript. Identity-keyed
deltas drive responsive preview and reconcile with authoritative completion.
Refusal is a typed non-retryable model outcome, hosted-search data survives tool
continuation and replay, unknown wire forms fail before ordinary success, and
only authoritative completed executable items can dispatch.

The pinned contract contains 53 public stream events, 28 public output-item
discriminators, 16 public tool schemas represented by 18 public tool literals,
and a separately classified Codex overlay. Every output discriminator has an
authoritative validator and an explicit inert, executable, conditional, or
unsupported posture.

## Finding closure map

| Finding | Candidate closure claim |
|---|---|
| `STATE-01` | Stream and terminal authority produce the P3 canonical item transcript, which persists and replays under `store:false` as exact provider items without leaking local stream coordinates. |
| `EVT-01` | Refusal survives assembly, tool-loop handling, persistence, resume, CLI, and TUI as a typed non-retryable model outcome; it cannot become empty success or schema retry. |
| `EVT-02` | Item order, multiple message boundaries, phase including explicit null, IDs, and item/content coordinates are reconciled without lossy flattening. |
| `EVT-03` | Hosted-search action, sources, citations/annotations, local-tool continuation, persistence, and resumed stateless replay survive in canonical order. |
| `EVT-04` | Missing preview suffixes are repaired only from authoritative completion; non-prefix conflicts and malformed terminal data fail atomically. |
| `EVT-05` | Unknown future item/event JSON is retained raw for diagnosis and produces a typed failure before an ordinary assistant/tool turn is published. |
| `EVT-06` | Output-index, item, and call identity are stable; optional IDs are not fabricated; exact duplicates are idempotent; conflicts and rebinding fail closed. |
| `EVT-07` | Only authoritative completed executable items dispatch; incomplete, delta-only, unresolved, or malformed calls cannot execute. |

The original EVT-06 row records the submitted claim and was too broad. The
corrected claim is limited to exact duplicate sequences processed before
terminal delivery; direct post-terminal mapper input fails closed.

Strong source anchors include:

- `crates/norn/src/provider/openai/request/canonical_replay_tests.rs` for exact
  ordered replay and hosted-search continuation.
- `crates/norn/src/provider/openai/execute/reconciliation_tests/equivalence.rs`
  for streamed/terminal canonical equivalence and authoritative repair.
- `crates/norn/src/loop/runner/tests/refusal_matrix.rs` for refusal across loop,
  persistence, resume, usage, schema-retry, and dispatch behavior.
- `crates/norn-cli/src/print/output.rs`,
  `crates/norn-cli/src/print/output/provider_events.rs`,
  `crates/norn-tui/src/app/dispatch.rs`, and
  `crates/norn-tui/src/app/dispatch/finalization.rs` for CLI/TUI refusal output,
  preview, and terminal handling.
- `crates/norn/src/loop/runner/tests/responses_replay_matrix.rs` for hosted-search
  plus local-tool continuation and resumed replay.
- `crates/norn/src/provider/openai/response_reconciler/tests/terminal.rs` and
  `call_identity.rs` for terminal authority, duplicate/conflict handling, and
  executable gating.
- `crates/norn/src/loop/unsupported_response_loop_tests.rs` for unknown-wire
  failure before ordinary loop success.

## Final machine evidence

The final runner used the main repository's normal `target/` directory and the
native host for loopback fixtures. No OS temporary build directory was used.

| Artifact | SHA-256 |
|---|---|
| [`final gate`](evidence/p3-p4/2026-07-18-p3-p4-final-gate-7f47218.json) | `b7e86b9ede8add13f68ceb1e88f9fa949dbe01a97e7577228eca8bcb65e2a451` |
| [`policy`](evidence/p3-p4/2026-07-18-p3-p4-final-policy-7f47218.json) | `aecbd180b4678783fc0cf7e7127adbcbaa76de3e10aef86c454b88e284524fd5` |
| [`distributions`](evidence/p3-p4/2026-07-18-p3-p4-final-distributions-7f47218.json) | `99c3c4f126d38d7260bd4774ddd23e893402b220e8105584b46b7636dcba3e63` |
| [`redaction`](evidence/p3-p4/2026-07-18-p3-p4-final-redaction-7f47218.json) | `63a4fbbb86fbf1e90d660905baa91064e84148cb5ba156c121aafd2935f9d182` |
| [`attestation`](evidence/p3-p4/2026-07-18-p3-p4-final-attestation-7f47218.json) | `8d5907283f1cfcb7adc2580aefa47591f6e13d1bc07408a9434f5ee6ca1f6123` |

The attestation reports `generation_mode: single_process_final`, `passed: true`,
and no errors. It binds the gate, policy, distribution, and redaction hashes to
the exact source and tree. The gate binds 350 changed paths through a
NUL-delimited inventory hash.

| Gate leg | Result |
|---|---|
| Pinned Rust 1.94.0 formatting | pass |
| Strict workspace, all-target, all-feature Clippy with `-D warnings` | pass |
| `norn` complete test surface | 4,035/4,035 |
| `norn-cli` complete test surface | 551/551 |
| `norn-tui` complete test surface | 700/700 |
| Workspace all-target, all-feature tests | 5,364/5,364 |
| Workspace all-feature doctests | 8/8 |
| Isolated redaction sentinels | 23/23 |
| Exact-range diff check and syntax-aware policy | pass |

The policy covers 298 changed Rust files, including 78 test-only files, and has
zero production files over 500 lines, zero entrypoint/module-shape violations,
and zero added-line unwrap, expect, panic, lint suppression, ignored-test,
unresolved-marker, `todo!`, or `unimplemented!` violations. The per-command test
counts overlap and are not summed into a unique-test claim.

The redaction report covers 213 source-bound records: 198 changed Rust fixture
blobs, 12 changed or hash-pinned historical JSON artifacts, and three current
generated artifacts. It has zero findings. Its separately counted 352 absolute
build-path fields belong to accepted historical artifacts and remain disclosed;
current fixtures and generated artifacts must be path-neutral. Arbitrary human
prose remains an explicit manual-review boundary.

| Repeated case | Distribution |
|---|---|
| Same-ID concurrent open/resume convergence | 20/20 |
| Concurrent migrated-resume convergence | 20/20 |
| Two-audio-artifact top-level fork ownership after source deletion | 20/20 |
| Total | 60/60 |

## D11 and lifecycle evidence

D11 records 28 output variants, 274 contextual property occurrences, 659 legal
schema-state assertions, and a seven-class by ten-surface lifecycle matrix with
45 covered and 25 reasoned-inapplicable cells. The 659 figure is a contract
enumeration, not 659 executions through each surface. Unsupported executable
forms remain failure-only and cannot inflate the success corpus.

The accepted response-audio slice retains response-level delta and done frames
in a private sidecar because the official events expose no output-item identity,
content coordinates, codec/MIME, or terminal media payload. The implementation
does not fabricate any of those fields. M-1's full-payload duplicate retention
and F-2's erased link diagnostic are closed in
[`2026-07-18-p3-p4-response-audio-correction-review.md`](2026-07-18-p3-p4-response-audio-correction-review.md).

## Honest boundaries

- P4's P3 dependency is satisfied by whole-phase review `06be7c7`, and D15 is
  owner-approved. This handoff does not itself satisfy P4 review.
- Local `SessionEvent::Compaction` remains P5/D3 prompt-view work, not an
  eleventh P4/D11 carrier surface. Provider `ResponseItem::Compaction` remains
  covered among the 28 output items.
- Request-side audio, playback, export, audio-specific TUI rendering, and a
  fabricated terminal audio item are not claimed.
- P6 owns retry-producer termination, retry-attempt TUI cleanup, and
  absent-versus-zero legacy `Usage` projection. Raw terminal usage already
  preserves presence, but P4 does not claim the P6 projection fix.
- Deterministic public and Codex contract fixtures are complete. D15 supersedes
  the earlier live Codex-subscription fixture as a P4 blocker and requires
  credentialed real-wire conformance later under D7/P9 before overall integrated
  acceptance. The fixture was not run, and D15 is not credential-use or
  spending approval; those still require explicit approval at the point of use.
- Unknown output items and non-allowlisted events retain raw evidence and fail
  typed. The candidate does not advertise execution for unsupported forms.

## Carried observation ledger

The accepted D2 observations remain disclosed: global index-lock occupancy and
O(n²) repeated-append rescans; typed `TornTail` without a repair verb; asymmetric
lookalike posture without cleanup authority; logged counter repair; retained lock
files; a non-type-enforced cutover guard that a manual canonical path passed to
explicit custom-store authority `SessionManager::new` can bypass; stage ancestor
durability by journal ordering; and genuine migrated provider IDs as recorded
input under a severed provider anchor. `SessionManager::standard()` remains the
guarded standard-store front door.

The accepted audio observations remain disclosed: unreferenced private orphan
sidecars after interruption; semantic terminal-row rewrite tolerance; small I/O
on a current-thread executor; best-effort-loud abnormal partial-output append;
and the end-to-end rather than symmetric-unit proof for plain-text transcript
deltas versus Base64 audio.

The D11 precision observations remain non-blocking: contextual tool-union
occurrences dominate 274; one fixture cosmetically reuses an ID string without
an identity collision. The final whole-range runner supersedes the earlier crude
LOC heuristic and literal OS-temp field with syntax-aware policy and canonical
target enforcement. D11's substring-based test-anchor lookup was guarded by an
exactly-one result and was not exploitable; the final runner uses separate
hash-pinned contract and exact result validation.

## Required independent review

The P4 review may begin: P3's `READY` prerequisite and D15 scope ruling are both
satisfied. It should use five disjoint responsibilities:

1. A streaming/item reviewer re-enumerates the 53 public events, 28 item
   discriminators, Codex overlay, SSE framing, identity reconciliation, terminal
   authority, executable gating, and unknown-wire failure.
2. A UI/session reviewer checks preview versus canonical authority, suffix-only
   repair, refusal display, raw CLI events, TUI no-duplication behavior, strict
   persistence/reload, and `store:false` resume.
3. The streaming review-domain owner confirms every immediately advertised
   capability survives request, wire, reconciliation, persistence, replay, and
   user surface.
4. A fresh Fable adversarial reviewer attacks duplicates, interleaving, identity
   rebinding, missing deltas, malformed or unknown items, post-terminal input,
   partial publication, and replay divergence.
5. The coordinator reproduces the artifact hashes and final gate or an equivalent
   source-bound battery, adjudicates every finding, and issues a P4-only verdict.

`READY` requires explicit closure of `STATE-01` and `EVT-01` through `EVT-07`,
no unresolved P4-owned or newly unowned implementation defect, and no lint,
file-size, module-shape, evidence, or scope bypass. Any source correction requires
a new frozen source and regenerated final evidence before P4 can close.
