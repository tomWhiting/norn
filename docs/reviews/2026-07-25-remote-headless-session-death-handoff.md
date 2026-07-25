# Remote headless session deaths: investigation handoff

Date: 2026-07-25

Repository: `/Users/tom/Developer/ablative/norn`

Branch inspected: `main`

HEAD inspected: `97cd24e6e554de585fa1cf47c03b9ec1455d0b91`

Incident packet:
`/Users/tom/Downloads/norn-full-export-20260724`

Investigation mode: read-only. No production code, tests, configuration, session
data, credentials, worktrees, or Git state were changed. This document is the
only file created by the investigation and is intentionally uncommitted.

## Executive conclusion

The exact root cause of the fourteen remote headless session deaths cannot be
determined from the available export.

That is not a statement that the failure is outside Norn. The reported timing,
the known live HTTP 400, and the request-shape changes establish ample reason to
investigate Norn's updated request path. The limitation is narrower: the export
does not contain the failed outbound request, the provider's error payload, the
process stderr, the process exit status, or a durable terminal failure event.
Every one of those artifacts is absent at the precise boundary where the
failure occurred.

The export does prove all of the following:

1. All fourteen dead sessions durably completed an ordinary tool result.
2. All fourteen then stopped before durably accepting another provider
   response.
3. The stopping point is not tied to one tool, one iteration count, one token
   count, one output size, or one elapsed time.
4. The packet's proposed duplicate-wire-emission theory is false. Norn does
   not emit both the canonical and normalized copies of an assistant response.
5. Recent Norn work did materially change stateless Codex continuation
   requests. Commit `128b282` began replaying canonical response items
   verbatim.
6. That canonical replay currently retains server response-item IDs and fields
   such as function-call `status`.
7. The current implementation contradicts its own adjacent documentation,
   which says those server-internal IDs are deliberately omitted.
8. The current Codex reference client removes response-item IDs by default for
   `store: false`; its `item_ids` feature is under development and disabled by
   default.
9. The packet nevertheless contains 537 observed successful continuations
   through Norn's same canonical-replay branch. Therefore the presence of those
   fields alone does not prove why the next request in each dead session
   failed.
10. Norn deliberately drains and discards non-2xx response bodies and replaces
    them with `response body omitted`. Provider failures also leave no terminal
    session row. That combination destroys the evidence required to classify
    this incident after the fact.

The canonical-item replay mismatch is a real post-update implementation defect
or contract inconsistency that must be resolved independently. It is not
identified here as the cause of these deaths because the available evidence
does not support that stronger claim.

## Terminology correction

The phrase "Norn replays Codex Subscription history" was imprecise and caused
understandable confusion.

Norn does not read or replay conversation history belonging to the Codex CLI,
Codex desktop application, or ChatGPT. Norn does not import Codex session
files. OAuth supplies authentication and account authority; it does not supply
another client's transcript.

What Norn does is resend Norn's own current conversation on each stateless
Responses request:

- the Norn-owned system instructions;
- the original Norn user message;
- prior provider response items captured by Norn;
- prior tool calls;
- tool outputs produced inside Norn; and
- any later Norn conversation messages.

This is necessary because the Codex-subscription provider advertises
`response_threading: false`, so Norn selects `store: false` and cannot use a
provider-side `previous_response_id` chain for the conversation.

Relevant implementation:

- `crates/norn/src/provider/openai/provider.rs:251-265`
  - Codex subscription explicitly reports `response_threading: false`.
- `crates/norn/src/loop/conversation_state/request_state.rs:94-97`
  - `store()` is the threaded-mode flag.
- `crates/norn/src/loop/conversation_state/request_state.rs:183-202`
  - non-threaded mode returns the complete Norn message vector.
- `crates/norn/src/provider/openai/request.rs:115-149`
  - the request builder lowers every Norn message into Responses `instructions`
    or `input`.

## Evidence packet inspected

The packet manifest identifies:

- 14 dead sessions;
- 3 survivor controls;
- complete `trace.jsonl` files;
- per-session artifact directories where present; and
- per-tool output files where present.

The packet did not contain:

- an outbound Responses request dump;
- a provider response or error-body dump;
- captured stderr from the Norn process;
- the Norn process exit code;
- a shell/supervisor termination reason;
- an OS crash report;
- a durable Norn terminal error event;
- the remote binary hash;
- the remote Norn Git commit;
- the exact remote command line;
- the resolved remote Norn configuration;
- a debug API JSONL file; or
- a network/proxy trace.

### Files enumerated

The investigation enumerated all files below the packet root. Apart from
`.DS_Store`, they were:

- `MANIFEST.txt`;
- 17 `trace.jsonl` files;
- output logs for selected calls;
- one spool artifact under dead session `I3`; and
- one spool artifact under survivor `F1`.

No additional hidden diagnostic stream was present.

## Session-by-session terminal evidence

`output bytes` below is the JSON-serialized size of the final `ToolResult`
output in the exported trace, not the request size.

| Class | Label | Assistant/tool pairs | Final tool | Last input tokens | Last output tokens | Final output bytes | Elapsed |
|---|---:|---:|---|---:|---:|---:|---:|
| dead | F3 | 59 | search | 142,966 | 135 | 238 | 691.251s |
| dead | F3b | 10 | read | 36,762 | 125 | 4,380 | 67.082s |
| dead | F3c | 10 | read | 33,750 | 67 | 11,498 | 51.185s |
| dead | FD2 | 27 | apply_patch | 69,902 | 4,181 | 2,949 | 370.321s |
| dead | FD2b | 37 | apply_patch | 96,717 | 757 | 2,767 | 586.539s |
| dead | FD3 | 13 | edit | 34,073 | 107 | 364 | 265.356s |
| dead | FD3b | 9 | read | 34,526 | 71 | 4,626 | 93.649s |
| dead | FD3c | 10 | edit | 30,739 | 1,354 | 2,808 | 133.806s |
| dead | I3 | 12 | read | 49,748 | 80 | 3,307 | 59.579s |
| dead | L1 | 15 | read | 42,792 | 212 | 11,209 | 76.938s |
| dead | L1r | 23 | bash | 64,146 | 139 | 2,390 | 139.485s |
| dead | L2 | 57 | read | 123,610 | 81 | 3,600 | 352.215s |
| dead | L3 | 16 | read | 35,761 | 81 | 4,813 | 76.253s |
| dead | L5 | 19 | search | 50,344 | 378 | 6,435 | 129.026s |
| survivor | D2 | 98 | structured_output | 166,080 | 1,357 | 10 | 1,558.409s |
| survivor | F1 | 55 | structured_output | 67,198 | 2,676 | 10 | 786.468s |
| survivor | L2s | 84 | structured_output | 179,848 | 6,449 | 10 | 877.682s |

Important interpretation:

- Every exported assistant response is followed by its matching tool result.
- Every dead trace ends on an ordinary `ToolResult`.
- Every survivor ends on the expected `structured_output` result with
  `"accepted"`.
- A dead trace has no later `ProviderEpochBoundary`, provider-state provenance
  row, `AssistantMessage`, partial-output row, cancellation row, or terminal
  error row.
- The dead runs stop across six different final tool names.
- Dead runs range from 9 to 59 assistant/tool pairs.
- Last reported input usage ranges from 30,739 to 142,966 tokens.
- Final exported tool-result size ranges from 238 to 11,498 bytes.
- Elapsed time ranges from approximately 51 seconds to 11.5 minutes.
- Survivor sessions exceed several dead sessions in iteration count, token
  usage, output usage, and duration.

This rules out a single fixed iteration limit, a single fixed token threshold,
a single final-output-size threshold, a single wall-clock timeout, and a
tool-specific crash as explanations supported by this packet.

## Trace structure and request-boundary finding

For each successful model iteration in these traces, the durable group is:

1. `ProviderEpochBoundary`;
2. `Custom` event with `event_type: "provider.state.provenance"`;
3. `AssistantMessage`; and
4. `ToolResult`.

The provenance rows report:

- version `2`;
- `stored: false`; and
- a prompt-seed SHA-256 value.

This fingerprints the stateless Codex-subscription path after the recent
provider-state publication work.

At the end of every dead trace, step 4 exists and the next step 1 does not.
Consequently, the interruption occurred after Norn durably stored the tool
result and before Norn durably published another accepted provider response.

That interval contains several distinguishable runtime classes which become
indistinguishable in the exported trace:

- request construction or replay validation fails locally;
- a pre-LLM hook rejects or fails;
- the outbound request receives a non-2xx response;
- the stream fails before a complete response is accepted;
- the step is cancelled or timed out before durable response publication;
- the process panics, aborts, is killed, or loses its supervisor; or
- the process exits after returning an error which is only printed externally.

This list is a classification of logically possible boundaries, not a ranking
or diagnosis.

## Packet duplication lead: disproven

The manifest correctly observes that each persisted `AssistantMessage` carries
two representations:

- canonical provider items in `response_items[].item`; and
- normalized compatibility projections in `reasoning[]` and `tool_calls[]`.

The manifest asks whether the request builder emits both, producing duplicate
IDs on the wire.

It does not.

In `crates/norn/src/provider/openai/request.rs:377-385`,
`serialize_assistant_into` checks whether `response_items` is nonempty. If it
is, the function appends only the canonical items and immediately returns.
The normalized `reasoning` and `tool_calls` loops are fallback logic and are
not reached.

Therefore:

- persistence contains redundant views;
- the current request builder chooses one view;
- the packet does not show duplicate item IDs in an outbound request; and
- "both persisted views are emitted" is not the cause.

## Identity and pairing checks

Across all 17 exported sessions:

- 554 assistant messages were observed;
- 554 matching tool results were observed;
- 1,076 canonical response items had item IDs;
- no canonical response-item ID was duplicated within a session history;
- 554 function calls had call IDs; and
- no function-call `call_id` was duplicated within a session history.

Canonical item shapes were:

- 522 reasoning items with keys
  `content, encrypted_content, id, summary, type`;
- 554 function calls with keys
  `arguments, call_id, id, name, status, type`.

The relevant tool result in each trace matches a prior call by `call_id`.
No missing, empty, or reused call ID was found in the exported histories.

This rules out the packet's histories already containing a simple duplicate-ID
or unpaired-call defect.

## Definite post-update request-shape change

Commit:

`128b2826` - `feat(responses): preserve canonical output transcripts`

Date:

2026-07-16 23:06:06 +1000

Git blame assigns
`crates/norn/src/provider/openai/request.rs:378-384` to this commit.

### Before `128b282`

Assistant replay was reconstructed from normalized Norn fields:

- reasoning items included `type`, `summary`, and `encrypted_content`;
- optional reasoning `content` was included when present;
- reasoning item `id` was omitted;
- function calls included `type`, `call_id`, `name`, and `arguments`;
- function-call item `id` was omitted; and
- function-call `status` was omitted.

### After `128b282`

If canonical response items exist, Norn clones each raw provider JSON item
unchanged into the next request:

```rust
if !msg.response_items.is_empty() {
    input.extend(
        msg.response_items
            .iter()
            .map(|transcript_item| transcript_item.item.raw().clone()),
    );
    return;
}
```

For the exported sessions this retains:

- `rs_*` reasoning item IDs;
- `fc_*` function-call item IDs;
- `status: "completed"` on function calls;
- empty reasoning `content: []`; and
- any other provider-returned item field not removed elsewhere.

### Internal contradiction

The current documentation immediately above the raw-clone branch says:

- the server-internal `rs_*` ID is deliberately omitted; and
- the `fc_*`/`ctc_*` item ID is server-internal and is not echoed.

The production branch does the opposite whenever canonical items exist.

This is not an interpretive concern. The code and its stated wire contract
directly disagree.

## Codex reference implementation comparison

Reference repository inspected:

`/Users/tom/Developer/tools/harness/codex`

Files inspected:

- `codex-rs/core/src/client.rs`;
- `codex-rs/core/src/context_manager/history.rs`;
- `codex-rs/core/src/context_manager/normalize.rs`;
- `codex-rs/protocol/src/models.rs`; and
- `codex-rs/features/src/lib.rs`.

Relevant reference behavior:

- `codex-rs/core/src/client.rs:830-837` removes every response-item ID when
  `store` is false unless the `item_ids_enabled` feature is active.
- `codex-rs/features/src/lib.rs:1155-1159` marks `item_ids` as under
  development and disabled by default.
- The typed Codex `ResponseItem::FunctionCall` serializer does not carry the
  response-side `status` field in the same way as Norn's raw JSON replay.
- Empty optional reasoning content is skipped by the typed serializer.
- Codex's history normalization includes explicit call/output pairing logic.

The Norn request therefore differs from the reference client's normal
`store: false` request in at least these respects:

- Norn retains provider item IDs;
- Norn retains response-side function-call `status`; and
- Norn retains raw optional/empty fields.

This comparison establishes a parity mismatch. It does not, without the
rejected request or provider error, establish which field caused these deaths.

## Why the canonical-replay mismatch is not declared the cause

The exported histories themselves contain 537 successful continuation
boundaries before their final stopping points. Those continuations were built
from earlier assistant/tool pairs using the same canonical raw-replay branch.

The final assistant/tool pairs have the same broad item taxonomy and key shapes
as earlier successful pairs:

- reasoning item;
- function call;
- matching function-call output; and
- no duplicate item or call identity.

No failure-only field or item type was found in the final pairs.

Accordingly:

- the raw-replay change is definitely present;
- the mismatch with Norn's comments and Codex's default client behavior is
  definite;
- it was introduced by recent Norn work;
- it must not be dismissed; but
- this packet cannot demonstrate that it is the event which terminated these
  fourteen sessions.

Declaring it the root cause would convert a strong correlation and a real
defect into a fabricated causal conclusion.

## Other recent request-path work inspected

### Codex turn state and client metadata

Commit:

`64e5585` - `feat(responses): retain Codex state within a turn`

Files inspected:

- `crates/norn/src/provider/openai/codex_turn.rs`;
- `crates/norn/src/provider/openai/execute.rs`;
- `crates/norn/src/provider/openai/request.rs`;
- `crates/norn/src/provider/openai/provider.rs`;
- `crates/norn/src/provider/turn.rs`;
- `crates/norn/src/loop/runner/provider_call.rs`; and
- `crates/norn/src/loop/runner/setup.rs`.

This commit added, for the trusted Codex-subscription path:

- `client_metadata` with Norn session/thread/turn identifiers;
- capture of `x-codex-turn-state` from response headers or metadata;
- reuse of the first accepted turn-state value on later requests in the same
  logical Norn turn;
- redaction of that value from lower-trust event/debug sinks; and
- a protected-option check preventing user configuration from overriding
  `client_metadata`.

The current context uses one `OnceLock` and first-value-wins behavior for the
turn-state header.

The packet does not contain:

- the response headers;
- the captured turn-state presence/value;
- the outbound continuation headers;
- the generated `client_metadata`; or
- the failed request.

No failure-only turn-state or metadata condition can therefore be extracted
from the packet.

The runs also contain many successful requests after their first response,
which means adding these fields does not produce an unconditional immediate
failure in the exported executions.

### Durable provider-state transitions

Commit:

`97f63a5` - `feat(p5): make provider state transitions durable`

Files inspected included:

- `crates/norn/src/loop/conversation_state.rs`;
- `crates/norn/src/loop/conversation_state/request_state.rs`;
- `crates/norn/src/loop/runner/provider_call.rs`;
- `crates/norn/src/loop/runner/prompt.rs`;
- `crates/norn/src/loop/runner/setup.rs`;
- `crates/norn/src/provider/openai/request.rs`; and
- provider-state publication/session files named in the commit diff.

This work accounts for the durable boundary and provenance rows seen in the
export. For the exported `stored: false` path it does not enable
`previous_response_id`; the full Norn conversation remains the request input.

The missing next publication group proves the next provider response was never
durably accepted. It does not say why.

### D8 prompt-authority work

Commit:

`4fa6c67` - `feat(responses): complete D8 prompt authority`

The scoped inspection covered request/prompt/conversation-state changes,
including:

- stable prompt seed handling;
- managed system and developer context placement;
- prompt-command context;
- replay validation;
- stateless managed-tail construction; and
- the request-level `instructions` separator change.

The packet exposes prompt-seed hashes but not the resolved prompt messages or
outbound payload. No failure-only prompt-seed transition or instruction shape
was found in the exported session events.

### Other relevant Responses commits enumerated

The investigation enumerated recent history on the OpenAI request, transport,
terminal, replay, and state paths, including:

- `d86a4ed` - honor Codex completed-item authority;
- `b967967` - pin empty Codex terminal success;
- `acfcb69` - close D3 state review gaps;
- `201f4b5` and `ef3b9c7` - preserve safe resume anchors;
- `c3a7aa1` - reject unreplayable Codex state;
- `de92211` - redact nested Codex turn state;
- `98b0266` - honor Codex `end_turn`;
- `e448133` - abort request producer when stream drops;
- `ab26632` - reject orphan core preview deltas;
- `ad9fffe` - preserve caller ownership through replay;
- `7429490` - authoritative output-item contracts;
- `65dc1d5` - atomic terminal reconciliation;
- `4b70a53` and `f962a64` - P4 event/conformance work; and
- `27df51d` - encrypted reasoning replay.

No claim is made that every line in those commits was audited. They were
enumerated to define the changed surface and to identify request-affecting
commits for focused inspection.

## OpenAI public documentation checked

Official Responses conversation-state guidance was checked:

`https://developers.openai.com/api/docs/guides/conversation-state#manually-manage-conversation-state`

The public API guidance for manual stateless state tells clients to preserve
the response output items and append them to subsequent input.

That explains the motivation for preserving canonical output. It does not
settle the exact private Codex-subscription wire contract:

- the public guide and the Codex reference client's default ID stripping are
  not identical behaviors;
- Norn targets the Codex-subscription backend under OAuth;
- the failed request and provider error are absent; and
- the investigation did not infer private-backend behavior solely from public
  API documentation.

## Error handling and the observability failure

File inspected:

`crates/norn/src/provider/exec.rs:292-369`

For a non-success status other than the separately handled cases:

1. Norn streams the response body to a sink;
2. it does not buffer, persist, or expose the provider error payload;
3. after the drain it creates `ProviderError::StreamError`; and
4. the rendered reason is:
   `HTTP {status} from {backend}; response body omitted`.

The stated security motivation is valid: an authority-controlled error body
may echo prompts, tool content, or credentials. The operational consequence is
also now proven: Norn retains no bounded, structured, non-sensitive error code
or request correlation sufficient to diagnose this failure.

File inspected:

`crates/norn/src/loop/runner/provider_call.rs:30-81`

The provider error propagates with `?` out of the step. The trace already ends
at the previous tool result, and no durable terminal failure event records:

- error class;
- HTTP status;
- retry disposition;
- request correlation;
- provider request stage; or
- process outcome.

The combination means an exported session cannot distinguish a provider 400
from several other failures at the same boundary.

## Relationship to the separately observed live errors

Two live errors were reported during the surrounding work:

1. A response rendered text and then failed with:
   `Responses protocol violation: completed response item was absent from
   terminal response.output`.
2. A later fresh invocation failed with:
   `stream error: HTTP 400 Bad Request from responses; response body omitted`.

The completed-item terminal-policy issue had a separate correction and review.
It is not the terminal signature present in this packet because the packet has
no error output at all.

The live HTTP 400 proves that the updated Norn build has received at least one
client-error response from the Responses endpoint. It does not prove that all
fourteen packet sessions ended for the same reason. The packet has no stderr,
status, or body with which to make that link.

The owner's report that the Responses service was healthy and fast, the same
account worked elsewhere, and the failures began after the Norn update is
accepted as incident context. Service health does not make an invalid Norn
request valid; conversely, it does not identify which Norn request field was
invalid.

## Theories explicitly ruled out by current evidence

The following must not be presented as the cause:

- Norn reading Codex CLI or desktop conversation history;
- both persisted assistant representations being emitted on the wire;
- duplicate canonical response-item IDs in the exported histories;
- duplicate function-call IDs in the exported histories;
- an unpaired final function-call output;
- one specific final tool implementation;
- a fixed maximum-iteration limit;
- a fixed token threshold visible in the usage data;
- a fixed exported tool-output-size threshold;
- a fixed wall-clock timeout;
- the OpenAI Responses service being unavailable; or
- the canonical item-ID mismatch by itself.

The last entry is important: it is a definite defect/inconsistency, but the
available evidence cannot promote it to the incident's proven cause.

## Unresolved boundaries

The following remain unclassified because the packet lacks the discriminating
artifact:

- whether each dead run returned HTTP 400;
- whether any failed before dispatch during request serialization or replay
  validation;
- whether turn-state or client metadata was present on the failed request;
- whether the provider rejected an item ID, response-only field, tool schema,
  instruction shape, model option, or another field;
- whether the process was externally terminated;
- whether the headless wrapper dropped stderr or an exit envelope;
- whether all fourteen deaths share one cause; and
- whether the fresh `hi` HTTP 400 and the long-running deaths are the same
  defect.

## Minimum evidence required for a definitive next conclusion

At least one failed execution needs a single correlated diagnostic bundle:

1. exact Norn binary hash or Git commit;
2. exact command line and resolved account/model/backend selection;
3. process exit code and termination signal, if any;
4. captured stdout and stderr;
5. the exact outbound request body with secrets and reusable routing state
   redacted;
6. the HTTP status;
7. a bounded, sanitized provider error classification or request ID;
8. whether request construction completed;
9. whether the request was dispatched;
10. whether response headers arrived; and
11. the final durable session event ID.

An outbound request dump alone would allow a byte-level comparison against:

- the previous successful request in the same session;
- the pre-`128b282` normalized serializer;
- the current Norn raw serializer; and
- the current Codex reference serializer.

An HTTP error code/body classification or provider request ID would determine
whether the provider rejected the request and why. A process exit status would
distinguish that from an external kill or panic.

## Recommended next investigation sequence

This is a handoff sequence, not work performed by this investigation.

1. Do not modify the exported packet.
2. Reproduce one headless failure with stdout, stderr, and exit status retained.
3. Enable or add a narrowly redacted outbound request capture for that run.
4. Capture request-stage markers before serialization, before dispatch, after
   headers, and after terminal response.
5. Preserve the provider request ID and a structured non-sensitive error code.
6. Compare the failed request with its immediately preceding successful
   request.
7. Run the same conversation projection through:
   - current raw canonical replay;
   - normalized replay with response-item IDs removed;
   - normalized replay with response-only fields removed; and
   - the Codex reference request preparation.
8. Change one wire property at a time. Do not treat a passing retry with
   multiple simultaneous changes as causal proof.
9. Add a durable terminal failure record so future session exports carry the
   failure stage and classification without exposing provider bodies.
10. Separately reconcile the raw canonical replay code with its own documented
    ID-omission contract and the Codex reference default.

## Scope not investigated

Norn is a large workspace. This investigation did not attempt a whole-codebase
audit and must not be represented as one.

Areas not exhaustively inspected include:

- every CLI/headless orchestration path;
- every wrapper or supervisor used on the remote host;
- all hooks and conventions active on the remote host;
- all cancellation and timeout paths;
- OS resource exhaustion on the remote host;
- all authentication/account-selection UX;
- MCP execution;
- tool implementation internals;
- the complete persistence system;
- unrelated D8 message-delivery durability;
- all model catalog entries;
- all environment-variable resolution;
- all process/signal handling;
- remote system logs;
- remote network intermediaries; and
- code not named in the focused request, replay, transport, and runner paths.

The unrelated requirement to spell `--account default` when only one account
exists was noted by the owner. It was intentionally not investigated or
changed because the requested scope was solely the unexplained session deaths.

## Inspection commands and methods

Read-only inspection used:

- `find` to enumerate the export;
- `sed`, `head`, and `tail` to inspect the manifest, traces, and source;
- `rg` to locate request, state, transport, debug, and session-export paths;
- `git log`, `git show`, `git diff`, and `git blame` for request-path history;
- small in-memory Node scripts to parse JSONL and calculate counts,
  uniqueness, terminal event types, usage ranges, output sizes, and elapsed
  time; and
- direct source comparison with the local Codex reference clone.

No build, test, clippy, formatter, network request, login, process termination,
installation, commit, push, checkout, reset, stash, or worktree operation was
performed.

## Final handoff statement

The current evidence supports two simultaneous statements:

1. Recent Norn work introduced a concrete stateless continuation wire-shape
   change whose production behavior, documentation, and Codex-reference parity
   are inconsistent.
2. The available export cannot prove that this change, or any other specific
   mechanism, caused the fourteen remote process deaths.

The correct next action is not another speculative patch. It is to retain one
failed outbound request, its sanitized provider classification, and its process
exit outcome, then perform a controlled one-property comparison. Until that
exists, any more specific root-cause claim would be invented.
