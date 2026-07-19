# P5 `CODEX-02` Gate D handoff

Date: 2026-07-19  
Candidate branch: `codex/p5-codex-turn-state`  
Finding: `CODEX-02` plus the directly coupled trusted `client_metadata` slice  
Requested verdict: `READY` or `NOT READY` as an implementation candidate  
Whole-P5 acceptance: explicitly out of scope

## Exact review target

The primary implementation is commit
`64e55856e3bfe64ebe7d9a0f6209b65753ea04e5` over accepted CODEX-01 merge base
`c1aa862f2e5fc58364b6af92134d2608ea794d7b`. Narrow correction
`c3a7aa111e6e2389bd07097af7bf606037359ae8` rejects metadata state that cannot
be replayed as an HTTP header before it can occupy the first-wins slot. The
corrected implementation tree is `af275c4d81055f9554617682172387ff710538fd`.

The NUL-delimited 25-path inventory for `c1aa862..c3a7aa1` has SHA-256
`2ce052c32099b463272c0cbcba1898720b95a5f0104590d95023e4b91ea2fc82`:

- `crates/norn/src/loop/classify.rs`
- `crates/norn/src/loop/classify_audio_tests.rs`
- `crates/norn/src/loop/runner/machine.rs`
- `crates/norn/src/loop/runner/mod.rs`
- `crates/norn/src/loop/runner/provider_call.rs`
- `crates/norn/src/loop/runner/setup.rs`
- `crates/norn/src/loop/runner/turn_context_tests.rs`
- `crates/norn/src/loop/summarization.rs`
- `crates/norn/src/provider/debug.rs`
- `crates/norn/src/provider/events.rs`
- `crates/norn/src/provider/exec.rs`
- `crates/norn/src/provider/exec_emit.rs`
- `crates/norn/src/provider/exec_tests.rs`
- `crates/norn/src/provider/mod.rs`
- `crates/norn/src/provider/openai/codex_turn.rs`
- `crates/norn/src/provider/openai/execute.rs`
- `crates/norn/src/provider/openai/execute/turn_state_tests.rs`
- `crates/norn/src/provider/openai/mod.rs`
- `crates/norn/src/provider/openai/provider.rs`
- `crates/norn/src/provider/openai/request.rs`
- `crates/norn/src/provider/openai/response_stream_event.rs`
- `crates/norn/src/provider/openai_compatible/provider.rs`
- `crates/norn/src/provider/owned_stream_tests.rs`
- `crates/norn/src/provider/traits.rs`
- `crates/norn/src/provider/turn.rs`

The planning reconciliation and this handoff are intentionally a later,
separate documentation commit. Review product behavior against corrected head
`c3a7aa1`.

## Pinned source contract

The private overlay is frozen to official OpenAI Codex source commit
[`0396f99cf1a27fc87dd12d23403b25e840b6ecbd`](https://github.com/openai/codex/tree/0396f99cf1a27fc87dd12d23403b25e840b6ecbd),
already recorded with the previously inventoried overlay blob hashes in the
[`P1 Gate A contract`](2026-07-15-p1-gate-a-contract.md). The load-bearing
sources for this slice are pinned
[`core/src/client.rs`](https://github.com/openai/codex/blob/0396f99cf1a27fc87dd12d23403b25e840b6ecbd/codex-rs/core/src/client.rs)
for per-turn lifetime and HTTP-header replay, pinned
[`codex-api/src/sse/responses.rs`](https://github.com/openai/codex/blob/0396f99cf1a27fc87dd12d23403b25e840b6ecbd/codex-rs/codex-api/src/sse/responses.rs)
for `response.metadata` header extraction, pinned
[`core/src/responses_metadata.rs`](https://github.com/openai/codex/blob/0396f99cf1a27fc87dd12d23403b25e840b6ecbd/codex-rs/core/src/responses_metadata.rs)
at blob `c76d3078cde4ab48baebf6192e9703191e6beea3` for the
client-metadata keys and nested turn payload, pinned
[`utils/string/src/json.rs`](https://github.com/openai/codex/blob/0396f99cf1a27fc87dd12d23403b25e840b6ecbd/codex-rs/utils/string/src/json.rs)
at blob `fd5e7d65bc722c8ec2ad4526bb874527661e1f81` for the ASCII
JSON formatter, and pinned
[`codex-api/src/common.rs`](https://github.com/openai/codex/blob/0396f99cf1a27fc87dd12d23403b25e840b6ecbd/codex-rs/codex-api/src/common.rs)
for the outer request field type.

The frozen behavior is:

| Concern | Candidate contract |
|---|---|
| Lifetime | One fresh state container per logical user turn; later requests in that turn share it; later turns do not. |
| Capture | Accept `x-codex-turn-state` from a successful HTTP response header or validated Codex `response.metadata`. |
| Metadata value shape | Codex uses a case-insensitive key and recursively takes the first array value. Norn intentionally narrows admission to a scalar string or a first array element that is directly a string; nested arrays and non-strings do not capture. |
| Authority | First accepted value wins; an exact repeat is harmless; a conflicting later value is ignored. |
| Replay | Send the accepted value on later HTTP requests in the same turn as `x-codex-turn-state`; do not place it in the JSON body. |
| Persistence | Never serialize or persist turn state. Dropping the live turn context clears it. |
| Backend | Enable only for the compiled OAuth `codex_subscription` backend. Public/custom/unknown backends omit it. |

This state is not public Responses threading and is never represented as
`previous_response_id`, `conversation`, or transcript content.

Norn also requires a candidate to be representable as an HTTP `HeaderValue`
before it can become authoritative. An invalid metadata string is ignored
payload-free and does not prevent a later valid candidate from being captured.
This and nested-array rejection are deliberate fail-closed Norn narrowings over
the pinned client, not claims of exact Codex parser parity.

## Norn client-metadata projection

The pinned Codex client has process/UI identities that Norn does not possess.
The owner therefore approved an honest Norn projection rather than invented
installation, terminal-window, sandbox, source, workspace, parent, or subagent
values. For resolved session `S` and persisted prompt/wake event `T`, the exact
request field is:

```json
{
  "client_metadata": {
    "session_id": "S",
    "thread_id": "S",
    "turn_id": "T",
    "x-codex-turn-metadata": "{\"session_id\":\"S\",\"thread_id\":\"S\",\"turn_id\":\"T\",\"request_kind\":\"turn\"}"
  }
}
```

The nested string uses the pinned Codex ASCII JSON formatter. Missing real
identities cause omission, not substitution. `client_metadata` is a protected
provider option, so project/model/provider input cannot override it. The same
trusted-backend filter that gates turn state gates this field.

## Implementation shape

`ProviderTurnContext` is a public opaque, cloneable library context. Direct
embedders construct one with `ProviderTurnContext::for_turn(session_id,
turn_id)`; empty identities fail with a typed `InvalidRequest`. Its private
inner state uses `Arc<OnceLock<SecretString>>`. The public surface exposes only
the caller-provided session and turn identities. Turn-state observation and
header projection remain crate-private, and `Debug` reports presence only.

`Provider::stream_with_context` is an object-safe default method that delegates
to `stream`, preserving all existing third-party/provider behavior. The OpenAI
provider overrides it and discards the context unless its compiled backend is
`codex_subscription`. The ordinary `stream` path remains context-free.

The Norn loop constructs one context while creating each `StepMachine`, using
the resolved session ID and the persisted prompt/wake `EventId`. The same clone
crosses loop retries and `ContinueTurn` calls. A later user step constructs a
new context, even in the same session. No session event or resume model contains
this type.

`StreamExecutor` applies immutable trusted request headers to every HTTP attempt
and exposes successful response headers to its mapper before body processing.
The Responses mapper also examines raw metadata only after the redacted clone
has passed the typed envelope/discriminator validation. This prevents a
malformed event from seeding reusable state before it fails closed.

## Non-disclosure boundary

Turn state is treated as a reusable transport secret:

- `HeaderValue::set_sensitive(true)` prevents ordinary HTTP debug formatting
  from rendering its contents.
- `SecretString` and manual presence-only context `Debug` prevent object debug
  disclosure.
- emitted raw `ProviderEvent::ResponseStreamEvent` metadata contains
  `[REDACTED]`, not the state value;
- debug SSE dumping performs the same case-insensitive key-based redaction even
  when the outer SSE and inner JSON discriminators disagree;
- validation still uses the original value only after the redacted envelope is
  admitted, so disclosure redaction does not weaken state capture.

The candidate therefore narrows the prior "raw event" wording: admitted event
structure remains available, but reusable transport credentials are redacted.
It does not claim byte-exact disclosure of that sensitive header.

## Executable evidence

All commands used the repository's normal shared
`/Users/tom/Developer/ablative/norn/target` directory. No temporary Cargo target
was used.

- `cargo fmt --all -- --check`: pass.
- `cargo check -p norn --all-targets --all-features`: pass.
- `cargo clippy -p norn --all-targets --all-features -- -D warnings`: pass.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`: pass.
- `cargo test --workspace --all-features --quiet`: 5,405/5,405 unit and
  integration harness tests pass; 8/8 doctests pass.
- `provider::openai::execute::turn_state_tests`: 4/4 pass in the
  network-capable run.
- `loop::runner::turn_context_tests::turn_context_survives_retry_and_continuation_but_not_the_next_step`:
  1/1 pass.
- `git diff --check`: pass.

The first sandboxed focused run compiled and passed the two mapper-only cases,
while its two Wiremock cases failed to bind a loopback socket with the sandbox's
`PermissionDenied`. The identical 4/4 module passed outside that network
sandbox, followed by the complete workspace pass above. The sandbox result is
not counted as candidate evidence and is not represented as a product failure.

The local-wire Codex fixture proves all of the following in one real transport
sequence:

- request one has no state header;
- its successful HTTP header value becomes authoritative;
- a different later metadata value cannot replace it;
- request two replays the authoritative header;
- a fresh context on request three sends no prior state;
- requests in one turn carry identical projected metadata and the new turn has
  its new turn ID;
- emitted raw metadata is redacted.

Separate fixtures prove public API-key absence, validation-before-capture,
case-insensitive array capture, nested-array rejection, invalid-then-valid
header admission, public mapper absence, presence-only debug output, and
debug-dump redaction independent of the event label. The loop fixture makes the
provider's ordinary `stream` path fail so a
silent fallback cannot pass; it observes one context through a transient retry
and continuation, then a fresh state-free context in the next step.

## Policy and source-size audit

The 25-path Rust diff adds no `#[allow]`, `unwrap(`, `expect(`, `panic!`,
`todo!`, `unimplemented!`, `unsafe`, `TODO`, or `HACK` occurrence. Strict
Clippy runs with `-D warnings`; no suppression was introduced. No timeout,
retry count, cadence, queue size, or other operational limit is added.

All production files remain below 500 lines. The largest touched physical
production file is `provider/exec.rs` at 498 lines, with its test module
starting at line 496. The new `provider/turn.rs` is 249 physical lines with a
192-line production prefix; `provider/openai/codex_turn.rs` is 151 physical
lines with a 123-line production prefix. The two larger new fixture files are
test-only and remain below 500 physical lines.

## Internal review already completed

A read-only adversarial pass examined state poisoning, header and raw-event
disclosure, backend authority, array parsing, downstream library access,
loop-lifetime binding, and source-size policy. Its initial findings were fixed
before the corrected head: capture now occurs only after metadata validation;
redaction does not trust the SSE label; outgoing headers are sensitive; nested
arrays are rejected; the context has a real public constructor; wording no
longer overclaims raw sensitive values; and a loop-level binding fixture was
added. A final edge review found that an unreplayable metadata string could
occupy the first-wins slot; correction `c3a7aa1` validates replayability before
storage and adds unit plus mapper regressions. The final static pass found no
remaining correctness or security defect. The external Gate D reviewer must
still verify the frozen commit independently.

## Honest boundaries

- This is a CODEX-02/client-metadata implementation candidate, not P5
  acceptance.
- External review has not accepted corrected head `c3a7aa1`.
- The fixture proves retry, explicit continuation, and next-step reset. It does
  not claim ordinary session resume, concurrent-agent isolation, process
  restart, cancellation, or terminal-error coverage.
- Turn state is not yet bound to the opaque P2 credential identity. Account
  switching and resumed-account affinity remain open P5 work.
- D3 compaction/anchor policy and D8 role authority remain open.
- The candidate uses the HTTP Responses transport. WebSocket support and its
  state transport are a separate future slice.
- The implementation intentionally redacts a reusable credential from the raw
  metadata event. "Raw" means admitted provider structure, not disclosure of
  transport secrets.
- No repeated-distribution claim is made for these deterministic fixtures.

## Reviewer questions

1. Can any public, API-key, custom-base-URL, or unknown backend receive turn
   state or `client_metadata`?
2. Can malformed or discriminator-conflicting metadata seed state before the
   stream fails closed?
3. Can a response-body value replace an earlier successful HTTP header value?
4. Can a second continuation, retry, or new user step observe the wrong
   context lifetime?
5. Can state appear in a provider event, transcript, debug dump, error, or
   `Debug` representation?
6. Can project/provider options override the trusted `client_metadata` field?
7. Does the Norn projection invent any Codex identity, or claim full Codex CLI
   metadata parity?
8. Do the tests genuinely bind ordinary-stream non-use, first-wins replay,
   next-step reset, public absence, validation ordering, and redaction?
9. Does any statement above exceed the code or the reported gate evidence?
