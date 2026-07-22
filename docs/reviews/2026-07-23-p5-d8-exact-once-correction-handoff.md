# P5 D8 pending-message exact-once correction handoff

Date: 2026-07-23

Candidate branch: `codex/p5-d8-exact-once`

Requested narrow verdict:
`BLOCKER-1 CLOSED; D8 READY as an implementation candidate`

D8 acceptance, whole-P5 acceptance, D7/P9 real-wire conformance, and the
separate headless-process investigation are explicitly out of scope.

## Frozen review boundary

| Boundary | Commit / tree |
| --- | --- |
| Original D8 Gate D verdict and correction base | `6d168830ee3c4edad5893d39a0e1e67950da98ad` |
| Frozen correction source | `de0b4d3287b0d5e11e21d6259cb5d8058f736260` |
| Frozen correction source tree | `82e4eecbe60a31af9d22862dabf8738f90b32aaf` |
| Source inventory | 69 paths, NUL SHA-256 `a09cc19ba30e592b312e9161af0a979d2914f02d20ee8bec2dcd8b44f2a68a10` |

The handoff and evidence commit is documentation-only over the frozen source.
The full ordered path inventory is reproduced below and in the source-bound
evidence record.

## Why this correction exists

The external D8 review at `6d16883` reproduced one blocking exact-once defect.
The old pending flush durably appended a `UserMessage`, then awaited a hook,
and only removed the authoritative pending entry after the whole batch. A
cancelled future, process crash, or later audit failure could therefore leave
the same accepted message both durable in the transcript and live in the retry
store. A later flush appended it again.

This correction closes that exact seam and the adjacent acceptance/terminal
seams needed to make the claim true end to end. It does not weaken the D8
authority, prompt, role-lowering, cache, or setup-timeout work already rated
sound by the original review.

## Corrected durable protocol

Each pending message has one random message UUID and two deterministic reserved
event identities:

- Q: `norn:pending-agent-message:queued:{message_uuid}`
- U: `norn:pending-agent-message:delivered:{message_uuid}`

A durable `MailboxId` binds Q to one persistent session generation. A resumed
runtime may have a new agent UUID but retains the same mailbox identity; a new
ephemeral root, replacement generation, or child receives a different one.
Aliases and runtime recipient IDs therefore cannot claim another timeline's
pending work.

The exact ordering is:

1. A fallback send is not reported accepted until its exact authoritative Q is
   idempotently durable in the recipient's exact `EventStore`. In-memory
   publication happens after that append. An ambiguous Q write retains the
   cached exact event and must reconcile it before later FIFO work; callers are
   told not to resend.
2. A pending flush first proves the FIFO head's Q durable, prepares one stable
   U containing the exact harness-framed user content, and performs an
   equality-checked idempotent append.
3. With no intervening await, the flush removes that exact FIFO head and adds
   the message to the current request. Only then may hooks, broadcasts, and the
   secondary `agent_message.dequeued` / `agent_message.delivered` observations
   run.
4. Replay treats the exact namespaced U as the authoritative consumption
   record. A crash after U but before either secondary audit therefore consumes
   Q once; an exact U replay is a no-op, and a conflicting row under either
   reserved identity fails typed.
5. Terminal teardown closes inbound admission, closes the mailbox, deregisters
   the route, and drains already accepted channel work to Q under the same
   recipient ordering gate. A capacity permit reserved before close is
   revocable: publication after the final drain fails and returns the unsent
   message instead of silently landing behind the terminal boundary.

This intentionally differs from the review's suggested `UserMessage +
dequeued` batch. U itself is the sole authoritative delivery record, while both
audits are explicitly secondary. The crash proof no longer depends on an audit
being present.

## Additional closure required by the end-to-end claim

### Stable acceptance and shutdown

`InboundPermit::send` is now fallible and checks a shared admission gate.
`MessageRouter` commits a recipient sequence only after permit publication
succeeds. Spawn and fork completion wrappers revoke outstanding permits before
their final drain, close the mailbox, remove the route, and persist every
already accepted message exactly once. Tests exercise send-first, close-first,
and contested outcomes through the real wrappers.

### Strict pre-D8 disposition

Old Q rows did not record durable mailbox authority and were copied into more
than one timeline. The correction does not guess ownership from a volatile
runtime agent UUID. A completed exact legacy Q + `UserMessage` + audit remains
valid, but any unresolved authority-less Q fails with the fieldless typed error
`PreD8PendingMessageOwnershipUnknown` and requires operator action. No automatic
adoption or speculative migration is claimed.

### Public embedder path

The full workspace suite exposed a real public-API gap: an embedder could wire
an agent ID and pending store manually without registering the matching exact
mailbox/store. `LoopContext::install_pending_mailbox(agent_id, binding, store)`
is now the supported public construction path. It rebuilds from the exact
store, registers the binding's `MailboxId`, retains the controller lease, and
publishes the context fields only after all validation succeeds.

Every public `run_agent_step*` entry validates pending-mailbox wiring before
prompt persistence, provider dispatch, or any other step side effect. A
half-wired context fails typed with an unchanged store and zero provider calls.

## Intentional public API changes

Downstream embedders, including Meridian, need a source migration review:

- `PendingAgentMessages::from_events` now takes
  `(recipient_id, mailbox_id, events)` and returns `Result<Self, SessionError>`.
- Direct public queue/drain/dequeue/remove methods were removed. Pending content
  can leave the store only through the authority-bound transition.
- `PendingAgentMessage` internals used to establish mailbox and durable-attempt
  authority are private; external struct-literal construction is no longer a
  supported path.
- `PendingAgentMessageLifecycle::Queued` gained exact timestamp, authoritative
  observation, and mailbox-identity fields. Exhaustive downstream matches must
  handle the new shape.
- `InboundSender::reserve` returns Norn's revocable `InboundPermit`.
  `InboundPermit::send` returns `Result<(), InboundPermitSendError>`; the error
  has redacted `Debug` and returns the unsent message through `into_inner`.
- Public direct-step embedders must call
  `LoopContext::install_pending_mailbox` rather than assigning coordination
  fields independently.

These are intentional authority hardening changes, not compatibility shims.

## Verification

Retained summary:
[`2026-07-23-p5-d8-exact-once-correction.json`](evidence/p5-d8/2026-07-23-p5-d8-exact-once-correction.json),
SHA-256 `85d063df705d86e7166996d7a54053a8c06165d3026803d2111c6fef31d7c0b7`.

All Cargo commands used
`CARGO_TARGET_DIR=/Users/tom/Developer/ablative/norn/target`; no temporary
build target was used.

- `cargo +1.94.0 fmt --all -- --check`: pass.
- `git diff --check`: pass.
- strict package Clippy, all targets/features, `-D warnings`: pass.
- strict workspace Clippy, all targets/features, `-D warnings`: pass.
- full workspace all-feature test command: exit 0, including Norn library
  `4345/4345`, CLI `522/522`, TUI `683/683`, with every printed integration and
  doctest summary green.
- Focused corrected runner groups: inbound sweep `5/5`, update requeue `4/4`,
  public pending-mailbox API `2/2`.
- Other focused suites: pending replay `16/16`, pending messages `10/10`,
  pending delivery `5/5`, signal agent `22/22`, process delivery `11/11`,
  schedule executor `16/16`, pending transition `3/3`, terminal wrappers `2/2`,
  permit close/drop `1/1`.
- Five concurrency/cancellation cases passed `20/20` each: terminal send race,
  spawn reserved-permit teardown, fork reserved-permit teardown, indeterminate
  Q reconciliation, and cancellation after authoritative U append.

The five distributions used exact filters and one test thread. Their assertions
were inspected for non-vacuity, but no runner or per-iteration raw outputs were
retained. The JSON says that explicitly and does not invent hashes, timings, or
provenance. The reviewer should reproduce the distributions rather than treat
the summary as self-attesting evidence.

## Policy and size audit

- Zero production lint suppressions were added; strict Clippy passed without a
  bypass.
- No `include!` split was introduced.
- Every new/refactored production module and new split test module is below 500
  lines.
- Seven touched legacy files remain above 500 physical lines because of
  existing inline test bodies: `message_router.rs` (697),
  `schedule/executor.rs` (1431), `session/branch.rs` (1104),
  `session/events.rs` (1022), `coord/wake.rs` (516), `fork_outcome.rs` (636),
  and `tools/agents_messages.rs` (834). Their production portions remain below
  500; the exact qualifications are in the JSON.

## Honest boundaries and residuals

- Unresolved pre-D8 pending queues are deliberately not auto-migrated. Resume
  fails typed until operator action. A richer binding-aware migrator remains
  future work.
- Exact-once covers the coordinated pending-store path with a canonical mailbox
  and exact `EventStore`. Coordination-less runners have no durable pending
  queue, as already documented.
- An embedder calling its own `PersistenceSink` directly remains outside
  `EventStore` guarantees.
- Secondary dequeue/delivery audit failure is logged but does not invalidate an
  already durable authoritative U. This is deliberate observability behavior,
  not a second consumption authority.
- The evidence record summarizes observed distributions but is not a retained
  execution runner. Independent reproduction remains necessary.
- A pre-existing ignored worktree-local `target/` directory was not used for
  these builds and is not staged. Worktree retirement can remove it after
  review; this correction performed no destructive cleanup.

## Requested reviewer work

Please perform a narrow same-reviewer correction confirmation, with adversarial
attention to the adjacent seams the complete fix necessarily touched:

1. Re-run the original drop and crash/reconstruction reproductions and confirm
   one accepted pending message produces exactly one durable U.
2. Attack Q/U equality with duplicate, conflicting, reordered, and
   wrong-mailbox rows; confirm exact replay is idempotent and conflicts fail
   typed before delivery.
3. Hold a reserved permit across spawn and fork terminal close; confirm it
   cannot publish after the final drain and its unsent payload remains
   recoverable without `Debug` disclosure.
4. Confirm unresolved pre-D8 Q fails with the fieldless operator-action error,
   while completed exact legacy history remains readable.
5. Exercise the public installer and a half-wired direct-step context; confirm
   exact-store registration and failure before prompt/provider/store side
   effects.
6. Reproduce the frozen base/source/tree, 69-path NUL inventory, strict gates,
   and all five 20-run distributions.

Requested verdict: `READY` or `NOT READY` for the D8 exact-once correction as an
implementation candidate. Do not issue whole-P5 acceptance from this handoff.

## Full source inventory

```text
crates/norn/src/agent/arming.rs
crates/norn/src/agent/assembly/runtime/infra.rs
crates/norn/src/agent/assembly/runtime/tests.rs
crates/norn/src/agent/assembly/tooling.rs
crates/norn/src/agent/build_support.rs
crates/norn/src/agent/builder/build.rs
crates/norn/src/agent/message_router.rs
crates/norn/src/agent/mod.rs
crates/norn/src/agent/pending_delivery.rs
crates/norn/src/agent/pending_mailbox.rs
crates/norn/src/agent/pending_messages.rs
crates/norn/src/agent/pending_messages_tests.rs
crates/norn/src/agent/pending_queue.rs
crates/norn/src/agent/pending_record.rs
crates/norn/src/agent/pending_replay.rs
crates/norn/src/agent/pending_replay_tests.rs
crates/norn/src/agent/pending_transition.rs
crates/norn/src/agent/pending_transition_tests.rs
crates/norn/src/agent/process_delivery.rs
crates/norn/src/agent/process_delivery_tests/completion.rs
crates/norn/src/agent/process_delivery_tests/mod.rs
crates/norn/src/agent/process_delivery_tests/support.rs
crates/norn/src/agent/process_delivery_tests/watch.rs
crates/norn/src/error/subsystems.rs
crates/norn/src/loop/delivery.rs
crates/norn/src/loop/delivery_closed.rs
crates/norn/src/loop/delivery_pending.rs
crates/norn/src/loop/delivery_pending_tests.rs
crates/norn/src/loop/inbound.rs
crates/norn/src/loop/inbound_tests.rs
crates/norn/src/loop/loop_context.rs
crates/norn/src/loop/mod.rs
crates/norn/src/loop/runner/entry.rs
crates/norn/src/loop/runner/tests.rs
crates/norn/src/loop/runner/tests/inbound_sweep.rs
crates/norn/src/loop/runner/tests/pending_inbound.rs
crates/norn/src/loop/runner/tests/pending_mailbox_api.rs
crates/norn/src/loop/runner/tests/update_requeue.rs
crates/norn/src/schedule/executor.rs
crates/norn/src/session/branch.rs
crates/norn/src/session/branch_child.rs
crates/norn/src/session/branch_materialize.rs
crates/norn/src/session/events.rs
crates/norn/src/session/mod.rs
crates/norn/src/session/store.rs
crates/norn/src/session/store/idempotent_append.rs
crates/norn/src/tools/agent/coord/mod.rs
crates/norn/src/tools/agent/coord/signal_agent.rs
crates/norn/src/tools/agent/coord/signal_agent_tests/durable_queue_races.rs
crates/norn/src/tools/agent/coord/signal_agent_tests/mod.rs
crates/norn/src/tools/agent/coord/signal_agent_tests/routing_audit.rs
crates/norn/src/tools/agent/coord/signal_agent_tests/scope_terminal.rs
crates/norn/src/tools/agent/coord/signal_agent_tests/test_support.rs
crates/norn/src/tools/agent/coord/signal_queue.rs
crates/norn/src/tools/agent/coord/signal_recipient.rs
crates/norn/src/tools/agent/coord/wake.rs
crates/norn/src/tools/agent/fork_launch.rs
crates/norn/src/tools/agent/fork_outcome.rs
crates/norn/src/tools/agent/fork_tool.rs
crates/norn/src/tools/agent/fork_tool/tests/mod.rs
crates/norn/src/tools/agent/fork_tool/tests/terminal_mailbox.rs
crates/norn/src/tools/agent/mod.rs
crates/norn/src/tools/agent/spawn/execute.rs
crates/norn/src/tools/agent/spawn/tests/mod.rs
crates/norn/src/tools/agent/spawn/tests/terminal_mailbox.rs
crates/norn/src/tools/agent/spawn_completion.rs
crates/norn/src/tools/agent/spawn_controller.rs
crates/norn/src/tools/agent/spawn_launch.rs
crates/norn/src/tools/agents_messages.rs
```
