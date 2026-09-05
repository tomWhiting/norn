---
type: design
cluster: norn-retained-tui
title: Retained foreground transcript and exact tool Changes
---

# Retained foreground transcript and exact tool Changes

## Intention

Make terminal history compact, readable and stable under resize while preserving exact semantic evidence and existing execution behavior.

## Problem

- Native scrollback plus fixed-region cursor accounting loses reliable geometry under resize.
- Completed tool arguments are discarded from the current UI projection; old result expansion and exact-call evidence need stable correlation.
- Switching to alternate screen requires an owned reading, selection/copy/search/export story and migration of every notice/error/status producer.

## Solution

Share terminal-independent session_view semantics with the later host while delivering the local foreground retained screen now. Keep current agent/editor/control owners.

### CORE: Shared semantic core

Typed identities, deterministic projection, approved body capabilities and demanded history.

### VIEW: Owned viewport

Compact transcript and read-only Changes above a full-width composer, one terminal owner and preserved focus.

### PROOF: Real terminal acceptance

Semantic assertions, real PTY resize/copy/controls, supported-terminal evidence and exact-commit venue release checks.

## Decisions

### D1: D1 checkpoint decision

One full-screen owner; conversation and collapsible workspace above a full-width composer; no global header or permanent global footer. Necessary target, model and status information uses pane-local labels and composer controls.

**Rationale:** Recorded owner direction and root settlement, 5 September2026 Melbourne; source model preserved in this cluster.

### D2: D2 checkpoint decision

Deliver the complete local foreground retained-transcript checkpoint first. Keep the current agent driver, editor and cancellation owners; make norn::session_view reusable by the later independent host. No Liminal, Aion or Iridium runtime dependency is introduced by this checkpoint.

**Rationale:** Recorded owner direction and root settlement, 5 September2026 Melbourne; source model preserved in this cluster.

### D3: D3 checkpoint decision

Compact original tool descriptions/status by default, expandable individual historic bodies and a read-only per-call Changes view. The latter is evidence of calls, with truthful commit/diagnostic states; it is not a session-start or Git comparison.

**Rationale:** Recorded owner direction and root settlement, 5 September2026 Melbourne; source model preserved in this cluster.

### D4: D4 checkpoint decision

First-slice shared core owns deterministic item/body/cursor semantics and demanded store reads; the local TUI adapter owns its single projection. It consumes existing broadcasts and marks missing live coverage explicitly. Reliable retained producer feed, atomic Ready registration, committed publication across all append families and hosted control remain the coordinated later host foundation.

**Rationale:** Recorded owner direction and root settlement, 5 September2026 Melbourne; source model preserved in this cluster.

### D5: D5 checkpoint decision

Display collapse, search, copy, scrolling, resize, expansion and pane selection never change stored events, provider input, active output contract, selected model policy or execution ownership.

**Rationale:** Recorded owner direction and root settlement, 5 September2026 Melbourne; source model preserved in this cluster.

### D6: D6 checkpoint decision

A completed local checkpoint is not the complete workspace programme. Retain NWP-02 host/attach, NWP-05 Iridium and configurable send, NWP-08 mutation receipts/baselines/guarded editing/Work/Session/modules/request contracts, and full inline-diff preference as named wanted follow-on work.

**Rationale:** Recorded owner direction and root settlement, 5 September2026 Melbourne; source model preserved in this cluster.

### D-F1: Settled F1

Accept D2/D4. Shared session_view vocabulary and demanded history/body ownership; local adapter now, retained producer/actual host later. R1 then R2 then integration rows; no parallel store mutation.

**Rationale:** Root implementation decisions within Tom-authorized staged improvements; no new user confirmation required. Recorded 2026-09-05T21:40:51.490572+10:00.

### D-F2: Settled F2

Changes initially closed; explicit tool link or pane toggle opens it. Save split/visibility per frontend. Declare initial equal upper-pane split, minimum content width40 cells each plus one divider. Below81 cells, single upper-pane switch. Full-width composer uses existing12-row maximum and at most half terminal rows; under height pressure preserve draft/caret and a minimal conversation row, then popup only when fit; zero geometry no paint. Tiny geometry explicitly requests resize. No global header/footer.

**Rationale:** Root implementation decisions within Tom-authorized staged improvements; no new user confirmation required. Recorded 2026-09-05T21:40:51.490572+10:00.

### D-F3: Settled F3

Accept per-item overrides surviving global Ctrl+O default; reset via view controls. F6 focus, F2 upper-pane toggle, PgUp/PgDn focused browse, non-composer Enter expand, F3 scoped search, F4 copy, F5 export; focused divider arrows. All actions additionally discoverable/available through /view command palette (and existing slash catalog) for terminals where function keys unavailable. Existing composer, popup, cancellation and control bindings keep precedence. Focus cycling skips closed panes; Shift+F6 reverse.

**Rationale:** Root implementation decisions within Tom-authorized staged improvements; no new user confirmation required. Recorded 2026-09-05T21:40:51.490572+10:00.

### D-F4: Settled F4

Declare UI history demand20 events, reusing existing initial replay count; body range demand65536 bytes per explicit load, UTF-8-safe chunking and visible load-more state. These are named UI defaults documented/tested centrally, not hidden retention limits. Keep demanded semantic rows; body cache only explicitly expanded/visible/selected revisions, evict unpinned collapsed bodies. No whole-body spool decoding on render/input; background work with revision check. OSC52 only explicit user-selected clipboard capability, no startup probing; default copy offers export when capability unspecified. Say sent for unacknowledged OSC52. Required PTY verification plus accessible local terminal/multiplexer manual evidence, do not claim unsupported matrix.

**Rationale:** Root implementation decisions within Tom-authorized staged improvements; no new user confirmation required. Recorded 2026-09-05T21:40:51.490572+10:00.

### D-F5: Settled F5

Accept response_items authority and generic attempt windows with explicit incomplete association. Loaded-scope default search labelled, older-history search explicit; selection/body revision bound. Exclude opaque reasoning/provider state/audio capabilities. /new retains existing draft command semantics, discards old view source, initializes new binding; no cross-session cursor reuse.

**Rationale:** Root implementation decisions within Tom-authorized staged improvements; no new user confirmation required. Recorded 2026-09-05T21:40:51.490572+10:00.

### D-F6: Settled F6

Amend registration dependency: start isolated stacked worktree on exact frozen/pushed NCS candidatef0cb now; NCS must be venue-green and integrated before NRT landing. No edits to NCS worktree. Refresh inventory to f0cb; merge/rebase and repeat exact battery if NCS changes. This removes an unnecessary authoring wait without borrowing NCS verification.

**Rationale:** Root implementation decisions within Tom-authorized staged improvements; no new user confirmation required. Recorded 2026-09-05T21:40:51.490572+10:00.

## Goals

- All eight identity invariants and all producer obligations remain explicit.
- Visible per-call changes never claim session-start baselines or all filesystem authorship.
- One complete local foreground checkpoint preserves current controls while making history readable.

## Non-Goals

- These are checkpoint boundaries, not permanent feature cuts: independent host/attach, Iridium composer, editable/guarded file workspace, Work/Session panels, Liminal modules and request-local structured output remain wanted as recorded below.

## Structure

- `crates/norn-cli/src/commands/slash/registry.rs`
- `crates/norn-tui/Cargo.toml`
- `crates/norn-tui/src/agents/activity_log.rs`
- `crates/norn-tui/src/agents/status_line.rs`
- `crates/norn-tui/src/agents/tabs.rs`
- `crates/norn-tui/src/app/autocomplete.rs`
- `crates/norn-tui/src/app/changes.rs`
- `crates/norn-tui/src/app/changes_tests.rs`
- `crates/norn-tui/src/app/child_results.rs`
- `crates/norn-tui/src/app/dispatch.rs`
- `crates/norn-tui/src/app/dispatch/finalization.rs`
- `crates/norn-tui/src/app/edit.rs`
- `crates/norn-tui/src/app/event_loop.rs`
- `crates/norn-tui/src/app/export.rs`
- `crates/norn-tui/src/app/export_tests.rs`
- `crates/norn-tui/src/app/focus.rs`
- `crates/norn-tui/src/app/helpers.rs`
- `crates/norn-tui/src/app/mcp_slash.rs`
- `crates/norn-tui/src/app/mod.rs`
- `crates/norn-tui/src/app/model_selection.rs`
- `crates/norn-tui/src/app/notices.rs`
- `crates/norn-tui/src/app/render.rs`
- `crates/norn-tui/src/app/rotation.rs`
- `crates/norn-tui/src/app/search.rs`
- `crates/norn-tui/src/app/search_tests.rs`
- `crates/norn-tui/src/app/selection.rs`
- `crates/norn-tui/src/app/selection_tests.rs`
- `crates/norn-tui/src/app/session_replay.rs`
- `crates/norn-tui/src/app/slash.rs`
- `crates/norn-tui/src/app/slash_catalog.rs`
- `crates/norn-tui/src/app/state.rs`
- `crates/norn-tui/src/app/streaming.rs`
- `crates/norn-tui/src/app/tool_calls.rs`
- `crates/norn-tui/src/app/transcript.rs`
- `crates/norn-tui/src/app/transcript_tests.rs`
- `crates/norn-tui/src/app/turn/mid.rs`
- `crates/norn-tui/src/app/turn/run.rs`
- `crates/norn-tui/src/app/view_actions.rs`
- `crates/norn-tui/src/app/view_config.rs`
- `crates/norn-tui/src/app/view_config_tests.rs`
- `crates/norn-tui/src/app/viewport.rs`
- `crates/norn-tui/src/app/viewport_tests.rs`
- `crates/norn-tui/src/events/schema_render.rs`
- `crates/norn-tui/src/input/keybindings.rs`
- `crates/norn-tui/src/lib.rs`
- `crates/norn-tui/src/render/changes.rs`
- `crates/norn-tui/src/render/content.rs`
- `crates/norn-tui/src/render/fixed_panel.rs`
- `crates/norn-tui/src/render/frame.rs`
- `crates/norn-tui/src/render/frame_tests.rs`
- `crates/norn-tui/src/render/layout.rs`
- `crates/norn-tui/src/render/layout_tests.rs`
- `crates/norn-tui/src/render/markdown.rs`
- `crates/norn-tui/src/render/markdown/dim.rs`
- `crates/norn-tui/src/render/markdown/dim/list.rs`
- `crates/norn-tui/src/render/markdown/emitter.rs`
- `crates/norn-tui/src/render/markdown/scan.rs`
- `crates/norn-tui/src/render/markdown/scan/list.rs`
- `crates/norn-tui/src/render/mod.rs`
- `crates/norn-tui/src/render/retained_text.rs`
- `crates/norn-tui/src/render/scroll_region.rs`
- `crates/norn-tui/src/render/streaming_indicator.rs`
- `crates/norn-tui/src/render/style.rs`
- `crates/norn-tui/src/render/syntax.rs`
- `crates/norn-tui/src/render/text.rs`
- `crates/norn-tui/src/render/thinking.rs`
- `crates/norn-tui/src/terminal/caps.rs`
- `crates/norn-tui/src/terminal/clipboard.rs`
- `crates/norn-tui/src/terminal/mod.rs`
- `crates/norn-tui/src/terminal/setup.rs`
- `crates/norn-tui/src/tools/compact.rs`
- `crates/norn-tui/src/tools/helpers.rs`
- `crates/norn-tui/src/tools/minimal.rs`
- `crates/norn-tui/src/tools/mod.rs`
- `crates/norn-tui/src/tools/renderer.rs`
- `crates/norn-tui/src/tools/rich/bash.rs`
- `crates/norn-tui/src/tools/rich/edit.rs`
- `crates/norn-tui/src/tools/rich/patch.rs`
- `crates/norn-tui/src/tools/rich/read.rs`
- `crates/norn-tui/src/tools/rich/search.rs`
- `crates/norn-tui/src/tools/status.rs`
- `crates/norn-tui/src/tools/summary.rs`
- `crates/norn-tui/src/tools/summary_tests.rs`
- `crates/norn-tui/src/tools/verbosity.rs`
- `crates/norn-tui/tests/mcp_channels.rs`
- `crates/norn-tui/tests/model_selection.rs`
- `crates/norn-tui/tests/pty_smoke.rs`
- `crates/norn-tui/tests/retained_workspace.rs`
- `crates/norn-tui/tests/support/mcp_channels_tui.rs`
- `crates/norn-tui/tests/support/retained_workspace.rs`
- `crates/norn/src/lib.rs`
- `crates/norn/src/loop/commands.rs`
- `crates/norn/src/session/spool.rs`
- `crates/norn/src/session/spool/range.rs`
- `crates/norn/src/session/spool/range_tests.rs`
- `crates/norn/src/session/store.rs`
- `crates/norn/src/session/store/history_page.rs`
- `crates/norn/src/session/store/history_page_tests.rs`
- `crates/norn/src/session_view/body.rs`
- `crates/norn/src/session_view/committed.rs`
- `crates/norn/src/session_view/contract.rs`
- `crates/norn/src/session_view/contract_tests.rs`
- `crates/norn/src/session_view/error.rs`
- `crates/norn/src/session_view/live.rs`
- `crates/norn/src/session_view/mod.rs`
- `crates/norn/src/session_view/projection.rs`
- `crates/norn/src/session_view/projection_tests.rs`
- `crates/norn/src/session_view/response.rs`
- `crates/norn/src/session_view/tools.rs`
- `docs/design/norn-retained-tui/CHECKLIST.md`
- `docs/design/norn-retained-tui/DESIGN.md`
- `docs/design/norn-retained-tui/USER-STORIES.md`
- `docs/design/norn-retained-tui/acceptance.json`
- `docs/design/norn-retained-tui/briefs/NRT-001.json`
- `docs/design/norn-retained-tui/briefs/NRT-001.md`
- `docs/design/norn-retained-tui/checklist.json`
- `docs/design/norn-retained-tui/design.json`
- `docs/design/norn-retained-tui/producer-coverage.json`
- `docs/design/norn-retained-tui/stories.json`

## Constraints

- **SCOPE** — No provider/tool execution engine changes, session-host/Ready transport, active-input policy change, Liminal dependency, Iridium embedding, filesystem watcher, session baseline capture or mutation-owner work in this checkpoint. Those remain wanted programme work, not permanent feature cuts.
- **SCOPE** — No added dependencies or arbitrary configuration values. R4 adapts existing presentation APIs and the named Cargo package description only; dependency changes are outside the wall.
- **SCOPE** — No new durable transcript store, raw terminal recording, ANSI emulator or sidecar agent. Use existing EventStore plus one semantic view.
- **SCOPE** — Root owns programme/tracking updates and NCS integration outside these product walls.
- **SCOPE** — No silent skipped prerequisites, lint bypasses, or reduction of existing semantic assertions. Source changes need the existing rigorous review and exact-commit venue receipt.

## Topics

### Public shared core contracts for R1

**ViewSource / StoreInstanceId / SessionIdentity** (R1 vocabulary; R2 minting): A source binds persisted-or-explicit-ephemeral session, local store instance and agent identity. StoreInstanceId is minted independently for every EventStore instance; it is not a persisted generation or host identity.

**HistoryCursor / HistoryRead / HistoryPage** (R1 types; R2 implementation): Opaque cursor binds exact source, event ordinal and EventId, including empty-start. Read uses explicit direction/anchor/event demand. Page exposes approved compact semantic items, next cursor and coverage; never full opaque event payloads. Typed errors identify wrong source, stale generation, mismatched anchor and unavailable data.

**ItemId / ProvisionalKey / ViewRevision / ViewItem** (R1): Committed identity and provisional execution/response/attempt/segment identity are disjoint. Revision is local projection revision, not history durability. Tool IDs retain call_id with supplied item aliases; authoritative response_items replace flat projections exactly once. Item bodies are references, not cloned raw events.

**BodyRef / BodyRepresentation / BodyRead / BodyPage / BodyAvailability** (R1 types; R2 validated store/spool minting): Only allowlisted event display fields, owner-validated spool event references and revision-bound provisional display fragments can be read. BodyRead is an explicit byte range; response repeats source/item/representation/revision, actual range and continuation. Public callers cannot mint arbitrary paths/roots/JSON pointers. No opaque reasoning, provider state, transport credential or raw audio capability.

**Projection / ProjectionInput / ProjectionChange / CoverageState** (R1): Pure deterministic reducer accepts typed approved semantic inputs with explicit provenance. It records replacements/aliases, tool status, model metadata and source resets without terminal I/O, store reads or provider calls. Lost transient coverage stays explicit; this interface has no Ready, observer snapshot, durable feed or host lifecycle guarantee.

**SessionViewError** (R1): Typed source/cursor/body/projection errors carry the referenced identity or field. Failed validation cannot yield an empty body/successful association. No unwrap/expect, ignored errors, arbitrary defaults or terminal dependencies.

### Source, model and body invariants

**I1 Source and timeline** Every item is scoped by ViewSource: persisted session identity when available, explicit ephemeral identity otherwise; root/child agent identity and parent relation; local store-instance generation. Store-instance generation changes on store replacement/reopen, never because geometry changes. A persisted EventId plus validated ordinal identifies committed content. Do not call local store-instance generation a durable store generation or reuse another session cursor after /new. R2 adds a distinct store-instance identity at all three EventStore constructors; persisted SessionBinding supplies the session identity. R1 makes cursor fields private and gives crate-internal validated minting to the store adapter; external callers cannot manufacture a validated cursor.

**I2 Model/configuration** Capture the actual accepted ModelRuntime selection at local turn admission: canonical model, route, effective context and selected effort/tier, with a local configuration revision. ModelChange events retain their old/new values. Earlier history with no model evidence stays unknown; do not label it using today's model. Rendering a historical item must not apply its configuration. Preserve active/pending/refused semantics supplied by current control owners, including NCS after landing.

**I3 Provisional identity** ProvisionalKey = source + agent + local execution + response iteration + attempt + semantic segment. This key is explicitly volatile, not an EventId. Response/part IDs are used when supplied; unkeyed TextDelta/ThinkingDelta are local ordered segments only. Retry invalidates the interrupted attempt's uncommitted fragments and retains a retry notice; already committed items/results remain. Never correlate by text equality, tool name, clock time or guessed provider identity.

**I4 Authoritative replacement** At accepted committed-history boundaries, reconcile by event/source IDs and explicit local attempt/response mapping. When AssistantMessage.response_items is present it is authoritative; flat content/thinking/tool_calls are projections and must not be rendered a second time. Preserve stable anchors with explicit provisional-to-committed alias mappings only when association is proven. If generic events cannot establish the association, replace the identified response window from committed history and expose incomplete provisional association; never silently invent exact identity.

**I5 Tool identity and state** ToolKey = source + agent + call_id, narrowed by committed invocation identity when needed. Streaming item_id is an alias while call_id is unavailable, never the call_id itself. Keep call kind, original argument string, description availability, invocation reference and result reference after completion. Custom/freeform arguments remain strings. A result in an incomplete history page may be an orphan awaiting explicit older-history demand; do not manufacture empty arguments or join unrelated same-name calls. Lifecycle states include assembling/running/completed/failed/blocked/cancelled/incomplete, each only when evidenced.

**I6 Bodies** BodyRef is typed: a committed event plus an allowlisted display field/item path; a validated spool reference obtained from that event and approved session root; or provisional fragment storage owned once by the projection with its exact revision. A caller cannot supply an arbitrary path or arbitrary JSON pointer. Body identity includes source, owner item, representation and revision; reads return the same identity/range or an explicit stale/unavailable/malformed error. Historical content never resolves through today's workspace bytes. Opaque reasoning, credentials, reusable provider transport state and raw audio bytes are not display body capabilities. Spool capabilities are minted from a stored event through the owning SpoolWriter, which retains its private data_dir, registered entry and root_session_id. Body callers supply the capability and range, not a root path; the owner opens through PrivateRoot and validates the reference.

**I7 Demand and cursor** HistoryRead requests explicit before/after cursor and event demand. Cursor binds source/store generation, ordinal and EventId, with a distinct empty-start value. Validate all components; clone only selected records while locked and do body/file work after releasing the store lock. No events() clone on paint/resize/scroll. BodyRead names an explicit byte range/representation; preserve UTF-8 boundaries and incomplete chunk state. Current spools hold serialized JSON: raw serialized-JSON ranges are honest; field decoding cannot be claimed range-bounded without an additional index. Parsing/formatting occurs off the render/input path and results are revision-tagged. History items are lightweight semantic headers/body capabilities, not cloned raw SessionEvent values or opaque provider payloads. Projection adapters borrow only approved fields. Source generation/cursor validation precedes every page operation.

**I8 Coverage and authority** Projection revision advances for local view changes; committed-history cursor advances only over observed accepted store events. Broadcast lag marks live state stale/incomplete, retains committed content and reconciles the committed suffix at the normal boundary. Missing transient status/messages stay explicitly incomplete even after history catches up. Store sink acceptance is not an fsync guarantee. No first-slice Ready, gap-free live snapshot, automatic effect replay or durable crash-resume claim. The later host must install retained producer state before fan-out and publish every append family, including provider-identity adoption.

### Viewport and controls

- Viewport state belongs to the frontend: focused region, selected item/change, expansion overrides, follow/pin, search state, logical scroll anchor and text-selection endpoints. No terminal cells enter norn::session_view.
- Anchor = stable item/body identity plus logical text offset and affinity, not absolute screen row. Selection endpoints additionally bind body revision. Soft-wrap/Unicode width mapping changes on resize; original content offsets do not. If a selected provisional body is replaced or becomes unavailable, retain a named stale selection until explicitly reselected; never copy different bytes.
- Follow applies only when selected by the person. Scrolling back, selecting, typing or opening an older change pins that view. Incoming activity increments a notice without replacing the selected historical body, stealing focus or moving the composer caret. Return-to-live is explicit.
- One terminal event reader and one frame writer own input, mouse reports, geometry, alternate-screen entry/exit and the active caret. Local styled spans/cells are composed once. Reuse markdown parsing, syntax and diff data; do not parse captured terminal output or create a second terminal driver/emulator.
- The composer spans the full terminal width even when Changes fills the upper region. Existing editor wrapping/height behavior is preserved where it fits; height-pressure ordering is declared in D-F2. No global header/footer or empty routine-notice strip; errors have retained items, and currently relevant status remains accessible through composer/pane controls.
- Narrow mode displays one upper pane with a visible keyboard/mouse switch. Widening restores saved split preference, selected change, scroll anchors, focus and draft. Never submit, append semantic rows, restart a request or move content to native scrollback as a resize side effect. A zero-size report suspends painting; very small geometry retains state and offers a truthful resize-required view when controls cannot fit.
- Popup ownership precedes view navigation and send. Composer Up/Down/history, Enter/Alt-Enter/Kitty Shift-Enter, Escape and Ctrl+C keep their current meaning. Browsing keys only act in their focus or via distinct global view shortcuts. Mouse selection/scroll has keyboard equivalents; click targets derive from the current layout and item identity.
- Copy uses selected logical content with original hard newlines, excluding wrap-inserted newlines, chrome and terminal escapes. Copy transport is capability-specific and reports sent/unavailable/failed truthfully; an OSC52 write without acknowledgement is not proof the clipboard changed. Export to an explicitly chosen path remains usable when clipboard is unavailable, with a visible overwrite decision and no hidden transcript persistence.
- Search is a deliberate read operation, not a provider request. Show scope (loaded transcript / selected body / explicitly requested older history), direction and whether the search is complete. Never report no match for history or body ranges not searched. Export names its scope and missing ranges too.
- All external/model/tool/file display bytes are data. Control characters, OSC, CSI and hyperlinks are safely escaped or rendered through approved typed styling/actions; filename or tool text cannot emit terminal commands. Body loading errors remain visible and do not take down active agent execution.

### Read-only call Changes

- The initial title is Changes with a scope label such as Tool call <call_id>. Group by evidenced path and exact call; preserve source/agent identity. There is no implicit session-start baseline, current-file preview, Git HEAD comparison, rename detection or external-writer attribution.
- edit: show old_string → new_string as an edit fragment, not a whole-file diff. Preserve committed and after_hash independently of diagnostics/error. Missing/ill-typed strings are evidence unavailable, never empty strings. A blocked edit is a proposed fragment labelled not committed.
- write: show submitted content and committed/bytes_written/path facts when available. There is no captured before-content in current write output, so before comparison is unavailable; an empty before-file is not inferred. Post-write diagnostic failure may follow a committed write.
- apply_patch: show the supplied patch tied to the call and the result's evidenced outcome. A submitted patch alone is intent, not proof all hunks committed. Never regenerate the old patch against current disk contents.
- Unknown/Bash/MCP tools retain description, call identity, raw detail and evidenced result. Their arbitrary filesystem writes are outside structured receipt coverage; a nearby shell/tool invocation does not supply authorship or before/after hashes. No action-log-only filter, because its successful-only mutation entries omit committed-with-error cases.
- The compact row retains the original description when supplied; otherwise show the tool name and explicitly absent description, with a separately identified argument summary only if the existing semantics supply it. Never invent a description. Keep running/error/commit state and duration visible; body collapse applies to unknown tools too.
- Per-item expand/collapse survives new activity and history paging. Ctrl+O changes the global expansion default; explicit per-item overrides remain until the user resets them through the view controls. This policy is settled by root D-F3.
- Changes is read-only in this checkpoint, with no save/editor mutation capability. This does not remove the wanted guarded human editor, immutable mutation receipts, session baselines, rich inline-diff preference or Work/Session tabs from NWP-08.

### Declared UI defaults and override owner

Geometry defaults and validation live in render/layout.rs (R8). History demand20, body demand65536, clipboard capability unspecified and Changes closed are declared centrally in app/view_config.rs with tests (R3), documented here and inspectable/settable through /view (R6). These are UI request/geometry preferences, not retention quotas or execution limits. Explicit values override defaults. Body cache retains only visible/expanded/selected revision bodies; unpinned collapsed bodies are evictable, while semantic identities and demanded rows remain. /view settings are frontend-local for this checkpoint, with no hidden file persistence or claim of broader startup settings integration.

### Independent layout ownership

R8 (label R4.layout) is root-owned, depends only on R0, and creates exactly render/layout.rs plus render/layout_tests.rs. R4 consumes it after R3 and R8. LayoutRequest/LayoutPolicy describe geometry only; Layout returns content/composer/divider rectangles and no-paint/resize-required/single/split mode. Equal default split assigns an odd surplus cell to conversation; clamping never mutates the saved ratio. Default40-column minima and one divider imply81-column split threshold; default composer maximum12 and one-half screen retain existing policy. Typed explicit overrides are validated; no provider/session/state dependency is introduced.

### Producer coverage

[producer-coverage.json](producer-coverage.json) is the exhaustive local AgentEventKind/ProviderEvent/SessionEvent disposition and all producer-site obligations. Raw observability/private replay data is never a generic display body. It is refreshed to the exact NCS baseline; no TUI/event-enum source changed in the51-path NCS delta.

### Required acceptance

[acceptance.json](acceptance.json) defines PTY-01..PTY-09, including actual PTY geometry, real producer/store oracles and actual terminal/multiplexer copy/restoration evidence. These are requirements, not executed results.

### Dispatch and landing

Implement the settled NRT-001 checkpoint on frozen NCS commit f0cb7476ceb80ce6b9c85a02088cdb8297e960a3. R0 registers this contract; R1 and R8 (R4.layout) may then run independently. R2 follows R1; R3 follows R2; R4 follows R3 and R8; R5, R6 and R7 follow sequentially. Root owns R8 only; one integration owner owns overlapping R3-R6 paths. NCS must be exact-commit venue-green and integrated before NRT landing. If NCS changes, reconcile/rebase and repeat exact-candidate verification. No approval or preference placeholder remains. Stop and name required files before editing outside a row wall.

### Wanted follow-on scope

- NWP-02 independent host, current-state attach, observer lifecycle and Liminal transport
- NWP-05 Iridium editor adapter, configurable Enter/Alt-Enter send and focused editor capabilities
- NWP-08.1 baseline/receipt/file activity with explicit external-writer attribution limits
- NWP-08.2 guarded human editor and immutable before/after references; human draft/conflict/undo behavior
- NWP-08.3 Work/agent/task/goal panel with existing owners
- NWP-08.4 Session instructions/skills/tools/model settings with validated control boundaries
- NWP-08.5 shared Liminal panel extension contract
- NWP-08.6 per-request schemas and request-local validated result views
- NWP-04 optional full inline diffs alongside workspace views; no silent loss of search/copy/export

### Current source evidence

Baseline `f0cb7476ceb80ce6b9c85a02088cdb8297e960a3` on `codex/norn-retained-tui`. Refreshed 2026-09-05T21:48:02.621060+10:00 Melbourne. All73 previously inventoried source files are byte-identical to main5227; NCS changes51 other/launch paths. Detailed source hashes and exact path delta are fields of design.json. This registration edits documentation only and makes no NCS/NRT implementation or release proof claim.

### Implementation progress

R0 documentation is registered and validated. R1 shared semantic core is in progress. R8 pure layout is implemented with six standalone tests, rustfmt check, standalone pedantic Clippy and bounded AST policy passing against its recorded two-file hashes. The layout is not wired into the TUI. This is local row proof only; the retained-TUI checkpoint, full workspace validation and exact-commit venue acceptance remain incomplete. Detailed supplied proof bindings are in design.json.implementation_status.
