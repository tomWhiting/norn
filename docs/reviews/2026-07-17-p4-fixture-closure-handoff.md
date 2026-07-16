# P4 fixture-closure candidate handoff

**Date:** 2026-07-17

**Status:** REVIEWABLE IMPLEMENTATION CANDIDATE, not P4 acceptance

**Source head:** `65dc1d5`

**Source commits:** `4b70a53`, `65dc1d5`

**Review range:** `f962a64..65dc1d5`

**Tracking plan:** [`RESPONSES-API-REMEDIATION-PLAN.md`](../RESPONSES-API-REMEDIATION-PLAN.md)

## Result

This increment closes the retained P4 refusal, hosted-search continuation, and
supported non-audio stream-equivalence fixture gaps. It also closes the
authoritative-display gap found during read-only review: a truncated preview now
receives only its missing authoritative suffix, while a non-prefix conflict is a
typed protocol failure rather than silent replacement. Rejected item and
multi-item terminal reconciliation is atomic with respect to preview state.

This does not accept P4. Response-scoped audio still depends on a durable P3
artifact representation, the private Codex subscription path still needs its
retained live fixture, and independent review plus final retained gates remain
open. P6 separately owns retry-attempt UI cleanup and absent-versus-zero usage
projection.

## Official contract basis

OpenAI Developer Docs MCP was the primary authority for this increment:

- [Responses streaming events](https://developers.openai.com/api/reference/resources/responses/streaming-events)
  defines the `response.output_text.delta` logprob shape and the event contracts
  exercised by the equivalence fixture.
- [Create a response](https://developers.openai.com/api/reference/resources/responses/methods/create)
  defines `include` and the canonical output-item union.
- [Migrate to Responses](https://developers.openai.com/api/docs/guides/migrate-to-responses#2-map-messages-to-items)
  anchors item-oriented replay rather than lossy message flattening.
- [Conversation state](https://developers.openai.com/api/docs/guides/conversation-state#passing-context-from-the-previous-response)
  anchors explicit stateless continuation behavior.

The request now includes `web_search_call.action.sources` whenever a hosted web
search tool is present. Stateless requests continue to request
`reasoning.encrypted_content`; stateful requests do not. Output-text delta
logprobs require the documented outer token/logprob fields and preserve the
documented optional nested top-logprob fields.

## Closed evidence gaps

- Four runner fixtures cover pure and mixed refusal, structured-output refusal
  without schema retry, refusal after one actual tool dispatch, persistence,
  stateless replay, and a fresh resumed turn with exact usage.
- A runner fixture performs a hosted-search plus local-tool turn, inspects the
  actual second provider request, checkpoints and reloads JSONL, and proves the
  hosted action, sources, citation, function call, and correlated result survive
  stateless replay in canonical order.
- The streamed/terminal-only fixture covers message text, refusal, reasoning
  summary and detail, annotations and logprobs, hosted web search, image result,
  MCP, code-interpreter outputs, function calls, duplicate completion,
  interleaving, truncated previews, and synthesized terminal-only content.
  Norn still sends `stream: true`; this is deliberately not described as a
  separate non-streaming API path.
- Missing authoritative suffixes reach existing text, refusal, and thinking
  live projections. Tool argument authority remains in the existing completed
  call path. Non-prefix content conflicts fail closed.
- Item and terminal rejection tests prove failed reconciliation does not mutate
  accumulated preview content.

## Verification

All builds used the repository's normal `target/` directory.

| Check | Result |
|---|---|
| `cargo test -p norn provider::openai` | 551/551 passed at `65dc1d5` |
| Refusal runner matrix | 4/4 passed |
| Hosted-search runner/resume matrix | 1/1 passed |
| Streamed/terminal-only equivalence | 1/1 passed |
| `cargo test --workspace --all-targets` | Passed at `65dc1d5`; major targets included `norn` 3710/3710, CLI 485/485, TUI 682/682, and PTY 17/17 |
| `cargo test --workspace --doc` | Passed |
| `cargo clippy --workspace --all-targets -- -D warnings` | Passed at `65dc1d5` |
| `cargo fmt --all -- --check` | Passed |
| `git diff --check` | Passed |
| Added-bypass scan | Zero added `allow`, `unwrap`, `expect`, `panic`, `todo`, or `unimplemented` |

Production prefixes remain below 500 lines. The tightest touched unit is
`response_reconciler.rs` at 490 production lines; `request.rs` is 453 and
`execute.rs` is 289. New focused fixture files are 499, 345, and 170 lines.

## Open before P4 acceptance

- P3 must own a durable response-scoped audio artifact and its lifecycle matrix.
- P3 D2 and the full spawned/forked/cross-session matrix remain open.
- A retained live Codex-subscription fixture should confirm the private backend
  remains compatible with the public-contract request and mapper changes.
- P6 owns retry-attempt producer termination, TUI cleanup after `StreamRetry`,
  and absent-versus-zero legacy usage projection.
- Independent streaming/item and UI/session review, Fable review, and final
  retained Universal Gates A-D remain unchecked.

## Requested review

Review `f962a64..65dc1d5`. Re-enumerate the request/event contracts from the
official OpenAI docs, trace the three runner/equivalence matrices, verify that
authoritative repair is append-only and atomic on rejection, rerun the strict
battery in the normal repository build directory, and return `READY` only for
this implementation candidate.
