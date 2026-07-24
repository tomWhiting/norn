# Responses stream-defects fix branch: scope, deferrals, and design questions

Date: 2026-07-25
Branch: `fix/responses-stream-defects` (base: main @ 97cd24e)
Author seat: Sable Nightwick (fix seat, NORN-RESPONSES-STREAM-DEFECT)
Reviewer: Waffles the Terrible (byte-review before landing)
Incident references: `docs/reviews/2026-07-25-remote-headless-session-death-handoff.md`,
tracker row NORN-RESPONSES-STREAM-DEFECT in `ablative/docs/tracking/jobs.jsonl`.

## What this branch delivers

Every commit red-first (test written and observed failing before the fix).

1. **Evidence-destruction family, items 1a + 1c** (`2ccf08a`)
   - `send_with_retries` records a `response_meta` debug-dump entry for
     every attempt whose headers arrive — 401, 429, and terminal non-2xx
     included, in attempt order. Previously the dumper never saw the retry
     path at all (structural: it wasn't a parameter), so a failed request
     left no status, headers, or correlation evidence even when armed.
   - `error_response_to_provider_error` carries drain-transport
     diagnostics inside the `StreamError` reason instead of dropping them
     after a trace line.

2. **Class 1: standalone `error` event classification** (`7fae964`)
   - The `error` arm deserializes the flat `ResponseErrorEvent` shape
     (verified against the current official streaming-events reference),
     logs the redacted frame, and classifies through the shared
     provider-error-code table. `server_error` — the captured strike
     payload whose message says "You can retry your request" — carries
     `TransientKind::ServerError{status: 0}`, the any-5xx wildcard the
     default retry policy already matches, plus a bounded pattern-matched
     request-id token (`req_*` or UUID; never the free-text message).
     Unknown codes stay terminal with an opaque-tagged diagnosis.
   - Chain proof in-test: the captured strike frame classifies retryable
     under `RetryPolicy::default()`.

3. **Class 2: unknown-event policy** (`7fae964`)
   - Manifest-Unknown events are skipped, not fatal: lossless envelope
     still flows to consumers and the armed dump, an opaque-tagged warn
     records the skip, and terminal safety holds downstream (no known
     terminal ⇒ no `Done` ⇒ assembly fails the turn loudly). This is the
     Codex reference behavior (`codex-rs` ignores unrecognized types).
   - The captured `keepalive` frame is the regression fixture.
     **`keepalive` gets no named manifest admission**: it appears in
     neither the pinned public schema nor the pinned Codex overlay
     sources; naming it would fabricate provenance. If a third,
     capture-evidence-backed manifest family is wanted, that is a design
     decision, not a table edit.
   - `UnsupportedResponseEvent` removed (no producers remain).

4. **Replay doc-truing + call_id mirror** (`6f897ae`)
   - `serialize_assistant_into` rustdoc now describes both replay paths
     truthfully; the Codex-reference parity delta (id stripping under
     `store: false`) is documented as an OPEN wire-contract question, not
     resolved silently.
   - Fallback tool calls with empty `call_id` fail loud with a typed
     `RequestSerializationFailed`, mirroring `serialize_tool_result`.

Gates at branch head: norn 4416/4416, norn-cli 575/575, norn-tui 702/702,
doctests 8/8, `cargo clippy --workspace --all-targets -- -D warnings`
clean, `cargo fmt --check` clean.

## Deliberately deferred (exact shapes, not vague intentions)

These are wide schema threads that do not gate the bug killers above.
Deferring them kept the death-stopping fixes small and reviewable.

### 1d — served-model recording

Gap (Seth's forensics, confirmed): `AssistantMessage.model` does not
exist; a server-side model downgrade is locally invisible. The served
model arrives in `response.created`/`response.completed` payloads.

Proposed shape: thread `served_model: Option<String>` from the terminal
response object through `ProviderEvent::Done` (or a dedicated
response-metadata event — decide ONE), capture it in
`assemble_response`, persist as a new optional serde-defaulted field on
`SessionEvent::AssistantMessage`. Blast radius measured: 177
`ProviderEvent::Done` references workspace-wide; mechanical but wide.

### 1e — durable terminal-failure event

Gap (handoff doc, proven): a provider failure leaves no durable session
row — error class, HTTP status, retry disposition, request correlation,
and failure stage all die with the process. Proposed shape: new
`SessionEvent` variant persisted when a turn fails, carrying error
class + transient kind + bounded request-id (from the Class-1
classifier) + failure stage (constructed/dispatched/headers/terminal)
+ retry count. This is ALSO the right home for the response-header
`x-request-id` VALUE, which `write_response_meta` deliberately redacts
(tested threat: authority echoing credentials into that header) — a
sanitization ruling belongs to this event's design, not to a dump edit.

### Never-die retry architecture (escape valve invoked)

Tom's direction: transient failures retry indefinitely (backoff +
jitter, gentle ceiling), non-transient failures fail the TURN loudly
while the WORKER survives; termination only by user cancel. Waffles'
recorded seat-parameters: 60s backoff ceiling, full jitter, every retry
logged with the preserved frame.

What this branch already changes: Class-1 strikes now classify
transient, so the EXISTING bounded retry policy absorbs observed
wobble-window strikes (the corpus's 4-strike day would have cost
seconds, not fourteen deaths).

What needs Tom's working session before unbounded retry is built:

1. **Loop placement** — transport-level (`StreamExecutor`) vs loop-level
   (`retry.rs`) vs both. Mid-stream deaths need loop-level replay (the
   request must be rebuilt); connect failures could retry at transport.
   Recommendation: one loop-level policy, transport stays bounded-simple.
2. **Cancellation plumbing** — "only user cancel ends the retry loop"
   requires the cancel token to reach the sleep between attempts on
   every driver (TUI, print, driven, MCP). Print mode has no interactive
   cancel: is Ctrl-C/SIGTERM the cancel, and does an orphaned headless
   retry loop poll a dead provider forever? Needs an owner ruling on
   print-mode semantics.
3. **Config shape** — current `RetryPolicy` expresses bounded attempts.
   Unbounded-with-ceiling needs a new shape (e.g. `max_attempts:
   Option<u32>` where `None` = unbounded + `backoff_ceiling` + jitter
   flag), all configurable, with the 60s/full-jitter values recorded as
   seat-ruled defaults pending Tom's veto (NO invented values).
4. **Jitter source** — full jitter needs an RNG in the loop layer;
   norn's determinism-sensitive paths must not inherit it accidentally.
5. **Interaction with worker survival** — "turn fails, worker survives"
   is a driver-lifecycle property (driven/TUI keep the agent; print
   exits by design). The retry loop must not paper over non-transient
   failures to fake survival — that is the no-silent-fallbacks line.
6. **Session persistence across restarts** — does an in-retry turn
   survive a process restart as "retrying" state, or restart the turn?
   Touches the pending/terminal-recovery machinery (D8 family).

### Batch-pass queue (verified, not blocking)

- `stream_renderer.rs:119` — any stdout write failure (not just broken
  pipe) reads as clean completion in `-f stream-json` mode; exit 0 with
  truncated output. Distinguish broken-pipe (conventional quiet stop)
  from other write errors, and consider a stderr note + nonzero exit.
- `rate_limiter.rs` cooldown TOCTOU — re-check cooldown after permit
  acquisition (loop) or hold the state lock across `try_acquire`.
- `exec.rs:136 debug dumps` for send-failures (no response): only
  responses are dumped; a connect-failure attempt leaves just the
  request entry. Acceptable today; note for the terminal-failure event.
- Flake observed (pre-existing, unrelated):
  `account_catalog::tests::catalog_failure_after_slot_scrub_self_heals_without_old_credentials`
  failed once under full-suite parallelism, passes in isolation and on
  rerun. Worth its own tracker note.

## Live-specimen note (Vesper Lynd, 2026-07-24 18:10Z)

An "unsupported Responses stream event" death INSTANTLY at stream-open
(zero tokens, zero events; identical relaunch ran clean; their box,
session 866268d6). The trace cannot name the frame — the discard defect
itself. The unknown-event policy in this branch makes that class
non-fatal regardless of which frame it was, and the always-on skip log +
armed dump will name it on any recurrence. No `--debug-api` rerun was
requested; the question is moot unless a skip-logged unknown correlates
with misbehavior.
