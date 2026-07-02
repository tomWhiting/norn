# Norn Driven Protocol (`norn-driven/1`)

**Status:** normative — this document is the consumer-neutral source of truth
for Norn's driven JSON-RPC mode (`norn --protocol jsonrpc`). Code comments in
`crates/norn-cli/src/print/jsonrpc/` and `crates/norn-cli/src/print/driven.rs`
cite the section names of this document. Any external design document (e.g.
aion-side worker-adapter notes) is descriptive of a *consumer*, never of this
contract.

**Contract version:** `norn-driven/1`, advertised as the `protocol` field of
the `initialize` result. The `jsonrpc: "2.0"` tag on every frame names only
the JSON-RPC envelope framing, NOT this contract; consumers gate on
`protocol`. The version is bumped when the contract changes incompatibly.

---

## Transport and framing

- The Norn process's **stdin+stdout** form a single bidirectional JSON-RPC
  2.0 channel. stderr stays human logs (tracing) and never carries frames.
- Frames are **newline-delimited JSON**: one complete JSON object per line,
  written atomically. Blank lines between frames are ignored on read.
- Exactly ONE task owns stdout (the single serializing writer); every
  outbound frame — `event/*` notifications, `intervene/*` acks, and the
  terminal `run/execute` response — funnels through it, so two producers can
  never interleave-corrupt a line.
- Inbound frames with a wrong `jsonrpc` tag are answered `-32600`; a missing
  tag is tolerated. Unparseable lines are answered `-32700` with `id: null`.
  Neither is fatal to the channel.
- Inbound **notifications** (frames without an `id`) are not served; they
  are logged and ignored.

## Initialize and capabilities

`initialize` (request) may be sent at any time, including mid-run — it is
idempotent and read-only. The result:

```json
{
  "protocol": "norn-driven/1",
  "serverInfo": { "name": "norn", "version": "<crate version>" },
  "capabilities": {
    "methods": ["initialize", "run/execute"],
    "events": [
      "event/message", "event/toolCall", "event/toolResult",
      "event/progress", "event/stop", "event/raw"
    ],
    "interventions": ["inject_message", "cancel"],
    "runLifecycle": "one_shot"
  }
}
```

- `capabilities.methods`: the request methods served inbound.
- `capabilities.events`: the notification methods emitted outbound.
- `capabilities.interventions`: the neutral intervention primitives Norn can
  serve mid-run. Primitives not listed (e.g. `pause_resume`,
  `update_budget`, `respond_to_approval`) are unsupported and answered
  `-32601` — the capability gate.
- `capabilities.runLifecycle: "one_shot"`: the channel serves exactly one
  `run/execute` per process (see below).

## One-shot run lifecycle

1. The peer sends `initialize` (recommended, not required) and then exactly
   one `run/execute` request. Params: `{ "prompt": "<string>" }` (`input`
   is accepted as an alias). stdin is the JSON-RPC channel, so it is never
   read as a prompt in driven mode.
2. The agent runs. While it is in flight, the channel streams `event/*`
   notifications outbound and serves `intervene/*` requests inbound.
3. The single terminal result is returned as the **Response whose `id`
   matches the `run/execute` request**, and only as that response — never
   as a notification. It is emitted after every `event/*` notification of
   the run is on the wire.
4. The process then drains its writer and exits. The channel is one-shot:
   there is no second run on the same process.

Guarantees on the acceptance boundary:

- **Every accepted `run/execute` is answered.** Any failure after
  acceptance — output-schema parse, runtime assembly, provider
  authentication, or the run itself — is returned as the id-matched
  **error** response (`-32603`, message carrying the typed CLI error text)
  before the process exits. The peer never observes EOF in place of a
  Response.
- A `run/execute` with no string prompt is answered `-32600` and the
  pre-run loop keeps serving (the request was not accepted).
- A prompt that resolves entirely to a local slash command (no agent call)
  is answered with a success Response whose `result` is `null` — the run
  was accepted and served, but there is no step envelope to report.
- A **second** `run/execute` while a run is in flight is answered with the
  invalid-state error `-32000` ("run already active …"), never `-32601`:
  the method exists, the channel is busy. After the terminal response the
  process is exiting; further requests are not read.
- If the peer closes stdin before sending `run/execute`, the process exits
  0 with nothing to do.

## Event notifications

Every `AgentEvent` the run emits is streamed live as a JSON-RPC
notification (no `id`). The `method` carries the coarse semantic category
(`event/message`, `event/toolCall`, `event/toolResult`, `event/progress`,
`event/stop`, `event/raw`); the `params` are the byte-identical stream-json
payload of the event (`type`-tagged), with two fields added:

- `agent_id` (UUID string) and `agent_role` (string) — attribution, since
  multi-agent runs interleave events from several agents.

Delta events are always forwarded on this channel (the `--partial` render
flag does not apply to the transport). Events with no on-wire form
(provider `Error`, unserialisable payloads) are skipped; the run's failure
still reaches the peer through the terminal response.

Notifications and responses are structurally disjoint: a notification never
carries an `id`; a response never carries a `method`.

## Interventions

While the run is in flight, the same stdin reader keeps being read and the
following requests are served:

### `intervene/injectMessage`

Params: `{ "text": "<string>", "priority": "normal" | "interrupt" }`.

- `text` is required (`-32600` otherwise). `priority` defaults to
  `"normal"`; an unknown value is `-32600`.
- `"interrupt"` steers the running agent at the next tool boundary;
  `"normal"` is a queued turn that batches to a stop boundary.
- The injection is attributed to the OPERATOR (nil sender id, literal
  `operator` label) and can never impersonate a peer agent.
- Ack: `{ "status": "injected", "priority": ... }`. A delivery failure
  (agent inbound channel full/closed) is answered `-32603` with the reason.

### `intervene/cancel`

Params: `{ "reason": "<string>" }` (optional; defaults to
`"cancelled by operator"`).

- Trips the run's cancellation token; the run returns at its next boundary.
- Ack: `{ "status": "cancel_requested", "reason": ... }`. After a
  successful cancel ack the intervene reader stops; the terminal response
  follows.

### Capability gate

Any other `intervene/*` method — and any unknown method mid-run — is
answered `-32601`. `initialize` mid-run is re-served with the capabilities;
`run/execute` mid-run is `-32000` (above).

### Cancel acknowledgement semantics

`"cancel_requested"` acknowledges that the **cancellation signal was
applied**, not that the run's outcome will be `cancelled`. There is an
inherent race at run completion: a cancel that lands after the run has
reached its own terminal outcome (but before the channel winds down) is
still acked `cancel_requested`, and the terminal `run/execute` response
then reports the actual outcome (e.g. `stop.reason: "completed"`). The
terminal response's `stop.reason` is ALWAYS authoritative; consumers must
not infer the run outcome from a cancel ack. The window is already
narrowed as far as the architecture allows: the intervene reader honours
the run-finished stop signal before dispatching a request that arrived in
the same tick, and it is stopped and joined before the terminal response
is emitted.

### Degraded intervention mode

If the run's control channel cannot be assembled (the harness message
router fails to resolve — an assembly invariant that should not fail in
practice), the channel still reads stdin for the duration of the run and
answers **every** `intervene/*` request with `-32603` carrying the
unavailability reason. Peer requests never sit unread until EOF. The
condition is error-logged on stderr.

## Stop envelope

The terminal `run/execute` **result** is the same structured envelope
`norn -p -f json` prints, and the stream-json `completed` event carries the
same contract fields. Shape (envelope version 1):

```json
{
  "envelope_version": 1,
  "stop": { "reason": "<snake_case reason>", ...detail },
  "output": <value | null>,
  "usage": {
    "input_tokens": 0, "output_tokens": 0,
    "cache_read_tokens": 0, "cache_write_tokens": 0,
    "cost_usd": 0.0
  },
  "model": "<model id>",
  "session_id": "<id | null>",
  "events": [ ...session events of this step ],
  "diagnostics": [ ... ]
}
```

`stop` is internally tagged on `reason`:

| `stop.reason` | detail fields | `output` holds | process exit code |
|---|---|---|---|
| `completed` | — | the final output value | 0 |
| `schema_unreachable` | `attempts` (u32), `validation_errors` (string[]) | best attempt, if any | 1 |
| `max_iterations` | — | `null` | 1 |
| `timed_out` | `elapsed_ms` (int), `iterations` (int) | partial output, if any | 1 |
| `cancelled` | — | `null` | 1 |
| `truncated` | `truncation` (`max_tokens` \| `content_filter`), `iterations` (int) | partial text, if any | 1 |

The stream-json `completed` event is
`{ "type": "completed", "envelope_version": 1, "stop": {...}, "output": ...,
"usage": { "input_tokens", "output_tokens" } }`, emitted after any
`{"type":"diagnostic",...}` lines.

**There is deliberately NO `retryable` field.** Whether a stop is worth
retrying is the *caller's* judgment — it depends on the caller's budget,
policy, and how it values the partial — and Norn will not encode that
judgment into the wire contract. Consumers branch on `stop.reason` (plus
the detail fields, e.g. `truncation`) and decide for themselves. This
supersedes the earlier `stop: {reason, retryable}` proposal in
`docs/OPTION-B-WORKER-KILL-DURABILITY-SCOPING.md` (Gap C).

## Error codes

| code | meaning |
|---|---|
| `-32700` | parse error (invalid JSON line); `id: null` |
| `-32600` | invalid request (bad `jsonrpc` tag, missing/invalid params) |
| `-32601` | method not found / unadvertised intervention primitive |
| `-32603` | internal error: run failure on the accepted `run/execute`, intervention delivery failure, degraded intervention mode |
| `-32000` | invalid state: `run/execute` while a run is already in flight (one-shot lifecycle) |

## Shutdown handshake

- Pre-run EOF (stdin closes before `run/execute`): the writer is drained
  and joined; exit 0.
- After the terminal response is enqueued, every writer handle is dropped,
  the writer task drains its queue to stdout and exits, and the process
  exits with the CLI exit code (0 for `completed`, non-zero otherwise, 2
  for argument errors, 3 for auth errors — the id-matched error response
  has already been delivered in the failure cases).
- Mid-run EOF on stdin only stops the intervene reader; the run continues
  to its own terminal result, which is still written to stdout.
