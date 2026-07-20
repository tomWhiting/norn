# P5 `CODEX-02` Gate D review

**Date:** 2026-07-19

**Reviewer:** Sable Nightwick (coordinator) + three Opus area seats
(lifetime/threading; metadata/validation/contract; adversarial leak/poison) +
norn cross-model pass (GPT-5.6 Sol, safety preset, read-only, session
`3331d3b0-5e16-4901-a0dd-1d8d040f43df`)

**Handoff:**
[`2026-07-19-p5-codex-02-gate-d-handoff.md`](2026-07-19-p5-codex-02-gate-d-handoff.md)

**Reviewed head:** `c3a7aa1` (product `64e5585` + correction `c3a7aa1` over
accepted base `c1aa862`; docs head `fdc7328`)

## Verdict

**CODEX-02: NOT READY as an implementation candidate — one BLOCKER
(reproduced secret disclosure).** The turn-state redactor is structure-fragile:
it redacts `x-codex-turn-state` only when the metadata `headers` field is a
top-level JSON object. A `response.metadata` frame whose `headers` is an array
(or otherwise non-object) passes envelope validation and is emitted verbatim,
disclosing the reusable transport secret to the always-on agent-event observer
stream and, when debug dumping is enabled, to the on-disk debug JSONL. This
defeats the candidate's explicitly-claimed non-disclosure invariant. The fix is
small and localized. Everything else in the slice — trust gating, lifetime,
first-wins authority, validation ordering, client-metadata projection and
override protection, and the ASCII formatter — verified SOUND.

## BLOCKER-1 — structure-fragile turn-state redactor discloses the secret (CONFIRMED, reproduced)

`redact_codex_turn_state` (`crates/norn/src/provider/turn.rs:180-191`) obtains
the headers node with `redacted.get_mut("headers").and_then(Value::as_object_mut)`
and returns the clone **unchanged** when that node is not a top-level object.
`ResponseStreamEvent::from_raw` (`response_stream_event.rs:120-149`) validates
only object-ness, `type`, and sequence policy — it does not validate the overlay
`headers` shape — so a `response.metadata` frame with `headers` as an **array**
is admitted. Both disclosure sinks then carry the secret in cleartext:

- **Always-on** — the emitted `ProviderEvent::ResponseStreamEvent` is built from
  the (unredacted, because array-shaped) clone at `execute.rs:115-116` and sent
  to every agent-event observer unconditionally at `classify.rs:261` (CLI
  raw-event mode, event-bus subscribers, other agents).
- **Opt-in disk** — `emit_mapped` dumps the raw frame pre-mapping via
  `write_sse_event` (`exec_emit.rs:17-20`), which uses the same shallow redactor
  (`debug.rs:160-169`), so an enabled debug JSONL persists the secret to disk.

**I reproduced this end-to-end** (temporary test, since removed; worktree
restored byte-clean): a `response.metadata` frame with
`"headers": [{"x-codex-turn-state":"LEAKED-SECRET-XYZ"}]` driven through the
real `ResponsesMapper` on the Codex backend produced an emitted envelope whose
`raw()` contained `LEAKED-SECRET-XYZ` verbatim, and `redact_codex_turn_state`
returned the frame unchanged. The existing redaction fixtures
(`debug.rs:366-391`, `turn_state_tests.rs:317-340`) use only a canonical
top-level object `headers`, so they miss this shape.

**Reachability, stated honestly.** Under honest pinned-Codex operation the
server presumably sends `headers` as a top-level object, so this does not
trigger in normal traffic. But the redactor's explicit job — per the handoff
("debug SSE dumping performs the same case-insensitive key-based redaction even
when the outer SSE and inner JSON discriminators disagree") — is robustness to
malformed/unexpected shapes. A security redactor that silently no-ops on any
shape but one provides false assurance; the threat is a non-canonical frame
variant, a compromised/MITM'd stream, or future wire drift, and the sink it
leaks to is lower-trust than the TLS channel the value arrived on. Note the
leak of the emitted event is not even conditional on prior capture — any frame
placing the sensitive key in a non-object-headers shape is emitted verbatim.

**Fix (small, localized):** make `redact_codex_turn_state` structure-independent
— recursively walk objects and arrays and replace every value whose key equals
`x-codex-turn-state` (case-insensitive), regardless of nesting or surrounding
shape — keeping it before both debug dumping and envelope construction. Add an
integrated regression: one 2xx response whose HTTP header captures a sentinel
and whose `response.metadata` body places the same sentinel under the sensitive
key inside an array (and a nested-object case), asserting the debug file, the
emitted `ResponseStreamEvent::raw()`, and the `ProviderEvent` output contain no
sentinel. Keep the existing canonical-shape tests.

## Process note

Three Opus seats — including a dedicated adversarial leak/poison seat — returned
this surface SOUND. The adversarial seat *noticed* the top-level-`headers`
structural assumption (its OBS-1) but dismissed it as "symmetric, no disclosure
risk," reasoning that a value Norn does not capture is not Norn's secret. That
reasoning breaks across the capture/disclosure boundary: the redactor's no-op is
unconditional on capture, and the disclosure sinks are real. The norn GPT-5.6
Sol cross-model pass found the reachable case; I confirmed it end-to-end before
flipping the verdict. Cross-model diversity earned its seat, as it did on
CODEX-01. (The intended Fable adversarial seat hit the Fable credit ceiling and
was relaunched on Opus 4.8.)

## Verified SOUND (all load-bearing claims re-verified by me)

- **Trust gating:** turn state and `client_metadata` reach only the compiled
  OAuth `codex_subscription` backend — `stream_inner` filters the context with
  `.filter(|_| self.backend.is_codex_subscription())` (`provider.rs:190`), the
  default `stream_with_context` discards it (`traits.rs`), and
  `is_codex_subscription` is OAuth-only against the pinned chatgpt.com endpoint
  (CODEX-01 chain). Bound by `public_backend_ignores_private_turn_context`.
- **Override protection:** `client_metadata` is a rejected protected option
  (`request.rs:279`), and the trusted projection is inserted *after*
  `build_payload` (`execute.rs:54-55`), so config layers cannot inject or
  pre-seed it.
- **Validation-before-capture:** redact → `from_sse` validate (Err → terminal,
  no capture) → `known_event_type()=="response.metadata"` gate → capture from
  original data. A malformed/wrong-discriminator event cannot seed state.
- **First-wins + c3a7aa1:** `observe_codex_turn_state` rejects any
  non-`HeaderValue` candidate *before* the `OnceLock`, so an unreplayable value
  can't occupy the slot and block a later valid one. I **mutation-verified**
  this: removing the guard fails `mapper_validates_metadata_before_seeding_turn_state`
  and `first_turn_state_wins_without_debug_disclosure`; restored clean.
- **Lifetime:** one `ProviderTurnContext` per `StepMachine`
  (`setup.rs:199-205`), the same `Arc<OnceLock>` clone shared across retries and
  `ContinueTurn` continuations (`classify.rs:367`), a fresh context for the next
  step, dropped (secret-clearing) at step end, never persisted; concurrent
  agents get distinct contexts (no cross-agent bleed). `stream_with_context` is
  genuinely wired (`classify.rs:254`), not dead code.
- **Non-leak sinks that DO hold:** outgoing header `set_sensitive(true)` on
  every attempt incl. retries; `write_response_meta` blanket-redacts every
  response-header value; presence-only context `Debug`; `SecretString` blocks
  `Debug`/`Display`; no `ProviderError` variant embeds the value; capture
  `expose()` used only for equality and `HeaderValue::try_from`.
- **client_metadata projection:** exact field shape, `thread_id`=`session_id`,
  ASCII-escaped nested string (é→é, λ→λ), omission (not substitution)
  on missing identity.
- **My battery (network-capable, primary-repo target):** fmt clean, clippy
  `-D warnings` clean, full workspace green (norn 4,051 / cli 501 / tui 683),
  all nine focused turn-state tests pass — matching the implementer's reported
  5,405 total.

## Observations (non-blocking; carry to correction)

1. **Case-sensitive protected-key rejection** (`request.rs:263-280`): a
   mis-cased `Client_Metadata` bypasses rejection but is inert JSON the server
   ignores while the trusted `client_metadata` insert owns the canonical key.
   Defense-in-depth: make rejection case-insensitive.
2. **Public `Default` constructor** (`turn.rs:45-49`): `default()` bypasses
   `for_turn`'s non-empty guard, but yields the benign identity-less context
   summarization uses (`summarization.rs:112`) — no `client_metadata`, no secret
   access. Handoff wording ("can't construct a malformed context") is imprecise;
   consider narrowing the surface or a doc note.
3. **`from_sse` fatal on a metadata frame missing its `event:` line** — a valid
   `response.metadata` body with no SSE event name becomes a fatal
   `ResponseParseError` (fail-closed; HTTP-header capture path unaffected).
4. **ASCII formatter / `request_kind:"turn"` parity** verified by algorithm and
   the pinned-source citation, not by diffing the frozen Codex blobs; the
   mandatory D7/P9 real-wire test should close exact byte parity.

## Boundaries

- This is a CODEX-02/client-metadata candidate review, not P5 acceptance.
- The mandatory D7/P9 authenticated real-wire conformance test remains open and
  is where the exact Codex `response.metadata` wire shape (relevant to both the
  BLOCKER fix and observations 3–4) should be confirmed.
- D3, D8, account affinity, resume/concurrency isolation, and WebSocket state
  transport remain explicitly open per the handoff.
- Expected path: structure-independent redaction fix + array/nested regression →
  narrow same-reviewer confirmation (D2 F1 / audio M-1 / CODEX-01 M-1
  precedent).
