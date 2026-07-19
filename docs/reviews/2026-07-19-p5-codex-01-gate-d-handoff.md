# P5 `CODEX-01` Gate D handoff

Date: 2026-07-19  
Candidate branch: `codex/p5-codex-turn`  
Finding: `CODEX-01` only  
Requested verdict: `READY` or `NOT READY` as an implementation candidate  
Whole-P5 acceptance: explicitly out of scope

## Exact review target

The product implementation is commit `98b026695dbcac53cd2d9b7cd51fd51c6f906a53`
over base `62a852e1ffbe6bfcc7cc7a977c11a7778d716593`. Its tree is
`7d14354f045cfddc0127a68be5d65947031c4c04`.

The test-only no-shortcut correction is
`fcaf0e4143e0245c169f08951f9158eac665b4ee`. Its parent is merge commit
`5959dfa60de523e83fe1a45a69a6d7af13e9353f`, which brings the separately
reviewed `TRANS-01` documentation/evidence package onto this branch and changes
no `CODEX-01` product source. The corrected combined tree is
`b8835d0b229717c262aa440f8054742083119556`.

Review the 13 paths in `62a852e..98b0266` plus the single test correction in
`fcaf0e4`:

- `crates/norn-cli/src/print/output.rs`
- `crates/norn-cli/src/print/output/provider_events.rs`
- `crates/norn/src/loop/classify.rs`
- `crates/norn/src/loop/runner/dispatch.rs`
- `crates/norn/src/loop/runner/provider_call.rs`
- `crates/norn/src/loop/runner/tests.rs`
- `crates/norn/src/provider/events.rs`
- `crates/norn/src/provider/openai/execute.rs`
- `crates/norn/src/provider/openai/response_terminal.rs`
- `crates/norn/src/provider/openai/response_terminal/tests.rs`
- `crates/norn/src/provider/openai/sse.rs`
- `crates/norn/src/provider/openai/sse_types.rs`
- `crates/norn/src/session/events.rs`

## Source contract

The private wire field and Codex-overlay identity trace to the immutable
official OpenAI Codex snapshot
[`0396f99`](https://github.com/openai/codex/tree/0396f99cf1a27fc87dd12d23403b25e840b6ecbd),
including the pinned
[`common.rs`](https://github.com/openai/codex/blob/0396f99cf1a27fc87dd12d23403b25e840b6ecbd/codex-rs/codex-api/src/common.rs)
and
[`sse/responses.rs`](https://github.com/openai/codex/blob/0396f99cf1a27fc87dd12d23403b25e840b6ecbd/codex-rs/codex-api/src/sse/responses.rs)
blobs recorded in the
[`P1 Gate A contract`](2026-07-15-p1-gate-a-contract.md). This candidate
freezes the following Norn behavior rather than inferring private semantics on
public/custom backends.

| Terminal shape | Actionable call | Typed terminal stop reason |
|---|---:|---|
| Codex `end_turn: false` | no | `ContinueTurn` |
| Codex `end_turn: true` | no | `EndTurn` |
| Codex `end_turn: null` | no | `EndTurn` |
| Codex field absent | no | `EndTurn` |
| Codex any legal value/absence | function, custom, or schema call | `ToolUse` |
| Public/custom field present, including `null` | any | typed parse failure, no `Done` |
| Unknown catalog backend | any | public semantics |

The exact raw `ResponseStreamEvent` preserves the distinction between `true`,
`null`, and absence. `ProviderEvent::Done` intentionally carries only the
behavioral projection, so those three shapes collapse to `EndTurn`.

Refusal behavior is explicit. Refusal-only plus `false` persists and replays
the canonical refusal before another sample. `true`, `null`, or absence returns
the existing `Refused` outcome. A malformed refusal plus any actionable call
remains refusal-authoritative and executes no tool.

## Implementation shape

`ResponsesDialect` is selected from the trusted catalog backend before
dispatch. Only `codex_subscription` enables the overlay; all other values fail
closed to public semantics. Stateful `ResponsesMapper` owns
`response.completed` reconciliation and terminal projection. The old
backend-blind `map_sse_event` completed arm and last-item-only tool inference are
deleted.

`StopReason::ContinueTurn` crosses assembly, classification, dispatch,
persistence, and CLI event projection. The intermediate assistant response is
appended with `stop_reason = "continue_turn"` before the next provider request.
Text and canonical response items replay normally. Empty continuation adds no
synthetic assistant message.

Actionable calls always win over `end_turn`. This includes ordinary function
calls, custom tool calls, and the configured structured-output schema call.
A call-free continuation while a schema is configured does not consume schema
budget or insert a nudge.

## Verification run by the implementer

All Cargo commands used the repository's normal shared `target/` directory.
No temporary build target was used.

- `cargo fmt --all -- --check`: pass.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`: pass.
- `cargo clippy -p norn --all-targets --all-features -- -D warnings` after
  `fcaf0e4`: pass.
- `cargo test -p norn-cli --lib --quiet`: 501/501 pass.
- `stateful_terminal_mapper`: 2/2 pass.
- `continue_turn`: 4/4 pass before the no-shortcut correction; its changed
  exact fixture was rerun after correction and passed 1/1.
- `schema_call_precedes_an_explicit_continue_directive`: 1/1 pass.
- `custom_tool_call_precedes_an_explicit_continue_directive`: 1/1 pass.
- `provider::openai::response_terminal::tests`: 11/11 pass.
- `mapper_honors_end_turn_only_for_the_codex_dialect`: 1/1 pass.
- `trusted_catalog_backend_selects_the_terminal_dialect`: 1/1 pass.
- `provider_event_continue_turn_serialises_without_collapsing_to_end_turn`:
  1/1 pass.
- `git diff --check`: pass.

The full `norn` library suite was not claimed at this candidate snapshot. An
earlier broad sandbox run compiled all 4,032 then-current tests but loopback
fixtures failed with the environment's `PermissionDenied` bind restriction.
The external gate should run the complete suite in its network-capable review
environment.

## Policy and size audit

The net Rust diff adds none of the campaign policy patterns: no `unwrap`,
`expect`, `panic!`, `todo!`, `unimplemented!`, `unsafe`, lint suppression,
`TODO`, or `HACK`. The `fcaf0e4` correction removes the only initially added
test `expect` instead of relying on that module's pre-existing test allowance.
No timeout, retry count, cadence, queue size, or other operational limit is
added.

All modified production prefixes remain below 500 lines. The largest are
`print/output.rs` 469, `session/events.rs` 463, `runner/dispatch.rs` 400,
`loop/classify.rs` 386, and `openai/sse.rs` 337. The large runner and terminal
test modules are test-only.

## Internal review already completed

A core static review returned `READY` before the downstream CLI projection was
added. A second final read-only audit included that CLI projection and returned
`READY` with no blocker, major, or minor. Together they verified the dialect
boundary, terminal matrix, refusal/actionable precedence, persistence/replay,
CLI projection, no-arbitrary-limit rule, and production-prefix sizes. The later
`fcaf0e4` change only replaces a test `expect` with `?`; its exact fixture and
strict Norn Clippy were rerun, but it has not been independently re-reviewed.
Neither auditor edited source or ran Cargo; the implementer gate above is the
execution evidence.

## Honest boundaries

- This is `CODEX-01` candidate review, not P5 acceptance.
- Persistence does not automatically restart an interrupted in-flight
  continuation after ordinary process/session resume.
- Repeated `end_turn:false` can continue indefinitely when the existing
  optional cancellation, `max_iterations`, and step-timeout controls are all
  absent. No invented default was added.
- Exact `true`/`null`/absence is raw-event metadata, not a downstream typed
  three-state field.
- The pre-existing legacy `response.incomplete` compatibility projection is
  outside this completed-terminal slice.
- D3, D8, `CODEX-02` turn state, client metadata, account-bound anchors, and
  whole-P5 evidence remain open.

## Reviewer questions

1. Can any public/custom backend reach Codex `end_turn` semantics?
2. Can a malformed or conflicting directive silently end or continue a turn?
3. Can any actionable function/custom/schema call be skipped because of
   `end_turn`?
4. Can refusal plus an actionable call execute despite refusal authority?
5. Is the intermediate response durable before the next provider request?
6. Does any completed-terminal compatibility path bypass the dialect-aware
   decoder?
7. Do the tests bind backend selection, no-`Done` public failure, schema budget,
   custom-tool precedence, persistence, replay, and CLI output?
8. Does any claim above exceed the code or the reported evidence?
