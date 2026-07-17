# P3/P4 response-audio M-1/F-2 correction handoff

**Status:** Narrow correction candidate awaiting confirmation by the same
coordinator who recorded the original review. This handoff does not accept P3
or P4.

**Original review:** `50115bff5ba615dde9a02631fbc4ee1b3ef30d1b`,
[`2026-07-18-p3-p4-response-audio-review.md`](2026-07-18-p3-p4-response-audio-review.md),
which returned `NOT READY` on M-1 and carried F-2 into the correction.

**Correction source:** `df47e9ed09a9e5241206d7ee7a8835f884c8f95c`;
tree `c87706ee79bb633838e41f5c0a1ca36fe2104e77`.

**Narrow source diff:** `50115bf..df47e9e`; nine Rust paths. Five contain
production changes and four are test-only modules.

## Correction

### M-1: bounded retained frame identity

The original candidate retained an owned event-type string and complete parsed
JSON value for every accepted sequence number. Once response-audio frames were
accepted, that cache retained the full Base64 media stream for the response.
The memory statement in the original implementation handoff was therefore
invalidated by the review and is superseded by this correction.

`ResponseReconciler` now stores one `FrameSignature([u8; 32])` per accepted
sequence number. The signature is a domain-separated SHA-256 digest over the
event type and an explicitly tagged, length-framed representation of the parsed
`serde_json::Value`:

- JSON types have distinct tags;
- strings, arrays, objects, event types, and object keys are length-framed;
- object keys are sorted so object insertion order does not change identity;
- numbers use the equality representation exposed by `serde_json::Number`,
  including equality-based signed-zero normalization; and
- the event type is part of the signature independently of the payload.

This preserves equal-value duplicate classification and distinguishes the
known integer/float collision pairs from the discarded generic-`Hash` design.
Classification relies on SHA-256 collision resistance; it is not a
mathematical identity proof. The cache remains `O(sequence count)` and retains
the map key/node overhead plus 32 digest bytes per sequence. It no longer
retains the event string, JSON tree, Base64 string, or decoded media through
duplicate detection. This is not a claim of constant total memory.

The focused tests require `FrameSignature` to be `Copy`, drop-free, and exactly
32 bytes. They pin parsed-value equality, reordered object keys, event-type and
payload changes, the two deterministic numeric collisions found during local
review, and exact-versus-conflicting replay of a real audio delta.

### F-2: preserved transcript-association diagnostics

Resume and ownership-changing fork publication previously collapsed every
`ResponseAudioReferenceError` into a generic
`InvalidResponseAudioArtifact` containing the placeholder `<transcript>`.

`SessionPersistError` now has a typed
`InvalidResponseAudioReference(ResponseAudioReferenceError)` variant, and both
seams propagate the source with `?`. The structural pass-through covers all
source variants. Two real-path regressions directly pin representative routes:

- resume reports the actual artifact UUID for a duplicate artifact link; and
- fork publication reports the actual assistant event UUID when a link does
  not precede its assistant, then proves no destination timeline, index row, or
  audio publication stage became visible.

These diagnostics contain static reasons and local canonical identifiers, not
provider payload or media contents.

## Exact scope

Production paths:

- `crates/norn/src/provider/openai/response_reconciler.rs`
- `crates/norn/src/provider/openai/response_reconciler/frame_signature.rs`
- `crates/norn/src/session/manager/open.rs`
- `crates/norn/src/session/persistence/publication_audio_links.rs`
- `crates/norn/src/session/persistence/types.rs`

Test-only module paths:

- `crates/norn/src/provider/openai/response_reconciler/tests/audio.rs`
- `crates/norn/src/provider/openai/response_reconciler/tests/sequence.rs`
- `crates/norn/src/session/persistence/publication_audio_tests.rs`
- `crates/norn/src/session/response_audio_lifecycle_tests.rs`

No persisted row, sidecar, link, request, provider-event, CLI-event, or TUI
format changed. The public `SessionPersistError` enum gains one variant; an
external exhaustive match may therefore require a new arm.

## Retained evidence

The committed runner honors `CARGO_TARGET_DIR`, refuses dirty or post-correction
Rust source, and was run with the main repository `target` directory. The full
library leg ran outside the managed network sandbox because loopback-binding
tests require local socket authority; it used no external network result.

[`2026-07-17-response-audio-correction-gate-df47e9e.json`](evidence/p3-p4-audio/2026-07-17-response-audio-correction-gate-df47e9e.json)
records:

- 12/12 gate legs passed;
- strict workspace/all-target Clippy and `cargo fmt --check` passed;
- full Norn library tests passed 3,994/3,994;
- the reconciler suite passed 113/113;
- the response-audio filter passed 41/41;
- all six exact new sentinels passed 1/1;
- the complete nine-path correction inventory is retained with per-path source
  hashes;
- the NUL-delimited inventory SHA-256 is
  `6b0800774e74fdd80661cd5331bcf5d2478ca4f927519a3ffd32fe61a33602b2`;
- the Rust source-manifest SHA-256 is
  `f00ae24121da91e3b8844c4f2d374dbac0cfbf423e1117edb16c7610fe6ab365`;
- the enumerated prohibited-added-line scan found zero matches; and
- all nine production-prefix counts are below 500 lines, with the maximum 498
  in `session/persistence/types.rs`.

The runner is committed at `47270d8`; its SHA-256 is
`7e3cb2a2b6755f567738cce82846a6162f5a548f93c97cf25277934ae091936f`.
The retained JSON is committed at `12a50ab`; its SHA-256 is
`d76ee1809907ab2d981da2c91be21975c56f5be2aaae85c69cbd2b3f0a651bb5`.
The JSON filename uses the runner's UTC date; this handoff uses the local
Australia/Melbourne date.

No repeated distribution was added. M-1 and F-2 are deterministic corrections,
and the original review explicitly accepted the existing 64/64 lifecycle
fixture evidence.

## Standing

The original review accepted all six lifecycle fixtures for their named claims;
they were not reworked or re-accepted here. The finite optional-shape inventory,
retrospective P3/P4 phase-base disposition, P1/P2 dependencies, full-range P3
and P4 gates, and separate phase acceptance reviews remain open. This package
does not close any of those items.

## Narrow confirmation request

The same review coordinator should inspect `50115bf..df47e9e` and:

1. Confirm the complete nine-path scope and the five-production/four-test-only
   classification.
2. Verify that the reconciler retains only a fixed-width digest per sequence,
   while the map remains `O(sequence count)`.
3. Attack the framed digest's type, object-order, number, event-type, and payload
   equality behavior, including the two pinned numeric collision pairs.
4. Verify typed `ResponseAudioReferenceError` propagation through resume and
   fork publication and the two real-seam diagnostics.
5. Reproduce the runner, JSON, inventory, source-manifest, and per-path hashes,
   then rerun the focused tests and strict Clippy.
6. Return `M-1 CLOSED; F-2 CLOSED; focused response-audio slice READY`, or
   identify a remaining defect.

This is a same-coordinator correction confirmation, not a repeat of the full
panel and not a request to accept P3 or P4.
