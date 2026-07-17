# P3/P4 response-audio implementation handoff

**Status:** Implementation candidate. This handoff does not accept P3 or P4.
The broader exhaustive lifecycle matrix, full-range phase evidence, dependency
closure, and independent phase review remain open.

**Candidate base:**
`460c192b5160fcabfa647418a75ecf29665f6743`.

**Candidate source:**
`0512953e650c4961e790f5987896c131e82ba4f3`; tree
`1aeac724119bb525340cf7cef67dbac906131ac0`.

**Source range:** `460c192..0512953`.

**Owning records:** `docs/DECISIONS-2026-07.md` section 16 and the P3/P4
checklists in `docs/RESPONSES-API-REMEDIATION-PLAN.md`.

## 1. Claimed boundary

The candidate establishes these claims:

1. The provider recognizes and validates `response.audio.delta`,
   `response.audio.done`, `response.audio.transcript.delta`, and
   `response.audio.transcript.done` without creating an output-item identity or
   item/content coordinates that are absent from the official event contract.
2. Raw accepted envelopes remain observable. A separate typed actionable
   projection is emitted only after response-stream sequence and channel-state
   reconciliation succeeds. Exact duplicate sequence frames are raw-only and do
   not mutate durable media twice.
3. Accepted frames are written once to a private response-scoped JSONL artifact.
   The artifact records its owner, attempt, exact raw frames, actual completion
   flags, optional response ID, counts, hashes, and a corruption-detection
   digest. It does not manufacture codec, MIME, voice, or terminal media data.
4. Successful turns seal the sidecar before linking it to the assistant event.
   Cancellation and hard-cut paths checkpoint only an unsealed partial
   reference. A failed attempt never becomes a success link, and a retry uses a
   distinct artifact.
5. The accepted D2 format-2 `AssistantMessage` production codec remains
   unchanged. A versioned `response.audio.artifact` custom event links the
   sidecar to the future assistant event ID; the assistant then records that
   link as its parent. Hook insertion between the two events is supported by
   precedence and parentage rather than adjacency.
6. Ordinary resume and in-root filtered agent forks remain structural
   `O(events)` operations and validate sidecar content when read. An
   ownership-changing top-level session fork instead validates, manifests,
   copies, syncs, and crash-recovers the referenced audio bundle before
   publishing the destination timeline and index row.
7. The CLI emits the lossless raw event and suppresses the typed duplicate. The
   TUI consumes the raw and typed projections without forcing redraw.

The candidate does **not** claim request-side audio generation, playback,
export, WebSocket transport, TUI rendering, or TUI playback. It does not claim
that every fork eagerly reads media. It does not claim that a pre-audio binary
can safely fork or delete an audio-bearing format-2 session. It does not close
the broader optional-shape lifecycle matrix or accept P3/P4.

## 2. Official contract

The OpenAI Developer Docs MCP was queried and the following official pages were
fetched on 2026-07-17:

| Event | Official reference | Candidate interpretation |
|---|---|---|
| `response.audio.delta` | [Reference](https://developers.openai.com/api/reference/resources/responses/streaming-events#response.audio.delta) | Base64 media delta plus response-stream sequence number. |
| `response.audio.done` | [Reference](https://developers.openai.com/api/reference/resources/responses/streaming-events#response.audio.done) | Audio-channel completion plus sequence number. |
| `response.audio.transcript.delta` | [Reference](https://developers.openai.com/api/reference/resources/responses/streaming-events#response.audio.transcript.delta) | Transcript delta plus sequence number. |
| `response.audio.transcript.done` | [Reference](https://developers.openai.com/api/reference/resources/responses/streaming-events#response.audio.transcript.done) | Transcript-channel completion plus sequence number. |

The generated event schemas contain no output-item identity, output index,
content index, codec, MIME type, voice, or terminal media payload. Their examples
also carry a response ID, so the candidate preserves an observed response ID as
an optional binding without requiring or fabricating one. The generated
[`Responses create` reference](https://developers.openai.com/api/reference/resources/responses/methods/create)
and the [`Responses benefits` guide](https://developers.openai.com/api/docs/guides/migrate-to-responses#responses-benefits)
do not justify advertising outbound audio generation; the guide still labels
Responses audio as coming soon. This implementation is deliberately receive
only.

## 3. Durable representation

Each artifact is stored beneath the root session at:

```text
artifacts/response-audio/<canonical-uuid-v4>.jsonl
```

The private artifact has three strict record classes:

- A header binds schema version, artifact ID, original root-session ID,
  generation, attempt, and creation time.
- Frame rows preserve each accepted raw envelope exactly once. Base64 media is
  decoded transiently during live reconciliation and hashing, and by the read
  API when materializing durable content; capture does not retain a second
  decoded durable copy.
- One terminal row records media/transcript counts and hashes, independent done
  flags, optional response ID, and an integrity digest. The digest detects
  corruption; it is not a MAC, signature, identity proof, or attestation.

The reader rejects malformed headers, ordering violations, duplicate or
conflicting frames, trailing data after a terminal row, digest mismatch,
invalid generation shape, invalid references, and link/response-ID mismatch. A
torn final non-newline row is classified as unsealed rather than silently
repaired. Generation ABA is enforced by the store authority around
open/create/seal; decode does not compare the preserved original header with a
fork destination generation. The writer and full reader each hold one governed
descriptor permit for their lifetime.

The format-2 assistant row has no new field. The durable sequence is:

1. Seal and sync the sidecar.
2. Mint the assistant event ID.
3. Append `SessionEvent::Custom` with event type `response.audio.artifact`,
   version 1, the future assistant ID, reference, and optional response ID.
4. Append the assistant with the custom link event as parent.

A real `SessionEventHook` fixture appends another event between steps 3 and 4;
strict resume accepts the resulting precedence and parent relation. A crash
after step 3 leaves an honest orphan precursor. A successful link must resolve
to a sealed sidecar whose terminal response ID exactly matches the link,
including the absence case when exercised by the read API.

## 4. Runtime and failure behavior

Capture begins lazily on the first validated typed audio frame for an attempt.
If the loop has no managed audio store, the frame produces typed
`UnsupportedResponseMedia`. A sidecar I/O or validation failure becomes a typed
session failure and does not enter the provider retry path.

Raw and typed audio events remain available to live consumers but are excluded
from the response-assembly vector, so the loop does not retain the complete
media stream in memory. On success, sealing precedes assistant persistence. On
cancellation or a hard cut, the durable partial record contains an unsealed
reference only after the reference checkpoint is synced; checkpoint failure
clears the reference rather than exposing an unproven path.

Top-level ownership-changing fork publication uses a version-3 journal and an
exact sorted audio manifest containing reference, byte length, SHA-256, sealed
state, and terminal response ID. Version-2 timeline-only journals remain
readable and recoverable. Source copy and validation stream through bounded
buffers, publish the artifact directory before the timeline and index, sync
files and directories, reject replacement/collision/tamper/missing input, and
recover after source deletion. Orphan owned stages are reclaimed.

The global index lock remains held across ownership-changing bundle validation
and copy. Registered append also retains D2's full-timeline rescan and fsync.
This preserves the accepted publication and generation-ABA invariants but can
serialize a store and make repeated append work quadratic over a long session.
That scale residual is explicit and is not claimed fixed by this slice.

## 5. Compatibility boundary

The strict format-2 reader allowlist is unchanged, and current source reads
pre-audio format-2 sessions. The production-prefix hashes for both D2 codec
seams are identical between base and candidate:

| Production seam | Base SHA-256 | Candidate SHA-256 |
|---|---|---|
| `session/events.rs` | `de8003fce3cd134330321e1738951ca06312ab03a50d535b765402471fd97415` | `de8003fce3cd134330321e1738951ca06312ab03a50d535b765402471fd97415` |
| strict reader | `e1e4a004c06f03d87f3b25abf20492fcd0fb5a3d71daae06ff462c9086a10e02` | `e1e4a004c06f03d87f3b25abf20492fcd0fb5a3d71daae06ff462c9086a10e02` |

An older pre-audio binary can parse and preserve the custom row, but it does
not copy audio sidecars during fork or understand their ownership during
deletion. Mutating an audio-bearing session with that binary is therefore an
unsupported operational downgrade, not a safe compatibility promise.

## 6. Retained evidence

The retained gate used Rust 1.94.0 and the repository's normal `target/`
directory. It records nine passing legs:

| Gate | Result |
|---|---|
| `cargo +1.94.0 fmt --all -- --check` | Pass |
| `cargo +1.94.0 --locked clippy --workspace --all-targets -- -D warnings` | Pass |
| workspace all-target tests | 5,345/5,345 |
| workspace doctests | 8/8 |
| focused `response_audio` library suite | 37/37 |
| publication/recovery suite | 24/24 |
| filtered-fork suite | 6/6 |
| CLI raw-only audio consumer | 1/1 |
| TUI no-redraw audio consumer | 1/1 |

The repeated runner records five cases at 20/20 each, 100/100 total:

- cancellation after an accepted frame leaves only an unsealed partial;
- every durable audio-publication checkpoint recovers without a dangling link;
- a top-level fork owns its copied audio after source deletion;
- a real session hook may append between the link and assistant; and
- fork publication deduplicates references and survives source-artifact
  deletion.

Retained artifacts and SHA-256 hashes:

| Artifact | SHA-256 |
|---|---|
| [`run_response_audio_gate.sh`](evidence/p3-p4-audio/run_response_audio_gate.sh) | `bd7e27713f9a97afc0cd0914f2e3548aae331ca6e6627899c73e40f944ebacf0` |
| [`2026-07-17-response-audio-gate-0512953.json`](evidence/p3-p4-audio/2026-07-17-response-audio-gate-0512953.json) | `38a20fb6d99f7eea23327870d79dbb40233039df6e2d3475984b5f911abe824c` |
| [`run_response_audio_distributions.sh`](evidence/p3-p4-audio/run_response_audio_distributions.sh) | `024e771bcee8f30281d84e92073c0ebc41b9cd9cee42b2fcb6f1b2298d24b14b` |
| [`2026-07-17-response-audio-distributions-0512953.json`](evidence/p3-p4-audio/2026-07-17-response-audio-distributions-0512953.json) | `d4bac58deea36713ef6ead9522cc2f639babf0dd1f497aa58a5bef06ac6113a6` |

The gate JSON mechanically inventories all 64 Rust paths in the source range,
42 modified and 22 added. The list is sorted, unique, and equals the complete
`460c192..0512953` path inventory. Production LOC is counted before the first
exact `#[cfg(test)]`; the maximum is 499 lines in
`provider/openai/response_reconciler.rs`, with no path at or above 500. The
retained policy regex reports zero additions matching its enumerated
`allow`/panicking-helper patterns. This is not expanded into a claim that one
regex proves every possible bypass class absent.

### Working-tree overlay disclosure

The user had two pre-existing, unrelated dirty build inputs that were not
modified or staged by this work. Both runners bind their committed, initial,
and final observed hashes. The initial and final hashes match, so no change was
observed at those two checkpoints; this is not continuous monitoring or an ABA
proof:

| Input | `0512953` SHA-256 | Observed SHA-256 | Effect |
|---|---|---|---|
| `crates/norn/src/tools/diagnostics_check/tests.rs` | `ad94045c8bbc5c5bccc495781203ac09076ceab80bc69a0b1a41688000314b4c` | `7b6871677c045facbc5e40fe47ffd9f7435592418f4616a91acd6a6e33e1f455` | Adds two unrelated diagnostics tests to the workspace count. |
| `CONVENTIONS.toml` | `9e5526c25854f3d47ea3c4e418d2b1f4d843a3f427a446d3e3360faae309d4e7` | `6b5b0adaf0bbba77069a1a3adf67b918a3fd606c0020cf94cff3eb9edb0657df` | Compiled into those tests through `include_str!`; no response-audio production path reads it. |

Accordingly, the retained gate is described as committed response-audio source
plus a cryptographically bound test-only overlay, not as a wholly clean
checkout. The 5,345 workspace count includes the two overlay tests. None of the
five repeated response-audio cases executes that diagnostics module.

The first workspace attempt inside the managed network sandbox was invalid:
3,881 tests had passed and 111 failed when loopback binds consistently returned
`EPERM`. That environmental run is not evidence. The identical gate was rerun
outside the network sandbox and produced the 9/9 result above in the normal
repository target directory. No external-network result is part of this
evidence. Command-output hashes in the JSON are fingerprints of discarded
output strings, not retained test logs.

## 7. Deliberately open exhaustive cases

The focused audio contract is implemented and evidenced, but the broader P3
line-1607 optional-shape and all-lifecycle matrix remains unchecked. The
following combined cases are still useful before whole-phase acceptance:

- hard cut after sidecar sealing but before link publication;
- an end-to-end success path whose response-ID binding is absent;
- an explicit exactly-one persisted terminal-marker assertion;
- multiple distinct audio artifacts copied by one top-level fork;
- a seeded real `ForkTool` audio path beyond the existing context-filter and
  manager-fork fixtures; and
- malformed transcript-delta symmetry with malformed media-delta coverage.

These are not claimed by the checked focused audio evidence. They remain in the
exhaustive phase ledger for the final P3/P4 gate and reviewer disposition.

## 8. Independent review request

The reviewer should inspect `460c192..0512953` and this complete package, then:

1. Compare all four event shapes and sequence semantics with the official
   references above.
2. Attack raw-to-typed ordering, duplicate handling, independent channel
   completion, cancellation, retry, and missing-store behavior.
3. Verify strict sidecar parsing, sealing, corruption classification, link
   binding, hook insertion, and the unchanged format-2 codec boundary.
4. Attack version-3 publication and recovery across collision, tamper, source
   deletion, no-replace publication, generation-ABA, and every durable
   checkpoint.
5. Confirm the narrower fork claim: only ownership-changing top-level forks
   eagerly validate/copy; resume and in-root forks validate media when read.
6. Confirm CLI raw-only output, TUI no-redraw/no-playback behavior, and the
   pre-audio-binary operational downgrade limitation.
7. Reproduce the artifact hashes, 64-path inventory, LOC and policy results,
   D2 codec hashes, 9/9 gate, and 100/100 distributions, including the disclosed
   two-file test-only overlay.
8. Return `READY` or `NOT READY` for this implementation candidate and state
   whether any item in section 7 blocks whole P3/P4 acceptance.

The implementation review may close this response-audio slice. It must not mark
P3/P4 accepted unless the remaining dependency, exhaustive-matrix, full-range
evidence, and universal review/exit requirements are also satisfied.
