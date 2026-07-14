# P1 Gate A contract candidate (2026-07-15)

**Status:** Ratified for P1 implementation entry by the
[`independent Gate A review`](2026-07-15-p1-gate-a-review.md). This document
records finding ownership, invariants, production touch points, and evidence
methods. It closes no source finding and claims no fixture or policy
implementation.

**Phase base:** `2917c8e` (accepted P0 closure documentation on top of the
accepted P0 source and evidence range).

**Scope:** P1 creates executable protocol fixtures and one shared repository
policy authority. It must not change provider behavior. Runtime remediation of
the findings remains assigned to P2-P8.

**Owner dispositions already recorded:**

- D0 remote merge enforcement is deferred to P1 exit. Checked-in local hard
  enforcement and retained evidence are mandatory, and no remote-protection
  claim is permitted while D0 is open.
- P1 may retrieve official public documentation and source. It may not use a
  live provider, credentials, an account identifier, or a billable request.
- No GitHub Actions workflow is introduced under the current D0 ruling.
- Builds, scratch space, and retained evidence stay beneath the repository.

## Ratification boundary

Gate A is satisfied only when the implementer and a fresh domain reviewer agree
this document accurately records:

1. the two protocol dialects and their source provenance;
2. all 62 source-review findings and their one owning remediation phase;
3. the invariants and production seams P1 must protect;
4. the baseline, regression, measurement, or design evidence class for each
   finding; and
5. the P1 fixture, policy, redaction, and clean-checkout evidence method.

Implementation may not convert a qualified or unknown capability below into a
supported claim. A later public-schema or Codex-source change fails the pinned
inventory check and requires an explicit reviewed contract update.

## Source contract

### Public Responses API

The public contract was retrieved through the OpenAI Developer Docs MCP on
2026-07-15. The endpoint is `POST https://api.openai.com/v1/responses`; the MCP
endpoint slice reported OpenAPI 3.1.0 and API description version 2.3.0. Public
documentation does not expose an immutable documentation commit. P1 therefore
records the retrieval date, API-description version, exact source URLs, and the
SHA-256 of each sanitized checked-in extraction.

Primary schema sources:

- [Create a response](https://developers.openai.com/api/reference/resources/responses/methods/create)
- [Responses streaming events](https://developers.openai.com/api/reference/resources/responses/streaming-events)
- [Responses WebSocket `response.create`](https://developers.openai.com/api/reference/resources/responses/websocket-events#response.create)
- [Compact a response](https://developers.openai.com/api/reference/resources/responses/methods/compact)

Primary semantic sources:

- [Message roles and instruction following](https://developers.openai.com/api/docs/guides/text#message-roles-and-instruction-following)
- [Assistant phase](https://developers.openai.com/api/docs/guides/reasoning#phase-parameter)
- [Preserving reasoning across calls](https://developers.openai.com/api/docs/guides/reasoning#preserve-reasoning-across-calls)
- [Conversation state](https://developers.openai.com/api/docs/guides/conversation-state)
- [Compaction](https://developers.openai.com/api/docs/guides/compaction)
- [Streaming responses](https://developers.openai.com/api/docs/guides/streaming-responses)
- [Prompt caching](https://developers.openai.com/api/docs/guides/prompt-caching#requirements)
- [Tools](https://developers.openai.com/api/docs/guides/tools)
- [Web-search output and citations](https://developers.openai.com/api/docs/guides/tools-web-search#output-and-citations)
- [Function-call streaming](https://developers.openai.com/api/docs/guides/function-calling#streaming)

The public contract has these important semantics:

- `system`, `developer`, `user`, and top-level `instructions` remain distinct
  wire forms. `instructions` has higher authority than user input, applies only
  to the current request, and is only roughly equivalent to a developer
  message. Instructions are not inherited through `previous_response_id`.
- Assistant `phase` is an optional public Responses field with
  `commentary` and `final_answer` values. Manual replay preserves an original
  phase when present and distinguishes absence from either value. The guide
  recommends phase for long-running or tool-heavy
  GPT-5.5/GPT-5.4 flows; neither the schema nor guide implies universal model
  emission.
- `previous_response_id` cannot be combined with `conversation`.
- Stateless replay preserves every output item, encrypted reasoning requested
  with `include: ["reasoning.encrypted_content"]`, and assistant phase.
- A refusal is message content, not a transport failure.
- Missing usage remains distinct from usage reported as numeric zero.
- Standalone compaction output is a canonical replay window and is replayed
  without reconstructing or reordering its items.

The pinned public output-item union has 28 variants:

```text
message
file_search_call
function_call
function_call_output
web_search_call
computer_call
computer_call_output
reasoning
program
program_output
tool_search_call
tool_search_output
additional_tools
compaction
image_generation_call
code_interpreter_call
local_shell_call
local_shell_call_output
shell_call
shell_call_output
apply_patch_call
apply_patch_call_output
mcp_call
mcp_list_tools
mcp_approval_request
mcp_approval_response
custom_tool_call
custom_tool_call_output
```

Message output content includes `output_text` and `refusal`. The public
annotation union contains `file_citation`, `url_citation`,
`container_file_citation`, and `file_path`.

The public input union has 32 schema variants. Three distinct schema shapes use
the `message` discriminator and must remain distinct: `EasyInputMessage`
(`user`, `assistant`, `system`, or `developer`), input `Message` (`user`,
`system`, or `developer`), and assistant `ResponseOutputMessage` with optional
phase. The remaining variants are
`file_search_call`, `computer_call`, `computer_call_output`, `web_search_call`,
`function_call`, `function_call_output`, `tool_search_call`,
`tool_search_output`, `additional_tools`, `reasoning`, `compaction`,
`image_generation_call`, `code_interpreter_call`, `local_shell_call`,
`local_shell_call_output`, `shell_call`, `shell_call_output`,
`apply_patch_call`, `apply_patch_call_output`, `mcp_list_tools`,
`mcp_approval_request`, `mcp_approval_response`, `mcp_call`,
`custom_tool_call_output`, `custom_tool_call`, `compaction_trigger`,
`item_reference`, `program`, and `program_output`. `compaction_trigger` is
input-only, must be the final input item, and is not one of the 28 output
variants.

The pinned public SSE registry has 53 event types. Every documented event has a
`sequence_number`:

```text
response.created
response.in_progress
response.completed
response.failed
response.incomplete
response.output_item.added
response.output_item.done
response.content_part.added
response.content_part.done
response.output_text.delta
response.output_text.done
response.refusal.delta
response.refusal.done
response.function_call_arguments.delta
response.function_call_arguments.done
response.file_search_call.in_progress
response.file_search_call.searching
response.file_search_call.completed
response.web_search_call.in_progress
response.web_search_call.searching
response.web_search_call.completed
response.reasoning_summary_part.added
response.reasoning_summary_part.done
response.reasoning_summary_text.delta
response.reasoning_summary_text.done
response.reasoning_text.delta
response.reasoning_text.done
response.image_generation_call.completed
response.image_generation_call.generating
response.image_generation_call.in_progress
response.image_generation_call.partial_image
response.mcp_call_arguments.delta
response.mcp_call_arguments.done
response.mcp_call.completed
response.mcp_call.failed
response.mcp_call.in_progress
response.mcp_list_tools.completed
response.mcp_list_tools.failed
response.mcp_list_tools.in_progress
response.code_interpreter_call.in_progress
response.code_interpreter_call.interpreting
response.code_interpreter_call.completed
response.code_interpreter_call_code.delta
response.code_interpreter_call_code.done
response.output_text.annotation.added
response.queued
response.custom_tool_call_input.delta
response.custom_tool_call_input.done
error
response.audio.delta
response.audio.done
response.audio.transcript.delta
response.audio.transcript.done
```

The following table records minimum cross-event correlation invariants. It is
deliberately non-exhaustive and is not the structural schema registry:

| Event family | Required correlation payload in addition to `sequence_number` |
|---|---|
| output item added/done | `item`, `output_index` |
| content part added/done | `content_index`, `item_id`, `output_index`, `part` |
| output-text delta | `content_index`, `delta`, `item_id`, `logprobs`, `output_index` |
| refusal delta | `content_index`, `delta`, `item_id`, `output_index` |
| function/custom/MCP argument delta | `delta`, `item_id`, `output_index` |
| reasoning summary part added | `item_id`, `output_index`, `part`, `summary_index` |
| reasoning summary text delta | `delta`, `item_id`, `output_index`, `summary_index` |
| reasoning text delta | `content_index`, `delta`, `item_id`, `output_index` |
| annotation added | `annotation`, `annotation_index`, `content_index`, `item_id`, `output_index` |
| terminal response lifecycle | complete `response` object |
| standalone error | required `message`, nullable `code`, nullable `param` |

A reasoning output item requires `id`, `summary`, and `type`. Its `content` is
optional non-null reasoning text, `encrypted_content` is optional nullable, and
`status` is optional non-null. Fixtures keep those absence/null distinctions.

Before fixture implementation, the first P1 foundation produces a sanitized
public-schema extraction covering all 32 input variants, 28 output variants,
53 SSE event structures, 16 tool schemas/18 accepted literals, annotations,
status, usage, include, cache, reasoning, and compaction shapes. Its acquisition
record pins the retrieval date, OpenAPI/API-description versions, exact official
URLs, extractor version, per-source digest, and sanitized output SHA-256. The
schema manifest enumerates every event's required, optional, and nullable fields,
including hosted-search, image, code-interpreter, MCP, audio `response_id`, and
delta/done-specific payloads. A source reviewer must compare the extraction to
the official Developer Docs MCP output and approve its hash before downstream
fixtures are accepted. The checked manifest, not this minimum table, is the
authoritative exhaustive structural pin; missing or additional shapes fail.

The pinned request-tool union has 16 schema variants:

```text
function
file_search
computer
computer_use_preview
web_search
mcp
code_interpreter
programmatic_tool_calling
image_generation
local_shell
shell
custom
namespace
tool_search
web_search_preview
apply_patch
```

The `web_search` schema also accepts discriminator literal
`web_search_2025_08_26`; `web_search_preview` also accepts
`web_search_preview_2025_03_11`. The manifest records 16 schema variants and 18
accepted discriminator literals rather than conflating the counts.

The public response status union is `completed`, `failed`, `in_progress`,
`cancelled`, `queued`, and `incomplete`. No `response.cancelled` SSE event is
documented. Documented incomplete reasons are `max_output_tokens` and
`content_filter`.

`usage` is optional. Its exact paths are `input_tokens`, `output_tokens`,
`total_tokens`, `input_tokens_details.cached_tokens`,
`input_tokens_details.cache_write_tokens`, and
`output_tokens_details.reasoning_tokens`. The generated schema requires
`cache_write_tokens` inside present input details while endpoint examples omit
it, so fixtures accept absence as unknown and never coerce it to zero.

The schema accepts eight `include` values:
`file_search_call.results`, `web_search_call.results`,
`web_search_call.action.sources`, `message.input_image.image_url`,
`computer_call_output.output.image_url`, `code_interpreter_call.outputs`,
`reasoning.encrypted_content`, and `message.output_text.logprobs`. Request prose
enumerates seven and omits `web_search_call.results`. Prompt-cache sources also
disagree on read lookback: the guide says 50 breakpoints while generated
request schema prose says 80. P1 records both source discrepancies rather than
silently selecting one.

Current cache controls are `prompt_cache_key`,
`prompt_cache_options.mode: "implicit" | "explicit"`,
`prompt_cache_options.ttl: "30m"`, legacy/deprecated
`prompt_cache_retention: "in_memory" | "24h"`, and content-block
`prompt_cache_breakpoint: {"mode":"explicit"}`. P1 pins the wire shapes
without claiming that every backend/model supports them.

### ChatGPT/Codex overlay

The overlay is pinned to official `openai/codex` source commit
[`0396f99cf1a27fc87dd12d23403b25e840b6ecbd`](https://github.com/openai/codex/tree/0396f99cf1a27fc87dd12d23403b25e840b6ecbd).
That commit is nine commits ahead of the original review snapshot
`325cf161940c4be5d5792dc09940624ba7543b44`, with the older commit as its merge
base. P1 records both so the source-review-to-remediation drift is visible.

| Overlay source | Blob at the pinned commit | Contract use |
|---|---|---|
| [`codex-rs/core/src/client.rs`](https://github.com/openai/codex/blob/0396f99cf1a27fc87dd12d23403b25e840b6ecbd/codex-rs/core/src/client.rs) | `f5896595c6fe1ec1b477096e5a41548039f673c7` | request construction, per-turn state, WebSocket/prewarm behavior |
| [`codex-rs/codex-api/src/sse/responses.rs`](https://github.com/openai/codex/blob/0396f99cf1a27fc87dd12d23403b25e840b6ecbd/codex-rs/codex-api/src/sse/responses.rs) | `70f96cb855005d577c57fd768062d035cc919b12` | Codex SSE overlay |
| [`codex-rs/codex-api/src/common.rs`](https://github.com/openai/codex/blob/0396f99cf1a27fc87dd12d23403b25e840b6ecbd/codex-rs/codex-api/src/common.rs) | `e4600e26aab62a8495248346cd78ab3cb52b7191` | request/WebSocket shapes, `client_metadata`, and `end_turn` model |
| [`codex-rs/protocol/src/models.rs`](https://github.com/openai/codex/blob/0396f99cf1a27fc87dd12d23403b25e840b6ecbd/codex-rs/protocol/src/models.rs) | `91fd42a5558a3836343ffb94ffef3a7f4050b332` | Codex protocol models |
| [`codex-rs/login/src/server.rs`](https://github.com/openai/codex/blob/0396f99cf1a27fc87dd12d23403b25e840b6ecbd/codex-rs/login/src/server.rs) | `804d05434e231049ffa63709728a5ed8b004e247` | login/account transport context |

Codex-only overlay surfaces are kept separate from the public registry:

- `end_turn` semantics;
- `x-codex-turn-state` as turn-scoped transport state;
- Codex `response.metadata` transport events; and
- `client_metadata` request shape.

Public Response-object `metadata` is not the Codex `response.metadata` event.
Codex turn state is not public Responses threading and must not be represented
as `previous_response_id` or `conversation`.

At retrieval, official `openai/codex` main was one commit ahead of the selected
pin, but all relevant pinned blobs were unchanged. `0396f99` is the immutable
P1 snapshot, not a claim to track repository HEAD.

The pinned Codex client sends `stream: true`, includes encrypted reasoning,
derives a default prompt-cache key from the session ID, adds `client_metadata`,
and sends `store: false` for non-Azure providers. These are known client request
choices, not proof that the backend rejects other forms or that its cache scope
matches the session identifier.

Public documentation and source inspection do not establish the ChatGPT/Codex
backend's support for every public Responses capability. P1 must not claim
support for public threading, conversations, compaction, WebSockets, the full
28-item or 53-event taxonomy, newer tools, or public error behavior without a
pinned Codex source path or a separately owner-approved sanitized capture.
P1 performs no such live capture.

The same restriction applies to account-affinity headers, exact `store: false`
requirements, 401/429/5xx bodies, rate-limit headers, whether a failed request
started execution, cache scope, duplicate/interleaved frames, and post-terminal
behavior. These remain explicit unknowns rather than inferred compatibility.

## Backend and state matrix

| Concern | Public Responses contract | ChatGPT/Codex overlay | P1 treatment |
|---|---|---|---|
| Request authority | system/developer/user/instructions are distinct | Public rules apply only where pinned source agrees | Separate fixtures; no semantic collapse |
| Stored continuation | `previous_response_id` or `conversation` | Not established for Norn's Codex path | Never infer support |
| Stateless continuation | replay all output items, phase, encrypted reasoning | Norn currently depends on stateless replay | Contract fixture, no runtime change |
| Assistant phase | optional public `commentary` / `final_answer` | Codex protocol models round-trip optional phase; provider/model availability varies | Public registry, not overlay-only |
| Turn state | not a public threading primitive | `x-codex-turn-state`, turn-scoped | Separate overlay transport fixture |
| Completion | public terminal response/status | Codex adds `end_turn` behavior | Separate overlay event fixture |
| Metadata | public Response object field | Codex `response.metadata` transport event | Distinct types and fixtures |
| Compaction | public server and standalone contracts | backend support not established | Public target fixture only |
| Error/retry semantics | response/SSE errors partly documented | exact HTTP behavior not established | No invented Codex fixture |
| Cache reporting | public usage fields and guide semantics | scope/reporting not established | Unknown distinct from zero |

## Finding inventory and evidence ownership

P1 supports all 62 source-review findings and closes none itself. P0-owned rows
remain accepted campaign findings; later owning phases remain open. The exact
inventory is grouped below by its one owning remediation phase.

| Owner | Finding IDs | Count |
|---|---|---:|
| P0 | `SEC-01`-`SEC-16`, `SEC-08A`, `BACKEND-01`, `BACKEND-02`, `NF-1`, `NF-2`, `NF-4`, `QUAL-01` | 23 |
| P2 | `AUTH-01`-`AUTH-07`, `CONFIG-01`, `CONFIG-02` | 9 |
| P4 | `STATE-01`, `EVT-01`-`EVT-07` | 8 |
| P5 | `STATE-02`, `STATE-03`, `ROLE-01`, `CODEX-01`, `CODEX-02`, `TRANS-01` | 6 |
| P6 | `TRANS-02`, `USAGE-01`, `NF-3`, `NF-5`, `ROUTE-01` | 5 |
| P7 | `MODEL-01`, `ROLE-02`, `TOOL-01`, `REQ-01`, `SCHEMA-01`, `STRUCT-01` | 6 |
| P8 | `CACHE-01`-`CACHE-05` | 5 |
| **Total** | **unique source-review IDs** | **62** |

The exact evidence-class split is 55 confirmed defects; two gate findings
(`SEC-08A` and `QUAL-01`); one measurement (`CACHE-01`); two design items
(`ROUTE-01` and `STRUCT-01`); one enhancement (`AUTH-07`); and one accepted
limitation (`NF-4`). `AUTH-02` remains a confirmed defect even though P0
partially contained its in-process race.

Source severity is also pinned rather than inferred from evidence class: five
Critical, 25 High, 24 Medium, two Low, two Low/Medium, one Medium/gate, one
Enhancement/security-sensitive, one Design, and one Informational. The strict
registry normalizes these as `critical`, `high`, `medium`, `low`, `low_medium`,
`medium_gate`, `enhancement`, `design`, and `informational` while retaining
exact source-to-row equality.

The checked-in candidate
[`finding-traceability.jsonl`](evidence/p1/finding-traceability.jsonl) has exactly
one line per finding. Each row records ID, source severity, evidence class,
owning phase, fixture category, current seams, campaign closure status,
expectation class, evidence method, unique planned evidence ID, source evidence,
target assertion, and
fixture applicability plus planned fixture IDs. Accepted P0 rows use
`not_applicable_accepted_p0` with an empty array; open rows use `planned` with
one stable ID. Unrelated security/auth findings do not receive invented
Responses fixtures merely to fill a column. The gate rejects missing,
duplicate, unknown, or multiply owned IDs.

## Fixture and regression contract

Fixtures use two explicit dialect manifests, never a permissive union. Codex
fixtures may reference an applicable public scenario explicitly, but unknown
Codex fields cannot silently enter the public contract.

Planned corpus:

```text
crates/norn/testdata/openai_responses/
  contract-pins.json
  backend-state-matrix.json
  index.json
  public/manifest.json
  public/requests/*.json
  public/streams/*.sse
  codex/manifest.json
  codex/requests/*.json
  codex/streams/*.sse
  codex/transport/*.json
crates/norn/tests/openai_contract_fixtures.rs
docs/reviews/evidence/p1/finding-traceability.jsonl
```

Every manifest entry records `id`, `dialect`, `fixture_path`, source references,
categories, finding IDs, owning phase, expectation class, current observation,
target assertions, and secret profile.

Fixture-manifest expectation classes are:

- `supported_green`: current behavior is correct and executable now;
- `baseline_red`: a characterization test proves the exact current defect and
  must be replaced by its owning phase;
- `contract_target`: validates a preregistered source, design, or measurement
  target without claiming current runtime conformance; and
- `dialect_only`: records a backend/transport distinction not yet represented
  in the canonical transcript.

The policy rejects a `baseline_red` entry after its owning phase. A test that
currently proves an unknown event is skipped, for example, characterizes
`EVT-05`; it cannot remain permanent evidence of desired behavior.

The finding registry uses `accepted_evidence`, `baseline_red`, or
`contract_target`. `supported_green` and `dialect_only` apply only to concrete
fixture entries. A later-phase `planned_fixture_id` is a stable namespace
reservation, not a claim that P1 already implements that future regression.

The corpus covers ordered text and multiple assistant phases, reasoning summary
and encrypted content, function and custom calls, refusal, web/file search and
annotations, compaction, usage/cache detail, failures, standalone error,
rate-limit classification, incomplete results, interleaved identities,
duplicate completion, malformed terminal data, unknown items/events, Codex
`end_turn`, turn-state receipt/replay, and Codex metadata transport. Synthetic
robustness inputs are labelled as such and never described as observed OpenAI
output.

## Production touch points

P1 changes no provider behavior, but the fixtures must characterize these seams
so later phases can replace them without moving the contract:

| Seam | Current responsibility | Findings supported |
|---|---|---|
| `provider/openai/request.rs` | request roles, instructions, replay, tools, threading, cache fields | state, role, Codex, request, tool, cache |
| `provider/openai/sse.rs` and `sse_types.rs` | event parsing, item assembly, terminal mapping, usage | event, state, transport, usage |
| `provider/openai/execute.rs` | stream execution and item/call correlation | event, transport, usage |
| `provider/openai/provider.rs` and `backend.rs` | backend capability and request path selection | backend, state, Codex, route |
| `provider/openai/tools.rs` and `schema_downlevel.rs` | tool and schema serialization | tool, schema, model |
| `loop/assembly.rs`, `loop/conversation_state.rs`, `loop/runner/provider_call.rs` | loop assembly, continuation state, provider cancellation | state, event, role, Codex, transport |
| `session/events.rs`, `session/conversion.rs`, `session/persistence/replay.rs` | durable assistant/tool/reasoning representation and replay | state, event, role, Codex |
| `norn-cli/print/stream_renderer.rs`, `print/provider.rs`, `tui/driver.rs` | user-visible streamed text, refusal/tool/error rendering | event, transport, usage |
| `provider/openai_oauth/*`, `provider/auth.rs`, CLI `commands/auth.rs` | credential lifecycle and trusted selection | auth, config, backend |
| `assets/models.json` | model/backend capability declarations | model, cache, route |
| `tools/write.rs`, `tools/edit.rs`, `tools/patch.rs` | first-party staging and atomic publication | P1 hard mutation enforcement |
| `tool/registry.rs`, `tool/lifecycle.rs`, `tool/context.rs` | lifecycle modes/flags and shared child context | P1 non-downgradable enforcement |
| `tools/agent/spawn.rs`, `spawn_context.rs`, `infra.rs` | propagation of one workspace policy coordinator to child loops | P1 cross-agent enforcement |
| `tools/diagnostics_check/{infra,post_check,stop_hook}.rs` and `tools/diagnostics_infra.rs` | current advisory checks, modified-file tracking, empty-stop skip | P1 full/completion parity |
| planned CLI `commands/policy*.rs` and `scripts/p1-gate` | Git snapshot adapter, deterministic rendering/exit, local gate | P1 repository enforcement |
| `CONVENTIONS.toml` | current contradictory whole-file/advisory rules | P1 policy enforcement |

## P1 invariants

1. P1 does not change provider wire behavior, authentication, transcript,
   replay, Responses tool semantics, or retry behavior. It deliberately adds
   the staged first-party mutation enforcement defined by the policy contract.
2. Every fixture has one explicit dialect and source provenance.
3. Public assistant phase remains public contract behavior.
4. Output-item identity, call identity, ordering, output index, and content
   index remain distinct in target assertions.
5. Missing usage remains distinct from a reported zero.
6. Unknown actionable items/events and malformed terminal data have a
   fail-closed target; lifecycle-only allowances are explicit and pinned.
7. Stateless replay preserves every item, encrypted reasoning, and phase.
8. Codex turn state remains turn-scoped transport state, not public threading.
9. Baseline characterizations cannot become permanent accepted behavior.
10. All fixtures, manifests, reports, and retained evidence pass one redaction
    validator.
11. Every `all`, `every`, or `complete` claim ships its generated inventory.
12. Tests and production code add no lint suppression or prohibited bypass.

## Shared policy authority

The exact implementation semantics are in the separate
[`P1 repository-policy contract`](2026-07-15-p1-policy-contract.md). That
contract supersedes the earlier architecture paragraph and is part of Gate A.

In summary, P1 uses one pure evaluator over owned immutable snapshots. CLI and
runtime adapters supply data; the engine owns no Git, filesystem, process,
network, credential, provider, rendering, or exit-status authority. Immediate
first-party enforcement evaluates complete staged `write`, `edit`, and
`apply_patch` overlays before publication through a hard path that no tool flag
or post-validation mode can downgrade. Full evaluation always runs at
task-complete/stop and in the local gate.

The policy contract pins Cargo/`cfg`/module reachability, LOC projection, module
shape, debt fingerprints, writer-sink coverage, evidence redaction, baseline
origin/governance separation, monotonic tightening, traceability, and the local
clean-checkout gate. Computed origin facts are reconstructed from `2917c8e`;
reviewed owner/due/remediation metadata is separate. The local lock is explicitly
tamper-evident rather than tamper-resistant while D0 remains open.

The existing P0 evidence scripts remain frozen historical artifacts. P1 does
not retrofit them into the new authority or weaken policy to match them.

## Evidence and redaction method

P1 evidence is generated by checked-in commands rather than transcribed from a
single run. The final local gate runs from a clean checkout at the exact
candidate commit, uses repository-local `target/` lanes, serializes Cargo, and
records every command, exit status, toolchain, commit, test count, and artifact
hash. Concurrency-sensitive tests run at least 20 times and retain their full
pass/fail distribution.

The redaction validator rejects credentials, bearer/API-key/JWT shapes, real
account identifiers, email addresses, private prompts, absolute home paths,
reusable turn state, raw cache keys, and unregistered opaque values. Fixtures
use reserved synthetic identifiers, `example.invalid`, and non-reusable
sentinels. Negative cases are assembled from harmless fragments at test runtime
so there is no excluded unsafe-fixture directory.

Gate C remains exactly the strict repository gate in the plan, including:

- phase-specific and touched-crate integration tests;
- `cargo fmt --all -- --check`;
- `cargo clippy --workspace --all-targets -- -D warnings`;
- workspace all-target and doc tests;
- phase-base diff check and added-line audit;
- shared repository/post-mutation policy parity; and
- fixture/evidence redaction validation.

No `#[allow]`, `#[expect]`, ignored test, warning downgrade, alternate base,
reduced scope, advisory mode, or exclusion flag is an accepted gate path.

## Gate A review questions

The independent reviewer must answer each item explicitly:

- Do the public source inventory and semantic claims match the current official
  OpenAI reference?
- Do the Codex commit/file/blob pins establish each overlay claim, with public
  Response metadata kept distinct from Codex transport metadata?
- Are all 62 IDs unique, correctly owned, and correctly evidence-classified?
- Do the fixture expectation classes prevent a known defect from being blessed
  as desired behavior?
- Do the touch points cover request, stream, persistence, replay, loop, auth,
  user surface, and policy enforcement without claiming P1 runtime changes?
- Can the shared policy engine enforce identical repository and post-mutation
  semantics without acquiring provider, subprocess, or credential authority?
- Is every completeness claim paired with a checkable inventory?
- Are the D0 deferral and no-live-provider restrictions stated without implying
  remote protection or Codex capabilities that have not been demonstrated?

Only an explicit reviewer agreement, followed by correction of any finding,
permits the universal Gate A agreement checkbox and the P1 source-contract work
items to be marked complete.
