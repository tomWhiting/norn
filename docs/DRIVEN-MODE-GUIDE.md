# Driven mode (`--protocol jsonrpc`) — a consumer's guide

How to drive Norn as a subprocess over JSON-RPC: spawn it once, run sequential
agent steps, stream events, intervene mid-run, and read each result. This is
the practical companion to the normative contract in
[`docs/design/norn-cli/DRIVEN-PROTOCOL.md`](design/norn-cli/DRIVEN-PROTOCOL.md)
— where this guide and that document disagree, the protocol document wins.

## 1. What it is, and when to use it

```
norn --protocol jsonrpc [flags...]
```

turns the Norn process into a **bidirectional JSON-RPC 2.0 peer over
stdin/stdout**. Newline-delimited JSON frames, one object per line; stderr
stays human-readable logs and never carries frames.

Choose it over `-p -f stream-json` when you need the **write direction**:
stream-json gives you the same live events, but driven mode also lets you
send requests *into* a running step — inject a steering message, cancel on
a deadline — and gives you an unambiguous, id-matched terminal response
instead of watching for a final NDJSON line. If you only ever read, and
never steer, stream-json is simpler and equivalent.

The channel is **one-shot by default**: one accepted run, one terminal response,
then stdout EOF and process exit even when stdin stays open. To opt into a
persistent conversation, send `initialize` with
`params: {"runLifecycle":"persistent"}` and check the selected lifecycle in
the reply. Sequential requests then share one provider, MCP runtime, in-memory
history and slash state. Wait for each terminal response; overlapping runs are
rejected. Close stdin or send `/exit` when finished. Existing one-shot consumers
can keep their current startup and shutdown behavior.

## 2. Spawning

Everything about the agent is configured with the normal CLI flags; the
protocol flag only changes the transport. A typical workflow spawn:

```bash
norn --protocol jsonrpc \
     --provider openai \
     -m <model> \
     --workspace-root /path/to/project \
     --session-id step-042 \
     --timeout 5m \
     -s ./step-output.schema.json
```

Flags most relevant to a driven-mode consumer:

| Flag | Why you care |
|---|---|
| `--workspace-root <DIR>` | Confines `read`/`write`/`edit`/`patch` to the directory (symlink-aware canonicalization). Omitted = unconfined. |
| `-s, --output-schema <JSON\|PATH>` | JSON Schema the final `output` must satisfy — the loop retries/nudges the model until it validates or the schema budget is exhausted (`stop.reason: "schema_unreachable"`). |
| `--session-id <ID>` / `--resume <ID>` | Choose the persistent conversation at startup. Requests on the same connection reuse it directly; `--resume` recovers it after a process restart. `--resume-if-exists` with `--session-id` gives explicit create-or-resume. |
| `--no-session` | Skip persistence entirely for throwaway steps. |
| `--timeout <DURATION>` | Step budget; expiry is `stop.reason: "timed_out"` with partial output, not a killed process. |
| `--max-turns <N>` | Provider round-trip cap; `stop.reason: "max_iterations"`. |
| `--allowed-tools` / `--disallowed-tools` | Tool surface control (unmatched names warn on stderr). |
| `--rules <PATH>` | Explicit guardrail rules, merged with project-discovered rules; the explicit file wins on rule-ID collision. |
| `--variables k=v` | `{{k}}` expansion in prompts/skills. |
| `-C, --working-dir <DIR>` | Tool execution cwd (also the scope for bare `--resume`). |

Notes:

- **stdin is the channel** — the prompt arrives via `run/execute`, never
  as an argument or piped text.
- **Auto-compaction is armed by default for catalog models** (2026-07-03) —
  for the root agent **and every child it spawns or forks** (each child
  resolves the window against its own model). The context window comes from the
  model catalog (`assets/models.json`; e.g. gpt-5.5 → 272k) and compaction
  triggers when `max(client estimate, last provider-reported usage)`
  exceeds `window − 30_000`. Tune with
  `-c auto_compact_reserve_tokens=<u64>`, disable with
  `-c auto_compact_reserve_tokens=off` (for orchestrators that manage
  context themselves), and supply `-c context_window=<u64>` for models the
  catalog doesn't know — without a window, compaction stays off and a long
  run can die with a terminal `context window exceeded` error. On very
  large-window models, prefer *lowering the ceiling* over turning
  compaction off: an explicit `-c context_window=500000` on a 1M model
  compacts at 500k − reserve (explicit window always beats the catalog).
- `--partial` and `-o` do not apply to the transport; delta events are
  always forwarded on the channel.
- Gate on the `protocol` field of the `initialize` result
  (`"norn-driven/1"`), not on the `jsonrpc` tag — the tag only names the
  envelope framing.

## 3. The conversation

Full lifecycle, as actual frames. `→` is you writing to Norn's stdin,
`←` is Norn's stdout.

```jsonc
// 1. Explicitly select persistence before the first run.
// Omit params to retain the one-shot default.
→ {"jsonrpc":"2.0","id":"init","method":"initialize",
   "params":{"runLifecycle":"persistent"}}
← {"jsonrpc":"2.0","id":"init","result":{
     "protocol":"norn-driven/1",
     "serverInfo":{"name":"norn","version":"..."},
     "capabilities":{
       "methods":["initialize","run/execute"],
       "events":["event/message","event/toolCall","event/toolResult",
                 "event/progress","event/stop","event/raw"],
       "interventions":["inject_message","cancel"],
       "runLifecycle":"persistent",
       "runLifecycles":["one_shot","persistent"],
       "sessionRotation":"close_with_receipt"}}}

// 2. Start a run. "input" is accepted as an alias for "prompt".
→ {"jsonrpc":"2.0","id":"run-1","method":"run/execute",
   "params":{"prompt":"Summarise the diff in ./changes.patch"}}

// 3. Events stream as notifications (no id) while the run is live.
← {"jsonrpc":"2.0","method":"event/toolCall","params":{"type":"tool_call", ...,
   "agent_id":"<uuid>","agent_role":"root"}}
← {"jsonrpc":"2.0","method":"event/toolResult","params":{...}}
← {"jsonrpc":"2.0","method":"event/message","params":{...}}

// 4. The terminal result is the id-matched Response — always last,
//    after every event of the run is on the wire.
← {"jsonrpc":"2.0","id":"run-1","result":{
     "envelope_version":1,
     "stop":{"reason":"completed"},
     "output":"The patch refactors ...",
     "usage":{"input_tokens":1201,"output_tokens":364,
              "cache_read_tokens":0,"cache_write_tokens":0,"cost_usd":0.0},
     "model":"<model id>",
     "session_id":"step-042",
     "events":[ ... ],
     "diagnostics":[ ... ]}}

// 5. Continue on the SAME process and conversation.
→ {"jsonrpc":"2.0","id":"run-2","method":"run/execute",
   "params":{"prompt":"Now explain the second change"}}
// ...events, then the matching run-2 response...

// 6. Close stdin when finished, or explicitly exit with a local command.
→ {"jsonrpc":"2.0","id":"exit","method":"run/execute",
   "params":{"prompt":"/exit"}}
← {"jsonrpc":"2.0","id":"exit","result":null}
// The writer drains and the process exits.
```

Rules your client can rely on:

- **Every accepted `run/execute` is answered.** Failures after acceptance
  (assembly, auth, provider, the run itself) come back as the id-matched
  *error* response (`-32603`, message carrying the typed CLI error) — you
  never see EOF instead of a Response. A prompt that resolves entirely to
  a local slash command (including `/exit`) is answered with a success
  Response whose `result` is `null`. Explicit `/clear` rotation instead returns
  the typed closing receipt described below. Delivery requires a writable
  transport; broken stdout cannot carry a result.
- Notifications and responses are structurally disjoint: a notification
  never carries an `id`, a response never carries a `method`. Dispatch on
  that, not on ordering.
- One serializing writer owns stdout — frames never interleave-corrupt.
- Unparseable lines you send are answered `-32700` (`id: null`); bad
  requests `-32600`; neither kills the channel. Notifications you send
  (frames without `id`) are logged and ignored.

## 4. Reading the event stream

Event notification `params` are the byte-identical stream-json payloads
(`type`-tagged), plus two attribution fields on every event: `agent_id`
(UUID) and `agent_role`. **Key by `agent_id`** — multi-agent runs (spawned
children, forks) interleave events from several agents on the one channel.

The `method` is a coarse category for cheap routing
(`event/message`, `event/toolCall`, `event/toolResult`, `event/progress`,
`event/stop`, `event/raw`); the `params.type` field carries the precise
event type. Delta events are always present; render or drop them as you
like. Events with no on-wire form are skipped — a run failure still
reaches you through the terminal response, so the event stream is
best-effort observability, not the source of truth. **The terminal
response is the source of truth.**

For streaming voice, use `event/progress` with `params.type: "text_delta"`,
filtered to the intended agent. `event/message` with `params.type: "text"`
repeats the completed text, so speaking both duplicates the response. Tool
progress and reasoning deltas are separate payload types and must not be
treated as spoken answer text. Norn does not yet attach playback cursors or
speech section identifiers to these messages.

## 5. Interventions (the write direction)

While the run is in flight, the same stdin keeps being read and serves:

### `intervene/injectMessage`

```jsonc
→ {"jsonrpc":"2.0","id":"i1","method":"intervene/injectMessage",
   "params":{"text":"Skip the tests directory","priority":"interrupt"}}
← {"jsonrpc":"2.0","id":"i1","result":{"status":"injected","priority":"interrupt"}}
```

- `priority: "interrupt"` steers the agent at its **next tool boundary**;
  `"normal"` (the default) queues a turn that batches to a stop boundary.
- The injection is attributed to the operator (nil sender, literal
  `operator` label) — it cannot impersonate a peer agent.
- Delivery failure (agent inbound channel full/closed) is `-32603` with
  the reason.

### `intervene/cancel`

```jsonc
→ {"jsonrpc":"2.0","id":"c1","method":"intervene/cancel",
   "params":{"reason":"deadline exceeded"}}
← {"jsonrpc":"2.0","id":"c1","result":{"status":"cancel_requested","reason":"deadline exceeded"}}
```

**The ack is advisory.** `cancel_requested` means the signal was applied,
not that the outcome will be `cancelled` — a cancel landing after the run
reached its own terminal outcome is still acked, and the terminal
response then reports the real outcome (e.g. `completed`). Branch on the
terminal `stop.reason`, never on the ack.

After that terminal response, another run can execute with a fresh cancellation
token on the same runtime. Previously cancelled descendants keep their cancelled
tokens. A voice controller should stop audio locally immediately; RPC steering
and runtime cancellation do not themselves control speaker playback.

Anything else (`intervene/pauseResume`, unknown methods) is `-32601` —
check `capabilities.interventions` from `initialize` rather than probing.
A second `run/execute` mid-run is `-32000` ("run already active"), never
`-32601`.

Interventions received during assembly wait for the current run's controls.
If assembly fails, they receive unavailability errors before the matching
run error response and connection close. Further interventions during cancelled
run cleanup are refused, while `initialize` remains available.

## 6. The stop envelope

The terminal `result` is the same versioned envelope `norn -p -f json`
prints (`envelope_version: 1`). `stop` is internally tagged on `reason`:

| `stop.reason` | detail fields | `output` holds | exit code |
|---|---|---|---|
| `completed` | — | the final output value | 0 |
| `schema_unreachable` | `attempts`, `validation_errors[]` | best attempt, if any | 1 |
| `max_iterations` | — | `null` | 1 |
| `timed_out` | `elapsed_ms`, `iterations` | partial output, if any | 1 |
| `cancelled` | — | `null` | 1 |
| `truncated` | `truncation` (`max_tokens` \| `content_filter`), `iterations` | partial text, if any | 1 |
| `error` | `message`, `class` (`agent` \| `auth` \| `io` \| `session`) | `null` | 1, or 3 for `auth` |

With `--output-schema`, a `completed` envelope's `output` is the
schema-validated JSON value (not a string).

The `error` reason appears only in **plain** print mode (`-p -f json` /
`-f stream-json`), where every post-argument-parsing failure emits this
envelope so a machine consumer never sees an empty stdout; argument
errors (exit 2) stay stderr-only. In driven mode you will never receive
an `error` stop — a failing accepted `run/execute` is answered as the
id-matched `-32603` error response instead (§7).

**There is deliberately no `retryable` field.** Whether a stop is worth
retrying depends on *your* budget and how you value the partial — Norn
won't encode that judgment. A reasonable workflow policy:

- `completed` → done.
- `timed_out` / `max_iterations` → retry with a bigger budget, or accept
  the partial; the session persists, so `--resume <id>` continues rather
  than restarts.
- `schema_unreachable` → inspect `validation_errors`; usually a schema or
  prompt problem, not a transient.
- `truncated` + `max_tokens` → raise limits and retry; `content_filter` →
  don't.
- `cancelled` → you asked for it.

## 7. Errors, exits, EOF

| JSON-RPC code | meaning |
|---|---|
| `-32700` | parse error on a line you sent (`id: null`) |
| `-32600` | invalid request (bad `jsonrpc` tag, missing/invalid params) |
| `-32601` | method not found / unadvertised intervention |
| `-32603` | internal: run failure on the accepted run, intervention delivery failure |
| `-32000` | invalid state: second `run/execute` while one is in flight |

On connection close, process exit codes reflect the last request: `0` completed,
`1` any other stop or run failure,
`2` argument errors, `3` auth errors — in the failure cases the
id-matched error response has already been delivered before exit.

EOF semantics:

- In default one-shot mode, Norn exits after its terminal response without
  requiring stdin EOF. The following rules cover opted-in persistence.

- You close stdin **before** sending `run/execute` → Norn exits 0,
  nothing to do.
- You close stdin **mid-run** → only the intervention reader stops; the
  run continues to its own terminal result, which is still written to
  stdout, then the connection closes. (So: closing your write side does not cancel — use
  `intervene/cancel`.)

In persistent mode, `/clear` deliberately rotates the persisted session and closes the connection
after a success response with this `result`:

```json
{
  "handled_locally": true,
  "session_id": "<replacement session ID>",
  "connection": { "state": "closing", "reason": "session_rotated" }
}
```

Use that ID with `--resume` on a new connection; do not retry the completed
rotation. Under `--no-session` the replacement ID is null. This explicit
rotation avoids retaining tools or provider bindings to the retired session.
It is separate from ordinary turns, which keep the connection and runtime.

## 8. Recipes

**Deadline with graceful partials.** Prefer `--timeout 5m` at spawn (the
run stops itself with `timed_out` + partials) over killing the process.
For a caller-side dynamic deadline, send `intervene/cancel` and wait for
the terminal response — never SIGKILL first; you'll lose the envelope.

**Multi-step conversation.** Start with `--session-id job42`. Select
`initialize` parameters `{"runLifecycle":"persistent"}`. Send one
`run/execute`, await its terminal response, then send another on the same
stdin/stdout connection. No process creation, provider reassembly or disk
replay occurs between these turns. After a restart, `--resume job42` restores
the saved conversation. Sessions are persisted per working
directory; a bare `--resume` (no ID) resolves to the latest session *for
that working directory*, not the globally newest.

**Structured handoff between steps.** Give every step
`-s <schema>` and treat only `stop.reason == "completed"` as
machine-readable output; feed `output` straight into the next step's
prompt (or a `--variables` value).

**Sandboxed execution.** `--workspace-root <dir>` plus
`--disallowed-tools bash` gives you file-tool confinement with no shell
escape; add `--allowed-tools read,search` for read-only analysis steps.

**Live steering.** Watch `event/toolCall` notifications; when you see the
agent heading somewhere unproductive, `intervene/injectMessage` with
`priority: "interrupt"` lands at the next tool boundary. `"normal"`
priority is better for "also do X" additions since it waits for a stop
boundary instead of derailing the current chain.

## 9. Gotchas

- **Don't dispatch on frame order alone** — dispatch on `id` presence
  (response) vs `method` (notification).
- **Don't infer the outcome from a cancel ack** (§5) — only the terminal
  `stop.reason` is authoritative.
- **Don't parse stderr** — it's human logs (tracing); its format is not a
  contract.
- **Don't overlap `run/execute` requests** — wait for the current terminal
  response. Sequential requests require explicit persistent negotiation.
- **Don't treat event-stream absence as failure** — some events have no
  on-wire form; the terminal response is the contract.
- **Gate on both `initialize.result.protocol` and `capabilities.runLifecycle`**.
  A client omitting the new selection retains the one-shot default. A client
  selecting persistence must close stdin or send `/exit` when finished.
- **Choose the lifecycle before the first run.** Unknown selections fail
  explicitly; changing selection after execution starts is refused. Later
  parameterless `initialize` calls are read-only and preserve the selection.
- Timestamps/ordering across multiple agents' events are interleaved
  as produced — sequence per `agent_id` if you need per-agent order.

## 10. Reference

- Normative contract: `docs/design/norn-cli/DRIVEN-PROTOCOL.md`
- Implementation: `crates/norn-cli/src/print/driven.rs`,
  `crates/norn-cli/src/print/jsonrpc/`
- Integration tests (good copy-paste transcripts):
  `crates/norn-cli/tests/jsonrpc_driven_mode.rs`
