# P5 D3 Gate D correction confirmation

**Date:** 2026-07-21

**Reviewer:** Sable Nightwick (same reviewer as the original D3 Gate D verdict)
+ one Opus H1-mechanism seat + norn cross-model adversarial pass (GPT-5.6 Sol,
`correctness`/xhigh, read-only, session `claude-review-d3corr.wSMey9`)

**Controlling review:**
[`2026-07-21-p5-d3-gate-d-review.md`](2026-07-21-p5-d3-gate-d-review.md)
(`8a90c68`, NOT READY on R1 + R2)

**Correction handoff:**
[`2026-07-21-p5-d3-gate-d-correction-handoff.md`](2026-07-21-p5-d3-gate-d-correction-handoff.md)

**Confirmed source:** correction tip `0c7fe3e` on `codex/p5-d3-correction`
(H1 range `2ee38a5..0c7fe3e`; R1/R2/H2/H3 in the reconstructed history at
`acfcb69` and the split/feature reconstruction). **`main` untouched (`35d0b3d`).**

## Verdict

**R1 CLOSED; R2 CLOSED; H2 CLOSED; H3 CLOSED; H4 corrected. D3 READY as an
implementation candidate.** The two required corrections that made the original
candidate NOT READY are genuinely closed and mutation-verified, and the elected
hardening H2/H3/H4 is closed.

**H1 is implemented and closes the runner-reachable retry surface, but its
unqualified "whole-group commitment" claim is not fully delivered:** three
residual gaps survive, all **runner-unreachable** and consistent in class with
the hardening this campaign has repeatedly ruled owner-acceptable. They do not
block D3 (the required corrections are closed and the product is sound
by-construction), but "H1 CLOSED" should be stated as **"H1 runner-path integrity
closed; residual gaps documented,"** not as a complete whole-group-commitment
guarantee. I recommend — not require — closing the two cheap enforcement gaps so
H1's public contract is uniform.

## R1 — CLOSED (mutation-verified)

`ContextFilter::apply` now returns `Result<Vec<SessionEvent>, ContextFilterError>`;
identity filtering is an exact `Ok(events.to_vec())`; both response-audio scans
(`fork_context_filter.rs:186`, `:228`) use
`ResponseAudioArtifactLink::from_event(event)?`, so only `Ok(None)` reaches the
`continue` and a malformed reserved row propagates typed. `apply`'s `Result` is
`#[must_use]` and clippy `-D warnings` is clean, so no in-crate caller can drop it.

**Mutation kill (mine):** reverting **both** `?` sites to the old swallowing
`let Ok(Some(link)) = … else { continue }` fails
`malformed_reserved_audio_link_fails_typed_only_for_nonidentity_filter`
("a malformed reserved response-audio row was silently filtered") while
`unrelated_custom_event_with_audio_shaped_data_remains_opaque` still passes;
restored byte-clean. (Reverting only one site does not fail the test — the other
site catches it — so both are load-bearing.)

## R2 — CLOSED (built standalone; feature tree byte-identical)

The history was reconstructed rather than squash-hidden. At the corrected split
`61c7a52`, `prompt_context.rs` retains its **local `ContextEdits::new()` fallback**
and `with_prompt_context_edits` is **not referenced** — no forward dependency. I
built it: `cargo check --locked -p norn --all-targets --all-features` at `61c7a52`
**finished clean (exit 0)**. The feature commit `97f63a5` introduces the helper,
both callers, the tracker-free durable-mark docs, and the tracker-free regressions
atomically, and its tree `78702204…` is **byte-identical** to the reviewed feature
tree. No per-commit-bisectability waiver was needed.

## H2 — CLOSED

`legacy_closed_before_epoch` now scans the **whole prefix** for any `FilteredFork`
(`provider_state_validation.rs:90-100`), not just the immediate pre-epoch cut, so
`[FilteredFork, Compaction, unframed assistant]` classifies the assistant as
`UnmarkedAfterProvenance`, not `Legacy`. The `NotStored` anchor-walk arm
(`conversation_state.rs:137-139`) now clears `legacy_candidates` while leaving
`proven` untouched. Both properties have regressions.

## H3 — CLOSED

`valid_target_shape` gains `if target_base.id != *target { return Ok(false); }`
(`provider_state_validation.rs:408-410`) **before** both the direct and audio
branches, so a positional assistant whose id differs from the provenance's declared
target fails local frame recognition, stays visible (not hidden), and fails full
validation.

## H1 — implemented; runner-path integrity closed; three residual gaps (runner-unreachable)

**What works, and is mutation-verified.** New publications use
`ProviderEpochBoundaryReason::ResponseStatePublicationV1(commitment)` carrying an
`event_count` + a domain-separated, type-tagged, length-framed, key-sorted
SHA-256 over the commitment-free group projection. `seal_response_publication_group`
seals before persistence; `validate_new_response_publication_batches` gates
`EventStore::append_batch` and `RetryPlanner::new`; full/strict/offline reads
re-run `verify`. **My mutation kill:** neutering `verify()` to always-`Ok` fails
`durable_committed_prefix_rejects_divergent_unwritten_suffix`,
`complete_committed_group_rejects_tampered_suffix_on_replay`, and both direct/audio
tamper sentinels; restored byte-clean. The Opus H1 seat independently rated the
mechanism READY (no digest collision that matters, every gated path validates,
enum break exhaustive with no wildcard arm).

**The three residual gaps (all runner-unreachable, do not block D3):**

- **H1-a — signed-zero non-injectivity (owner-rulable; recommend a decision).**
  `response_publication_commitment.rs:130-137` normalizes `-0.0` and `+0.0` to the
  same `"0.0"`; `canonical_commitment_sorts_nested_objects_and_normalizes_signed_zero`
  **asserts the two digests are equal**. So the digest does not bind a
  signed-zero-differing suffix. This was a **deliberate** choice to match the
  `RetryPlanner`'s serde-`Number` value-equality (which also treats `-0.0 == 0.0`).
  Within H1's **stated non-adversarial retry-integrity threat model it is inert** —
  a crash never flips a sign and the runner retries the identical group; a
  divergent-sign submitter is the "actor who can rewrite the group" H1 explicitly
  does not defend against. Cheap strict fix: stop normalizing signed zero so the
  commitment is strictly injective (a signed-zero-divergent retry then fails
  closed, which is correct). Recommend the owner rule: accept as inert, or make the
  digest sign-injective.
- **H1-b — `EventStore::append` and custom sinks are not gated (recommend the cheap
  fix).** Unlike `append_batch`, `EventStore::append` (`store.rs:326-349`) runs
  only a duplicate-ID check before `sink.persist`; it does not call
  `validate_new_response_publication_batches`. The runner is safe (it publishes via
  the gated `append_batch`/JsonlSink, and JsonlSink's own single-append validates
  and fails closed on an orphan boundary), but the **public
  `EventStore`/`PersistenceSink` contract** does not uniformly enforce H1 — an
  embedder using a custom sink with single appends can write an uncommitted legacy
  publication. Given the disclosed Meridian embedder surface, adding the same
  `validate_new_response_publication_batches` call to `append` is a cheap way to
  make the public contract enforce H1 uniformly.
- **H1-c — a durable legacy orphan boundary can be completed by a suffix-only
  request.** `validate_new_response_publication_batches` inspects only the requested
  batch, so a boundary-less `[provenance, assistant]` request passes the gate, and
  the completed timeline `[B_legacy, provenance, assistant]` is accepted by full
  validation (legacy reason permitted). Runner-unreachable: a legacy orphan only
  arises from a pre-correction crash, and such a timeline fails closed on normal
  reopen, so reaching this needs a direct persistence-API append bypassing the
  failing open. Contradicts the "cannot extend legacy groups" wording; closes with
  a completed-timeline (not request-local) new-publication check.

## Battery (mine, network-capable, repository target, at `0c7fe3e`)

`cargo fmt --all -- --check` clean; `cargo clippy --locked --workspace
--all-targets --all-features -- -D warnings` clean, no suppression; full workspace
green — **norn 4,213 / cli 518 / tui 683**, 8/8 doctests — matching the handoff.
`61c7a52` builds standalone (`cargo check --all-targets --all-features`, exit 0).

## Cross-model adjudication note

The cross-model pass returned NOT READY on H1 with one blocker and two majors; I
reproduced all three at the code level and confirmed each is **runner-unreachable**
— the signed-zero "blocker" is inert within H1's declared threat model (the pass
applied a stricter adversarial model H1 disclaims), and the two enforcement gaps
sit off the runner's gated `append_batch` path. This is the mirror of last round,
where the pass ran too permissive; here it ran slightly too strict. The findings
are nonetheless valuable: two are cheap, worthwhile completeness fixes for the
public contract, and the third is a real design decision on signed-zero. None
re-opens a D3 blocker, because the required corrections (R1, R2) are closed and the
product is sound by construction.

## Disposition and boundaries

- **D3 READY as an implementation candidate.** R1, R2 (required) and H2, H3, H4
  (elected hardening) are closed; H1 closes the runner-reachable retry surface.
- Recommended before claiming "H1 CLOSED" unqualified (owner-rulable, non-blocking):
  gate `EventStore::append` (H1-b), add a completed-timeline new-publication check
  (H1-c), and rule on signed-zero (H1-a). These are narrow and would warrant only a
  small same-reviewer re-confirmation.
- Out of scope, unchanged: D8 role authority, broad volatile-context / concurrent
  matrices, WebSocket transport, P2 acceptance, whole-P5 acceptance, and the
  mandatory D7/P9 authenticated real-wire gate.
- The pre-existing non-driven `stream-json` exit-class inversion
  (`orchestrator.rs:597-601`) remains a separate follow-up.
