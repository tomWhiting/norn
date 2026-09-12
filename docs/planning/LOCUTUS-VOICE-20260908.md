# Norn voice integration with Locutus

8 September 2026, Melbourne. File-read integration assessment and proposed handoff only. No builds, checks, model loads, microphone/speaker access, service launches, network probes or runtime tests were performed. Locutus source was not edited. Working source may change independently; this document does not certify a release or installed binary.

This supplies the concrete external service for D08/D09 in [the ordered Norn plan](NEXT-WORK-20260908.md). The supplied location is `/Users/tom/Developer/projects/locutus-merge`.

## Existing interfaces to reuse

| Surface | What the read files provide | Norn use |
| --- | --- | --- |
| `locutus-mcp` over MCP stdio | Five direct tools: `say`, `listen`, `ears`, `voices`, `transcript`; empty `claude/channel` capability and `notifications/claude/channel`. | Initial agent-controlled speech and admitted external spoken input through Norn's existing MCP client. |
| Optional `locutus-mcp --control PATH` | Unix socket, UTF-8 newline-delimited JSON; state on connect and pushed state/transcript/speech events. | Native TUI voice controls, live captions, hold-to-talk and hush. No terminal scraping. |
| `locutus-contract` | Typed door, ears and mouth wire definitions in `contract/src/`. | Reuse the declared wire shapes where practical; do not maintain a divergent hand-written protocol. Dependency/version and unknown-field policy require an explicit integration choice. |
| `presence-core` | Portable presence/client state machinery, separate from the Mac skin. | Evaluate reuse of connection/reducer logic; Norn owns terminal rendering, drafts and key dispatch. Do not import the desktop UI just to draw captions. |
| `gob` / `gob-core` | Local synthesis, optional remote WebSocket synthesis, voice enumeration; Locutus also has an ElevenLabs adapter. | Locutus owns backend selection, synthesis and playback. Avoid a duplicate Norn speech engine or per-utterance model process. |
| `earhorn` | Local JSONL transcript events; remote audio streaming; a separately documented Liminal publisher. | Locutus owns capture/turn assembly for the first integration. Direct Earhorn/Liminal use is an optional later adapter, not an additional microphone reader. |
| Standalone `gobetween` | A separate conversational relay that can run a small model and consult a larger model. | Optional delegated voice conversation later. Not required for direct Norn read-aloud or dictation. |

`MOUTH-MCP.md` distinguishes the five direct tools from a proposed sixth delegated-conversation tool. Its statement that the mouth design is not built must not be mistaken for the five existing MCP tools being absent, or for the sixth tool already existing. Norn's first voice integration should speak the selected Norn answer directly, not silently hand the conversation to a proxy model.

## MCP path

Read source: `src/bin/locutus-mcp.rs`, especially `SayArgs`, `ListenArgs`, `EarsArgs`, `get_info`, `notify`, and `Gobetween::connect`.

- `say` accepts `text` and optional `voice`, and returns after playback. It has a cancellation guard intended to hush abandoned speech. Cancellation propagation through Norn's MCP client and concurrent calls still needs end-to-end acceptance; the guard alone is not proof of it.
- A missing per-call voice uses `LOCUTUS_VOICE`; if neither supplies a voice, the source reports an error rather than inventing one. Use `voices` to obtain valid choices. The server also supports local Kokoro, an explicitly selected remote Gob backend, or an explicitly selected ElevenLabs backend; conflicting backend choices are refused.
- `listen` waits for a turn, with optional `timeout_s`. `ears` takes required `on` and optional `mode`; `mode` selects `off`, `open`, `hold` or `wake` when provided. Locutus hold/wake are microphone-turn modes, distinct from Norn's channel next-turn/wake admission policies.
- The notifier sends unclaimed turns as `content` plus string metadata `speaker`, `text`, and `at`. The current `speaker: "tom"` is a producer label, not authenticated speaker recognition: the microphone can hear other people and playback.
- Norn must explicitly admit the configured MCP source, using its existing settings/flags. Norn's normal generic channel delivery admits model input; that is **not** the proposed dictate-into-draft behaviour. Dictation must use a separate, explicit input route that preserves draft review.
- Notifications currently lack a stable source turn ID, and `unclaimed()` drains turns before sends finish. Do not advertise durable end-to-end replay or exactly-once delivery on this interface. Correlated turn IDs, failure/acknowledgement semantics and any replay belong in a coordinated contract update.
- The source currently polls unclaimed turns on `NOTIFY_TICK` (250 ms). Norn should not add another poller; ask the Locutus owner to move notification readiness onto a pushed signal when scoping production integration.

A direct MCP tool demonstration is the smallest future compatibility checkpoint. It must still exercise echo rejection, stop behaviour and disabled-resource behaviour before enabling unattended open-microphone wakeups. No launch command or executable installation path is claimed verified by this read.

## Control socket path

The interface is documented in [Locutus presence door](/Users/tom/Developer/projects/locutus-merge/docs/presence-door.md) and represented in `contract/src/door.rs` and `src/bin/locutus-mcp/door.rs`.

Client intents currently are `register`, `mode`, `press`, `release`, `hush`, `take`, and `give`. Registration names a seat and optional account/voice. Door events include `state`, `partial`, `turn`, `say`, `word`, `spoken`, `hushed`, `error`, and a reserved `correction` variant. Events carry a server-minted `session` and `at`, seconds on its monotonic clock. These are not UTC instants; persisted Norn history needs a separately labelled host receipt time, not a conversion that pretends the monotonic value is wall time.

`partial.text` is the **whole current partial**, not an append-only token delta. Replace/reconcile that provisional display without appending every partial to the user draft or session history. A `turn` gives committed text, `delivered`, a dropped reason and `to`. State carries `holder`, mode and playback/capture state. Preserve drops and errors as visible state rather than treating every received turn as user input.

Recommended Norn mapping:

| Norn action or view | Existing Locutus operation | Boundary still needed |
| --- | --- | --- |
| Connect voice UI | Subscribe to the named control socket and render initial `state`. | Protocol/version negotiation and socket ownership/access policy; no assumption that a seat string authenticates a client. |
| Select a voice participant | `register` with the chosen seat/account/voice. | Bind Norn's stable recipient identity to that seat explicitly; account persistence is not implemented merely by carrying an account string. |
| Push to talk | `mode: hold`, then `press`/`release`. | Local key capture/focus loss, disconnect and release recovery; dropped turns must not enter drafts. |
| Display recognition | `partial`, then committed `turn`. | Recipient/draft-revision binding; partial replacement and a deliberate final draft edit with undo. |
| Stop speaking | `hush`; `press` also hushes the mouth. | Correlation and arbitration so stopping one participant does not unexpectedly cancel another participant's speech. |
| Inspect playback | `say`, `word`, `spoken`, `hushed`. | Keep server session and say ID together; synthesis timing and audible playback completion remain distinct. |
| Receive turns for a seat | Register and use the explicit `take`/`give` line ownership, or named wake routing. | Exactly one Norn admission route for that turn, respecting `to`; observers receive events but must not all submit them. |
| Request speech | MCP `say` currently carries text/voice. | The current control-intent enum has no `say` request, and MCP SayArgs has no seat field. Resolve seat-to-say attribution before claiming a shared socket is a complete multi-seat speech API. |

The door broadcasts events to clients. Filtering a displayed recipient is not access control, and registration/account fields alone do not provide tenant isolation. Decide the intended trust boundary and permitted readers/controllers before shared use. The present contract also has no general request ID or delivery acknowledgement. A `state` update can arrive independently or more than once, so it cannot be used as an exact command receipt without correlation.

For dictation, route final speech to the captured Norn draft instead of simultaneously delivering it through Channels. The documented holder/`to` mechanism suppresses owning-session notifications for seat-directed turns; preserve that property and specify how disconnect/reconnect, overlapping listen calls and stale recipients behave. Do not rely on matching transcript text to deduplicate two input paths.

## Resource ownership and echo

**Existing startup is not lazy speech activation.** `Gobetween::connect` opens playback (unless `silent`) and constructs the chosen mouth; a local mouth loads the engine at server startup. Therefore simply putting Locutus in always-started MCP settings does not satisfy the Norn requirement that disabled voice consumes no model/audio resources. `--silent` still generates audio and is not a no-resource mode.

Choose a concrete ownership arrangement: an explicitly enabled MCP/service instance, an explicitly selected shared remote service, or a Locutus change that defers mouth/device initialization until use. Norn must not hide this cost or spawn one resident engine for every agent by default. Leave microphone and playback ownership in Locutus for the first integration; a Norn TUI and the separate presence app are clients of that same authority.

`MOUTH-MCP.md` records an echo incident in which the server's own speech returned as a Channel message attributed to Tom. Current source has playback logging and newer echo-related tests, but this pass ran none and cannot say whether that incident is now fixed. **Re-test the actual notifier path**, not only the turn assembler, before autonomous voice response or always-open wakeup. Cover playback interruption/tail, device changes, simultaneous human speech, and multiple clients. A producer label must never override evidence about the origin of the audio.

Gob's WebSocket audio consists of ordered `begin`, binary samples/word events, then `spoken`; binary frames belong to the current say and carry no separate ID. Earhorn partial/final/clean events have their own semantics. If Norn later uses those lower-level interfaces directly, use their typed contracts and retain original versus corrected recognition. The initial integration should avoid duplicating their audio processing or clocks.

## Relationship to Liminal and other clients

The existing control door is Unix-socket JSONL, **not Liminal**. The presence document describes carrying its semantics over other transports as a future seam. Earhorn's separate `serve` interface already documents Liminal transcript/control channels; that does not make the whole Locutus door Liminal today.

Use a bounded adapter to the existing door initially, and record the later Liminal binding under D19 with the Locutus owner. Do not invent a parallel permanent plugin protocol, rewrite the external service just for Norn, or require the full Manifold/module programme before users can hear an answer. Locutus's registration is presently process-lifetime state; future durable seat/account voice settings must be reconciled with the lifetime-memory identity work, not conflated with it.

## Concrete future checkpoints

1. **Direct speech and cancellation:** use existing MCP `say`/`voices` for one explicit answer; verify real stop/playback behaviour and show failure honestly. This proves tool-mediated voice, not native TUI read-aloud.
2. **Native read-aloud:** a user-triggered, typed Norn output subscriber calls the adapter without requiring an extra model turn. Bind selected message/revision, persist preferences, and keep replay intentional. Disabled mode must be demonstrably lightweight.
3. **Native dictation:** subscribe to door partials and route one committed turn into one captured Iridium draft. Correct it and send with the configured key. No duplicate Channels delivery and no background-agent retargeting.
4. **Shared seats and presence:** settle say attribution, holder lifetime, observer permissions, correlation and reconnect/version handling; coexist with the Locutus presence app using the same switch.
5. **Optional streaming and delegated conversation:** only after echo and ownership acceptance. The proposed proxy mouth remains separate from ordinary direct speech and is enabled explicitly.

Resource/latency measurements, contract tests and interactive audio acceptance are deferred until Tom makes resources available. No supported-install, authentication, cancellation or live-audio result is inferred from source and docs alone.

## Source map for the handoff

All paths below are under `/Users/tom/Developer/projects/locutus-merge`:

- `src/bin/locutus-mcp.rs`: tools, startup ownership, channel capability/notifier, cancellation guard and control dispatch.
- `src/bin/locutus-mcp/door.rs`: socket, seats, line holder, mode and event broadcast.
- `contract/src/door.rs`, `contract/src/mouth.rs`, `contract/src/ears.rs`: wire shapes and decoding contracts.
- `docs/presence-door.md`: portable control and presence contract, including later seat-based owner correction.
- `MOUTH-MCP.md`: direct MCP versus delegated mouth distinction and recorded echo incident.
- `src/gob.rs`, `src/playback.rs`, `src/ears.rs`, `src/echo.rs`, `src/turns.rs`: existing audio and turn ownership to reuse, not reimplement in the Norn renderer.
- `gob/README.md`, `earhorn/README.md`: backend and transport details; no backend installation was inspected or attempted.
- `presence-core/`: portable client foundation to assess for reuse; no compatibility or resource-cost measurement performed here.
