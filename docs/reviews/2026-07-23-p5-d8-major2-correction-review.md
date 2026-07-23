# P5 D8 MAJOR-2 correction — Gate D confirmation

**Date:** 2026-07-23

**Reviewer:** Sable Nightwick (external Gate D coordinator)

**Handoff:** [`2026-07-23-p5-d8-major2-correction-handoff.md`](2026-07-23-p5-d8-major2-correction-handoff.md)

**Reviewed boundary:** `NOT READY` verdict / correction base `4f82e55`, frozen
correction source `7747dec` (tree `f9ff207d2c7ec7bd9958c480bc9977a497e74e97`),
range `eafa8db..7747dec` (14 Rust files, +530/−30). Branch head `753aa5b`
(`7747dec..HEAD` is documentation only). `main` untouched by this review (at
`c6cc081`).

**Confirmation shape:** the narrow same-reviewer correction requested by
[`2026-07-23-p5-d8-terminal-q-correction-review.md`](2026-07-23-p5-d8-terminal-q-correction-review.md).
My own deep read of the new authority/promotion/close-gate machinery, boundary
reproduction, a single-guard mutation of the all-record close gate, the full
strict/broad gates, and one cross-model adversarial concurrency pass (norn
GPT-5.6 Sol, xhigh; session `claude-review.6BOQdJ`) whose one comment I
reproduced by code-trace.

## Verdict

**MAJOR-2 is CLOSED; D8 is READY as an implementation candidate.**

The idle/live-requeue orphan I flagged in the prior review — a message accepted
for an idle persistent child, retained nondurable by
`persist_and_publish_locked` with `terminal_recovery=false` and no strong store
authority, then orphaned when the wrapper died before its terminal arm while
`close_agent`'s recovery gate (keyed only on the `terminal_recoveries` map)
reclaimed the entry and reported a successful forced close — is closed. Every
record that enters `PendingAgentMessages` now owns strong, generation-bound
store authority before it is published nondurable, and every built-in
reclamation seam discharges or refuses **all** nondurable records (not just
those already marked terminal) before `remove_terminal`.

This is a bounded implementation-candidate verdict, **not** whole-P5 acceptance:
the D7/P9 authenticated live-wire gate and whole-phase integration remain open,
as the handoff states.

## MAJOR-2 closure — verified (with evidence)

- **Strong authority attached before nondurable publication.** The Err/retain
  arm of `persist_and_publish_locked` requires `mailbox_id` (typed error if
  absent — no authority-less retain), attaches
  `PendingPersistenceAuthority { mailbox_id, Arc::clone(store) }` — a **strong**
  `Arc<EventStore>` — and only then calls `publish_pending(..., false)`
  (`pending_queue.rs:232-251`). The authority is cleared only when durability is
  proven (`ensure_recipient_prefix_durable_locked` after a successful idempotent
  append; `publish_pending` on `queue_durable=true`; `pending_queue.rs:311`,
  `pending_messages.rs:231-233`). Wrapper abort/panic/handle-loss can therefore
  no longer drop the only store authority for a retained record. The authority
  is non-`Serialize` with a `[REDACTED]`/`[REDACTED]` `Debug`.
- **Generation-bound promotion.** `promote_nondurable_for_terminal`
  (`pending_teardown.rs:47-113`) holds the recipient enqueue lock, validates a
  single `MailboxId` + `Arc::ptr_eq` store across the entire unresolved FIFO,
  rejects mixed authorities typed **before** mutation, and transfers ownership
  into the terminal-recovery surface via `adopt_closed_pending_locked` — which
  gained a guard rejecting any nondurable record whose authority ≠ the closing
  mailbox/store (or a `None`-authority non-terminal record).
- **All-record reclamation gate.** `recover_all_pending_before_reclamation`
  (`close/recovery.rs`) = promote-all → retry terminal Q → **independently
  assert** `nondurable_pending_status(id).is_none()` before permitting
  reclamation; a joined dead controller is marked `Failed` with its entry and
  authority preserved and the payload-free count error returned. `close.rs:277`
  routes a joined-with-handle controller (the MAJOR-2 site) through this gate;
  `reclaim_observed_terminal`, the `Reclaimed(tombstone)` path, and the
  resolved-tombstone path all use it. I enumerated every production
  `AgentRegistry::remove_terminal` caller and confirmed each is gated:
  forced-close is reachable only after the joined gate; spawn/fork headless
  reclaim runs behind the `persistence_failed` gate (now keyed on
  `nondurable_pending_status`); the TUI expiry probe uses `nondurable_pending_status`.
- **Subtree preflight is deliberately terminal-only** (`close.rs:439-446`) with
  a sound rationale — promoting a still-live controller's in-flight retained row
  would steal its queued work; promotion is correctly gated to
  post-join/observed-terminal. A live nondurable row can survive the preflight
  but cannot reach a removal path that skips the all-record gate (no-handle live
  descendants return `cancelling`/`unreachable` without removal; terminal/
  tombstoned branches route through the all-record gate).
- **No regression.** BLOCKER-1's no-await wake window is untouched
  (`delivery_pending.rs` unchanged since `de0b4d3`). MAJOR-1's terminal staging
  is intact: staged records carry `terminal_recovery=true` and no per-record
  authority, so the new `adopt` guard's `None => !terminal_recovery` branch
  admits them.
- **Mutation kill (coordinator).** Reverting the `close.rs:277` all-record
  wiring back to the terminal-only gate (`recover_terminal_pending_before_reclamation`
  for the had-handle branch) fails the public regression
  `idle_queue_failure_then_wrapper_abort_retains_exact_recovery` with *"close
  must refuse to reclaim unresolved accepted work"* — the exact MAJOR-2
  signature. This is distinct from the handoff's own mutation (the per-record
  authority attachment), so the two decisive links are independently bound.
  Restored byte-clean; worktree clean at `753aa5b`.

## Non-blocking comments

- **HARDENING (disclosed; required post-D8 follow-up) — channel-before-record
  wrapper-death window.** A message accepted into the inbound channel
  (`InboundSender::send → Ok`) but lost if the wrapper panics or is aborted
  **before** the controller drains it into a `PendingAgentMessage`. Confirmed:
  the idle transition calls `transition_live_route` with no terminal controller,
  so inbound admission stays open (`pending_transition.rs:35-40`,
  `spawn_controller.rs:174-183`), and a record is only constructed at
  `IdlePark`'s `recv → requeue_undelivered_inbound` (`spawn_controller.rs:402-421`).
  A public `SubagentHook` (no `catch_unwind` around invocation) that panics in
  that window, or an external `AgentHandle::join_handle`/`abort`, drops the
  buffered message; a later `CloseAgentTool` then reports `force_failed` success.
  This is a **real** silent loss of an acknowledged message — but it is a
  *distinct, broader, pre-existing* invariant (the in-memory channel→store
  handoff), explicitly scoped out by the handoff, occurring before the corrected
  seam, and **not** a MAJOR-2 relocation or regression (MAJOR-2 was specifically
  about a record already retained in `PendingAgentMessages`). It is the same
  class as the previously-disclosed live-route `delivered:true`-before-durability
  gap. It does not block the MAJOR-2 candidate, but Tom should track it as a
  named post-D8 hardening item: close it with an inbound-owning drop/abort-safe
  controller guard that drains and stages every accepted channel item with
  mailbox/store authority before lifecycle reclamation can succeed.
- **SHOULD-ADD (evidence gap).** The mixed mailbox/store rejection in
  `promote_nondurable_for_terminal`/`adopt_closed_pending_locked` has explicit
  production validation but no focused source test constructing two-generation
  authorities and asserting typed rejection before mutation. Add one so the
  generation-binding is bound by a test, not only by inspection.

## My gates (primary repository target `/Users/tom/Developer/ablative/norn/target`, at `753aa5b` = frozen Rust `7747dec`)

`cargo +1.94.0 fmt --all -- --check` clean; `cargo +1.94.0 clippy --locked
--workspace --all-targets --all-features -- -D warnings` **clean** (no
suppression); full `--workspace --all-targets --all-features` test **Norn
4367/4367, CLI 522/522, TUI 685/685** with every integration/trybuild/PTY/smoke
harness green; doctests **8/8**. Added-line policy scan over the 14 production
paths (excluding `*_tests.rs`/`_tests/`/`/tests/`): **zero** production
`unwrap`/`expect`/`panic!`/`unsafe`/lint-attribute. Boundary reproduced exactly
(source tree `f9ff207d…`, inventory NUL SHA `408be574…`). Retained runner and
distribution SHAs match the handoff; the checked-in `80/80` distributions were
inspected, not re-executed (a deliberate rigor/time tradeoff, stated plainly).

## Boundaries

- **MAJOR-2 CLOSED / D8 READY** is a bounded implementation-candidate verdict,
  mutation-verified, with no new loss/false-success/duplication hole found in
  the correction (my read + Sol's exhaustive `remove_terminal` enumeration).
- It is **not** whole-P5 acceptance. The D7/P9 authenticated live-wire gate,
  whole-phase integration, and owner acceptance remain open.
- The two non-blocking items above (channel-before-record hardening;
  mixed-authority test) should be tracked; neither blocks the candidate.
