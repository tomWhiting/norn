# Responses API implementation review (2026-07-10)

**Status:** Review complete. Findings are documented; runtime remediation is not
implemented by this review.

**Reviewed snapshot:** `main` at `263cc4f`.

**Primary scope:** the OpenAI Responses implementation used by both the ChatGPT
/ Codex subscription login and the direct Responses API. The review covers
credential routing, request construction, prompt roles, prompt caching,
conversation state, streaming events, response-item persistence and replay,
usage accounting, and tool/schema serialization.

**Sources:** current Norn source and tests, the OpenAI Responses and prompt-cache
documentation as published on 2026-07-10, and the current `openai/codex` request,
SSE, and protocol-model source. Public API details are time-sensitive and should
be rechecked when remediation begins.

---

## Executive verdict

Norn has several strong Responses-specific foundations. It uses top-level
`instructions` for the stable System prompt, sends Developer and tool items in
the Responses `input` array, keeps function/custom call IDs distinct, uses
`store: false` for the ChatGPT/Codex subscription backend, requests encrypted
reasoning for stateless replay, and now places volatile per-iteration context at
the request tail rather than ahead of all history.

The central weakness is architectural: Norn still models a Responses transcript
as a Chat-Completions-like sequence of flat messages. A Responses turn is an
ordered array of typed output items. Norn reduces it to one text string, a group
of reasoning items, and a group of local tool calls, then synthesizes a new wire
transcript on the next request. That is not lossless enough for the ChatGPT
`store: false` path. It drops assistant `phase`, item ordering, refusals,
annotations, hosted-tool records, and most current or future item types.

After the credential boundary, the highest-priority correction is therefore not
"add every missing SSE event." It is to make an ordered, durable Responses item
transcript canonical and derive Norn's display text and executable local calls
from that transcript.

The July 9 prompt-cache fix closes the known GPT-5.5 prefix-placement defect, but
its benefit has not been demonstrated on the wire. It is specifically uncertain
for GPT-5.6, whose default implicit cache breakpoint is the latest message. In
Norn, that latest message is volatile and is removed and recreated on the next
iteration. Norn also reports every GPT-5.6 cache write as zero, so the current
telemetry cannot establish whether the new layout saves money or creates
billable cache writes that are never reused.

---

## Finding index

| ID | Severity | Status | Finding |
|---|---|---|---|
| `SEC-01` | Critical | Open | Project-controlled `base_url` can redirect a Codex OAuth bearer token and account header. |
| `STATE-01` | High | Open | Stateless ChatGPT replay is lossy and changes the provider's ordered output-item transcript. |
| `STATE-02` | High | Open | Replaceable Developer context accumulates server-side under `previous_response_id`. |
| `EVT-01` | High | Open | A refusal can become a successful empty response. |
| `EVT-02` | High | Open | Assistant `phase`, message boundaries, content indices, and item ordering are destroyed. |
| `EVT-03` | High | Open | Hosted web-search actions, sources, and annotations are discarded and not replayed. |
| `CODEX-01` | High | Open | ChatGPT/Codex `end_turn` is ignored. |
| `CODEX-02` | High | Open | ChatGPT/Codex `x-codex-turn-state` is ignored. |
| `TRANS-01` | High | Open | Cancellation drops the consumer future but can leave the detached HTTP task running. |
| `TRANS-02` | High | Open | In-stream rate limits bypass both the HTTP-429 retry loop and the default loop retry policy. |
| `CACHE-01` | High | Unproven | The tail-placement cache win is not established for GPT-5.6 implicit breakpoints. |
| `CACHE-02` | High | Open | GPT-5.6 `cache_write_tokens` and cache-write cost are recorded as zero. |
| `EVT-04` | Medium | Open | Complete text/item events cannot repair a dropped or malformed delta. |
| `EVT-05` | Medium | Open | Unknown actionable item variants fail open and may be silently omitted. |
| `CACHE-03` | Medium | Open | Ephemeral roots, `--no-session`, spawned agents, and forks can omit `prompt_cache_key`. |
| `CACHE-04` | Medium | Open | Per-iteration variable expansion can mutate tool definitions and invalidate the whole prefix. |
| `CACHE-05` | Medium | Open | Current GPT-5.6 cache controls and content breakpoints have no typed representation. |
| `MODEL-01` | Medium | Open | Catalog reasoning-summary defaults and parallel-tool capability are not honored on the wire. |
| `BACKEND-01` | Medium | Open | An explicit ChatGPT URL is classified as the direct API rather than the Codex backend. |
| `SCHEMA-01` | Medium | Open | Schema downleveling can drop root `$defs` while retaining dangling `$ref` values. |
| `USAGE-01` | Medium | Open | Usage from failed attempts is discarded; missing usage silently becomes zero. |
| `AUTH-01` | Medium | Open | Norn's own login reads a flat account claim instead of the namespaced Codex claim. |
| `AUTH-02` | Medium | Partially fixed | Refresh is single-flight in-process but still races across Norn/Codex processes. |
| `AUTH-03` | Medium | Open | Credential-load and proactive-refresh failures are hidden as absence or stale-token fallback. |
| `AUTH-04` | Low/Medium | Open | The browser reports login complete before token exchange and durable storage. |
| `AUTH-05` | Low/Medium | Open | A remote revoke failure prevents local credential deletion. |
| `STRUCT-01` | Medium | Design tradeoff | Native Responses structured output is replaced with a synthetic function tool. |

Two previously suspected transport issues are in better shape at this snapshot:
`server_is_overloaded` and `slow_down` now carry a retryable 503 classification,
and a clean EOF without a terminal event now becomes `StreamInterrupted`. Those
should remain regression tests, not open findings.

---

## 1. Credential and backend boundary

### SEC-01: OAuth credentials can be sent to a project-selected origin

**Severity:** Critical.

Norn automatically loads project settings from `.norn/settings.json`
(`crates/norn/src/config/loader.rs:59-69`). `provider.base_url` flows through CLI
assembly without an origin trust check
(`crates/norn-cli/src/config/overrides.rs:295-317`). When no API-key environment
variable is selected, the OpenAI Responses provider still chooses OAuth
(`crates/norn-cli/src/print/provider.rs:154-181`). The explicit URL becomes the
request endpoint (`crates/norn/src/provider/openai/provider.rs:106-116`), after
which `Authorization: Bearer ...` and `chatgpt-account-id` are attached
(`crates/norn/src/provider/auth.rs:153-174`).

A cloned repository can therefore select an attacker-controlled HTTPS endpoint
and receive the user's Codex bearer token. Atomic auth storage and fixed OAuth
token/revoke endpoints do not mitigate this request-origin problem.

**Recommendation:** bind ChatGPT OAuth credentials to a compiled allowlist of
normalized HTTPS origins and paths. Arbitrary `base_url` must require API-key
auth, or an explicit user-level trusted-provider configuration that cannot be
introduced by repository config. Do not solve this with a warning alone.

### BACKEND-01: backend identity is inferred from the absence of an override

`is_chatgpt_backend` returns true only for OAuth plus `base_url: None`
(`crates/norn/src/provider/openai/provider.rs:118-137`). Explicitly setting the
canonical ChatGPT URL therefore changes behavior to direct-API semantics:
response threading and server compaction are enabled, service-tier lookup uses
the direct backend, and `store` changes.

Backend identity, credential authority, and endpoint URL are separate concepts.
They should be represented separately. A normalized URL comparison is better
than the current heuristic, but an explicit backend enum with an allowlisted
endpoint is the safer long-term model.

### AUTH-01 through AUTH-05

Norn's self-hosted login decodes `chatgpt_account_id` as a flat JWT claim
(`crates/norn/src/provider/openai_oauth/jwt.rs:29-40`). Current Codex tokens place
that field under the `https://api.openai.com/auth` object. The token endpoint's
top-level `account_id`, or an existing Codex `tokens.account_id`, masks the bug;
Norn's fallback path does not (`login_server.rs:339-350`).

In-process refresh is now correctly single-flight through a mutex and epoch
(`openai_oauth/manager.rs:215-250`). Cross-process instances still load separate
snapshots and can rotate the same refresh token concurrently. Atomic rename in
`storage.rs` protects file integrity, not refresh-token ownership.

Credential-load errors are discarded by `.ok().flatten()`
(`openai_oauth/manager.rs:129-133`). Proactive transient refresh failures are
also ignored while the stale credential is returned (`manager.rs:202-212`). The
resulting user error can say "no OAuth token found" instead of reporting a
malformed or unreadable auth file.

The browser receives "Login complete" before code exchange and storage
(`openai_oauth/login_server.rs:153-171,219-226`). Logout deletes `auth.json` only
after successful remote revocation (`openai_oauth/revoke.rs:43-50`), so a network
or authority failure leaves the local credential installed.

**Recommendations:** parse both namespaced and legacy flat claim shapes; add an
interprocess reload-lock-refresh-save transaction; preserve typed storage and
refresh errors; show browser success only after durable save; and always clear
local credentials while separately reporting remote revocation status.

---

## 2. Request construction and role semantics

### Current wire shapes

For the ChatGPT/Codex OAuth backend, capabilities disable response threading.
Each request is effectively:

```text
instructions: stable System prompt
input:        full locally reconstructed transcript
              + current user/tool input
              + fresh managed Developer context at the tail
tools:        current resolved tool definitions
store:        false
include:      ["reasoning.encrypted_content"]
cache key:    persisted session id when available
```

For the direct Responses backend after the first stored response:

```text
instructions: stable System prompt, resent every request
previous_response_id: last response id
input:        only local items after the response-thread cursor
              + fresh managed Developer context at the tail
store:        true
```

### What is correct

System messages are concatenated into top-level `instructions`
(`crates/norn/src/provider/openai/request.rs:109-121`). This is the right use of
the field. OpenAI explicitly states that prior `instructions` do not carry over
with `previous_response_id`, so resending the System prompt is required rather
than redundant.

Developer messages remain typed input messages (`request.rs:122-124`), user
messages use `input_text`, and function/custom call outputs retain the provider
`call_id`. `store: false` plus `reasoning.encrypted_content` for stateless
ChatGPT replay is also correct (`request.rs:144-163`).

Putting current dynamic context in a Developer role after the user message is
not an authority inversion. Role priority is more important than chronological
position. The role is appropriate for environment and harness instructions that
must outrank user content.

One naming issue remains: sections delivered through Norn's
`SystemContextAppend` path are ultimately combined into the managed Developer
message, not serialized as a System message. If callers rely on the name as an
authority guarantee, either the naming or the wire role should be made explicit.

### STATE-02: local replacement is not provider-side replacement

`ManagedDevMessage::detach` removes the old Developer item only from Norn's
local vector (`crates/norn/src/loop/dev_context.rs:84-107`). A fresh item is
appended at the tail (`runner/prompt.rs:175-183`). In provider-threaded mode,
`request_messages` sends only the System prefix plus items after the local cursor
(`loop/conversation_state.rs:162-171`).

The provider referenced by `previous_response_id` still retains every prior
Developer input item. Each timestamp, collaboration mode, rule set,
prompt-command output, and environment snapshot therefore becomes append-only
server history. Norn's token estimate and local prompt view cannot see those
stale items.

This does not affect the default ChatGPT OAuth path because that path correctly
sets `response_threading: false` (`provider/openai/provider.rs:163-170`). It does
affect direct Responses users and any explicit ChatGPT URL currently
misclassified as direct.

**Recommendation:** give replaceable context explicit state semantics. In
threaded mode, either place replaceable material on a truly replaceable request
surface, reset the response anchor and replay a cleaned transcript when it
changes, or disable threading for this prompt design. Local deletion must not be
treated as deletion from provider state.

### STATE-01: stateless replay violates the ordered-item contract

OpenAI's conversation-state guidance says a stateless reasoning client should
append every item from `response.output` to the next input, preserving encrypted
reasoning and assistant `phase` values.

Norn instead creates one `AssembledResponse` containing flat `text`, flat
`thinking`, a `Vec<ReasoningItem>`, and a `Vec<AssembledToolCall>`
(`crates/norn/src/loop/assembly.rs:32-52`). It persists one flat
`SessionEvent::AssistantMessage` and later serializes all reasoning first, one
assistant text message second, and all calls last
(`provider/openai/request.rs:348-389`).

That reconstruction can differ materially from the original output sequence.
A valid provider sequence such as:

```text
reasoning -> commentary message -> function call -> reasoning -> final message
```

becomes:

```text
all reasoning -> one phase-less combined message -> all function calls
```

The problem is correctness first and cache fidelity second. The reconstructed
suffix becomes a future request prefix, but it is not the prefix the provider
originally produced.

**Recommendation:** persist an ordered `Vec<ResponseItem>` or equivalent raw
tagged representation per response. Treat normalized assistant text, reasoning
display, and executable local calls as derived projections. Preserve unknown
items as raw JSON so protocol additions do not silently disappear.

### MODEL-01 and STRUCT-01

The model catalog says current models default reasoning summaries to `none` and
support parallel tool calls (`assets/models.json:17-40`). Request construction
turns a missing summary into `auto` and hard-codes `parallel_tool_calls: false`
(`provider/openai/request.rs:139-142,174-188`). The latter is currently required
by assembly's order-based call-completion correlation
(`loop/assembly.rs:81-95`), but it should be described as a Norn limitation, not
as provider capability.

Norn implements requested structured output as a synthetic function tool,
whereas current Codex uses the Responses `text.format` field. The synthetic tool
is defensible for provider portability and loop-level validation, but it expands
the cached tool prefix, forces tool/nudge semantics onto final output, and passes
through the function-schema downleveler. Responses-native structured output
should be evaluated as the primary path, with the tool strategy retained only
where its control-flow behavior is intentional.

---

## 3. Prompt caching

### Confirmed improvement from `aecae78`

Before July 9, the managed dynamic Developer message sat near the start of the
input. Its second-resolution timestamp changed every iteration, so exact-prefix
caching could not extend into the growing history. Commit `aecae78` moved the
whole dynamic container to the tail. This fixes the placement class rather than
one volatile field and is the right direction for pre-GPT-5.6 automatic prefix
caching.

The incident and measured ANKS impact are retained in
`docs/PROMPT-CACHE-INVALIDATION-FIX.md`. That document should remain the incident
record; this review adds the model-version and transcript-fidelity constraints.

### CACHE-01: GPT-5.6 changes the acceptance question

OpenAI's current GPT-5.6 behavior uses an implicit breakpoint on the latest
message unless request-wide mode is `explicit`. Norn's latest message is the
volatile Developer tail, and Norn deletes that item before constructing the next
request. The prior breakpoint prefix therefore does not necessarily occur in the
next request at all.

For GPT-5.5, "stable history before a changing tail" is enough to expect a
longer matching prefix. For GPT-5.6, it is not enough to infer that the service
will read the stable prefix or avoid a new billable write. Official Codex still
uses a thread-derived cache key without explicit breakpoints, so blindly adding
public-API cache fields to the private ChatGPT backend is also not justified.

**Verdict:** do not revert the tail change, but do not call it validated for the
new default model family. Measure it against the real backend before choosing a
5.6 policy.

### CACHE-02: current telemetry cannot answer the question

GPT-5.6 reports cache writes separately and bills them at a higher rate than
ordinary uncached input. Norn parses `cached_tokens` but hard-codes
`cache_write_tokens: 0` (`provider/openai/sse.rs:465-491`). The system therefore
cannot distinguish:

- a useful write followed by repeated reads;
- a write that is never reused;
- a full miss;
- a backend that does not report the field.

Missing usage should not collapse to the same value as a reported zero. Capture
presence as well as value, and preserve attempt-level usage before changing
cache policy.

### CACHE-03: cache keys are not universal

Managed persisted sessions correctly use the session ID as
`prompt_cache_key` (`agent/builder.rs:454-466`). This matches current Codex, which
defaults the key to its thread ID.

`AgentLoopConfig::default()` leaves the key unset. `--no-session` installs an
in-memory store without assigning one (`norn-cli/src/runtime/from_cli.rs:172-191`).
Spawned and forked agents resolve a default child loop config even though each
has a stable child UUID and, for persisted parents, a real child session
(`tools/agent/spawn.rs:383,503-530`; `tools/agent/fork_tool.rs:270,297-324`).

Current GPT-5.6 guidance says a key is needed for its more reliable matching
path. Every agent execution should therefore have a stable runtime/thread key
independent of disk durability. Persistent session IDs are suitable; ephemeral
roots and children can use their already-minted runtime IDs.

### CACHE-04: tool definitions are part of the cache prefix

Norn expands tool-description variables and rebuilds the provider tool surface
before every request (`loop/runner/prompt.rs:121-130`;
`loop/expansion.rs:37-53`). Shell variables with no TTL, computed variables, or a
changing `working_dir` can mutate the serialized `tools` array. OpenAI requires
tools to remain identical for a cache hit, so this invalidates the prompt before
message placement matters.

Resolve session-stable tool definitions once, or explicitly classify variables
allowed in tool descriptions as stable. Fingerprint the serialized tool surface
per request and include that fingerprint in cache diagnostics.

### CACHE-05: typed cache controls lag the current API

`ResponsesApiPayload` contains legacy `prompt_cache_retention` but always sets it
to `None`. There is no typed `prompt_cache_options`, and Norn's string-only
message content cannot attach `prompt_cache_breakpoint` to an `input_text`
block. Raw provider options can inject request-wide fields, but they cannot add a
content-block marker through the current message model.

For GPT-5.6, add typed, capability-gated controls only after the ChatGPT backend
has been tested. Keep legacy retention limited to models/backends that support
it. The current OpenAI guide and API reference disagree on whether 50 or 80
historical breakpoints are considered; Norn should encode neither number as a
client invariant.

### Required cache experiment

Run a real 20-call tool loop against both `gpt-5.5` and the current GPT-5.6
Codex-login model. Record one row per request with:

| Field | Purpose |
|---|---|
| model/backend/request number | Separate backend and model behavior. |
| prompt-cache key hash | Verify stable routing without logging the raw key. |
| instructions hash | Detect accidental System drift. |
| tool-surface hash | Detect schema, order, or description drift. |
| ordered input-item type/hash list | Prove the actual prefix, not a normalized message approximation. |
| input/output tokens | Establish total work. |
| cached-read tokens | Measure reuse. |
| cache-write tokens and field presence | Measure write cost and reporting support. |
| latency to first event and completion | Measure user-visible benefit. |

Compare at least four variants: current implicit tail, no dynamic message,
stable Developer message, and an explicit stable breakpoint where the backend
accepts it. Include hosted search, reasoning, and a variable-expanded tool
description in separate cases. Do not combine all volatility into one run or the
source of a miss will be unknowable.

---

## 4. Streaming events and output items

### Coverage summary

The public Responses reference checked on 2026-07-10 documents 52 streaming
event types. Norn maps 12, explicitly ignores or partially consumes 13, and
lets 27 fall through the unknown-event path. The raw count is not itself the
bug; many lifecycle/progress events are safe to ignore.

The public `ResponseOutputItem` union currently has 28 variants. Norn's
`response.output_item.done` switch recognizes four: `function_call`,
`custom_tool_call`, `reasoning`, and compaction aliases. Compaction is then
discarded during assembly. A completed `message` is not preserved as an item.

| Coverage | Event families | Assessment |
|---|---|---|
| Mapped | `response.completed`, `failed`, `incomplete` | Terminal handling exists, except Codex `end_turn` metadata is lost. |
| Mapped but lossy | output-text and reasoning `delta`/`done` | Text survives, but IDs, indices, parts, and phase do not. |
| Mapped | function/custom tool-input deltas and completions | Call-ID handling is comparatively strong. |
| Partially consumed | `output_item.added` | Used only for tool `item_id` to `call_id` correlation. |
| Explicitly ignored | response/content/reasoning lifecycle events | Mostly harmless UI/progress loss. |
| Explicitly ignored | hosted web-search lifecycle | Material because Norn advertises hosted search. |
| Unknown | refusal and annotation events | Immediate correctness and provenance defects. |
| Unknown | file search, code interpreter, image, audio, computer, MCP, native shell | Unsupported capabilities; must stay unadvertised until end-to-end support exists. |

### EVT-01: refusal becomes empty success

`response.refusal.delta` and `.done` are unknown. The completed message item is
also ignored by the `output_item.done` discriminator
(`provider/openai/sse.rs:267-361,458-460`). `response.completed` then produces an
ordinary `EndTurn`; a response with no tool calls is classified as a valid text
stop. A provider refusal can therefore surface as a successful empty answer.

Refusal should be a typed terminal outcome carrying the refusal text and policy
metadata available on the wire. It must never be indistinguishable from a model
that intentionally returned an empty answer.

### EVT-02: phases and order are operational data

Text deltas lose `item_id`, `output_index`, and `content_index`
(`sse.rs:198-205`) and are globally concatenated (`loop/assembly.rs:98-113`). The
durable `Message` has no `phase` or content-part model
(`provider/request.rs:151-190`).

OpenAI specifically warns that missing `phase` can make GPT-5.5 treat an
intermediate update as a final answer. Current Codex retains
`MessagePhase::Commentary` and `MessagePhase::FinalAnswer` in its canonical
`ResponseItem` model. Norn should do the same and preserve multiple assistant
messages around calls rather than merging them.

### EVT-03: hosted web search is advertised but not preserved

Norn serializes a native `web_search` tool (`provider/openai/tools.rs:33-65`) but
explicitly ignores its lifecycle events (`provider/openai/sse.rs:451-453`) and
does not handle the `web_search_call` output item. It also skips
`response.output_text.annotation.added`.

The final answer's plain text generally survives. The search action, sources,
URL citations, and item needed for exact stateless replay do not. This is an
end-to-end capability mismatch on a tool Norn currently exposes, not a future
feature request.

### EVT-04: authoritative completion data is thrown away

Norn maps `response.output_text.done` into `TextComplete`, but assembly ignores
that event (`loop/assembly.rs:203-211`). It also ignores the completed message
item. Malformed SSE JSON is warned and dropped (`provider/openai/sse.rs:162-185`).
One missing delta can therefore truncate text even when the server later sends
the complete text in both a `.done` event and a completed item.

Assembly should reconcile deltas against authoritative completion data. A
mismatch should be observable; complete data should repair a missing delta where
safe.

### EVT-05: actionable protocol additions fail open

Unknown events and unknown `output_item.done` variants return `None`. That is
appropriate for informational lifecycle events, but unsafe for a new actionable
item: the loop can report completion after silently omitting something the model
asked to execute.

Preserve unknown output items as raw tagged values. Capability gating should
require complete request serialization, stream parsing, persistence, replay,
execution, and UI behavior before a tool family is advertised.

### Recommended event architecture

Do not add 52 bespoke handlers to the current flat model. Use two coordinated
streams of information:

1. Canonical ordered items from `output_item.added/done`, persisted in provider
   order and replayed unchanged for `store: false`.
2. Incremental deltas for live UI and partial-output recovery, keyed by item and
   content indices and reconciled with canonical completion items.

This is the shape current Codex uses. It ignores many low-value lifecycle events
too, but it drives the turn from typed `ResponseItem` values rather than losing
them.

---

## 5. Codex-specific turn and transport behavior

### CODEX-01: `end_turn` is ignored

Current Codex parses optional `response.completed.response.end_turn`. Norn
instead infers `ToolUse` only when the last output item type is
`function_call`; every other completed response becomes `EndTurn`
(`provider/openai/sse.rs:503-521`).

For the ChatGPT backend, `end_turn` is explicit server guidance about whether
the client should finish or continue the current turn. Preserve it in
`ProviderEvent::Done` and define how it interacts with local tool calls,
refusals, and no-output continuations.

### CODEX-02: `x-codex-turn-state` is ignored

Current Codex captures `x-codex-turn-state` from HTTP headers or
`response.metadata` and replays it on later requests within the same user turn.
Norn does not expose response headers to the Responses mapper and explicitly
ignores `response.metadata`. Multi-request tool loops therefore omit the
backend's sticky-routing token.

Add typed turn metadata with an explicit lifetime. It must be reused within a
turn and cleared between turns; treating it as session-global would be another
routing bug.

### TRANS-01: cancellation does not own the detached producer

The loop comment says dropping the provider future aborts reqwest
(`loop/runner/provider_call.rs:36-69`). `OpenAiProvider::stream`, however,
detaches `sender.execute` with `tokio::spawn`
(`provider/openai/provider.rs:195-203`). Dropping the receiver is noticed only
on a later send. A task blocked on headers or the next SSE chunk can continue
until its timeout, consuming resources after the user sees cancellation.

Return a stream guard that aborts or cancels the producer on drop, avoid the
detached task, or select all transport waits against receiver closure. Current
Codex's `ResponseStream::drop` cancels the mapper task and is a useful reference.

### TRANS-02: retry ownership is split incorrectly

HTTP 429 responses are retried inside `StreamExecutor`. An in-band
`response.failed` with `rate_limit_exceeded` becomes `ProviderError::RateLimited`,
but the default loop retry policy excludes rate-limited errors. The same
condition is retried or not retried solely based on whether it arrived before or
after SSE began.

Choose one retry owner and carry attempt/budget metadata so an in-band error is
not mistaken for a provider-exhausted HTTP retry. `server_is_overloaded` and
`slow_down` now have retryable 503 classification; retain that improvement.

`emit_mapped` also stops only for `Done`, not `Err`
(`provider/exec.rs:454-477`). The consumer returns on the error, but the detached
producer can continue until its next failed send, EOF, or timeout. Treat a
mapped error as terminal immediately after delivery.

---

## 6. Schema, usage, and payload gaps

### SCHEMA-01: downleveling can produce dangling references

The schema flattener copies nested property schemas unchanged
(`provider/openai/schema_downlevel.rs:236-272`) and builds a new root containing
only type, description, properties, required, and optional
`additionalProperties` (`schema_downlevel.rs:284-310`). A property that still
contains `$ref: "#/$defs/..."` loses the root `$defs` it references.

Preserve referenced definitions or resolve/inline local references before
flattening. When a schema cannot be lowered, fail locally with a typed diagnostic
instead of knowingly sending a shape the provider rejects.

### USAGE-01: attempted spend is not represented

`response.failed` discards any terminal usage. Loop retries return only the
successful attempt's usage. Missing values silently become zero, and cost is
always `None` in the Responses parser (`provider/openai/sse.rs:465-491`).

Separate successful-response usage from total attempted/billed usage. Preserve
field presence and provider-reported detail, including cache reads, cache writes,
and failed-attempt usage. This is necessary for both budget enforcement and the
cache investigation.

### Other payload capabilities

Norn does not currently expose the full public request surface: native
`text.format`, truncation, prompt templates, input image/file content, most
hosted tools, background mode, and newer reasoning/cache controls are absent or
available only through raw provider options. That is acceptable if the provider
advertises only the subset Norn handles end to end.

The problem is not incomplete API feature count. It is inconsistency between
advertised capability and round-trip support. Hosted web search currently crosses
that line; unadvertised image, audio, MCP, computer, file-search, and code
interpreter events do not yet.

---

## 7. Remediation sequence

### Phase 0: credential containment

1. Reject OAuth plus non-allowlisted `base_url` before constructing a provider.
2. Separate backend identity from endpoint override and auth source.
3. Parse the namespaced account claim and add a redacted real-shape fixture.
4. Add cross-process credential locking and typed auth-load errors.

This phase should ship independently and first.

### Phase 1: canonical Responses transcript

1. Introduce a provider Responses item type with typed core variants and an
   opaque unknown variant.
2. Persist ordered output items on the session event, with a versioned migration
   or backward-compatible optional field.
3. Derive display text, reasoning summaries, local tool calls, and stop behavior
   from the item transcript.
4. Replay original items in original order for `store: false`, removing only
   server-internal IDs that the target backend rejects.
5. Preserve message phase, content-part indices, annotations, refusal, hosted
   search, and compaction items.

This phase fixes multiple findings at once and should precede broad event-family
expansion.

### Phase 2: conversation-state semantics

1. Decide whether replaceable dynamic context is compatible with provider
   threading.
2. Reset or disable threads when local context removal cannot be reflected on
   the provider.
3. Add a two-turn test proving stale Developer context is absent from effective
   state.
4. Preserve `end_turn` and turn-scoped sticky metadata.

### Phase 3: cache instrumentation and policy

1. Parse cache-write usage before changing request policy.
2. Assign stable keys to ephemeral roots, children, and forks.
3. Hash instructions, tools, and ordered input items in debug telemetry.
4. Run the GPT-5.5/GPT-5.6 A/B matrix described above.
5. Add explicit breakpoints only for model/backend pairs proven to accept and
   benefit from them.

### Phase 4: transport and model controls

1. Make stream cancellation own the HTTP producer.
2. Unify HTTP and in-band retry policy.
3. Terminate producers immediately after mapped errors.
4. Resolve reasoning-summary defaults from the selected catalog entry.
5. Correlate tool completions by item ID before enabling parallel calls.
6. Evaluate native `text.format` for Responses-native structured output.

---

## 8. Required conformance tests

| Test | Required assertion |
|---|---|
| OAuth origin containment | Project config cannot cause a Codex bearer token to leave the allowlisted ChatGPT origin. |
| Ordered stateless replay | Every prior output item is replayed in original order with phase and encrypted reasoning intact. |
| Multi-message phase | Commentary and final-answer messages remain distinct across a tool iteration. |
| Refusal | Refusal text becomes a typed non-success outcome, never empty success. |
| Hosted web search | Search-call item, sources, annotations, and answer survive persistence and the next stateless request. |
| Unknown item | An unknown actionable item is persisted opaquely and prevents unsupported execution from being reported as ordinary success. |
| Delta reconciliation | A missing text delta is repaired by `.done`/completed item data and emits a mismatch diagnostic. |
| Threaded dynamic context | The second request cannot see the first request's replaceable environment/rules after they are removed locally. |
| Codex end-turn | `end_turn: false` and `true` drive distinct, explicit loop behavior. |
| Turn-state lifetime | Sticky state is reused within one turn and never leaked into the next. |
| Cancellation | Dropping/canceling the stream promptly terminates the server-observed request or connection. |
| In-band rate limit | A streamed rate-limit failure consumes the intended retry budget exactly once. |
| Cache-key coverage | Persistent, ephemeral, spawn, and fork paths all send a stable non-secret key. |
| GPT-5.6 accounting | Reported cache-write tokens survive SSE parsing, persistence, and total-usage aggregation. |
| Tool stability | Tool surface hash remains stable unless a deliberate tool/schema/config change occurs. |
| `$defs` schema | Lowering never emits a dangling local `$ref`. |
| Cross-process refresh | Two processes sharing one rotating refresh token perform one authority exchange and converge on one stored credential. |

---

## 9. Official references

- [OpenAI prompt caching](https://developers.openai.com/api/docs/guides/prompt-caching)
- [OpenAI conversation state](https://developers.openai.com/api/docs/guides/conversation-state)
- [OpenAI reasoning and assistant phase](https://developers.openai.com/api/docs/guides/reasoning#phase-parameter)
- [OpenAI Responses create reference](https://developers.openai.com/api/reference/resources/responses/methods/create)
- [OpenAI Responses streaming events](https://developers.openai.com/api/reference/resources/responses/streaming-events)
- [Current Codex request builder](https://github.com/openai/codex/blob/main/codex-rs/core/src/client.rs)
- [Current Codex Responses SSE parser](https://github.com/openai/codex/blob/main/codex-rs/codex-api/src/sse/responses.rs)
- [Current Codex response-item model](https://github.com/openai/codex/blob/main/codex-rs/protocol/src/models.rs)
- [Current Codex login server](https://github.com/openai/codex/blob/main/codex-rs/login/src/server.rs)

The official prompt-cache guide and Responses API reference currently disagree
on the historical-breakpoint lookback count. This review deliberately avoids
depending on either number.
