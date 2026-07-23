# P5 D8 terminal-Q loss correction — Gate D confirmation

**Date:** 2026-07-23

**Reviewer:** Sable Nightwick (external Gate D coordinator)

**Handoff:** [`2026-07-23-p5-d8-terminal-q-correction-handoff.md`](2026-07-23-p5-d8-terminal-q-correction-handoff.md)

**Reviewed boundary:** review verdict/correction base `531200f`, frozen
correction source `eafa8db` (tree `1c67561e5602a278fdcbeb1d16f1811f965c656d`),
range `de0b4d3..eafa8db` (27 Rust files, +2,718/−139). Branch head `6586612`
(`eafa8db..HEAD` is documentation only). `main` untouched by this review (at
`c6cc081`).

**Confirmation shape:** the narrow same-reviewer correction requested by
[`2026-07-23-p5-d8-exact-once-correction-review.md`](2026-07-23-p5-d8-exact-once-correction-review.md).
My own deep read of the terminal/recovery machinery, boundary reproduction, a
single-guard mutation of the retain-before-I/O seam, the full strict/broad
gates, and one cross-model adversarial concurrency pass (norn GPT-5.6 Sol,
xhigh; session `claude-review.G7w0sD`) whose finding I reproduced by code-trace.

## Verdict

**MAJOR-1 is CLOSED.** The terminal drain-to-Q loss I flagged in the prior
review — `transition_live_route` / `persist_undelivered_after_close` removing an
accepted message from its channel/buffer via `drain()`/`mem::take` *before* Q
persistence, then dropping the payload log-only on a store I/O failure while the
terminal outcome could still report success — is structurally corrected on the
ordinary terminal path. Verified on my own read across all five stated
properties, **mutation-killed** (below), and independently confirmed by Sol.

**D8 is NOT READY as an implementation candidate**, on one new, confirmed
finding: **MAJOR-2 — an accepted message retained nondurable by the idle/live
requeue path, whose wrapper then dies before its terminal arm, is never adopted
into terminal recovery, and `close_agent` then reclaims it while reporting a
successful forced close.** This is a *distinct sibling* of MAJOR-1 on the same
durability invariant, not a MAJOR-1 reopening. It defeats the correction's own
stated property 4 ("No built-in close … path can reclaim the only recovery
authority") on the exact wrapper-death scenario the close path explicitly claims
to handle.

This is **not** whole-P5 acceptance; the D7/P9 authenticated live-wire gate and
whole-phase integration remain open, as the handoff states.

## MAJOR-1 — CLOSED (with evidence)

The corrected terminal protocol makes retention structural:

- **Retain before the first fallible store op.** The terminal drain now stages
  each record via `stage_terminal_locked` (`pending_queue.rs:109-125`), which
  `publish_pending(..., false)`s the exact `PendingAgentMessage` into the
  recipient FIFO with `terminal_recovery = true` *before* any Q write.
  `finalize_closed_pending` adopts the strong `TerminalPendingRecovery`
  (`MailboxId` + `Arc<EventStore>`) authority, *then* runs
  `ensure_recipient_prefix_durable_locked` (the Q I/O), *then* retires only
  records proven durable (`pending_teardown.rs:55-72`,
  `pending_queue.rs:298-316`). An I/O failure leaves records nondurable and
  retained, and the authority reachable via `retry_terminal_pending`.
- **Ambiguous write cannot duplicate.** `queue_durable` is set only after a
  successful byte-exact `append_idempotent` (`pending_queue.rs:261-295`);
  `delivery_is_durable` cross-checks both the U and Q reserved events
  (`pending_queue.rs:318-369`); the fail-after arm retains the exact cached Q for
  byte-exact retry (`pending_queue.rs:228-239`).
- **Terminal outcomes cannot lie.** Spawn and fork compute
  `persistence_failed = … transition_hard_failure || finalizer_failed ||
  terminal_recovery_pending` and call `downgrade_terminal_persistence()`
  (`spawn_outcome.rs:99-111`, `fork_outcome.rs:54-66`) — a fixed, payload-free
  `TERMINAL_PERSISTENCE_FAILURE` diagnostic — *before* `succeeded` is derived and
  before lifecycle/result/status emission; reclamation is gated on
  `!persistence_failed` (`spawn_controller.rs:230-295`, `fork_launch.rs:329-480`).
- **Recovery is loud and payload-free.** `close` reclamation is gated behind
  `recover_terminal_pending_before_reclamation` → `retry_terminal_pending`,
  returning a typed count-only `unresolved_terminal_pending_error`
  (`close/recovery.rs:14-79`); the TUI holds a read-only observation closure
  (`terminal_pending_recovery_status(id).is_some()`) that performs no store I/O
  (`status_line.rs:341-346`).
- **BLOCKER-1's wake window is intact.** `delivery_pending.rs` is unchanged in
  this range; the no-await window between the authoritative U append and the FIFO
  consumption remains.
- **Mutation kill (coordinator).** Neutering the retain in `stage_terminal_locked`
  (dropping the `publish_pending` so the drained record is not staged) makes
  `terminal_fail_before_second_q_preserves_exact_messages_and_fifo` fail with
  *"second queue write failure unexpectedly reported success"* — i.e. the exact
  MAJOR-1 signature (payload dropped, finalizer has nothing to persist, terminal
  falsely reports success). Restored byte-clean; worktree clean at `6586612`.

## MAJOR-2 — idle-requeue orphan reclaimed by close on wrapper death

**Found by Sol; I confirmed every link against source.** Reachable via public
surfaces (`AgentHandle.inbound_tx`, `AgentHandle.join_handle`/`JoinHandle::abort`,
`CloseAgentTool`), or a natural dependency/hook panic during idle park — the
precise "a dependency panicked inside the wrapper or something external killed
the task" case `close.rs:258-268` reasons about.

Chain (persistent spawn, Idle, persistent recipient-store outage — disk-full /
EMFILE / fsync / a sink that rejects both attempts):

1. A message sent to the idle child via `AgentHandle.inbound_tx` is accepted
   (`InboundSender::send → Ok`, sender acknowledged). `IdlePark` receives it and
   calls `requeue_undelivered_inbound` (`spawn_controller.rs:402-425`).
2. `requeue_undelivered_inbound` → `persist_for_registered_store` →
   `persist_and_publish_locked`, whose Err arm **retains** the exact record
   `publish_pending(..., false)` (`pending_queue.rs:228-239`), and `mem::take`
   has already emptied the caller buffer (`delivery.rs:341,363-395`). The record
   is created by `PendingAgentMessage::new` with `terminal_recovery = false`.
   `IdlePark` only logs the Err ("affected messages will not survive a restart")
   and keeps parking (`spawn_controller.rs:413-421`).
3. The wrapper dies **before** its idle-close terminal arm: `launch_child`
   spawns `tokio::spawn(controller.run())` with no drop/RAII finalizer
   (`spawn_launch.rs`), so an abort or an in-park panic interrupts `run()` at a
   `select!` await and neither `finalize_closed_pending` nor adoption into
   `terminal_recoveries` ever runs.
4. `CloseAgentTool` joins the wrapper, observes the `JoinError`, and reaches the
   forced-failure branch. Its recovery gate
   (`recover_terminal_pending_before_reclamation`, `close.rs:277`) checks **only**
   `terminal_pending_recovery_status` (the `terminal_recoveries` map), which is
   empty for this orphan → the gate is a no-op. Close does not itself drain the
   inbound or finalize the target (`shutdown_one` relies on the wrapper for
   that). It then `mark_failed` + `reg.remove_terminal(id)` and returns
   `"force_failed"` (`close.rs:328-339`).

**Outcome.** The accepted (acknowledged) message remains in the FIFO nondurable,
with no Q on disk, no `terminal_recoveries` authority, and no retry surface
(`retry_terminal_pending` returns `NoRecovery`). `MailboxRegistry` holds only
`Weak<EventStore>`/`Weak<PendingMailboxLease>`; `TerminalPendingRecovery` — the
sole strong store authority — was never created, so the store can drop. Session
resume cannot recover it (`from_events` sees no Q, because none was written).
The registry entry is reclaimed and close reports success. This is a real silent
loss of an acknowledged message, and it violates the correction's stated
property 4.

**Severity — MAJOR (candidate blocker).** It requires a compound fault (Q I/O
outage plus wrapper death during park) rather than firing on the normal path,
which is why it is MAJOR rather than BLOCKER. But it is a genuine silent loss on
the message-durability path — the exact class the correction exists to
eliminate — reachable through public surfaces, on the scenario the close path
advertises handling; under the project's "no silent failures" rule it must close
before D8 ships.

**Scope honesty.** The underlying reclaim-on-idle-orphan predates this
correction (before it, `close` reclaimed with no recovery gate at all). But this
correction introduced the `recover_terminal_pending_before_reclamation` gate and
property 4 *precisely* to forbid "reclamation while an accepted message's Q is
unresolved," and implemented it too narrowly — keyed on `terminal_recoveries`
rather than "any retained nondurable record for the recipient." The invariant
the correction claims is therefore not established, which is why this blocks the
candidate rather than being a mere disclosure.

**Fix direction.** Two complementary closures, either of which removes the
loss, both of which are cheap relative to this correction's size:

1. The close forced-failure branch (and the idle/no-handle gates) must refuse
   `remove_terminal` whenever **any** nondurable record exists for the recipient
   — not only those already in `terminal_recoveries`. Adopt-and-retry those
   records first (install the authority from the still-registered mailbox
   store), or preserve the registry entry and return the same payload-free count
   error.
2. Give the controller a drop/panic/abort-safe teardown authority (RAII) that
   owns `mailbox_id`, `Arc<EventStore>`, the lease, and inbound ownership, and on
   drop closes admission, stages any buffered accepted records, and adopts all
   matching nondurable FIFO records into `terminal_recovery` before those
   authorities disappear.

Add a public-path regression: persistent spawn → Idle → accepted direct inbound
→ fail Q → abort/panic wrapper → `CloseAgentTool`, asserting no
tombstone/reclamation, one retained exact record, a strong recovery authority,
and exact retry without duplication.

## My gates (primary repository target `/Users/tom/Developer/ablative/norn/target`, at `6586612` = frozen Rust `eafa8db`)

`cargo +1.94.0 fmt --all -- --check` clean; `cargo +1.94.0 clippy --locked
… -D warnings` **clean** on `norn` and `norn-tui` (no suppression); full
`--workspace --all-targets --all-features` test **Norn 4366/4366, CLI 522/522,
TUI 685/685** with every integration/trybuild/smoke harness green; doctests
**8/8**. Added-line policy scan over the 27 production paths (excluding
`*_tests.rs` / `_tests/` / `/tests/`): **zero** production
`unwrap`/`expect`/`panic!`/`unsafe`/lint-attribute. Boundary reproduced exactly
(source tree `1c67561e…`, inventory NUL SHA `f1909134…`). Retained runner and
distribution SHAs match the handoff; the checked-in distributions are `180/180`
(inspected, not re-executed — a deliberate rigor/time tradeoff, stated plainly).
The retained suite does **not** cover the wrapper-death-before-terminal-adoption
scenario of MAJOR-2 (a correction needs a new deterministic regression).

## Boundaries

- **MAJOR-1 CLOSED** is definitive and mutation-verified. **NOT READY** turns
  solely on the new, confirmed MAJOR-2. MAJOR-2 is a distinct sibling path, not
  a MAJOR-1 reopening — MAJOR-1's terminal drain is correct.
- This is a bounded correction-confirmation verdict, not whole-P5 acceptance.
  The D7/P9 authenticated live-wire gate and whole-phase integration remain open.
- Fix MAJOR-2, then a same-reviewer re-confirmation (drive the persistent-spawn
  → idle → accepted inbound → Q-fault → wrapper-death → close path and assert no
  reclamation while a nondurable record survives, with exact retry and no
  duplication) closes D8 as an implementation candidate.
