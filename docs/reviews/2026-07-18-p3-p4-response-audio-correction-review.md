# P3/P4 response-audio M-1/F-2 correction confirmation

**Date:** 2026-07-18
**Reviewer:** Sable Nightwick (same coordinator as review `50115bf`,
[`2026-07-18-p3-p4-response-audio-review.md`](2026-07-18-p3-p4-response-audio-review.md))
**Handoff:** [`2026-07-18-p3-p4-response-audio-correction-handoff.md`](2026-07-18-p3-p4-response-audio-correction-handoff.md)
**Correction source:** `df47e9ed09a9e5241206d7ee7a8835f884c8f95c`
(tree `c87706ee79bb633838e41f5c0a1ca36fe2104e77`), narrow diff `50115bf..df47e9e`
**Method:** coordinator-only narrow confirmation per the D2 F1 precedent — no
panel, no subagents. Every claim below was verified directly by the reviewer.

## Verdict

**M-1 CLOSED; F-2 CLOSED; focused response-audio slice READY.**

The `NOT READY` returned by review `50115bf` was contingent on exactly M-1
(reconciler duplicate-detection cache retaining the complete media stream in
memory, undisclosed) and carried F-2 (collapsed transcript-association
diagnostics). Both are fixed at `df47e9e` by the mechanisms the original
review prescribed. The prior fixture acceptance (all six lifecycle cases
sufficient, 64/64 evidence) stands unchanged and was not re-reviewed.

This confirmation does not accept P3 or P4. The optional-shape/lifecycle
candidate at `56fd4dd` is reviewed separately in
[`2026-07-18-p3-p4-optional-lifecycle-review.md`](2026-07-18-p3-p4-optional-lifecycle-review.md).

## 1. Scope (handoff request 1)

`git diff --no-ext-diff --name-only 50115bf..df47e9e` returns exactly the nine
Rust paths in the handoff, none other. Classification verified by reading each
diff: five production paths (`response_reconciler.rs`,
`response_reconciler/frame_signature.rs`, `session/manager/open.rs`,
`session/persistence/publication_audio_links.rs`,
`session/persistence/types.rs`) and four `#[cfg(test)]`-gated test modules. No
persisted format, request, provider-event, CLI, or TUI surface is touched; the
only public-API change is the new `SessionPersistError` variant, as disclosed.

## 2. M-1 mechanism (handoff requests 2–3)

Verified by direct read of `response_reconciler.rs` and `frame_signature.rs`
at `df47e9e`:

- `frames: BTreeMap<u64, FrameSignature>` now maps each accepted sequence
  number to `FrameSignature([u8; 32])` — `Copy`, drop-free, exactly 32 bytes
  (pinned by `frame_signatures_are_fixed_width_copy_values`). The signature is
  built at the single call site in `accept_sequence` and has no other
  consumers. The event string, JSON tree, Base64 payload, and decoded media no
  longer survive duplicate detection; the cache remains `O(sequence count)`,
  as the handoff states (and does not claim constant total memory).
- The digest is domain-separated (`norn.responses.frame-signature.v1\0`) and
  the encoding is injective by construction: distinct one-byte type tags for
  all six JSON value kinds; strings, object keys, and the event type
  length-framed; arrays and objects count-framed with self-delimiting
  elements; object keys sorted, matching `serde_json`'s order-independent map
  equality.
- Numbers hash their `serde_json` equality representation. I attacked this
  seam specifically: the only place `serde_json::Number` display diverges from
  its equality relation is the signed zero (`-0.0 == 0.0` but distinct
  renderings), and the code normalizes exactly that case to `0.0`. Integer
  `0` correctly remains distinct from float `0.0` (different `Number`
  variants, unequal in serde_json). The two pinned collision pairs are the
  genuine bit-aliases a discarded generic-`Hash` design would conflate:
  `4607182418800017408` (the u64 bit pattern of `1.0f64`) vs `1.0`, and
  `u64::MAX` vs `-1i64`. Equality preservation is pinned in both directions
  (`0.0`/`-0.0` equal; `1.0`/`1.00` parse-equal) by
  `frame_signatures_preserve_value_equality_and_cover_all_content`.
- Behavior preservation: `reordered_object_keys_remain_an_identical_duplicate`
  pins reordered-key frames as `DuplicateSequence`, and
  `audio_delta_exact_duplicate_is_idempotent_but_changed_payload_conflicts`
  pins exact-duplicate vs `ConflictingDuplicateSequence` on a real audio
  delta. The handoff's disclosure that classification now relies on SHA-256
  collision resistance rather than value identity is accurate and acceptable.

## 3. F-2 mechanism (handoff request 4)

Verified by direct read: `SessionPersistError` gains
`InvalidResponseAudioReference(#[from] ResponseAudioReferenceError)`
(`types.rs:118-121`), and both seams — `manager/open.rs:181` (resume) and
`publication_audio_links.rs:22-23` (fork publication) — now propagate with
plain `?`, removing the `|_error|` discards and the `<transcript>`
placeholder. The pass-through is total over the source enum by construction.
Both regressions run the real seams: resume pins
`DuplicateArtifactLink` with the actual artifact UUID and exact rendered
message; fork publication pins `LinkDoesNotPrecedeAssistant` with the actual
assistant event UUID, exact message, and proves no destination timeline,
index row, or audio publication stage became visible. Diagnostics carry only
static reasons and local identifiers, never payload or media.

## 4. Evidence reproduction (handoff request 5)

All reproduced byte-exact on the reviewer's host:

| Artifact | Handoff/gate value | Reproduced |
|---|---|---|
| Runner SHA-256 | `7e3cb2a2…936f` | match |
| Retained gate JSON SHA-256 | `d76ee180…51b5` | match |
| NUL-delimited diff inventory SHA-256 | `6b080077…02b2` | match |
| Rust source-manifest SHA-256 | `f00ae241…b365` | match |
| Source tree | `c87706ee…4e77` | match |
| Nine per-path source SHA-256s | gate `production_loc.inventory` | all nine match |
| Production-prefix LOC (spot: `types.rs` 498, `response_reconciler.rs` 492) | gate values | match, all < 500 |

Battery rerun from a clean detached checkout of `df47e9e` with the primary
repository target directory:

| Leg | Result |
|---|---|
| `cargo fmt --all -- --check` (1.94.0) | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` (1.94.0, locked) | clean |
| Full norn library | 3,994 / 3,994 |
| `response_reconciler` filter | 113 / 113 |
| `response_audio` filter | 41 / 41 |
| Six exact sentinels (4 × M-1, 2 × F-2) | 6 × 1/1 |

The handoff's decision to add no new repeated distribution is sound: both
corrections are deterministic, and the previously accepted 64/64 lifecycle
distributions bind the surrounding behavior.

## 5. Standing

- M-1 and F-2 are closed; the focused response-audio slice (production range
  `460c192..0512953` plus correction `df47e9e`, lifecycle fixtures `f252cbb`)
  is **READY**.
- Unchanged from review `50115bf`: the five-item observation ledger, the
  fixture sufficiency acceptance, and all disclosed residuals.
- Still open, exactly as the handoff states: independent review of the
  optional-shape/lifecycle candidate (separate document), the retrospective
  P3/P4 phase-base disposition, P1/P2 acceptance, full-range final P3 and P4
  gates, and separate phase acceptance reviews.
