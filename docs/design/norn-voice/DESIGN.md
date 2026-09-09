# Native Norn voice

Status: implementation started, 9 September 2026, Melbourne. Owner: Ripley.

The first delivery is D08 from `docs/planning/NEXT-WORK-20260908.md`
in the Norn main checkout: read, stop and replay a completed answer through
Locutus. D09 adds dictation into the existing Iridium composer. Neither
delivery adds an audio model or another conversational model to Norn.

## Ownership

Locutus owns microphones, synthesis, output devices, echo handling and actual
playback measurements. Norn owns the operator's selection, draft, session,
read-aloud preference and the lifetime of its speech requests. Disabled voice
opens no voice connection and starts no audio task. A separately configured MCP
server retains its existing launch policy; disabling read-aloud does not turn
off an independently configured MCP server.

One active speech request is admitted at a time. A stop first prevents later
sections from being submitted, then asks Locutus to hush the active request.
Stopping speech does not cancel provider generation, tools or descendants.
Replay is an explicit new request, never a reconnect side effect. Switching
the viewed agent does not retarget a request already admitted.

## Native control contract

The adapter pins `locutus-contract` at
`56b6722d43380e6a5e59fe3f759bb907cbb17c87` from
`https://github.com/tomWhiting/gobetween`. Buckley supplied this commit on
9 September 2026. It replaces the earlier inspected contract that lacked a
speech-submission intent. Service gating and live playback proof are tracked
separately from this dependency pin.

Each read opens one Unix control connection, reads its initial server session,
registers the Norn seat, and submits `Say` with a client UUID. The adapter
consumes the committed Rust shapes, not hand-mirrored JSON. Request tags on
admission and terminal receipts distinguish our speech from other seats'
broadcasts. The socket holds one process descriptor admission for its lifetime.

Stop sends request-tagged `Hush` on the same ordered connection, including
before the server has assigned a speech ID. A control write alone is not a
confirmed stop. The adapter waits for `Spoken`, `Hushed`, an explicit refusal,
or a transport error; disconnection after submission is an unknown playback
outcome. It never retries speech automatically. `Hushed` retains the server
session, speech ID, output callback position and device latency.

Progress uses a latest-value channel. The UI cannot delay a stop by falling
behind a queue of rendering events. Hub admission remains authoritative:
accepted speech can be held pending Play in Dot. The terminal labels it as
playing only after the corresponding server event. Completed structured output
has no implicit speech projection and is not read as JSON.

## Driven extension

`norn-driven/1` currently serves one run per process. `event/progress` with
`type: text_delta` carries streaming answer text; `event/message` with
`type: text` repeats completed text and must not be spoken a second time.
Filter root-agent text explicitly; reasoning and tool progress are different
events. Resume flags continue persisted sessions, not live processes.

`intervene/injectMessage` with interrupt priority steers at tool boundaries.
`intervene/cancel` cancels the run and its descendants, and its acknowledgement
is not the terminal result. Immediate playback hush belongs to Locutus.

The pinned door contract provides speech identity and measured stop receipts.
Streamed section parsing and model-visible interruption delivery remain later
work. The driven lane supplies a negotiated persistent multi-run capability.
Waffles confirmed persistent multi-run is required in this delivery: retain
one driven process and accept sequential `run/execute` requests. Waffles ruled
at 14:06 Melbourne that the driver must opt in using initialize parameters
`runLifecycle: "persistent"`; omission retains the existing one-run EOF
behavior for Aion adapters. The separate
`codex/norn-driven-multirun` lane owns that change. A process per utterance is
not the delivered extension interface.

## Later spoken and written sections

Use one model response with separately framed spoken and written sections.
The spoken section contains sentences; the written section may use Markdown.
An incremental parser must handle tags split across provider chunks and must
refuse malformed framing. It must not read reasoning or raw tool JSON aloud.
About 250 words per section is a presentation target from Waffles's assignment,
not a truncation limit. All sections share one chain identity. Interruption
retains the actually played position and unspoken remainder, with uncertainty
explicit where the device cannot report exact playback.

## Verification and delivery

Use deterministic tests for cancellation ordering, replay identity, failures,
disabled startup and settings round trips. Exercise the actual Locutus
transport before claiming integration works. Run strict Clippy and formatting;
the full battery is an exact-commit workflow on the 205. Each delivery has its
own branch and review; installation is reported separately from implementation.
