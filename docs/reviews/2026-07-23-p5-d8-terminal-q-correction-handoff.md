# P5 D8 terminal-Q loss correction handoff

Date: 2026-07-23

Candidate branch: `codex/p5-d8-teardown-q-correction`

Requested narrow verdict:
`MAJOR-1 CLOSED; D8 READY as an implementation candidate`

This is the same-reviewer correction requested by
[`2026-07-23-p5-d8-exact-once-correction-review.md`](2026-07-23-p5-d8-exact-once-correction-review.md).
It does not request D8 owner acceptance, whole-P5 acceptance, D7/P9
authenticated live-wire acceptance, or a verdict on the separate device-auth,
terminal-output, or headless-process slices.

## Frozen boundary

| Boundary | Commit / tree |
| --- | --- |
| Review verdict and correction base | `531200f372741d96ea0810716267e19a0490f027` |
| Frozen correction source | `eafa8dba8a7b6427fa7fc3750f2c384267cca5fc` |
| Frozen correction source tree | `1c67561e5602a278fdcbeb1d16f1811f965c656d` |
| Rust inventory | 27 paths, bytewise-sorted NUL SHA-256 `f1909134142a9d990c81c589551a18ac982d7f668434b5b4d0a8c3df05baaaaf` |

The runner, evidence, handoff, and plan reconciliation are documentation-only
over that source. Main was not changed while preparing this correction.

## Finding being corrected

The prior correction made wake delivery exact-once, but terminal teardown still
had a distinct loss arm. Both `transition_live_route` and
`persist_undelivered_after_close` removed accepted messages from their channel
or local vector before attempting Q persistence. If the recipient store failed
before or ambiguously after the append, the code retained only the error and
continued. The payload had left its sole in-memory owner, the terminal result
could still report success, and later resume had no authoritative Q to replay.

The correction must therefore satisfy all of these properties together:

1. Every terminal-drained message is retained before the first failing store
   operation.
2. An ambiguous write retries the identical Q identity and cannot duplicate.
3. Spawn/fork cannot publish a successful terminal outcome while persistence
   remains unresolved.
4. No built-in close, headless, or TUI path can reclaim the only recovery
   authority.
5. Recovery failure is loud and typed without placing message content in the
   model-facing result.

## Corrected terminal protocol

### Stage before I/O

`PendingAgentMessages::stage_after_close` publishes the exact
`PendingAgentMessage` into the recipient FIFO before it attempts Q persistence.
The terminal finalizer first adopts every record for the closing `MailboxId`
under the recipient enqueue lock and installs a strong recovery authority
containing that mailbox identity and an `Arc<EventStore>`.

Only after that authority is reachable does the finalizer call the existing
FIFO durability routine. `drain()` and `mem::take()` still transfer ownership
out of the channel/buffer, but no longer discard it: the exact record now lives
in `PendingAgentMessages` before any store error can occur.

### Fail-before and fail-after

Each Q uses the existing deterministic event identity derived from the message
UUID. A fail-before leaves the staged record nondurable and retryable. A
fail-after leaves the same cached Q identity nondurable until the idempotent
append reconciles it against the store. Exact equality is a no-op success; a
different event under that identity fails typed.

Durable terminal records are retired only after the store proves their exact Q
present. If any record remains unresolved, the recovery map retains the exact
mailbox, FIFO records, and strong store authority. The public retry surface is
recipient-serialized and exposes a payload-free pending count plus a typed
outcome; it never asks a caller to reconstruct or resend message content.

### Terminal outcomes cannot lie

The spawn and fork wrappers perform the final closed-mailbox sweep before any
terminal lifecycle, result-channel, status, or reclamation publication. A
transition error, finalizer error, or surviving recovery marker changes the
outcome to `Failed` using one fixed payload-free diagnostic. The already
computed output and usage remain available for diagnosis, but `succeeded` is
false and reclamation is skipped.

The tests place a test-only gate after model outcome projection and before the
terminal route transition. They inject a real accepted message and a custom
sink failure at the formerly lossy window, then assert:

- the exact message remains under recovery authority;
- no successful spawn/fork result or completion lifecycle is emitted;
- the model-facing error is the exact generic diagnostic;
- sink diagnostics, message content, and message UUIDs are absent from that
  model-facing result.

### Recovery authority cannot be reclaimed

`close_agent` retries terminal recovery before subtree mutation, after joining
a wrapper, when observing terminal state without a handle, and when resolving a
tombstone/reclaimed entry. An unresolved retry preserves the registry entry and
returns a typed count-bearing, payload-free error. The tests cover recovery
created during join, late parent recovery, subtree preflight, and reclaimed
lookups through the public `CloseAgentTool` call path.

The headless spawn/fork reclaimers already run in the terminal wrappers and now
skip reclamation on `persistence_failed`. The TUI stores only a private
observation closure that strongly owns `PendingAgentMessages`; an unresolved
terminal entry remains visible and registered after its ordinary display hold
expires. Rendering never performs filesystem I/O or a retry.

## Verification

The retained distributions and every final gate below use the primary
repository target at `/Users/tom/Developer/ablative/norn/target`. No temporary
build directory is part of the retained evidence.

### Strict and broad gates

- `cargo +1.94.0 fmt --all`: pass.
- `git diff --check`: pass.
- `cargo +1.94.0 clippy -p norn --all-targets --all-features --locked -- -D warnings`:
  pass with no suppression.
- `cargo +1.94.0 clippy -p norn-tui --all-targets --all-features --locked -- -D warnings`:
  pass with no suppression.
- Network-capable `cargo +1.94.0 test --locked --workspace --all-targets --all-features --quiet`:
  exit 0 at the frozen source. Primary harnesses were Norn `4366/4366`, CLI
  `522/522`, and TUI `685/685`; every printed integration, trybuild, and smoke
  harness was green.
- `cargo +1.94.0 test --locked --workspace --all-features --doc --quiet`:
  `8/8` doctests.
- Focused finalizer/recovery groups: terminal teardown `6/6`, close/reclamation
  `8/8`, and TUI authority/visibility `2/2`.

Execution chronology is disclosed rather than collapsed. An earlier sandboxed
workspace run was invalid because loopback fixtures received
`PermissionDenied`; the first network-capable run had one unrelated CLI
settings-file race while Norn passed `4366/4366`; that CLI test then passed
`20/20` in isolation. Earlier exploratory verification and mutation passes also
used the linked worktree's local target because `CARGO_TARGET_DIR` was not
explicitly set after a context reset. Those runs are not retained as final gate
evidence. The checked-in distribution and the complete final strict, workspace,
and doctest reruns above explicitly use the primary repository target.

### Retained distributions

The checked-in runner is
[`run_p5_d8_terminal_q_correction.py`](evidence/p5-d8/run_p5_d8_terminal_q_correction.py),
SHA-256 `c1041e206bb0e932eea20df4b16fd7265f840b035734a7719b08cd41481dff94`.
It refuses a different branch, source commit/tree, any dirty non-evidence build
input, a symlinked primary-repository target, or an observation that does not
execute exactly one test. The target is derived from Git's absolute common
directory, so a linked worktree cannot silently select its own build directory.

The retained artifact is
[`2026-07-23-p5-d8-terminal-q-correction-distributions-eafa8db.json`](evidence/p5-d8/2026-07-23-p5-d8-terminal-q-correction-distributions-eafa8db.json),
SHA-256 `dcc53b419610384b3e83a35608c1ee1164635b2daab59678f0361db168d522dd`.
Nine process-isolated exact cases each passed `20/20`, for `180/180`:

- terminal send/close race;
- two-message fail-before exact FIFO retention;
- two-message fail-after exact FIFO reconciliation;
- real spawn terminal failure and false-success prevention;
- real fork terminal failure and false-success prevention;
- post-join close recovery gating;
- no-handle late-parent close recovery gating;
- cancellation after authoritative wake append;
- TUI recovery visibility and registry retention.

Each observation records exit status, observed test count, duration, and output
SHA-256. The artifact also carries the complete 27-path source inventory.

### Mutation kills

The coordinator mutated five independent source guards and ran their exact
tests before restoring the worktree byte-clean:

1. Unconditionally deleting terminal recovery after a failed finalization made
   the fail-before FIFO test fail with observed status `None` instead of
   `Some(1)`.
2. Removing the spawn downgrade made its real controller test fail with
   `Q failure cannot emit spawn success`.
3. Removing the fork downgrade made its twin fail with
   `Q failure cannot emit fork success`.
4. Removing the no-handle close recovery gate made the real close test fail
   with `terminal observation bypassed recovery`.
5. Removing the TUI visibility/reclamation predicates made its public snapshot
   test fail with `unresolved terminal recovery must remain visible after hold expiry`.

The restored worktree matched source tree
`1c67561e5602a278fdcbeb1d16f1811f965c656d` before the retained distributions.

## Policy and size evidence

An independent read-only audit scanned all 2,718 added Rust lines across the 27
source paths. Added production and test hits were each zero for `unwrap`,
`expect`, `unwrap_err`, `expect_err`, `panic!`, `unsafe`, lint attributes,
empty `cfg(any())`, and command-line allow forms. No `include!` split exists.

Using the repository's AST test-scope classification plus Tokei nonblank,
comment-stripped Rust code counts:

- maximum production module: `status_line.rs`, 480 code lines;
- maximum test module: `status_line::tests`, 495 code lines;
- terminal teardown test module: 467 code lines;
- close recovery modules: 371 and 344 code lines;
- no production or test module exceeds 500 code lines.

Three touched host files retain physical prefixes above 500 because comments,
blank lines, and split test declarations are included in that different
measure: `status_line.rs` 718 raw / 480 production code, `event_loop.rs` 589 /
418, and `error/subsystems.rs` 504 / 284. These are disclosed, not represented
as code-line violations.

## Honest boundaries

- A fail-before record is retained for the lifetime of the owning Norn process
  and session runtime. There is intentionally no second WAL: process death
  before Q becomes durable is not claimed to survive. Adding a second durable
  journal during failure of the primary store would be a separate design.
- Fail-after physical exact-once relies on the public `PersistenceSink`
  exact-retry contract already enforced by `EventStore` for in-tree sinks.
- The built-in spawn, fork, close, headless, and TUI reclaimers are gated.
  A low-level embedder that directly calls `AgentRegistry::remove_terminal`
  remains outside that built-in lifecycle contract.
- The public retry API returns the typed underlying storage error to its trusted
  caller. Model-facing results are sanitized. Existing operator tracing of a
  custom sink error can contain whatever that embedder placed in its error;
  this correction does not claim to redact arbitrary third-party error text.
- An ephemeral `EventStore` and its retained authority do not outlive complete
  runtime ownership teardown.
- A concurrent successful retry after a controller has already projected its
  failure can conservatively leave a failed, unreclaimed entry. It cannot cause
  message loss, duplication, or false success.
- The pre-existing live-route `delivered:true` response before recipient
  durability remains outside this correction, as disclosed by the review.
- The installer publication-order hardening observation from the review is
  unchanged; public step entry points still revalidate before side effects.

## Requested same-reviewer checks

1. Inject fail-before and fail-after on the second of two terminal messages and
   verify exact field preservation, FIFO order, stable Q identities, and no
   duplicate after retry.
2. Exercise the real spawn and fork controllers at the test-only terminal gate;
   confirm failure is projected before lifecycle, result, status, and reclaim.
3. Attack cancellation between staging, failing store I/O, outcome projection,
   and explicit retry. Confirm the in-process recovery authority cannot be
   dropped or replaced by a different mailbox/store.
4. Re-run the close public-call fixtures and verify every built-in reclamation
   seam either recovers or preserves the entry with a typed error.
5. Confirm TUI rendering only observes recovery and never performs store I/O.
6. Reproduce source/tree/inventory/runner hashes, `180/180` distributions,
   strict gates, and the added-line/LOC report.

Requested verdict: `MAJOR-1 CLOSED; D8 READY as an implementation candidate`
or a precise `NOT READY` finding. A full panel is not requested; this is the
narrow same-reviewer confirmation prescribed by the prior verdict.
