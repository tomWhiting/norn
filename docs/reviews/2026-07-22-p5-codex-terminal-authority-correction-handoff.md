# P5 live Codex terminal-authority correction: Gate D handoff

**Date:** 2026-07-22

**Candidate branch:** `codex/p5-codex-terminal-correction`

**Verdict requested:** `READY as a narrow implementation candidate`, or a
precise finding that prevents that verdict

**Acceptance boundary:** this is not P4 re-acceptance, D8 acceptance, whole-P5
acceptance, or the authenticated D7/P9 live-wire gate.

## Exact review boundary

- Base: `6d168830ee3c4edad5893d39a0e1e67950da98ad`.
- Product correction: `d86a4ed16335abd99fda80185a28bf74492b42ef`.
- Corrected source tree: `1d7a97078aa750d2cc8cfd2ff49e05b236c8e8da`.
- Source range: `6d16883..d86a4ed`.
- Changed paths: 7, with 653 insertions and 93 deletions.
- NUL-delimited path-inventory SHA-256:
  `378be8701723a23bb13fdfa1efd7bb5e0cceb30495ea788082db1c2cbb773896`.

The seven source paths are:

- `crates/norn/src/provider/openai/execute.rs`
- `crates/norn/src/provider/openai/execute/reconciliation_tests.rs`
- `crates/norn/src/provider/openai/execute/reconciliation_tests/codex_terminal.rs`
- `crates/norn/src/provider/openai/response_reconciler.rs`
- `crates/norn/src/provider/openai/response_reconciler/model.rs`
- `crates/norn/src/provider/openai/response_reconciler/terminal_authority.rs`
- `crates/norn/src/provider/openai/response_terminal.rs`

No authentication, credential, request-payload, persistence, WebSocket,
transport-retry, prompt-authority, or CLI source changes in this range.

## Reproduced product symptom

An authenticated Codex-subscription request emitted its visible answer and then
ended with:

```text
error: provider error: Responses protocol violation: completed response item was absent from terminal response.output
```

The error establishes that the stream had supplied a completed item through
`response.output_item.done` that was not present in terminal
`response.output`. The working diagnosis is the absent-or-empty terminal-output
shape covered by this correction, but no retained redacted live trace binds
which exact shape occurred. The mandatory live rerun below remains the check on
that diagnosis. The pre-correction reconciler rejected the already-completed
Codex item after projecting preview text.

This handoff does not present that one observed response as an exhaustive wire
contract. It presents the correction as support for the observed Codex shape,
bounded by the fail-closed conditions below.

## Corrected authority contract

The mapper selects one terminal-output policy from the trusted catalog backend
before processing the response:

- public and custom Responses backends retain `StrictPublic`;
- only the compiled Codex-subscription backend selects
  `CodexCompletedItemsFallback`; and
- the policy is stored in the per-response reconciler and has no configuration,
  prompt, event, environment, or response-body override.

Under the Codex policy, an absent `response.output` or an empty output array may
use the stream's previously validated `response.output_item.done` items as
terminal authority. The fallback:

- reuses the exact retained completed items rather than constructing items from
  preview text;
- orders them by `output_index` through the reconciler's `BTreeMap` identity
  order;
- requires indices to be contiguous and zero-based;
- for completed and incomplete outcomes, applies the existing schema, identity,
  added-family, call-identity, channel, item-channel, core-delta, and
  actionable-resolution validation; and
- rejects any announced item absent from the completed fallback set.

A nonempty Codex terminal `response.output` remains on the original strict path.
It is parsed in terminal order and cross-checked by JSON value against retained
completed items. A conflict remains `TerminalCompletionConflict`. A present
non-array output fails typed rather than activating the fallback.

Public Responses remains unchanged in authority: missing output is
`MissingTerminalOutput`, and an empty array cannot absorb an earlier completed
item. The default `ResponseReconciler::new()` policy is still public-strict.

## Fail-closed behavior under review

The correction is intended to accept only complete item-done authority, not to
promote previews. In the fallback path:

- a sparse or non-zero-starting output-index set fails;
- an unfinished `response.output_item.added` announcement fails;
- a delta without a completed item fails;
- a completed channel without its completed item fails;
- an unresolved function/custom actionable item fails;
- conflicting duplicate completion or identity rebinding still fails; and
- no `ProviderEvent::Done` is produced after one of those protocol errors.

Completed function and custom-tool calls remain canonical actionable items and
produce the existing `ToolUse` terminal classification. An incomplete response
may use complete done-item authority and retain its typed `MaxTokens` outcome;
unfinished state still fails. A failed response without terminal output keeps
the provider failure authoritative instead of replacing it with
`MissingTerminalOutput`.

## Regression matrix

The new 11-test Codex terminal module binds these cases:

1. Missing terminal output uses completed messages in `output_index` order even
   when the done frames arrived out of order.
2. An explicitly empty Codex terminal output uses completed-item authority.
3. Public Responses still rejects both the missing-output and contradictory
   empty-output shapes.
4. A nonempty Codex terminal output must match retained completion data.
5. Completed function and custom-tool calls reach canonical output and
   `ToolUse`.
6. Incomplete responses accept complete done authority but reject unfinished
   preview state.
7. Failed responses preserve provider-failure authority when output is absent.
8. A present non-array terminal output fails closed.
9. Fallback indices must be contiguous and zero-based.
10. An unfinished item announcement cannot disappear at the terminal frame.
11. Unresolved delta, channel-completion, and actionable state cannot disappear
    at the terminal frame.

The tests drive `ResponsesMapper` with the exact catalog backend constants, so
they exercise dialect selection, raw-event observation, reconciliation,
canonical item projection, and terminal projection together. They are
deterministic fixtures, not authenticated network observations.

## Verification already run

All Cargo commands used the repository's normal shared
`/Users/tom/Developer/ablative/norn/target` directory. No temporary target was
used.

```text
CARGO_TARGET_DIR=/Users/tom/Developer/ablative/norn/target \
  cargo test -p norn \
  provider::openai::execute::reconciliation_tests::codex_terminal --lib
```

Result: **11/11**.

```text
CARGO_TARGET_DIR=/Users/tom/Developer/ablative/norn/target \
  cargo test -p norn provider::openai::execute::reconciliation_tests --lib
```

Result: **26/26**.

```text
CARGO_TARGET_DIR=/Users/tom/Developer/ablative/norn/target \
  cargo test -p norn provider::openai::response_reconciler --lib --quiet
```

Result: **117/117**.

```text
CARGO_TARGET_DIR=/Users/tom/Developer/ablative/norn/target \
  cargo clippy -p norn --all-targets --all-features -- -D warnings
CARGO_TARGET_DIR=/Users/tom/Developer/ablative/norn/target \
  cargo fmt --all --check
git diff --check 6d16883..d86a4ed
```

Result: all pass. No lint suppression was introduced. The added lines contain
no `allow`, `unwrap`, `expect`, `panic!`, `todo!`, `unimplemented!`, `unsafe`,
`TODO`, or `HACK` match under the repository's direct added-line scan.

This candidate has not run a complete workspace test gate. The three reported
suites and strict Norn Clippy are the claimed local evidence.

### Narrow authenticated smoke after packaging

With the owner's explicit approval, the exact candidate source was built in
release mode and run once through the CLI's default OpenAI Responses selection
using the existing Norn-owned OAuth credential:

```text
norn -p "Reply with exactly: hi"
```

The command returned `hi`, exited `0`, and did not reproduce
`CompletionAbsentFromTerminal` or any other provider protocol error. This is
useful end-to-end evidence that backend selection, transport, mapper policy,
terminal projection, and print-mode completion are wired together for the
reported simple-text shape.

This narrow smoke retained no raw event trace and exercised only one completed
text response. It therefore does **not** complete D7/P9, prove every terminal
shape, or replace the deterministic matrix above. The broader authenticated
gate remains unchecked.

The release build command accidentally omitted the mandated shared
`CARGO_TARGET_DIR` and created a worktree-local `target/`. After the smoke, that
generated directory was removed with `cargo clean` (`5,953` files, `1.6 GiB`);
the worktree is clean. This build is reported for transparency and is not used
as policy-compliant Gate C evidence. The focused test and Clippy commands above
did use the mandated shared target.

## Source shape and size

The prior 495-line reconciler was reduced to a 421-line coordinator by moving
the terminal-authority policy and finish operation into the cohesive new
160-line `terminal_authority.rs` module. This is a functional module boundary,
not an `include!` or `#[path]` line-count bypass.

The new test module is 457 physical lines. The modified
`reconciliation_tests.rs` is 496 lines. The legacy `execute.rs` test container
was already 2,197 physical lines at the base and is 2,200 at the candidate; its
production prefix ends before line 363. This correction does not claim to have
decomposed that pre-existing test container. Every new file is below 500 lines,
and the changed production modules or production prefixes are below 500 lines.

## Internal source-read result

One independent adversarial source read returned `SOUND` with no blocker,
major, or minor finding. It specifically checked backend gating, missing/empty
fallback, nonempty strict cross-checking, unfinished state, actionable calls,
failed/incomplete behavior, and the expanded fixture set.

That read initially recorded sparse indices as a nonblocking residual. The
final source adds the zero-based contiguity guard and regression after that
observation. The read is useful internal evidence, but it is not the external
Gate D verdict requested here.

## Honest residuals and mandatory live check

- One authenticated simple-text request now succeeds against this candidate,
  with exit `0` and no prior protocol error. No raw event trace was retained,
  so the exact absent-versus-empty terminal-output shape remains unbound.
- The mandatory D7/P9 authenticated real-wire gate remains open. It still
  requires explicit approval for credentials, spending, redaction, and retained
  evidence across its complete matrix. A narrow smoke is not that pass.
- When that gate runs, the retained redacted trace should establish the actual
  terminal-output shape after the done-item frames, and should prove one
  canonical item plus one terminal outcome without the old protocol error.
- WebSocket transport is not exercised by this correction or its fixtures.
- The mapper still projects canonical terminal items before decoding terminal
  metadata. That ordering predates this range and is shared by the strict and
  fallback paths; this handoff does not claim to correct or re-evidence it.
- The range does not change raw stream-event retention, retry ownership,
  cancellation, usage-presence accounting, response-item persistence, or D8
  prompt authority.
- No statement here accepts P5 or reopens the already accepted P4 contract.

## Requested adversarial review

Please independently attack at least these seams:

1. Trace every way `catalog_backend` reaches `ResponsesDialect` and determine
   whether untrusted configuration or a custom endpoint can select the Codex
   fallback.
2. Prove public Responses remains strict and that malformed or nonempty Codex
   terminal output cannot silently fall into completed-item authority.
3. Attempt to construct a missing/empty-output stream that promotes deltas,
   announcements, channel completions, or unfinished calls without a matching
   validated done item.
4. Attempt output-index gaps, non-zero starts, reordered arrivals, duplicate
   indices, ID rebinding, duplicate completions, and conflicting completions.
5. Confirm the fallback retains exact completed-item JSON and does not
   synthesize canonical text, reasoning, refusal, or tool arguments from
   previews.
6. Check function/custom-tool actionability and stop-reason behavior under both
   missing-output and nonempty strict-terminal shapes.
7. Check completed, incomplete, and failed terminal classes separately,
   including whether any protocol error can be converted into `Done`.
8. Inspect event projection order and determine whether the disclosed inherited
   terminal-metadata ordering creates a new reachable defect in this range.
9. Mutation-test the dialect policy, public-strict branch, nonempty-output
   branch, contiguity guard, announcement guard, and actionable-resolution
   guard against the named regressions.
10. Reproduce the exact source/tree/inventory boundary and the three focused
    test counts without using a temporary Cargo target.

The requested result is `READY as a narrow implementation candidate` or a
specific blocker/major/minor finding. External review and the authenticated
real-wire gate must remain visibly separate verdicts.
