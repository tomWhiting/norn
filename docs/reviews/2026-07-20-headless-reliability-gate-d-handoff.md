# Headless driven-transport Gate D handoff

Date: 2026-07-20
Candidate branch: `codex/p5-d3-compaction`
Requested verdict: `READY` or `NOT READY` as an isolated reliability change
Responses phase acceptance: out of scope

## Exact review target

Review two isolated commit slices:

- initial implementation
  `974a2166185d8393b9977e178598d149e64b841e..e3549b4a39e9c7f44b686e051c91be9144d2f48d`,
  tree `f7e01752beb9dc449b21a2879d73c44c2d9be9e8`; and
- compound-failure correction
  `ef3b9c7b0fd12946d5b993457106dda0b34f0edd..31553e82dedf4e2de3a86b531bb2ce30f9c858bf`,
  final tree `99e8cfd8544a67ddfbae635c369953af7f9f52ed`.

The initial six-path sorted, NUL-delimited inventory has SHA-256
`9c857ab8434ba2e9a9ce06a9ab80012c3a13146d78c4d4b1ab798d44217a0f9a`.
The correction changes four paths: three from the initial inventory plus
`crates/norn-cli/src/print/error.rs`. Its corresponding inventory has SHA-256
`6860e4c070a98dddd2231df39fe6546aec586be9a49d0b819b73e22f28deef9a`.
The intervening D3 commits are unrelated to this path-limited review.

## Outcome in practice

Driven JSON-RPC runs no longer report clean success after their stdout writer,
event emitter, or accepted intervention channel has torn. Each background task
returns a typed result, the orchestrator joins all applicable tasks, and a
transport, lag, panic, or cancellation failure maps to an existing nonzero CLI
error class. Compound failures retain every diagnostic while preserving the
original run failure's exit classification. In particular, an authentication
failure remains exit 3 when intervention, event-emitter, terminal-response
enqueue, or stdout-writer teardown also fails.

The sole stdout writer flushes after every newline-delimited frame. Write and
flush failures therefore become observable to the driver rather than closing
the receiver silently. This does not make one operating-system write atomic:
the final frame may still be truncated by a sink failure, which is why the
protocol documentation now promises serialization and a nonzero failure rather
than atomic delivery.

Event shutdown snapshots the unread prefix at the shutdown cut and drains only
that prefix. A retained sender or live child cannot extend shutdown forever or
emit onto stdout after the terminal response. Broadcast lag is a typed
event-loss failure rather than a warning.

An intervention read or response-write failure is retained until the provider
run ends. It does not asynchronously cancel an already running provider task,
but the accepted run cannot subsequently return a clean result over the torn
control channel.

## Evidence run by the implementer

Post-source packaging commit `2ef1427c2d4bdfffdbb80e943e1b099a42d7e90b`,
tree `df4241f2d40c57ad1045ee9196cf90234fa018f3`, passed the following gates. Its
only later source change is an unrelated test-only OAuth response reader; the
four headless correction paths remain byte-identical to tree `99e8cfd`:

- `cargo clippy --workspace --all-targets --all-features -- -D warnings`;
- `cargo fmt --all -- --check`;
- `git diff --check`;
- `cargo test --workspace --all-targets --all-features --no-fail-fast --quiet`;
- `cargo test -p norn-cli --lib --all-features --quiet`: `518/518`;
- `cargo test --workspace --doc --quiet`: `8/8` doctests.

Focused tests pin per-frame flushing, write and flush failure propagation,
bounded event draining, broadcast lag, outbound receiver loss, intervention
transport failure, task cancellation, and preservation of dual diagnostics.
No lint suppression or Clippy bypass was added.

This slice has no retained distribution runner. The commands above are
coordinator observations at the packaging tree, not a source-bound Gate D
artifact. The reviewer should rerun the focused CLI tests across the two exact
source slices.

## Honest boundaries

- This change makes silent headless transport failures diagnosable and nonzero.
  It does not establish the root cause of every previously observed headless
  Norn process disappearing midstream.
- Mid-run stdin EOF remains a clean end of the intervention reader; the provider
  run may finish and publish its terminal response.
- A control-channel failure is surfaced after the provider task ends rather
  than aborting it asynchronously.
- The change does not alter provider cancellation, retry, request formation,
  session state, or Responses event semantics.
- No Norn process was killed while implementing or verifying this slice.

## Reviewer questions

1. Can any writer, emitter, or intervention failure still be logged and then
   followed by process success?
2. Can shutdown wait indefinitely on events published after its cut, or emit an
   event after the terminal response?
3. Are stdout write/flush errors and task joins classified without losing the
   primary diagnostic when multiple channels fail?
4. Does any changed path claim atomic frame delivery or a complete diagnosis of
   the reported headless deaths without evidence?
