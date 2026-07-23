# P5 D8 MAJOR-2 correction handoff

Date: 2026-07-23

Candidate branch: `codex/p5-d8-teardown-q-correction`

Requested narrow verdict:
`MAJOR-2 CLOSED; D8 READY as an implementation candidate`

This is the same-reviewer correction requested by
[`2026-07-23-p5-d8-terminal-q-correction-review.md`](2026-07-23-p5-d8-terminal-q-correction-review.md).
It does not request owner acceptance, whole-P5 acceptance, or D7/P9
authenticated live-wire acceptance.

## Frozen boundary

| Boundary | Commit / tree |
| --- | --- |
| `NOT READY` verdict and correction base | `4f82e55972ad2c643689c60396ae56c71c7ded69` |
| Frozen correction source | `7747decb6486d447d2c55aa1825352411cc07490` |
| Frozen correction source tree | `f9ff207d2c7ec7bd9958c480bc9977a497e74e97` |
| Rust inventory | 14 paths, bytewise-sorted NUL SHA-256 `408be5741e986346f00fe773a0a7bc5c6b29f560960e2a52817854b2e9d37aec` |

The source commit is pushed. The runner, retained evidence, this handoff, and
the plan reconciliation are documentation-only over that frozen source. Main
remains untouched.

## Finding being corrected

The prior terminal-Q correction made terminal drain retention structural, but
an idle live controller could still take a sibling loss path:

1. `AgentHandle::inbound_tx.send` accepted a message for an idle persistent
   child.
2. The idle requeue attempted Q persistence. On failure it retained the exact
   record in `PendingAgentMessages`, but the record was not terminal and held
   no strong store authority.
3. If the wrapper was then aborted or panicked before its terminal arm, no
   closed-mailbox finalizer installed `TerminalPendingRecovery`.
4. `CloseAgentTool` checked only the terminal-recovery map, reclaimed the
   failed child, and returned a successful forced close. The accepted message
   had no durable Q and no surviving retry authority.

This was MAJOR-2 in the review. MAJOR-1 remains closed and is not being
redesigned here.

## Corrected protocol

### Exact authority is retained at the common Q failure seam

Every live-mailbox Q persistence failure now attaches a private
`PendingPersistenceAuthority` to the exact pending record before publishing it
as nondurable. The authority contains the immutable `MailboxId` and a strong
`Arc<EventStore>` for that mailbox generation.

This occurs inside `persist_and_publish_locked`, the common failure seam for
the idle requeue and other registered-store paths. Consequently, wrapper
abort, panic, or handle loss cannot drop the only store authority after the
record has entered the retained FIFO. Successful durability clears the
per-record authority.

The authority is not serializable. Its manual `Debug` reports both fields as
`[REDACTED]`.

### Promotion is generation-bound and atomic

`promote_nondurable_for_terminal` takes the existing per-recipient enqueue
lock, validates that every unresolved record belongs to the same mailbox
generation and exact store, and transfers that authority into the existing
terminal-recovery surface.

The transfer uses the established `adopt_closed_pending_locked` path. It
installs the strong terminal authority before clearing per-record authorities,
then retries the unchanged cached Q identities in FIFO order. Mixed mailbox or
store authority fails typed before mutation.

### Reclamation checks every nondurable record

The built-in reclamation boundary is no longer keyed only on records already
marked terminal. After a wrapper has joined, `CloseAgentTool`:

1. promotes any retained live nondurable record;
2. retries terminal Q persistence;
3. independently proves that no nondurable record remains; and only then
4. permits `AgentRegistry::remove_terminal`.

If promotion or persistence fails, a dead controller is marked `Failed`, its
registry entry and exact recovery authority remain, and close returns the
existing payload-free count-bearing error. A later close retries the same Q.
The tombstone/reclaimed lookup and observed-terminal paths use the same
all-record recovery gate.

Spawn, fork, headless reclamation, and TUI expiry now test the all-record
nondurable status rather than only the terminal map. BLOCKER-1's authoritative
wake-delivery no-await window is unchanged.

## Decisive public regression

`idle_queue_failure_then_wrapper_abort_retains_exact_recovery` drives the
reviewer's complete public path:

1. spawn a persistent child and wait for `Idle`;
2. send an exact direct update through the public inbound sender;
3. make Q persistence fail and observe the nondurable retained record;
4. abort the public join handle before the wrapper's terminal arm;
5. reinsert the consumed handle and call the real `CloseAgentTool`;
6. prove close fails payload-free, the registry entry is `Failed` with no
   tombstone, one exact message is retained, and terminal recovery owns the
   store strongly;
7. remove the remaining external handle and prove a `Weak<EventStore>` still
   upgrades solely through recovery authority;
8. restore the sink, call the real close tool again, and prove exactly one Q is
   durable before reclamation; and
9. retry explicitly and prove `NoRecovery` with the Q count still exactly one.

The test is a semantic 272-line module, not an `include!` split. The production
prefix of `close.rs` is 499 physical lines.

## Verification

All Cargo commands used
`CARGO_TARGET_DIR=/Users/tom/Developer/ablative/norn/target`. No temporary build
directory was used.

- `cargo +1.94.0 fmt --all -- --check`: pass.
- `git diff --check`: pass.
- `cargo +1.94.0 clippy --workspace --all-targets --all-features -- -D warnings`:
  pass with no suppression.
- `cargo +1.94.0 test --workspace --all-features`: exit 0. Primary harnesses
  were Norn `4367/4367`, CLI `522/522`, and TUI `685/685`; every integration,
  trybuild, PTY, and smoke harness was green, as were `8/8` doctests.
- Focused close recovery `8/8`, terminal mailbox `4/4`, terminal teardown
  `6/6`, and pending-message `10/10`: pass.
- The 14-path added-line scan found zero added `unwrap`, `expect`, `panic!`,
  `unsafe`, or lint-suppression forms in production or test code.

An earlier sandboxed full-workspace run produced 144 loopback
`Operation not permitted` failures. It is invalid environment evidence, not a
product result. The network-capable full run above is the final broad gate.

### Retained distribution

The checked-in runner is
[`run_p5_d8_major2_correction.py`](evidence/p5-d8/run_p5_d8_major2_correction.py),
SHA-256 `402938307dca9b8b298aee86fcf74345bd68aef3493f337641f28666801b68b7`.
It refuses a different branch, source commit/tree, unexpected dirty build
input, a symlinked primary target, or an observation that does not execute
exactly one test.

The retained artifact is
[`2026-07-23-p5-d8-major2-correction-distributions-7747dec.json`](evidence/p5-d8/2026-07-23-p5-d8-major2-correction-distributions-7747dec.json),
SHA-256 `b4a5580e6f353674be3e9a36399c8184a5cec760c556aa4e09ce932d6509d8f2`.
Four process-isolated exact cases each passed `20/20`, for `80/80`:

- the complete public idle/Q-failure/wrapper-abort/close/retry scenario;
- live nondurable promotion into terminal recovery and exact retry;
- tombstone/reclaimed resolution refusing unresolved recovery; and
- the earlier authoritative wake-cancellation regression.

Every observation records exit status, observed test count, duration, and
output SHA-256. The artifact carries the complete 14-path source inventory.

### Mutation kill

The coordinator removed the per-record authority assignment from the Q failure
arm, leaving every other correction guard intact. The complete public
regression then failed because terminal recovery status was `None` instead of
`Some(1)`. Restoring the assignment restored the test; the source tree was
byte-clean before the source commit and retained distribution.

## Honest boundaries

- This correction guarantees in-process retention after a Q-failed record has
  entered `PendingAgentMessages`. If the whole Norn process dies while the
  primary store remains unwritable, there is intentionally no second WAL.
- A channel send followed by wrapper death before the controller receives and
  constructs a pending record remains outside this correction. Closing that
  broader arbitrary-abort window requires an inbound-owning task guard rather
  than another reclamation check.
- A nested externally aborted controller with no retained join handle can keep
  a strongly retained record but remain live-looking until task death is made
  authoritative to the registry. The message cannot be reclaimed or physically
  lost by the corrected built-in path; lifecycle convergence for that
  noncanonical topology is not claimed.
- Cancelling `close_agent` after it consumes a handle is data-safe because the
  record owns its store, but task-death/lifecycle convergence after caller
  cancellation remains a separate supervision concern.
- Raw embedder calls to the low-level public
  `AgentRegistry::remove_terminal` remain outside the built-in close contract.
  A subsequent built-in close repairs a tombstoned entry through the all-record
  recovery gate.
- Existing fail-after exact-once semantics rely on `EventStore`'s byte-exact
  idempotent append contract. Model-facing errors remain UUID/count-only and
  never include message payloads or sink diagnostics.

These boundaries do not recreate MAJOR-2: in its reviewed public path, the
failed Q record owns strong recovery authority, wrapper death cannot erase it,
the first close cannot reclaim it, and the later close writes exactly one Q
before reclamation.

## Requested same-reviewer checks

1. Drive the checked-in public regression and mutation-kill the per-record
   authority assignment.
2. Verify authority is installed before nondurable publication, cleared only
   after proven Q durability or atomic transfer, and rejected on mixed
   mailbox/store generations.
3. Confirm every built-in reclamation seam either discharges all nondurable
   records or preserves the failed entry with a payload-free typed error.
4. Confirm BLOCKER-1 and MAJOR-1 remain closed.
5. Reproduce the source/tree/inventory/runner hashes, `80/80` distribution,
   strict gates, and line-limit evidence.

Requested verdict:
`MAJOR-2 CLOSED; D8 READY as an implementation candidate`
or a precise `NOT READY` finding. A full panel is not requested.
