# P5 D3 conversation-state Gate D review

**Date:** 2026-07-21

**Reviewer:** Sable Nightwick (coordinator) + five Opus area seats
(provenance/anchor; persistence/exact-prefix; threading/resume/fork;
structural-split behavior; headless reliability) + norn cross-model pass
(GPT-5.6 Sol, `correctness` preset, xhigh, read-only, session
`claude-review-d3.ebVotj`)

**Handoffs:**
[`2026-07-20-p5-d3-gate-d-handoff.md`](2026-07-20-p5-d3-gate-d-handoff.md),
[`2026-07-20-headless-reliability-gate-d-handoff.md`](2026-07-20-headless-reliability-gate-d-handoff.md)

**Reviewed source:** `ef3b9c7`, functional range `b75e64b..ef3b9c7` (over
accepted merge base); structural split `e3549b4..b75e64b`; headless slices
`974a216..e3549b4` and `ef3b9c7..31553e8`. Branch `codex/p5-d3-compaction`,
HEAD `beeb1f5`. **`main` untouched (`35d0b3d`).**

## Verdict

**D3 (conversation-state): NOT READY as an implementation candidate — on two
narrowly-scoped required corrections, neither of which is a reachable security or
correctness defect in the running product.** The D3 functional core is strong:
the durable framing, exact-prefix persistence, anchor eligibility, public-vs-Codex
threading, resume repair, fork seeding, and forward/backward compatibility all
verify sound across four Opus seats and my own deep trace, and the full battery is
green (norn 4,192 / cli 518 / tui 683). No reachable path crosses provider state,
resurrects a stale anchor, drops/duplicates a delta item, or loses data.

The NOT READY rests on this codebase's own categorical bar, not on product
incorrectness:

1. **Required — bare `continue`-on-`Err` (CLAUDE.md categorical violation).**
   `crates/norn/src/agent/fork_context_filter.rs:180` and `:222`.
2. **Required — split-commit integrity.** `b75e64b` does not build standalone and
   folds a (correct) behavior change into a commit labelled a mechanical split.

**Headless driven-transport reliability: READY as an isolated change.** Its one
real finding is pre-existing and outside both reviewed slices.

## Required correction R1 — bare `continue`-on-`Err` on a malformed reserved audio row

`ContextFilter::apply`'s two audio-pairing passes both use:

```rust
let Ok(Some(link)) = ResponseAudioArtifactLink::from_event(event) else {
    continue;
};
```

`from_event` returns `Ok(None)` for an unrelated event and `Err` for an event
that **is** the reserved `response.audio.artifact` family but has a malformed
payload (e.g. an unsupported version). The `let-else` collapses both into
`continue`, so a malformed reserved row is **silently swallowed**. This is a
literal violation of CLAUDE.md's "No silent failures … no swallowed `Result`s …
no bare `continue` on `Err`."

- **Reachable input:** an application can emit
  `SessionEvent::Custom { event_type: "response.audio.artifact", data: <bad> }`
  through the ordinary custom-event surfaces; a forward-version link from a newer
  binary is another source.
- **Harm is low but non-zero:** the malformed row is retained as opaque
  application data (consistent with the slice's own collision-preservation
  design), so there is no anchor crossing, no security boundary breach, and no
  silent data loss in the filter. The defect is the swallowed parse error itself,
  which this codebase does not permit at any harm level.
- **Fix (Sol's, endorsed):** keep `Ok(None)` as the only `continue`; make the
  pass propagate a typed error on `Err`. Add a regression that a malformed
  reserved audio row in a fork source fails typed at the filtering boundary
  rather than being skipped.

## Required correction R2 — `b75e64b` split-commit integrity

The handoff frames `b75e64b` as a separately-reviewable *mechanical* module split.
That framing is falsified, and the commit is not self-contained:

- **Does not build standalone.** `crates/norn/src/loop/loop_context/prompt_context.rs:134`
  and `:169` call `crate::r#loop::context::with_prompt_context_edits`, which is
  **not defined until the next commit `5358bea`** (14 s later). The norn crate
  therefore does not compile at `b75e64b` — a `git bisect` hazard and a broken
  per-commit-CI invariant.
- **Folds a behavior change into the "split."** For tracker-free callers
  (`context_edits == None`), the pre-split code used an **empty** `ContextEdits`
  (no marks applied); the post-split path routes through `with_prompt_context_edits`,
  whose `None` branch applies `projected.apply_persisted_marks(store)`. This is the
  **correct, intended p5 durability semantics** (durable compaction/suppression/
  injection should project even without a live tracker) and the main runner path
  (`context_edits == Some`) is unaffected — so it is a *correct* change
  *mis-attributed* to the split commit rather than to `5358bea`.

The `spawn` and `fork` production splits **are** faithful mechanical moves
(verified token-identical modulo required `super::`/visibility path adjustments),
and the test-module rewrite is behavior-preserving (zero assertion loss, no
`#[ignore]`, no vacuous-pass path, no `#[cfg(any())]`), though it is discretionary
scope for a "split" commit.

**Resolution:** squash/reorder so the durability behavior change and its helper
definition land atomically in `5358bea` (leaving `b75e64b` a true, building,
mechanical split) — **or**, if the slice is squash-merged into a single commit,
an explicit owner acknowledgement that per-commit bisectability is waived for this
slice. This is the one point where merge policy governs the resolution.

## Recommended hardening (currently unreachable; owner-rulable)

The cross-model pass surfaced four guard weaknesses. I reproduced each and
confirmed **none is reachable in the running product** — every one is prevented by
a caller-side invariant rather than by the guard itself. They carry real
defense-in-depth value for a codebase whose callers may change, and this
codebase's "nothing deferred" standard favors making them robust-by-construction,
but they are not correctness blockers today. The owner may rule any of them
acceptable as safe-by-construction.

- **H1 — RetryPlanner is prefix-match, not whole-group-commit** (`persistence/io.rs`,
  Sol BLOCKER-1 / Seat B MINOR-2). After an interrupted batch leaves a durable
  prefix, `RetryPlanner` only rejects a divergence that is **already durable**; a
  divergent *not-yet-durable* suffix under a durable boundary would pass.
  **Unreachable:** boundary IDs are per-turn random UUIDs never re-submitted with
  a divergent suffix; a failed batch leaves an orphan that fails closed on reopen
  (`event_reader.rs:90`) and on the next append (validation rejects an orphan
  `ResponseStatePublication`); the network retry wraps the provider call, and the
  publication group is appended exactly once. Hardening: commit a canonical group
  length + digest in the publication boundary and require the whole requested
  group to match on retry.
- **H2 — legacy-eligibility closure is not monotonic**
  (`provider_state_validation.rs:74-82`, Sol MAJOR-2 / Seat A MINOR-1). `FilteredFork`
  closure is recognised only when the fork is the *immediate* pre-epoch cut, while
  a prior D3 publication is scanned over the whole prefix. **Unreachable:** the
  current binary never appends an unframed `AssistantMessage` with a non-empty
  `response_id` (`provider_call.rs:167-203` frames every such turn), and a later D3
  frame's own `ResponseStatePublication` boundary is itself an anchor cut that
  excludes any earlier assistant from the active slice (`None` disposition →
  skipped at `conversation_state.rs:141`). Hardening: track closure monotonically —
  scan the whole prefix for any `FilteredFork`, and clear legacy candidates on any
  framed D3 disposition including `NotStored`.
- **H3 — projection-hiding predicate is weaker than full validation**
  (`provider_state_validation.rs:291`, Sol MAJOR-3). `valid_target_shape`'s
  direct-target branch checks parent linkage but not `target_base.id == target`, so
  `response_publication_group_len` (used for prompt-projection hiding) can accept a
  frame that `validate_targets` later rejects. **Unreachable:** the runner always
  sets `provenance.target == assistant.id` and applications cannot forge a
  `ResponseStatePublication` boundary (every boundary construction is
  norn-internal; app surfaces emit only `Custom`). Hardening: add the
  `id == target` check for both target shapes, and — more durably — derive prompt
  invisibility from a single successful full-timeline validation pass rather than a
  weaker local re-implementation. (Seat A MINOR-2 is the same coupling seen from
  the anchor-walk side; at minimum, comment that boundary/suppress resets are
  enforced upstream by `active_start` slicing.)
- **H4 — lock invariant is over-stated** (`jsonl_sink/batch.rs:73`, Sol MAJOR-4 /
  Seat B MINOR-1). The registered batch releases the timeline+index lock at
  `drop(file)` before updating cadence and index counters, so the handoff's "one
  index-and-timeline lock through the append **and** counter update" is not literal.
  **Benign:** the index is a derived cache; the next writer recomputes counters from
  the durable timeline under the lock, and a flush failure is `tracing::error!`-logged
  with a documented reconciliation path, not swallowed. The *real* invariant —
  "counters are re-derived from disk under lock" — is stronger. Fix: reword the
  claim to match the code (or apply the exact counters under the held index
  authority before release if transactional index visibility is actually intended).

## Out-of-slice finding

- **Headless MINOR-1 (pre-existing).** `crates/norn-cli/src/print/orchestrator.rs:597-601`:
  on the non-driven `stream-json` path, a stream-renderer panic concurrent with a
  run error returns `renderer_failure` (exit 1) and discards the primary run
  error's exit class (e.g. an auth failure's exit 3) and diagnostic. Confirmed
  **untouched by both headless slices** (dates to `2955a27`) and off the driven
  path under review, so it does not affect the headless-reliability verdict. Worth
  a follow-up: route this branch through `preserve_run_failure` so the first causal
  run failure remains the exit authority, matching the correction's own principle
  one branch over.

## Verified sound (load-bearing, re-verified by me)

- **Provenance record:** `#[serde(deny_unknown_fields)]`, pinned version,
  canonical-UUID target; exact-family match; unrelated custom events ignored.
- **Frame validation:** every `ResponseStatePublication` boundary must form a
  complete `[boundary, provenance(parent=boundary), (audio-link), assistant]`
  group; duplicate/orphan/conflicting records and malformed payloads make the
  session fail closed on load and on append.
- **Projection hiding is gated on full-frame acceptance**, not the discriminator;
  unframed reserved-discriminator rows remain ordinary application data.
- **Exact-prefix persistence:** `RetryPlanner` matches the durable prefix by
  structural equality and rejects an already-durable divergence, an out-of-order id
  reuse, and a longer durable suffix; identical retry resumes, a lone orphan
  boundary fails closed; torn tail rejected by two independent layers.
- **Non-interleaving:** intra-process `sink.lock()` serializes single and batch
  appends; cross-process the advisory `index.lock` flock is held across the strict
  read, retry-prefix, provenance validation, writes and fsync; memory publishes
  only after sink durability.
- **Validation precedes mutation:** provenance validated before affinity adoption,
  prompt mutation, network dispatch, and child publication on every managed
  open/resume/latest/open-or-resume/fork path; fork seeding copies each complete
  frame with one `append_batch`.
- **Threading contracts stay distinct:** public = `store:true` + validated
  `previous_response_id` + post-anchor delta + resent instructions + server
  compaction; Codex = `store:false` + exact local replay incl. encrypted reasoning;
  delta computation neither drops nor duplicates; resume repair clears a stale
  anchor and fails typed pre-dispatch when replay material is missing; a stale
  public anchor fails typed after exactly one request with no weaker-history retry.
- **Forward/backward compatibility:** `ProviderEpochBoundaryReason` has no
  `#[serde(other)]`/untagged catch-all, so an older binary rejects the two new
  reasons (fail closed); unframed discriminator collisions never upgrade to
  provider state.
- **Structural splits:** `spawn`/`fork` production moves are token-identical;
  no production file crosses 500 lines; test rewrite is behavior-preserving.
- **My battery** (network-capable, repository target): `cargo fmt --all -- --check`
  clean; `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  clean; full workspace green — norn 4,192 / cli 518 / tui 683 — matching the
  handoff; 8/8 doctests.

## Adjudication note

The cross-model pass returned NOT READY with one blocker and four majors; on
reproduction, four are unreachable-by-construction guard weaknesses or a
claim-precision over-statement (real hardening value, no reachable defect), and one
(the bare `continue`-on-`Err`) is a genuine categorical-rule violation. Sol's
reachability model ran permissive — treating "the guard permits this input" as
"the runner produces this input" — the mirror image of a single-model panel running
too narrow. Reproducing each candidate against the caller-side invariants is what
separates a hardening item from a blocker, and is why the cross-model seat plus
coordinator reproduction remain jointly load-bearing on security-relevant slices.

## Boundaries

- This is a D3 candidate review, not D3 acceptance or whole-P5 acceptance.
- No authenticated live OpenAI/Codex wire test was run; request-shape conclusions
  are from production-path tracing and focused local tests. The public/Codex
  real-wire behavior remains a mandatory D7/P9 gate.
- D8 role authority, the broad volatile-context and concurrent-agent lifecycle
  matrices, WebSocket transport, and P2 acceptance remain open.
- Expected path: land R1 and R2 (plus any hardening the owner elects), then a
  narrow same-reviewer confirmation.
