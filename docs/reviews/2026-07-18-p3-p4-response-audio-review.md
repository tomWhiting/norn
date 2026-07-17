# P3/P4 response-audio implementation candidate — independent review

- **Review date (Australia/Melbourne):** 2026-07-18
- **Reviewer:** external review seat (coordinator) + three independent
  read-only Opus seats (event contract + sidecar; codec/link/fork/publication;
  lifecycle fixtures) + one Fable adversarial seat
- **Handoffs:** `2026-07-18-p3-p4-audio-lifecycle-handoff.md` (entry),
  `2026-07-17-p3-p4-response-audio-handoff.md` (production spec)
- **Frozen production range:** `460c192..0512953` (tree `1aeac724`)
- **Lifecycle fixture range:** `96d5f0e..f252cbb` (tree `3ec9515`), packaging
  through `4f559f5`
- **Owner contract:** `DECISIONS-2026-07.md` § 16

## Verdict: NOT READY — one confirmed undisclosed memory finding (M-1)

The candidate is otherwise acceptance-quality: every durability, ordering,
sealing, linking, publication, recovery, and reader-trust attack mounted by
the adversarial seat was defeated in read code, and the three area seats
returned SOUND. But one finding is confirmed, material, and undisclosed, and
it contradicts the handoff's own memory claim for exactly the workload this
slice introduces. The remediation is narrow; after it lands (or the owner
explicitly rules it a disclosed residual), nothing found here blocks READY.

**Fixture verdict (separate, per the handoff):** the `96d5f0e..f252cbb` range
is **test-only as claimed** (every hunk proven inside `#[cfg(test)]`; no
production line, suppression, `#[ignore]`, or invented gating), and **all six
lifecycle cases are SUFFICIENT for their named claims** — each drives the real
production seam it names, asserts against post-resume on-disk state or raw
sidecar bytes, and fails on every regression the seats could construct.

## Findings

**M-1 — MAJOR, CONFIRMED — the reconciler's duplicate-detection cache retains
the complete media stream in memory, undisclosed.**
`provider/openai/response_reconciler.rs:40-44,128-155`: `accept_sequence`
clones every accepted frame's full raw JSON — for `response.audio.delta`, the
entire base64 payload — into `frames: BTreeMap<u64, FrameSignature>`, retained
for the life of the response and never evicted. Full retention is load-bearing
for the current semantics (any earlier sequence can be replayed and must be
classified exact-duplicate vs conflicting), so this is not dead weight that
falls out for free. The structure predates the range, but at base `460c192`
all four audio events mapped to `UnsupportedMedia` and terminated the stream
(`response_reconciler/roles.rs:133-136` at base), so bulk media never lived in
the cache — this candidate is what first routes it through. Consequence: an
audio-bearing response accumulates ~1.33× the decoded media size in process
memory, unbounded, per in-flight response — the same OOM class as P0's GD-6,
scoped per-response. The handoff's §4 claim that "the loop does not retain the
complete media stream in memory" is true of the assembly vector and false of
the process; unlike the lock-occupancy and rescan residuals, this one is not
disclosed. Exposure today is tempered by Responses audio being not yet
generally available upstream, but the entire point of the slice is readiness
for that traffic. **Fix options (either closes it):** compare digests instead
of payloads — store a SHA-256 of the canonical envelope bytes in
`FrameSignature` (duplicate/conflict semantics preserved) — or an explicit
owner-ruled disclosure adding this to the recorded residuals. The first is
recommended and small.

**F-2 — MINOR, CONFIRMED — link-validation root cause discarded at resume.**
`manager/open.rs:181-186` (and the parallel fork-side mapping in
`publication_audio_links.rs:22-33`) map the six distinct
`ResponseAudioReferenceError` causes with `|_error|` into one generic
`InvalidResponseAudioArtifact { artifact_id: "<transcript>", reason: <static> }`.
The failure is typed and loud — resume/fork correctly abort — but the operator
of a session that refuses to reopen cannot see which artifact or which of the
six orderings failed. Preserve the source error (`#[source]` or embed the
variant and real reference). Diagnostic fidelity only; no incorrect behavior.

## What was proven

**Event contract + sidecar (Opus) — SOUND.** Exactly the four official shapes;
no fabricated item identity, index, codec, MIME, voice, or terminal media;
response ID optional-only. Dedup is exact-frame-signature: identical duplicate
= raw-only (capture untouched); same-sequence different-payload = typed
`ConflictingDuplicateSequence`, fail-closed. Sidecar: header binds
schema/id/owner/generation/attempt; frames written exactly once (writer
re-projects the raw envelope and requires equality with the typed event);
one terminal row; reader rejects the full malformation list; torn tail =
unsealed, never repaired; torn header = hard error; base64 decoded transiently
only; one governed descriptor permit per writer/reader lifetime.
Seal-before-link order confirmed end-to-end; checkpoint failure clears the
reference from both durable and typed partials; retry mints a distinct
artifact; no-store → terminal `UnsupportedResponseMedia`; sidecar I/O failure
is a typed session failure outside the provider retry path.

**Codec/link/fork/publication (Opus) — SOUND.** No new `SessionEvent` variant
or `AssistantMessage` field; the v1 link is `deny_unknown_fields` with
canonical-UUID and version pinning; all six broken orderings rejected
(assistant-before-link, duplicate assistant-link, duplicate artifact-link,
non-parent link, response-ID mismatch), honest orphan precursor accepted.
Fork taxonomy holds: resume and in-root forks are O(events) structural
(ForkTool inheritance resolves against root ownership + generation, no copy);
only ownership-changing top-level forks eagerly validate/manifest/copy/sync,
publishing the audio bundle before timeline and index. The v2/v3 journal split
is live discriminated durable-format evolution (v2 still written for non-audio
forks), not a compat shim. Bounded streaming copy with copy-vs-revalidation
digest cross-check; no-replace publication; exact-UUID orphan reclamation;
destination generation minted fresh and recovery requiring exact row equality
(generation included). CLI raw-only suppression and TUI no-redraw confirmed in
production code. D2 codec seams byte-identical (coordinator reproduced
`de8003fc…` production-prefix hash at base and candidate; strict reader
untouched).

**Adversarial (Fable) — all durability attacks DEFEATED.** Crash at every
publication checkpoint; orphan-link adoption (duplicate event IDs rejected at
append and read); hook-fabricated links brick resume fail-closed rather than
being adopted; hard-cut checkpoint is an fsync of the artifact itself under
the generation-checked index lock, with failure clearing both partials before
the durable record is built; journal-replay ABA defeated by exact-row-equality
recovery; staged-copy TOCTOU defeated by digest cross-checks; duplicate
terminal rows, post-terminal frames, interleaved attempts, sequence overflow,
and `None`-forging all rejected by the reader/writer pair. The one landed
attack is M-1 above.

**Lifecycle fixtures (Opus + Fable spot-check) — test-only, six/six
SUFFICIENT.** The two most load-bearing fixtures (hard-cut, ForkTool
inheritance) were independently attacked and cannot pass on a regressed
implementation. One observation: neither symmetry test directly asserts a
non-base64 transcript delta is *accepted* (the loop fixture proves it
end-to-end); the named §7 claim is still satisfied.

## Evidence integrity and battery (coordinator)

All mechanical claims reproduce exactly:

- Trees: `0512953^{tree}` = `1aeac724…`, `f252cbb^{tree}` = `3ec9515…`.
- All six retained artifact hashes match (lifecycle runner `398b6098…`,
  lifecycle JSON `4f1d5f81…`, gate runner `bd7e2771…`, gate JSON `38a20fb6…`,
  distribution runner `024e771b…`, distribution JSON `d4bac58d…`).
- Rust manifest hash `95be803c…` reproduced from `git ls-tree -r f252cbb`.
- 64-path production inventory count reproduced; lifecycle JSON binds source
  commit/tree exactly.

Battery (worktree, `CARGO_TARGET_DIR` = main repo target, native 1.94.0,
alone on host):

| Leg | Result |
|---|---|
| `cargo +1.94.0 --locked fmt --all -- --check` | pass |
| `cargo +1.94.0 --locked clippy --workspace --all-targets -- -D warnings` | pass |
| Focused `response_audio` filter | 39/39 |
| Full `norn` library suite | **3,988/3,988** |
| Lifecycle invocations (4×1 + 3×20) | **64/64** |
| Original distribution cases ×20 | 80/80 surviving four; fifth superseded |

The fifth original case
(`top_level_fork_owns_copied_response_audio_after_source_deletion`) was
renamed and strengthened in the fixture range into the two-artifact successor
(itself 20/20 here); its original 20/20 remains bound to `0512953` in the
retained JSON. Not a regression.

## Observations (non-blocking ledger)

1. Mid-stream sidecar append failure leaves an unreferenced orphan `.jsonl`
   (never linked; retry uses a fresh UUID) with no reclamation on the
   non-fork path; likewise a crash between seal and link leaves a sealed,
   wholly unreferenced file. Correctness-harmless residue until session
   deletion; not described by the handoff's orphan taxonomy.
2. Semantically-identical byte rewrites of the terminal row (e.g.
   `"response_id":null` vs field absence) survive the corruption digest; all
   data mutations are caught by the recomputed counts/hashes/flags.
3. `off_executor` runs sidecar I/O on the executor thread on a current-thread
   runtime (`block_in_place` unavailable there); small writes, deliberate.
4. The `loop.partial_output` append on abnormal stop is best-effort-loud
   (disclosed D2-era pattern).
5. Base64 asymmetry direction (transcript deltas accepted as plain text) is
   proven end-to-end by the loop fixture but not directly asserted in the
   symmetry tests.

## Plan honesty

DECISIONS § 16 and both handoffs record the receive-only boundary, the
disclosed scale residuals (index-lock occupancy during bundle copy, quadratic
rescans), the operational-downgrade posture for pre-audio binaries, the
sandbox-invalidated first test run, and the working-tree overlay — all
honestly. The P3/P4 checklists correctly keep phase acceptance open. The one
disclosure gap is M-1's memory retention, which is the finding.

## Standing

NOT READY for the focused response-audio slice on M-1 alone; F-2 rides along
as a bounded diagnostic fix. The six lifecycle fixtures are accepted for
their named claims and need no rework. This review does not accept P3 or P4;
the optional-shape matrix, phase-base disposition, P1/P2 acceptance chain,
and final phase gates remain open per the handoffs. A narrow correction
round (M-1 + F-2) with a same-reviewer confirmation is the expected path, per
the D2 F1 precedent.
