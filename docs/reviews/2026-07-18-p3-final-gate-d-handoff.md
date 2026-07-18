# P3 final Gate D handoff

**Date:** 2026-07-18

**Status:** READY FOR INDEPENDENT P3 REVIEW; not P3 acceptance

**Subsequent review:** [`P3 whole-phase Gate D — READY`](2026-07-18-p3-final-gate-d-review.md), committed at `06be7c7`

**Phase base:** `a90b730091bccaeaa03ba98c3b31425e40e32dac`

**Frozen source:** `7f47218d8629d55a09577348d6b1a57a78f2aecf`

**Frozen tree:** `b8b042f61b8d921b4cb27496d5a72b8d56b8bb0c`

**Exact review range:** `a90b730091bccaeaa03ba98c3b31425e40e32dac..7f47218d8629d55a09577348d6b1a57a78f2aecf`

**Tracking plan:**
[`RESPONSES-API-REMEDIATION-PLAN.md`](../RESPONSES-API-REMEDIATION-PLAN.md)

**Decision record:** D2, D11, D12, D13, and proposed D15 in
[`DECISIONS-2026-07.md`](../DECISIONS-2026-07.md)

## Verdict requested

Return one P3-only `READY` or `NOT READY` verdict for the canonical ordered
transcript outcome. Do not issue a P4 verdict in this review. The source range
contains interleaved P3/P4 work, but the acceptance claims remain separate. P4
cannot be accepted until this review returns `READY`.

This handoff and its retained JSON artifacts are documentation commits after the
frozen source. They do not move the reviewed source head. Any production-source
correction after `7f47218` invalidates this freeze and requires regenerated final
evidence.

## P3 outcome under review

The ordered Responses item vector is the canonical provider transcript. Display
text, reasoning, calls, and stop behavior are derived projections. Exact provider
item JSON and order survive assembly, strict JSONL persistence, reload,
`store:false` replay, persistent spawn, top-level fork, and agent fork. Stream
coordinates remain provenance and are not injected into replayable provider
items.

The reviewed source includes:

- The pinned 28-item public output union, the separate Codex overlay, explicit
  authoritative validators, typed supported variants, and opaque raw retention
  for unknown or unsupported forms.
- Exact preservation of phase, IDs, multiple message boundaries, refusal,
  annotations, hosted-search data, compaction items, encrypted reasoning, and
  unknown reasoning parts.
- The accepted D2 versionless strict `~/.norn/session-store/` namespace, offline
  migration, immutable backup, three fidelity classifications, bounded cutover
  proof, deep verification, and fresh-provider-epoch resume policy.
- Response-scoped receive-only audio sidecars with private storage, sealing,
  linking, reload, cancellation/retry behavior, and ownership-changing fork
  publication without inventing a twenty-ninth item.
- The D11 finite official inventory: 28 variants, 274 contextual properties,
  659 schema-state assertions, seven behavioral classes, and ten lifecycle
  surfaces. This is not 659 lifecycle executions or a Cartesian matrix.

## Final machine evidence

The final runner used the main repository's normal `target/` directory. No OS
temporary build directory was used. It executed on the native host because the
test corpus includes loopback fixtures.

| Artifact | SHA-256 |
|---|---|
| [`final gate`](evidence/p3-p4/2026-07-18-p3-p4-final-gate-7f47218.json) | `b7e86b9ede8add13f68ceb1e88f9fa949dbe01a97e7577228eca8bcb65e2a451` |
| [`policy`](evidence/p3-p4/2026-07-18-p3-p4-final-policy-7f47218.json) | `aecbd180b4678783fc0cf7e7127adbcbaa76de3e10aef86c454b88e284524fd5` |
| [`distributions`](evidence/p3-p4/2026-07-18-p3-p4-final-distributions-7f47218.json) | `99c3c4f126d38d7260bd4774ddd23e893402b220e8105584b46b7636dcba3e63` |
| [`redaction`](evidence/p3-p4/2026-07-18-p3-p4-final-redaction-7f47218.json) | `63a4fbbb86fbf1e90d660905baa91064e84148cb5ba156c121aafd2935f9d182` |
| [`attestation`](evidence/p3-p4/2026-07-18-p3-p4-final-attestation-7f47218.json) | `8d5907283f1cfcb7adc2580aefa47591f6e13d1bc07408a9434f5ee6ca1f6123` |

The single-process attestation binds the gate, policy, distributions, and
redaction report to the same source and tree and reports `passed: true` with an
empty error list. The gate binds a 350-path NUL-delimited inventory with SHA-256
`5532d614e21038fe67d38152f5c6eb7d8be23d942528f37732e81a5de0a85aa4`.

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
| Exact-range `git diff --check` | pass |
| Syntax-aware full-range policy | pass |

The test counts are per command and overlap; they must not be summed into a
unique-test claim.

The policy report covers 298 changed Rust files, of which 78 are test-only. It
reports zero production files over 500 lines, zero thin-entrypoint violations,
zero module-shape violations, and zero added-line unwrap, expect, panic, lint
suppression, ignored-test, unresolved-marker, `todo!`, or `unimplemented!`
violations. The retained 143-entry writer-candidate inventory is conservative and
remains available for manual ownership review.

The source-bound redaction report scans 198 changed Rust fixture blobs, 12
changed or hash-pinned historical JSON artifacts, and the three generated gate,
policy, and distribution artifacts. It reports zero findings. It separately
counts 352 absolute build-path fields in already accepted historical artifacts;
those disclosures are retained rather than rewritten, while current fixtures
and generated artifacts must be path-neutral. Arbitrary human prose remains a
disclosed semantic-review boundary rather than a mechanically exhaustive claim.

| Repeated case | Distribution |
|---|---|
| Same-ID concurrent open/resume convergence | 20/20 |
| Concurrent migrated-resume convergence | 20/20 |
| Two-audio-artifact top-level fork ownership after source deletion | 20/20 |
| Total | 60/60 |

## Accepted foundation

The corrected D2 range `2c0350d..e9755fe` holds unconditional `READY` after the
same reviewer closed F1 in
[`2026-07-17-d2-f1-correction-review.md`](2026-07-17-d2-f1-correction-review.md).
The response-audio M-1/F-2 correction is `READY` in
[`2026-07-18-p3-p4-response-audio-correction-review.md`](2026-07-18-p3-p4-response-audio-correction-review.md).
The D11 evidence candidate is `READY` in
[`2026-07-18-p3-p4-optional-lifecycle-review.md`](2026-07-18-p3-p4-optional-lifecycle-review.md).

Those verdicts are foundation evidence, not substitutes for this whole-phase
P3 review.

## Honest boundaries

- P3 establishes the canonical transcript foundation. It does not close
  `STATE-01` or `EVT-01` through `EVT-07`; P4 owns those findings.
- Local `SessionEvent::Compaction` is a P5/D3 provider-facing view transform, not
  an eleventh D11 carrier surface. Superseded canonical rows remain immutable in
  the audit store. Provider `ResponseItem::Compaction` remains covered among the
  28 output items.
- The replay normalization allowlist is empty for the pinned contract. Norn
  removes only its own stream-provenance envelope, not provider item fields.
- The source supports response-audio reception and durable carriage. It does not
  claim request-side audio, playback, export, or audio-specific TUI rendering.
- Deterministic public/Codex contract fixtures cover P3. The credentialed
  subscription real-wire fixture was not run. Proposed D15 would retain it for
  D7/P9; until the owner confirms that scope change, it remains an open P4 item
  but does not block this P3 transcript review.
- P6 owns absent-versus-zero legacy usage projection and retry-attempt UI
  cleanup. P5 owns provider/local conversation-state and turn-state semantics.

## Carried observation ledger

The following reviewed observations remain disclosed and non-blocking unless the
final reviewer proves a regression or an outcome violation.

| Source | Carried observation |
|---|---|
| D2 | Registered appends hold the global index lock across validation, write, and fsync; full-timeline rescans make repeated append cost O(n²) over a session. |
| D2 | A complete JSON tail without its newline fails closed as typed `TornTail`; no repair verb exists. |
| D2 | Index temporary-name lookalikes fail-stop while publication/deletion lookalikes are ignored; neither path grants cleanup authority. |
| D2 | Index counters are a repairable cache and resume repairs are logged rather than silently trusted. |
| D2 | Timeline and epoch lock files are intentionally retained and can accumulate. |
| D2 | The cutover guard is not type-enforced: an embedder can manually construct the canonical store path and pass it to explicit custom-store authority `SessionManager::new`; `standard()` is the guarded standard-store front door. |
| D2 | Intermediate stage ancestors rely on journal ordering; the reviewed backup-stage durability asymmetry itself was fixed by F1. |
| D2 | Migrated sessions retain genuine historical provider item IDs as recorded input, but the provider anchor is severed and no continuity is manufactured. |
| Audio | Interrupted unlinked sidecars can remain as private orphans until session deletion. |
| Audio | Semantically identical terminal-row rewrites can survive the corruption digest; data-changing mutations remain detected by counts, hashes, and flags. |
| Audio | Small sidecar writes can run on the current-thread executor when `block_in_place` is unavailable. |
| Audio | Abnormal-stop `loop.partial_output` append is best-effort but loud. |
| Audio | Transcript-delta plain-text/Base64 asymmetry is proven end to end rather than by a symmetric unit test. |
| D11 | The 274 count is contextual and tool-union dominated; it is not 274 unrelated schema surfaces. |
| D11 | Cosmetic fixture ID reuse has no identity collision in the corpus. |

The final runner supersedes D11's earlier crude production-LOC and literal
OS-temp evidence observations with the whole-range syntax-aware policy and
canonical target enforcement. D11's substring-based test-anchor lookup was
already guarded by an exactly-one test result and was not exploitable; the final
runner uses its own hash-pinned contract and exact result validation. The
accepted D11 artifact remains hash-pinned as historical evidence.

## Required independent review

The review should use four disjoint responsibilities:

1. A Responses-protocol reviewer checks the canonical item model, exact JSON,
   order, phase, IDs, normalization boundary, opaque forms, and actionability.
2. A session/persistence reviewer checks strict storage, migration/rejection,
   reload, `store:false` replay, spawn/fork ownership, audio links, and the
   severed provider anchor for migrated IDs.
3. A fresh Fable adversarial reviewer attacks evidence provenance and compares
   raw fixtures with assembly, persistence, reload, replay, spawn, and both fork
   forms.
4. The coordinator independently reproduces artifact hashes and the final gate
   or an equivalent source-bound battery, adjudicates every finding, and returns
   one P3-only verdict.

`READY` requires no unresolved P3-owned or newly unowned implementation defect,
honest agreement between code, evidence, and this handoff, and preservation of
all strict lint/LOC/module rules without bypasses. Any source fix creates a new
source head and requires a narrow correction review plus regenerated final
evidence before P3 can close.
