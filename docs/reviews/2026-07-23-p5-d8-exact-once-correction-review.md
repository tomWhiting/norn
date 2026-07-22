# P5 D8 pending-message exact-once correction — Gate D confirmation

**Date:** 2026-07-23

**Reviewer:** Sable Nightwick (external Gate D coordinator)

**Handoff:** [`2026-07-23-p5-d8-exact-once-correction-handoff.md`](2026-07-23-p5-d8-exact-once-correction-handoff.md)

**Reviewed boundary:** base `6d168830ee3c4edad5893d39a0e1e67950da98ad`, frozen
source `de0b4d3` (range `6d16883..de0b4d3`, 69 Rust files, ~7,958 insertions).
Branch head `2612609` (checked out in the `p5-d8-exact-once` worktree; `de0b4d3..HEAD`
is documentation only). `main` untouched by this review (at `c6cc081`).

**Panel:** three Opus 4.8 seats (core flush / exact-once append; terminal
teardown / admission linearization; mailbox identity / pre-D8 fail-closed /
installer), one cross-model adversarial seat (norn GPT-5.6 Sol, review/
concurrency, xhigh; session `claude-review.gfzLfV`), plus my own gates, boundary
reproduction, and a single-guard mutation of the exact-once seam.

## Verdict

**BLOCKER-1 is CLOSED.** The pending-store wake path that the prior D8 role-
authority review reproduced — a durable `UserMessage` append, then an awaited
hook, then FIFO removal only after the whole batch, so a cancelled step future /
crash / audit failure left the same accepted message both durable in the
transcript and live in the retry store and a later flush re-injected it — is
structurally eliminated. Verified by all three Opus seats, corroborated by Sol,
confirmed by my own read of the exact flush code, and **mutation-killed** (below).

**D8 is NOT READY as an implementation candidate**, on one new, distinct
finding: **MAJOR-1 — terminal drain-to-Q silently drops an already-accepted,
resume-destined message on a Q I/O failure.** This is not a reopening of
BLOCKER-1 (it is the opposite failure mode — silent *loss*, not *duplication*),
and it does not touch the normal wake path. It is a swallowed-`Result`/log-and-
continue hole on the terminal teardown path introduced by this correction, and
it falsifies the handoff's own step-5 guarantee ("persist every already accepted
message exactly once to Q"). Under CLAUDE.md's "no silent failures" law and the
patient-records standard, it must close before D8 ships.

This is **not** whole-P5 acceptance: the D7/P9 authenticated Responses live-wire
gate and the whole-phase integration remain open, as the handoff states.

## BLOCKER-1 — CLOSED (with evidence)

The corrected protocol makes exact-once structural, not incidental:

- **No await between the authoritative append and queue consumption.** In
  `flush_pending_agent_messages` (`crates/norn/src/loop/delivery_pending.rs:69-73`),
  `append_idempotent_off_executor(store, prepared.delivery_event.clone())?` (the
  stable-id `UserMessage` U append) is immediately followed — with no
  intervening `.await` — by `pending.commit_delivery(...)` (the FIFO-head
  removal). The first suspension point in the loop body is the hook await at
  line 103, strictly *after* consumption. A cancelled/dropped step future can
  therefore omit only the secondary observations (dequeue/delivered audits,
  hooks, broadcasts); it can never split the append from the consumption.
- **U is the sole authoritative consumption record.** The delivery row is a
  deterministic reserved-id `UserMessage`
  (`norn:pending-agent-message:delivered:{uuid}`), byte-identical on retry via
  `append_idempotent`'s `serde_json::to_vec` equality check (a structurally
  different value under the same reserved id fails typed; an exact retry is a
  no-op returning the existing id). Replay via
  `PendingAgentMessages::from_events(recipient, mailbox_id, events)` treats an
  exact U as durable consumption and removes the queue entry even when the
  process died before either secondary audit was written.
- **MailboxId binds Q to a persistent session generation, not a volatile agent
  UUID.** `MailboxId::from_generation` keeps a resumed runtime on the same
  mailbox across a new agent UUID, while a replacement generation / new
  ephemeral root / child gets a distinct MailboxId — so a foreign timeline's Q
  replays as `ForeignCanonical` (witness-only, never delivered), closing the
  wrong-mailbox re-consumption vector.
- **Mutation kill (coordinator).** Reintroducing BLOCKER-1 by deferring
  `pending.commit_delivery(...)` to *after* the hook await (a faithful re-creation
  of the original append-then-await-then-consume seam) fails exactly
  `cancellation_after_authoritative_append_cannot_redeliver_pending_message`
  at `crates/norn/src/loop/delivery_pending_tests.rs:131`
  (`authoritative append consumes the queue`) — the guard test precisely
  detects that the queue is *not* consumed before the await. Restored byte-clean;
  worktree clean at `2612609`.

Sol independently confirmed the duplication seam is closed and did not contest
BLOCKER-1; its NOT-READY (below) is a different failure mode that presupposes
BLOCKER-1 is fixed.

## MAJOR-1 — terminal drain-to-Q silently drops an accepted message on Q I/O failure

**Found by Sol; reproduced by me via end-to-end code trace.** New code in this
range; in scope for the exact-once contract.

At terminal teardown, already-accepted inbound work is meant to be persisted to
Q on the closing session's timeline for a future direct resume (handoff step-5).
Two sites implement this, and both **consume the payload out of its holding
structure and then drop it on a persist error**:

1. `PendingMailboxes::transition_live_route`
   (`crates/norn/src/agent/pending_transition.rs:68-89`): `for mut message in
   inbound.drain()` empties the channel; each message is persisted via
   `persist_closed_locked` → `append_idempotent_off_executor` (the *only*
   durability for this drained work). On error, `first_error = Some(error)` is
   captured **and the loop continues** — the drained payload is already gone from
   the channel, is not persisted to Q, and is not returned. `first_error` is
   returned to the caller, but the message *content* is unrecoverable.
2. `persist_undelivered_after_close`
   (`crates/norn/src/loop/delivery_closed.rs:21-48`): `std::mem::take(messages)`
   empties the buffer; on `persist_after_close` error it emits `tracing::error!`,
   accumulates `first_error`, and does **not** restore the failed record to
   `messages`. Same outcome — the accepted message is consumed and dropped.

Every caller then treats `first_error` as **log-and-continue**, so nothing
higher up preserves or retries the lost payload:
`spawn_controller.rs:180-186` (live-route transition), `spawn_controller.rs:289-295`
(deregistration drain), and the twin sites at `fork_launch.rs:287` and
`fork_launch.rs:440`.

**Reachability.** A single Q append failing under degraded IO — disk full, an
`EMFILE`/NOFILE ceiling, or an fsync error on the recipient's `EventStore` —
during a terminal transition (non-persistent child completion, cancellation, or
`Failed`-with-no-stop) drops an accepted, resume-destined message. The sender
already observed `delivered: true` on the live route (see disclosure below), the
recipient terminated before consuming it, and the resume path (Q) never receives
it: the message is lost end-to-end with only an error log. This is the exact
"swallowed `Result` / silent failure" pattern CLAUDE.md forbids, and it
falsifies step-5's "exactly once to Q" (it becomes *zero* times under fault).

**Severity — MAJOR, not BLOCKER, but a candidate blocker.** It is strictly
better than the pre-D8 baseline (which had no drain-to-Q net at all) and it
requires an I/O fault to trigger rather than firing on the ubiquitous normal
path the way BLOCKER-1 did. But the correction *explicitly promises* exactly-once
persistence to Q for accepted work, and this silently breaks that promise while
dropping patient-record-class message content. It is the recurring
failure-injection blind spot (cf. AFFINITY-01, P5 CODEX-02): the Opus teardown
seat verified the linearization assuming the append *succeeds*; Sol attacked the
`Err` arm. Per the project's own "no silent failures" rule, this closes before
D8 is READY.

**Fix direction.** On a persist failure during terminal drain, the accepted
message must not be dropped: retain the failed record under the closing
controller's owned authority and retry/surface it, or fail the transition
loudly to a caller that preserves the payload — never `drain()`/`mem::take`
then log-and-continue. Ideally unify this path with the exact-once idempotent-Q
discipline the flush path already uses, so a retry re-appends the same reserved Q
identity rather than losing or duplicating it.

## Disclosure (pre-existing; not a blocker for this correction)

**Live-route `delivered: true` precedes recipient durability**
(`crates/norn/src/tools/agent/coord/signal_agent.rs`, lines ~515/540/556). On
the direct live route, `router.deliver(...).await → Ok(seq)` returns
`{"delivered": true}` and writes the Sent audit to the *sender's* store before
the recipient has durably recorded the message; a recipient crash in that window
loses the message on the live path. **This is byte-identical at base `6d16883`**
— it predates this correction and lies outside the pending-store exact-once
scope. I disclose it because it compounds MAJOR-1 (a message the sender believes
delivered can be lost when the recipient terminates and the drain-to-Q net then
fails), and because the "delivered" contract on the live route deserves a
separate hardening pass. Not a blocker for D8 exact-once.

## HARDENING (identity seat)

Some pending-mailbox coordination fields are published on `LoopContext` before
the installer's validation completes in a way that is currently safe only
because every `run_agent_step*` re-validates pending-mailbox wiring before
prompt persistence / provider dispatch (verified: a half-wired context fails
typed with an unchanged store and zero provider calls). Tightening the installer
to publish coordination state only after validation would remove the reliance on
that downstream re-check. Non-blocking.

## Verified SOUND (with evidence)

- **Idempotent append equality.** `append_idempotent` compares the existing
  reserved-id event to the candidate by `serde_json::to_vec` bytes: an exact
  retry returns the existing id (no-op), a structurally different value under
  the same reserved identity (forged/mismatched U or Q) fails typed. Reserved-id
  parsing requires a canonical lowercase-hyphenated UUID, rejecting alias/case
  forgeries.
- **Replay is loss-free and dup-free.** `from_events` cross-checks reserved-id
  shape, treats `row_mailbox == mailbox_id` as `LocalCanonical` and any other as
  `ForeignCanonical` (witness-only), and an exact U as consumed. The crash
  matrix (crash after U before either secondary audit; crash after append before
  consumption is impossible by the no-await window) resolves to exactly one
  consumption.
- **Pre-D8 fail-closed.** An unresolved authority-less legacy Q fails typed
  `PreD8PendingMessageOwnershipUnknown` (operator action) rather than being
  speculatively adopted; a completed exact legacy Q + `UserMessage` + audit
  triple stays valid.
- **Admission linearization at terminal.** `transition_live_route` takes the
  per-recipient enqueue lock, closes inbound admission before draining, closes
  the mailbox, and deregisters the route; a capacity permit reserved before
  close is revocable and its payload is returned rather than landing behind the
  terminal boundary (aside from the MAJOR-1 I/O-failure arm).
- **Public installer / step gating.** `install_pending_mailbox` is the supported
  wiring path; every `run_agent_step*` validates pending-mailbox wiring before
  prompt persistence, provider dispatch, or any side effect.
- **Concurrency guard.** `try_delivery_flush` is a per-agent exclusive guard;
  concurrent steps for one agent are rejected typed.

## My gates (repository shared target, at `2612609` = frozen Rust `de0b4d3`)

`cargo fmt --all -- --check` clean; `cargo clippy --locked --workspace
--all-targets --all-features -- -D warnings` finished clean (no warnings, no
suppression); **full workspace `--workspace --all-targets --all-features`:
5,721 passed / 0 failed**; doctests **8/8**. Added-line scan: zero production
`unwrap`/`expect`/`panic!`/`unsafe`/`allow` in this range (the three `unsafe`
uses are Rust-2024 `env::set_var` inside `process_delivery_tests/support.rs`).
Boundary reproduced independently (inventory NUL SHA `a09cc19b`, 69 paths).
Single-guard mutation of the exact-once seam kills precisely (above).

**Concurrency distributions.** I relied on the handoff's honestly-disclosed five
20/20 concurrency distributions together with my full-workspace green run,
clippy, doctests, and the seam mutation; I did not independently re-run all five
20-run distributions (a deliberate rigor/time tradeoff stated plainly, not a
claimed independent sweep).

## Boundaries

- **BLOCKER-1 CLOSED** is a definitive finding on the specific defect this
  correction targeted, mutation-verified. **NOT READY** turns solely on the new
  MAJOR-1 (terminal drain-to-Q silent loss under I/O fault). MAJOR-1 is a
  distinct finding, not a BLOCKER-1 reopening — it is the opposite failure mode
  and does not touch the normal wake path.
- This is a bounded correction-confirmation verdict, **not** whole-P5
  acceptance. The D7/P9 authenticated Responses live-wire gate and whole-phase
  integration remain open, as the handoff states.
- Fix MAJOR-1, then a same-reviewer re-confirmation (re-run the terminal-drain
  I/O-fault path with a persist failure injected, asserting the accepted message
  is retained or loudly surfaced — never dropped) closes D8 as an implementation
  candidate.
