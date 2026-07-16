# P4 streaming and transcript candidate handoff

**Date:** 2026-07-17

**Status:** REVIEWABLE IMPLEMENTATION CANDIDATE, not P4 acceptance

**Source commit:** `f962a64` (`feat(responses): reconcile complete streaming taxonomy`)

**Review range:** `128b282..f962a64`

**Tracking plan:** [`RESPONSES-API-REMEDIATION-PLAN.md`](../RESPONSES-API-REMEDIATION-PLAN.md)

## Executive result

The candidate replaces Norn's partial, order-dependent Responses stream mapper
with an explicit wire contract and stateful reconciler. It inventories all 53
public streaming-event discriminators and all 28 public output-item
discriminators in the pinned OpenAI schema, while keeping 18 Codex-specific
wire surfaces in a separate overlay. Manifest tests pin all three registries.

Every admitted frame is retained as a complete parsed JSON value before
interpretation. Every public event is then explicitly classified as reconciled
content,
lifecycle-only, terminal, or typed unsupported. Unknown future events and output
items preserve their raw JSON and fail closed instead of becoming empty success
or executable content.

This is deliberately not a claim that P3 or P4 is accepted. Response-scoped
audio still lacks a durable canonical transcript representation, several full
end-to-end fixture matrices remain open, and independent phase review has not
yet occurred.

## Contract authorities

OpenAI Developer Docs MCP is the primary authority. The official Codex source is
used only for the separately labelled subscription-backend overlay.

| Authority | Candidate use |
|---|---|
| [Responses streaming events](https://developers.openai.com/api/reference/resources/responses/streaming-events) | Public 53-event discriminator inventory and event field contracts; retrieved through Developer Docs MCP on 2026-07-16 and rechecked on 2026-07-17 |
| [Create a response](https://developers.openai.com/api/reference/resources/responses/methods/create) | Public response and 28-item output union; API description version `2.3.0` at the pinned retrieval |
| [Function calling](https://developers.openai.com/api/docs/guides/function-calling) | Client-executable function-call semantics and `call_id` role |
| [MCP and connectors](https://developers.openai.com/api/docs/guides/tools-connectors-mcp) | Hosted MCP item semantics; no invention of client execution from lifecycle events |
| [WHATWG server-sent events](https://html.spec.whatwg.org/multipage/server-sent-events.html) | SSE line, field, BOM, data aggregation, and incomplete-EOF behavior |
| [Codex source at `9ff4786`](https://github.com/openai/codex/tree/9ff47868eb2afeec579183e01bb9d3d3e9df2bcd) | Immutable secondary source for the 18-entry Codex subscription overlay only |

The checked contract lives in
`crates/norn/src/provider/openai/response_contract.rs`. Public and Codex
registries are intentionally separate so backend-specific fields cannot be
mistaken for public Responses API guarantees.

## Implemented behavior

### Wire ingestion

- A raw event envelope preserves the complete parsed event JSON value,
  discriminator, optional sequence number, and manifest classification. It does
  not claim byte-for-byte preservation of SSE framing, JSON whitespace, or object
  member order.
- Public events that require `sequence_number` reject its absence or malformed
  value. The Codex overlay follows its independently pinned sequence policy.
- The SSE parser accepts arbitrary byte boundaries, UTF-8 split boundaries, one
  leading BOM, LF, CRLF, and CR line endings, and multi-line `data` fields.
- A pending unterminated event is discarded at EOF as required by the SSE
  processing model. A transport close before an authoritative terminal event is
  reported as retryable stream interruption, not success.
- Empty `data`, malformed JSON, invalid field encoding, and post-terminal frames
  produce deterministic typed outcomes.

### Identity and reconciliation

- `output_index` is the stable primary slot identity. Item IDs, call IDs, and
  content indices refine identity only when their event contracts provide them.
- Function and custom-tool output items correctly permit an absent item `id`.
  `call_id` is never substituted for item identity.
- Exact duplicate sequence frames are idempotent. Conflicting duplicates,
  conflicting item identity, invalid sequence order, and cross-call completion
  fail closed.
- Delta content is responsive preview data. Channel `.done`, content-part done,
  output-item done, and the terminal response are reconciled in increasing
  authority order.
- An executable call remains unresolved until an authoritative completed output
  item validates it. Delta-only calls cannot execute.
- A terminal completed or incomplete response rejects unresolved executable
  calls. A terminal failed response keeps its nested provider error authoritative
  rather than masking it with partial-call state.

### Item and content coverage

- Messages retain roles, phases, multiple content parts, output text, refusal,
  annotations, logprobs on authoritative completed content, and message
  boundaries.
- Reasoning retains summaries, summary parts, reasoning text, encrypted or
  opaque fields, and compaction items without converting them into executable
  content.
- Function and custom-tool argument streams reconcile by call identity.
- File search, web search, image generation, MCP, MCP tool listing, and code
  interpreter events retain item-scoped lifecycle and canonical completed-item
  data.
- Image partials remain previews; the completed image-generation item is
  canonical.
- Annotation payloads remain exact opaque JSON when the official schema does
  not expose a closed annotation union.
- Response-scoped audio and audio-transcript events are preserved raw and then
  terminate with typed `UnsupportedResponseMedia`. They are not silently
  dropped or flattened into text.

### Terminal and downstream behavior

- `response.completed`, `response.incomplete`, `response.failed`, top-level
  `error`, and the supported Codex terminal path each have explicit decoding.
- Terminal delivery is exactly once inside the parser/mapper. Later frames are
  rejected deterministically. P6 separately owns stopping the network producer
  as soon as terminal state is known.
- Refusal is a first-class, non-retryable model outcome. It remains distinct from
  provider failure through assembly, retry classification, runner output,
  partial-output persistence, CLI output, and TUI dispatch.
- CLI raw streaming emits each complete parsed Responses event rather than a
  lossy synthetic delta projection.
- Timeout, cancellation, and post-LLM hook boundaries retain the latest
  in-flight text or refusal instead of reverting to an older snapshot.

## Adversarial implementation review

A read-only adversarial pass found five material defects before the source
commit was packaged. All five were corrected in `f962a64`:

| Finding | Correction |
|---|---|
| Opaque annotation variants were rejected | Exact annotation JSON is now preserved and reconciled without inventing a closed schema |
| MCP completed/failed events had invented cross-field rules | Rules absent from the public schema were removed; lifecycle identity and optional final error follow the documented contract |
| Optional function/custom item IDs were treated as mandatory | Output-index identity now permits absent item IDs while delta events still enforce IDs where their schema requires them |
| Failed responses could be masked by an unresolved partial call | The terminal nested error is authoritative for `response.failed`; unresolved-call enforcement remains on completed/incomplete terminals |
| A bare CR line ending waited for another byte or EOF | CR is processed immediately and a following LF is swallowed, covering CR, CRLF, and split-boundary cases |

This was an implementation review, not the independent P4 Gate D review.

## Verification record

All commands used the repository's normal `target/` directory. No temporary
build directory, new or command-line lint suppression, or test-only production
bypass was used.

| Check | Candidate result |
|---|---|
| Response reconciler focused suite | 60/60 passed |
| OpenAI provider suite | 544/544 passed |
| Refusal hard-cut persistence/hook regressions | 4/4 passed |
| `cargo test --workspace --all-targets` | Passed; major targets included `norn` 3698/3698, `norn-cli` 485/485, `norn-tui` 682/682, trybuild 1/1 with 11 UI cases, macro tool-argument tests 77/77, and PTY tests 17/17 |
| `cargo test --workspace --doc` | Passed; `norn` 4/4 including four compile-fail cases; all other workspace doc targets green |
| `cargo clippy --workspace --all-targets -- -D warnings` | Passed with strict warnings-as-errors |
| `cargo fmt --all -- --check` | Passed |
| `git diff --check` | Passed |
| Added-bypass scan | Zero added `#[allow]`, `unwrap`, `expect`, `panic`, `todo`, or `unimplemented` in candidate production code |

The source commit changes 54 files with 8,228 insertions and 399 deletions.
Production prefixes remain below the 500-line policy ceiling. The largest
relevant production units are:

| Production unit | Lines |
|---|---:|
| `loop/compaction.rs` | 496 |
| `provider/openai/response_reconciler.rs` | 485 |
| `loop/runner/entry.rs` | 450 |
| `provider/openai/response_reconciler/item_channels.rs` | 437 |
| `provider/openai/response_reconciler/item_channels/authority.rs` | 427 |
| `loop/classify.rs` | 343 |
| `provider/openai/response_stream_event.rs` | 265 |
| `provider/openai/execute.rs` | 217 |
| `provider/openai/sse_parser.rs` | 206 |

## Honest residuals

The following work is intentionally open and remains unchecked in the plan:

- P3 needs a durable response-scoped audio artifact and the accepted complete
  media/content inventory. Raw preservation plus typed rejection is the current
  fail-closed boundary, not feature completion.
- P3 D2, migration/rejection behavior, and complete spawned/forked/cross-session
  transcript fixtures remain open.
- The full refusal matrix still needs tool-loop, structured-output, and resumed
  cases in one retained phase bundle.
- Hosted-search actions, sources, annotations, and answers still need a complete
  tool-loop continuation plus persisted-resume fixture.
- Streamed and non-streamed multimodal output still need a full equivalence
  matrix across duplicates, interleaving, missing deltas, and authoritative
  repair.
- Output-text delta logprobs are raw preview data. Authoritative completed
  content preserves canonical logprobs, but no promise is made that every delta
  logprob is independently replayable.
- The raw terminal envelope distinguishes absent `usage` from present usage.
  The legacy numeric `Usage` projection does not preserve all field-presence
  bits; `USAGE-01` remains assigned to P6.
- Provider WebSocket transport is a separate planned track and is not implemented
  by this candidate. It must reuse this raw envelope and reconciler rather than
  introduce a second event mapper after the P3/P4 contract is accepted.
- Independent streaming, UI/session, and adversarial review is still required,
  followed by the retained P3/P4 phase gates.

## Requested review

Review `128b282..f962a64` as a frozen source candidate. In particular:

1. Re-enumerate the public event and output-item unions from the official docs
   and compare them mechanically with the checked manifests.
2. Trace interleaved and duplicated item, content, and call identities through
   delta, done, output-item completion, and terminal reconciliation.
3. Verify that only authoritative completed executable calls can reach tool
   dispatch and that unknown/unsupported content cannot become ordinary success.
4. Trace pure and mixed refusals through provider, retry, persistence, CLI, and
   TUI boundaries.
5. Re-run the strict workspace battery in the normal repository build directory.
6. Return findings first and issue `READY` only for this implementation candidate;
   do not mark P3 or P4 accepted while the residual checklist remains open.
