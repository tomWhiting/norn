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

Collapsed tool rows prioritize the original description and lifecycle state, with concise duration only when recorded and explicit missing description. Technical call identity, commitment, missing-duration availability, parse errors and all retained metadata remain accessible in expanded detail or selected Changes. This preserves vertical reading space without discarding evidence. Individual historic bodies remain expandable, and the read-only per-call Changes view reports truthful commit/diagnostic states; it is not a session-start or Git comparison.

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

### D-R2-DISPATCH: Early R2 dispatch after shared contract freeze

R2 may dispatch after the R1 opaque cursor/body/HistoryRecord contract listed here is fixed and both owners have received it, while remaining R1 reducer tests and static refinement continue. Root has accepted that freeze at this amendment. norn_models owns only R2 store/spool paths; channels_compatibility owns R1 session_view paths. Any shared contract change is coordinated with both owners before editing it. R2 semantic completion and integration/test acceptance still require completed R1 and R2; depends_on remains R1.

R2 may transiently clone only the explicitly selected SessionEvents while holding the store lock, release the lock, and call project_committed off-lock. This bounded selected-event staging is not retained view state. Every public/retained HistoryPage carries only compact HistoryRecord values and approved body capabilities, never raw SessionEvent/opaque provider payloads. No filesystem I/O or display projection occurs under the store lock; no full-history clone is introduced.

**Rationale:** The fixed public/crate-internal seam allows useful independent work in exactly disjoint store/spool and session_view files. This changes dispatch timing, not completion or quality requirements.

### D-R2-MAILBOX-GENERATION: Validate managed store session generation

Modify crates/norn/src/session/branch.rs only to add a pub(crate) MailboxId::generation() -> Uuid accessor, or equivalent crate-private equality check. Initial managed EventStore view-source binding must validate actual SessionBinding/session-index generation against its owning SpoolWriter registered.generation as well as the session ID. Preserve all other branch behavior, existing mailbox serde/wire representation and visibility outside the crate. No generic identity service is introduced.

**Rationale:** Session ID alone does not prove that the initially bound managed store belongs to the current index generation; reuse the existing mailbox generation rather than inventing a second identity service.

### D-R1-INDEX: Incremental single-owner view index and stable correlation

R1 index.rs owns every ViewItem exactly once and maintains ordered committed keys, live order, direct ItemId lookup and per-call correlation indexes. Replace full-view sort/join per committed record and orphan-result × full-history rescans with incremental indexed operations. The new unreleased items() API becomes ordered iterator/range/lookup access; do not duplicate ViewItems or body storage. Existing typed body/cursor capability contracts remain unchanged.

- Each committed insertion/page application uses incremental ordering and direct identity/per-call lookup; it must not sort/join the entire view or rescan full history for each orphan. Meaningful operation-count fixtures distinguish local updates from total-history work; no invented timing SLA.
- A result whose parent invocation is outside the loaded page stays explicitly pending/unresolved until the correct invocation is available. Reuse of a call_id in a different invocation/response scope must not silently attach that result to the wrong call.
- ToolItem → ToolCall → Committed alias chains preserve the original early streaming anchor through final commit. Alias resolution is transitive, cycle-safe and source-scoped; index updates do not drop prior aliases.
- The ordered index stores each ViewItem once, exposes ordered iterator/range/lookup access and shares existing body references; no second body store or duplicated full retained item collection.

- Expose a borrowed indexed traversal beginning at an exact current ItemId in either direction, with declared inclusion semantics, using the existing ID-to-order lookup and BTreeMap range. Pinned viewport painting must not linearly seek through all earlier/later compact items. Missing or foreign identity is explicit, never an empty success or unrelated fallback; proven alias resolution remains separately explicit. No body loading/cloning, extra row owner or provider work. Regression fixtures cover committed/live boundaries, inclusivity, missing/foreign IDs and work bounded by requested rows rather than retained history length.

**Rationale:** review_tui identified work growing quadratically as committed history is applied and correctness gaps in page-orphan correlation and early-anchor aliases. Preserve semantic evidence while removing per-record full-view work.

### D-R4-GRAPHEMES: Use existing locked Unicode grapheme segmentation

Permit a direct unicode-segmentation = "1.13.2" dependency in crates/norn-tui/Cargo.toml and only its norn-tui dependency-list membership in Cargo.lock. The exact version already exists in the lockfile; no package version/checksum churn or other dependency/policy change. Grapheme boundaries govern retained styled-row clipping, wrapping and selection; scalar-only clipping is not an acceptable substitute. Existing ANSI-producing APIs are adapted to direct styled spans; no ANSI parser/capture is introduced.

**Rationale:** unicode-width measures display width but does not discover extended grapheme boundaries. Combining text and emoji ZWJ sequences must not be split by clipping, wrapping or selection.

### D-R3-BINDING: Carry actual session binding into the TUI frontend

Pass an explicit Arc<SessionBinding> into TuiInputs, whose declaration is in app/event_loop.rs already inside R3. The real CLI obtains its actual AgentToolInfra.session from its assembled coordination runtime; missing required infra is an explicit launch error, never a fabricated identity. Initialize and validate ViewSource through EventStore::bind_view_source. /new validates the newly created binding before the existing rotation commit, then replaces the view source and cursor/body state. Add transparent ViewError and HistoryReadError to TuiError in error.rs. Update only binding/error wiring in the three existing TuiInputs fixtures using their actual ephemeral setup; their semantic/PTY assertions remain unchanged until the sequential R7 work. Preserve resolved NCS launch-mode validation before provider/MCP construction.

**Rationale:** Session ID and directory strings do not prove the managed index generation. Reuse the coordination owner binding already installed by the CLI builder and validate it at the actual EventStore boundary.

### D-R2-SPOOL-INHERITANCE: Generation-bound inherited spool authority and writer replacement validation

R2-01 preserves exact ToolResult/spool_ref bytes and response-publication commitments. Closed inherited-spool authority is a publication-owned sidecar at the fixed owner-derived <destination-root>/spool-inheritance.json path, never an appendable Custom or ChildBranch grant. The existing atomic independent-fork journal carries its exact closed manifest: destination session/index generation, actual source session/generation/path, actual ChildBranch EventId/anchor, and unique exact copied EventId/reference grants with validated origin generations. Recovery publishes deterministic manifest bytes through a private temporary and exclusive publication before timeline/index publication; retries verify an existing sidecar exactly. Preserve existing journal recovery and strict directory inventory, admitting only the journal-declared known sidecar alongside audio. Resume/read hydration refuses duplicate, conflicting or mismatched manifests; source and destination registered generations are checked in one index transaction before PrivateRoot range reads. Carry only publication-owned validated prior grants through fork-of-fork. Appending a forged well-shaped Custom and matching ChildBranch with real current generation values confers zero authority. Pre-feature cross-root references without durable source-generation proof remain explicitly unavailable. R2-02 revalidates the current attached SpoolWriter against the immutable owner binding before every history/body read. No Custom metadata event, new index field, signature service, whole-body spool decoding or raw provider-state display is introduced.

**Rationale:** Root approved ten additional exact R2 source paths in two bounded amendments before edits. Independent review requires authority outside public EventStore append. No R1 metadata constant or display-classification change is needed.

### D-R4-TEXT: Independent styled-row geometry foundation

Use one plain displayed string with ordered typed style byte ranges. Segment extended grapheme clusters across style-span boundaries, then produce cell-bounded styled rows with displayed-byte ranges and hit-test boundaries. Style is data, never embedded ANSI. No norn/session types, body reads, terminal I/O, application state or provider work belongs in this module. The integration adapter separately maps original approved body bytes through visible escaping into these displayed-byte positions.

Root owns R9 (R4.text), an exact two-file independent slice after R0. Main R4 requires completed R3/R8/R9 and owns wiring/dependency integration. The external tiny harness is diagnostic only.

**Rationale:** Grapheme clipping/wrapping and source positions can be implemented and checked independently without competing with the semantic reducer or integration file walls.

### D-R6-ACTIONS: Reading-control split and independent search/export authoring

Hit testing and mouse dispatch validate the current frame source, body revision and rendered source-map identity. A logical placeholder anchor may preserve a requested item/offset while content is unavailable, but it is not the identity of a painted source map and cannot authorize selection, copy or an unrelated hit. Keep those identities distinct; stale/missing mappings are explicit and never rebound through a placeholder fallback.

Parallel authoring exception: app/search.rs and app/search_tests.rs may be authored independently as a pure literal-query search module over source-bound original text prefixes, with grapheme-safe match/selection boundaries. Every result retains exact source/body revision and original-byte coordinates and declares loaded-transcript, selected-body or explicitly requested older-history scope, including partial/unavailable coverage. An unsearched suffix or older range is never reported as no match. No I/O, provider work, module wiring or runtime mutation belongs in this slice. R5 remains prerequisite for R6 integration, validation and completion.

Parallel authoring exception: app/export.rs and app/export_tests.rs may be authored independently for exporting explicitly scoped, source/revision-bound original text to an explicit operator path. Default writes create a new file; replacement requires an explicit overwrite choice. Preserve original hard newlines and report missing/partial source coverage and path-specific errors; no hidden transcript persistence. Filesystem work must run off the terminal event loop when integrated. This authoring slice does not wire modules, dispatch runtime work or mutate session/provider state. R5 remains prerequisite for R6 integration, validation and completion.

Split reading-action orchestration into app/view_actions/reading.rs and reading_tests.rs: typed search state, navigation to an exact source/body-revision hit, and supervised explicit-path export I/O workers. Existing commands.rs owns command grammar. Preserve current-frame source/map identity independently of logical placeholder anchors, source-scoped partial/unavailable search/export status, create-new by default and explicit overwrite choice. Export filesystem work stays off the terminal event loop; worker completion/errors remain supervised and source-bound. This adds no provider, persistence or session-control capability. R5 remains prerequisite for R6 integration, validation and completion.

**Rationale:** Local command/key/mouse responsibilities remain within the R6 file wall. Current-frame source/map identity describes what was actually painted; a logical placeholder anchor preserves navigation intent for unavailable content and must not be substituted for that identity. Pure search and explicit-path export can be authored independently while R5 completes, but R5 still gates R6 integration, validation and completion.

### D-NRT-002: Exact producer publication tickets and input binding

One common owner owns observation schema, sender derivation, caller/setup wiring, shared append helpers, delivery_inputs caller, typed SessionError propagation and all producer regression fixtures. Use private OnceLock resolution cells and one coalescing execution Notify. Complete successful receipts synchronously in the actual append-owning closure before cancellable hooks or outer continuations; cloned tickets cannot publish. Validate source/sender identity; child senders drop parent scope.

The second owner alone owns indexed store history lookup, projection input/assistant reconciliation and retired-attempt fences, plus the ActiveInputDelivery leaf type. The common owner retains delivery_inputs/setup/shared append helpers and the single producer regression file. Fix the observation/index/input seam with both owners before parallel authoring; coordinate any schema change before dependent edits.

R3/R5 completion requires the NRT-002 exact producer publication-binding repair and its required proof. Existing implementation/integration authoring may continue under exclusive coordinated ownership; duplication, guessed association or an unverified repair cannot be reported complete. NRT-001 and NRT-002 form one final checkpoint; completion gates do not add circular source-authoring prerequisites.

Validate the managed current registered spool owner before acquiring the store lock and off the paint/input path, using the existing registered-entry guard in session/spool/range.rs. Cached store ID/generation alone cannot prove that a deleted/recreated session still owns the registered writer. Reuse existing authority; no new capability, filesystem work under the store lock or whole-history/body read.

Add a transparent boxed ObservationError variant in provider/channel_event.rs so checked scoped-envelope failures retain their actual typed source. Never relabel an observation/source failure as NoObservers or another delivery outcome; no payload duplication, new event route or observer authority.

**Rationale:** Actual PTY preview showed duplicate live/committed bodies. Only the real append owner supplies local acceptance provenance; response IDs, text and ordering are not substitutes.

### D-COMPLETION-ROW: One compact normal execution-completion presentation

For one exact admitted execution, normal End and TurnCompleted facts share one compact completed-execution presentation row. Selection or expansion exposes every original completion fact, source, model, usage, timing and availability field. Errors, cancellation and coverage gaps remain explicit. Never drop metadata, invent success or associate records by text, time or proximity. Implement presentation only through existing TUI file walls.

LiveReduction exposes completion_item: Option<ItemId> from the actual Done reduction; the core retains its compact Done label without adding a duplicate local metadata body. The TUI retains typed Done stop_reason, response_id and Usage under the exact active-execution identity, then moves those facts into the one final completion-details body. It uses exact returned routine notice IDs for compact presentation, never order/text/label guesses, and does not duplicate retained metadata or transcript caches. Every original fact remains inspectable; stale source/execution fences and existing source walls remain unchanged.

At finalise_turn, reconcile the actual root turn's final input/output usage against the already-accounted live_root_usage for that same turn. Add only that turn's missing deltas to the cumulative ledger and publish the actual current counters. The final result is authoritative. Reset/associate per-turn live accounting with exact root execution identity: a second turn with numerically equal usage must count again, not be mistaken for a duplicate. A duplicate Done fence has no UI usage side effect. Preserve all existing usage/model/budget controls and assertions; this is exact-turn reconciliation, not a new feature or a cross-turn numeric high-water mark.

**Rationale:** Tom prioritizes narrative and vertical space. Exact normal completion facts share presentation, while details preserve every fact and failures remain visible.

### D-R7-EARLY-FIXTURES: Canonical fixture authoring without early acceptance

Parallel fixture-authoring exception: after the current source freeze ends, channels_fixture alone may update the existing canonical pty_smoke.rs, model_selection.rs, mcp_channels.rs and support/mcp_channels_tui.rs. The already-declared retained_workspace.rs and support/retained_workspace.rs may be authored by a separate explicitly assigned owner. Preserve every semantic assertion and PTY acceptance. This permits authoring only: R7 validation/completion still requires R6 and the NRT-002 publication-binding repair. No source collision, early acceptance or extra path is authorized.

The already-registered R7 new_retained_workspace parallel-authoring partition is assigned to review_tui, exclusively for tests/retained_workspace.rs and tests/support/retained_workspace.rs. Author persistent actual-App PTY-04 reading-control/resize fixtures using the shared retained_screen oracle API and the existing 33-check external R6 evidence as a source of scenarios, not a substitute for executing these fixtures. channels_fixture remains the exclusive canonical-fixture/shared-oracle owner. No source paths or acceptance scope are added, and R6/NRT-002 prerequisites still gate R7 validation/completion.

**Rationale:** Separate fixture owners avoid source collisions while R6 and publication verification remain required.

### D-STRUCTURED-DETAILS: Source-mapped assistant secondary fields

Pure parallel authoring slice: render/retained_structured.rs and retained_structured_tests.rs provide a source-mapped RenderedMarkdown adapter for complete multi-field assistant JSON, preserving the existing secondary-fields/Ctrl+E acceptance. Every displayed field/range maps back to the supplied approved original source; partial JSON remains the original text with explicit partial state. Do not synthesize BodyRefs, invent content/fields or parse ANSI. No module wiring, body I/O, provider work or runtime mutation in this slice; the TUI owner integrates it against current source/revision mapping and retains all original bytes and secondary fields.

**Rationale:** Existing Ctrl+E and secondary-field behavior is part of the retained TUI acceptance. A pure adapter may be authored independently, with original-source mapping retained through integration.

### D-TEST-CLEANUP: Current modified-test cleanup without lint exceptions

Tom's no-allow-anywhere instruction overrides the inherited CLAUDE.md test exception. Before landing, remove inherited allow attributes and resolve the underlying test unwrap/expect uses in the eleven already-modified TUI files named by this partition. Preserve every assertion and propagate failures through Result; no skips, weaker assertions, renamed unused values or replacement lint bypasses. This is test-only cleanup of these current modified files, not unrelated unchanged-module or public-history audit work.

Exclusive test-cleanup partition: review_child_context owns test-only cleanup in crates/norn-tui/src/agents/tabs.rs, crates/norn-tui/src/app/autocomplete.rs, crates/norn-tui/src/app/helpers.rs, crates/norn-tui/src/events/schema_render.rs, crates/norn-tui/src/render/fixed_panel.rs, crates/norn-tui/src/render/markdown.rs, crates/norn-tui/src/render/text.rs. norn_tui retains crates/norn-tui/src/app/dispatch.rs, crates/norn-tui/src/app/event_loop.rs, crates/norn-tui/src/app/slash.rs, crates/norn-tui/src/app/state.rs. The active source owner must explicitly acknowledge handoff before parallel cleanup starts; no concurrent writes to any shared file. Production behavior remains unchanged, and all current brief acceptance and final verification requirements remain.

Core test-only cleanup within current modified source walls: review_child_context exclusively owns loop/commands.rs, session/branch.rs and session/spool.rs under crates/norn/src. norn_models retains loop/classify.rs and provider/agent_event.rs. Remove the five inherited allow attributes and resolve their underlying test unwrap/expect uses, preserving every semantic assertion and production bytes for this cleanup. Propagate Result failures; no skips, weaker assertions, wildcard bypasses or unrelated unchanged-module changes. Common-owner Clippy style repairs are a separately identified existing task, not permission to change production through this test-cleanup partition.

The 52-file pre-thaw core AST snapshot remains historical evidence. Source ownership must be acknowledged before parallel cleanup; no concurrent writes to a shared file. The whole-workspace Clippy01 run has ended and root explicitly released the document freeze. These are existing paths only and all NRT acceptance and final verification requirements remain.

The existing modified-core test cleanup includes inherited panic/panic!, expect_err and unwrap_err as well as unwrap and expect. Replace these with contextful fallible Result diagnostics while preserving every original semantic assertion and production bytes for the cleanup. No new skips, weaker assertions, or replacement bypasses.

norn_models additionally performs test-only cleanup in its already-owned NRT-002 R1 crates/norn/src/tests/integration.rs: the inspected inherited test code has six unwrap, sixteen expect and two panic uses. Do not remove or edit the allow attribute in unchanged tests/mod.rs, and do not expand into unrelated test modules. This is an existing-path cleanup; all owner coordination and final verification requirements remain.

**Rationale:** User instructions take precedence over the inherited repository test exception. Explicit file ownership protects concurrent implementation and preserves semantic tests.

### D-CLI-TEST-CLEANUP: Existing modified CLI test cleanup and canonical fixture failure reporting

After root released the actual-App source freeze, norn_models exclusively owns test-only cleanup in crates/norn-cli/src/print/orchestrator.rs and crates/norn-cli/src/print/output.rs, both already in NRT-002 R1. Remove their two inherited allow attributes and underlying 22/41 test unwrap, expect, panic, expect_err and unwrap_err operations. Preserve every existing test attribute, assertion predicate, expected-refusal case, lifetime guard and contextful failure diagnostic. Preserve the complete production prefixes byte-for-byte against the final core/CLI audit; no source behavior change, replacement suppression, skip, wildcard or unrelated unchanged-module cleanup is authorized.

Root records the TUI owner's explicit handoff of test-only crates/norn-cli/src/commands/slash/registry.rs to review_child_context. This existing NRT-001 R6 path has one inherited allow attribute and thirteen test unwrap/expect/panic operations. Remove those structurally with fallible Result helpers/tests, preserving all semantic assertions, test attributes, expected failures and resource lifetime guards. The production prefix must remain byte-identical to the final core/CLI audit. No suppression, skip, weaker oracle or unrelated module change.

Root records the TUI owner's explicit handoff of test-only crates/norn-cli/src/tui/driver.rs to review_child_context. This existing NRT-001 R3 path has one inherited allow attribute and one test expect operation. Remove those structurally with a contextful fallible Result while preserving the real pre-terminal startup/error assertion and all test attributes. The production prefix must remain byte-identical to the final core/CLI audit; no production behavior change, suppression, skip or weaker oracle.

Within the existing canonical pty_smoke.rs wall, channels_fixture owns the child-result reporting repair: do not discard tx.send(result).await. The supervised fixture must retain and surface send failure, including the undelivered child outcome, and inspect the spawned task outcome; merely joining an ignored Result is insufficient. Preserve the existing actual-App assertions, deadline/termination behavior and the original application failure as evidence. This is fixture error supervision, not permission to weaken failed scenarios or alter production code.

The final audit contains four inherited attributes and 77 test operations in these four CLI files. Original evidence remains immutable. Required tests, source-policy checks and exact-candidate validation remain pending until their actual results; registration is not a pass. Syntax-highlighter production handling and PTY05 read instrumentation are not approved by this cleanup.

### D-PUBLICATION-CHRONOLOGY: Observed event-gap chronology and exact retained completion placement

Chronology repair: channels_compatibility owns exactly session_view/index.rs, projection.rs, live.rs, publication.rs, projection_tests.rs and publication_tests.rs under the existing core responsibility. Core source remains frozen until root releases the running core library test; documentation registration alone is not source thaw. No new file, store/provider API or tool authority is authorized.

Index live/local presentation by the observed source-bound event gap, not wall-clock time or an invented sequence. Advance a monotonic cursor-ordinal boundary for every validated HistoryRecord, including zero-display metadata. Loading older history never rewinds that boundary or moves new local output ahead of already observed canonical events.

Associate the exact Done-created ItemId with its completed AttemptKey. When the exact accepted HistoryRecord reconciles, relocate that attempt’s retained Done within the matching completion bucket and update every existing index membership consistently. Preserve all ItemIds, source/body references and actual order evidence. A ticket with no provisional fragment still places its exact retained completion by the real record cursor. Receipt-before-history and history-before-receipt reconciliation are idempotent.

Bound completion relocation work to the matching attempt bucket with index updates O(bucket_size * log(indexed_rows)); no global completion scan, transcript clone, new identity map without ownership, arbitrary cap, polling or provider-ID/text matching. Preserve existing bounded tool lookup and indexed pinned traversal behavior.

Regression proof includes an earlier local error/final completion followed by a later canonical human prompt and assistant answer, both ordinary and pinned Later traversal; later older/tail pages; zero-row metadata boundary advance; Done receipt/history in both orders and with skipped fragments; stable exact IDs/body refs; and operation-count bounds for a matching completion bucket. Reading remains pinned until the operator explicitly returns to live. The TUI final summary continues after drain; do not add speculative completion IDs or weaken current source fences.

### D-CHRONOLOGY-SPLIT-TEST-ROOT: Bounded chronology owner and canonical fixture root

Keep the registered chronology repair within the 500 production-line policy by creating private session_view/chronology.rs. It owns only Order/Position, the observed source event-gap boundary/local counter and exact AttemptKey-to-Done-ItemId membership. The already-walled session_view/mod.rs adds only mod chronology;. ItemIndex remains the sole ViewItem owner and updates every existing row/tool/attempt index when rekeying. No public API, I/O, body clone, secondary transcript, new authority or acceptance reduction. channels_compatibility owns this split and the original chronology regressions.

Root owns a test-only correction in integration/mcp_channel_startup_tests.rs: the control helper canonicalizes the actual temporary directory with directory.canonicalize()? before McpConfigState::from_layers. macOS /var is a symlink to /private/var; the production no-follow-all-ancestors policy expects a canonical actual root. Preserve both existing tests, every assertion and error propagation. No production security/path policy, timeout, fallback or skipped case changes. The isolated built fixture reproduces the same NotDirectory error; do not attribute this failure to test interference.

### D-LEGACY-SYNTAX-ERROR: Legacy syntax errors preserve typed cause and buffered source

review_tui exclusively owns the approved legacy syntax repair in render/syntax.rs, render/content.rs, render/markdown/emitter.rs, render/markdown.rs, events/schema_render.rs and tools/rich/edit.rs under crates/norn-tui/src. The last file changes test callers only. Coordinate the already modified Markdown/schema files with their frozen owners before writes; no retained App or unrelated tool-renderer wiring is authorized.

Change legacy SyntaxHighlighter::highlight to Result<String, SyntaxError>, retaining the original syntect::Error as typed source and the actual code-line byte offset. Never turn highlighting failure into an empty range list, omit an original line or return a labelled successful String. Replace the four discarded format Results with infallible String construction. The already typed retained highlight_spans contract is unchanged.

Propagate the same typed SyntaxError through legacy render_blocks/code/diff, emitter handle/end-code, MarkdownRenderer render_segment/try_flush_styled/feed/finalize and schema render_assistant_message/render_structured/render_event/field helpers. Existing infallible user/thinking/tool/status arms remain successful values. Adapt all exact legacy test callers fallibly; no unwrap, expect, ignore or replacement suppression.

Render the candidate Markdown segment before draining pending source or publishing paragraph/dim state. A failure retains original pending content and consistent state; do not duplicate or replay an already accepted feed chunk. Borrowed ContentBlock/SessionEvent/JSON source bodies remain unchanged. No partial ANSI output may escape as a successful render after failure.

Prove a deterministic highlighting error retains its syntect source and exact byte offset, and prove failed streaming rendering preserves pending source/state. Preserve every existing ANSI/formatting expectation, successful multiline and numbered code, both diff sides and complete/partial fence behavior. Current retained App uses a separate typed path; this repair proves no new live-App recovery or on-screen error presentation.

Keep the existing no-allow-anywhere instruction: newly touched test modules cannot add or rely on inherited suppression to permit new fallible-call bypasses. Any complete test-only cleanup extent is explicitly assigned separately. Required tests, formatting, strict workspace Clippy and source policy remain pending until actual execution; this registration is not a pass.

Root additionally assigns review_tui complete test-only policy cleanup in these two already-approved syntax-repair files: remove the inherited allow attribute in render/content.rs (unwrap_used, missing_const_for_fn, uninlined_format_args) and resolve its underlying test style structurally; remove the inherited unwrap_used allow plus all six test unwraps in tools/rich/edit.rs. Preserve every test attribute, assertion predicate and meaningful refusal through contextful fallible Result diagnostics. tools/rich/edit.rs production bytes remain identical; content.rs production changes are limited to the separately approved typed syntax propagation. No unrelated production changes, replacement suppression, skip or weaker oracle. This cleanup remains a release/landing requirement and is not grounds to mislabel a separate ready preview as a release.

### D-CHECKED-PREVIEW-VERSION: Versioned checked local previews and spoken update notice

Root owns the minimal checked-preview version step before the next full build: change only workspace.package.version in Cargo.toml from 0.1.0 to 0.1.0-preview.1, and only the four local workspace package versions norn, norn-cli, norn-macros and norn-tui in Cargo.lock. No dependency resolution or dependency version/source/checksum change. Create docs/release-notes/0.1.0-preview.1.md naming actual preview features and known limitations; do not rewrite historical UNRELEASED.md or add an installer/CHANGELOG. Existing Clap Cargo version and protocol version fields derive from the Cargo package version; the inspected CLI tests contain no hard-coded 0.1.0 version assertion needing a new path. This root-owned metadata authoring may precede R7 final verification, whose R6/NRT-002 prerequisites remain unchanged.

User explicitly authorizes ongoing checked local preview installations. Increment the preview prerelease number for each subsequent successful checked update; first stable 0.1.0 still requires proper release checks. Before replacement, verify candidate source/artifact hash, version/help behavior, and a recoverable hash-verified backup. After replacement, verify installed hash/version and record the receipt/rollback path, then invoke macOS /usr/bin/say with the exact text Norn updated. If speech fails, retain the truthful installed state and report the announcement failure. No tag, public release, main merge, background updater, extra service or wider installer implementation is implied. This local preview procedure does not replace exact-candidate release/landing verification.

### D-USER-PREVIEW-PRESENTATION: Restore original styling within the retained renderer

Direct user correction at 09:16 Melbourne on 6 September 2026: restore the original visual styling inside the retained fullscreen renderer; do not roll back, discard current work or return to the old terminal ownership. Deliver the local fix within 30 minutes (requested 09:46 Melbourne). norn_tui owns exactly render/frame.rs, render/layout.rs, render/layout_tests.rs, app/render.rs, app/render/composer.rs and app/render/transcript.rs under crates/norn-tui/src. Restore the earlier dim top-left mode/token rule, plain input, bottom-right model/effort/tier/session rule and last-row key hints, keeping the composer full width. Reuse the inspected main styling; the owner's stated three chrome rows yield when terminal height is below six. This direct user ruling supersedes earlier no-footer/minimal-chrome appearance clauses only, not retained-frame ownership, resize safety or source identity.

Restore the original blue > first-line user prompt, two-column continuations and blank turn margin; remove generic You submitted/Assistant presentation headings. Preserve semantic role, exact source maps, IDs, original body bytes, keyboard operation and full-width composition. Borders, colors, prompt labels and layout are presentation changes; never reconstruct/drop transcript evidence or change runtime input meaning.

channels_compatibility owns exactly session_view/contract.rs, committed.rs, live.rs, contract_tests.rs, projection_tests.rs and publication_tests.rs. Introduce typed ViewItemKind::Metadata for retained explicit-details-only records, limited to ProviderEpochBoundary, the exact known provider.state.provenance discriminator, UsageEstimate and provider Done. Default input label becomes plain Input without asserting human origin. Preserve all records, ItemIds, provenance, usage, body capabilities and source fences; errors/refusals, unknown/unavailable content and external attribution remain visible. Unknown categories are not silently hidden. Routine Done still contributes its complete facts to the one normal completion detail presentation.

The ordinary TUI view hides only the typed Metadata category and exposes it through explicit detail inspection; do not remove it from the semantic projection or persisted history, or use arbitrary text/label matching. norn_tui owns app/render/transcript.rs presentation integration; root coordinates any shared visibility wiring with that owner rather than writing the same file concurrently. Existing data preservation, publication binding, completion grouping, pinned reading and typed error requirements remain.

### D-NUI-STYLE: Uniform multiline user-message styling

Apply the existing blue user-message body style to the whole displayed user body, not only the range before its first newline. Keep the existing first-line prefix and continuation placement, exact source mappings, wrapping, tabs and control-character sanitization. No transcript-format, composer or layout redesign.

Exercise multiline, leading blank line, CRLF, Unicode and wrapped rows; verify actual encoded frame styling where practical. Preserve existing identity/source and appearance assertions. No inserted body text, synthesized source offsets or lost graphemes.

Root owns only app/render/composer.rs and new app/render/composer_tests.rs under crates/norn-tui/src for this row. Add the test child declaration in composer.rs if required; do not expand into other render files without registration.

### D-NUI-FLICKER: Bounded native frame flicker repair

Retain one prepared visible-cell baseline and publish only whole-grapheme changed spans in affected rows. Routine publication must not emit ESC[2J or blank the full screen first. Removed tails become explicit blank cells; a resize invalidates the previous dimensions. Unchanged cells and an unchanged frame write no terminal output. Preserve current source-map identity and retained view semantics.

Assemble any supported synchronized-output prefix, the changed body and its suffix into one terminal write followed by one flush. Advance the committed baseline only after successful publication. A write or flush failure must leave the previous baseline unadvanced; cleanup preserves the original failure rather than replacing or swallowing it. No false successfully-published state after partial output.

The capability probe may consume already-ready replies after primary DA without adding a delay. Retain actual terminal capability evidence and existing cleanup behavior; do not add a timer, poll interval, arbitrary frame delay, provider request, history read or semantic event.

Use the named private render/frame/diff.rs child for the bounded pure comparison owner and diff_tests.rs for its regression tests. Exercise changed/unchanged frames, removed tails, whole graphemes, resize invalidation, synchronized assembly and publication failures with exact output/state assertions. Preserve existing frame and terminal tests and report the actual terminal/PTY evidence scope.

norn_tui exclusively owns the original seven R2 production/private-test paths; channels_fixture separately owns the explicitly registered terminal-observer/canonical verification files. Root separately owns R1 composer styling; neither author expands into the other row or modifies shared paths concurrently. No broad styling, transcript, layout, service or runtime redesign is authorized.

R2 independent verification partition: channels_fixture exclusively modifies crates/norn-tui/tests/support/retained_screen.rs and creates crates/norn-tui/tests/retained_screen_delta.rs. The current oracle assumes ESC[2J and starts a blank screen for each frame; replace that assumption with a stateful VTE model. For the same dimensions, apply changed-cell output to the last known frame. A resize starts a new geometry epoch whose acceptance requires proof of a full visible-cell paint, including explicit blanks and wide glyphs; do not treat unknown cells as known blanks. Preserve all existing style, geometry, bounds and failure assertions. Add actual delta/resize/blank/wide-glyph oracle regressions; no weakened assertions, skipped checks, invented successful screen or production source changes in this partition.

The channels_fixture R2 verification partition also modifies crates/norn-tui/tests/pty_smoke.rs. Five existing rendered-text waits incorrectly require contiguous raw bytes although styled/delta output may split those bytes. Use the decoded retained Screen state for rendered-text observations while preserving every semantic, geometry, bounds, style and failure assertion. Raw terminal protocol and OSC assertions remain raw and unchanged. Do not weaken assertions, skip checks, alter production behavior or claim text exists merely because it appeared historically in a byte stream.

The channels_fixture R2 verification partition also modifies crates/norn-tui/tests/support/retained_workspace.rs solely to retain repeated nonconsecutive requested terminal geometries. The existing geometries.contains check drops the second A in an actual A-to-B-to-A resize sequence; the stateful ordered-epoch observer requires that complete request sequence. Preserve the actual order and each returning geometry, all original resize scenarios, semantic assertions and bounds checks. No bounds relaxation, invented successful frame, skipped geometry, production change or broader fixture redesign.

### D-NUI-CHECKPOINT: Preview.2 is the UI repair checkpoint

Root owns Cargo.toml, Cargo.lock and docs/release-notes/UNRELEASED.md. Reserve0.1.0-preview.2 for this repair; change only workspace.package.version and the four workspace Norn package versions (norn, norn-cli, norn-macros, norn-tui) in Cargo.lock. Third-party versions, sources, checksums and dependencies remain unchanged.

Record the actual two repairs, measured local validation and known limits in the release note. Run meaningful source-bound styling/frame/PTY regressions, formatting, strict workspace/all-target Clippy and structural no-bypass/500-production-line checks before claiming the checked preview ready. A registered case is not a passed test; evidence retains actual command, source and result.

User-authorized checked local preview installation may precede the exact205 landing battery. Root verifies candidate and installed version/hash, retains a verified rollback copy and installation receipt, and invokes macOS say with the exact words Norn updated after successful installation. Report any speech failure separately from an otherwise installed binary. No main landing, public release, tag or waiver of exact venue/review requirements is authorized by this preview repair.

Keep NFP work intact in its separate branch/worktree; its next preview reservation becomes0.1.0-preview.3. No saved-preference, Iridium, daemon or attach capability is implied by this repair. Do not change any programme row ID.

## Goals

- All eight identity invariants and all producer obligations remain explicit.
- Visible per-call changes never claim session-start baselines or all filesystem authorship.
- One complete local foreground checkpoint preserves current controls while making history readable.

## Non-Goals

- These are checkpoint boundaries, not permanent feature cuts: independent host/attach, Iridium composer, editable/guarded file workspace, Work/Session panels, Liminal modules and request-local structured output remain wanted as recorded below.

## Structure

- `Cargo.lock`
- `Cargo.toml`
- `crates/norn-cli/src/commands/slash/registry.rs`
- `crates/norn-cli/src/print/orchestrator.rs`
- `crates/norn-cli/src/print/output.rs`
- `crates/norn-cli/src/print/output/provider_events.rs`
- `crates/norn-cli/src/tui/driver.rs`
- `crates/norn-tui/Cargo.toml`
- `crates/norn-tui/src/agents/activity_log.rs`
- `crates/norn-tui/src/agents/mod.rs`
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
- `crates/norn-tui/src/app/render/changes.rs`
- `crates/norn-tui/src/app/render/composer.rs`
- `crates/norn-tui/src/app/render/composer_tests.rs`
- `crates/norn-tui/src/app/render/hit.rs`
- `crates/norn-tui/src/app/render/transcript.rs`
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
- `crates/norn-tui/src/app/transcript/publication.rs`
- `crates/norn-tui/src/app/transcript/publication_tests.rs`
- `crates/norn-tui/src/app/transcript_tests.rs`
- `crates/norn-tui/src/app/turn/mid.rs`
- `crates/norn-tui/src/app/turn/run.rs`
- `crates/norn-tui/src/app/view_actions.rs`
- `crates/norn-tui/src/app/view_actions/commands.rs`
- `crates/norn-tui/src/app/view_actions/keys.rs`
- `crates/norn-tui/src/app/view_actions/mouse.rs`
- `crates/norn-tui/src/app/view_actions/reading.rs`
- `crates/norn-tui/src/app/view_actions/reading_tests.rs`
- `crates/norn-tui/src/app/view_config.rs`
- `crates/norn-tui/src/app/view_config_tests.rs`
- `crates/norn-tui/src/app/viewport.rs`
- `crates/norn-tui/src/app/viewport_tests.rs`
- `crates/norn-tui/src/error.rs`
- `crates/norn-tui/src/events/schema_render.rs`
- `crates/norn-tui/src/input/keybindings.rs`
- `crates/norn-tui/src/lib.rs`
- `crates/norn-tui/src/render/changes.rs`
- `crates/norn-tui/src/render/content.rs`
- `crates/norn-tui/src/render/fixed_panel.rs`
- `crates/norn-tui/src/render/frame.rs`
- `crates/norn-tui/src/render/frame/diff.rs`
- `crates/norn-tui/src/render/frame/diff_tests.rs`
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
- `crates/norn-tui/src/render/retained_markdown.rs`
- `crates/norn-tui/src/render/retained_markdown/mapping.rs`
- `crates/norn-tui/src/render/retained_markdown_tests.rs`
- `crates/norn-tui/src/render/retained_structured.rs`
- `crates/norn-tui/src/render/retained_structured_tests.rs`
- `crates/norn-tui/src/render/retained_text.rs`
- `crates/norn-tui/src/render/retained_text_tests.rs`
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
- `crates/norn-tui/tests/retained_screen_delta.rs`
- `crates/norn-tui/tests/retained_workspace.rs`
- `crates/norn-tui/tests/support/mcp_channels_tui.rs`
- `crates/norn-tui/tests/support/retained_screen.rs`
- `crates/norn-tui/tests/support/retained_workspace.rs`
- `crates/norn/src/error/subsystems.rs`
- `crates/norn/src/integration/mcp_channel_startup_tests.rs`
- `crates/norn/src/lib.rs`
- `crates/norn/src/loop/active_input.rs`
- `crates/norn/src/loop/classify.rs`
- `crates/norn/src/loop/commands.rs`
- `crates/norn/src/loop/delivery_inputs.rs`
- `crates/norn/src/loop/helpers.rs`
- `crates/norn/src/loop/response_publication.rs`
- `crates/norn/src/loop/runner/machine.rs`
- `crates/norn/src/loop/runner/provider_call.rs`
- `crates/norn/src/loop/runner/setup.rs`
- `crates/norn/src/loop/runner/tests.rs`
- `crates/norn/src/loop/runner/tests/publication_binding.rs`
- `crates/norn/src/loop/runner/tests/streaming_nudge.rs`
- `crates/norn/src/provider/agent_event.rs`
- `crates/norn/src/provider/agent_event/observation.rs`
- `crates/norn/src/provider/agent_event/observation_owner.rs`
- `crates/norn/src/provider/agent_event/observation_tests.rs`
- `crates/norn/src/provider/channel_event.rs`
- `crates/norn/src/session/branch.rs`
- `crates/norn/src/session/manager/fork.rs`
- `crates/norn/src/session/manager/open.rs`
- `crates/norn/src/session/persistence/index.rs`
- `crates/norn/src/session/persistence/publication.rs`
- `crates/norn/src/session/persistence/publication_audio.rs`
- `crates/norn/src/session/persistence/publication_journal.rs`
- `crates/norn/src/session/persistence/publication_spool.rs`
- `crates/norn/src/session/persistence/publication_tests.rs`
- `crates/norn/src/session/spool.rs`
- `crates/norn/src/session/spool/inheritance.rs`
- `crates/norn/src/session/spool/inheritance_tests.rs`
- `crates/norn/src/session/spool/range.rs`
- `crates/norn/src/session/spool/range_tests.rs`
- `crates/norn/src/session/store.rs`
- `crates/norn/src/session/store/history_page.rs`
- `crates/norn/src/session/store/history_page_tests.rs`
- `crates/norn/src/session_view/body.rs`
- `crates/norn/src/session_view/chronology.rs`
- `crates/norn/src/session_view/committed.rs`
- `crates/norn/src/session_view/contract.rs`
- `crates/norn/src/session_view/contract_tests.rs`
- `crates/norn/src/session_view/error.rs`
- `crates/norn/src/session_view/index.rs`
- `crates/norn/src/session_view/live.rs`
- `crates/norn/src/session_view/local.rs`
- `crates/norn/src/session_view/mod.rs`
- `crates/norn/src/session_view/projection.rs`
- `crates/norn/src/session_view/projection_tests.rs`
- `crates/norn/src/session_view/publication.rs`
- `crates/norn/src/session_view/publication_tests.rs`
- `crates/norn/src/session_view/response.rs`
- `crates/norn/src/session_view/tools.rs`
- `crates/norn/src/tests/integration.rs`
- `docs/design/norn-retained-tui/CHECKLIST.md`
- `docs/design/norn-retained-tui/DESIGN.md`
- `docs/design/norn-retained-tui/USER-STORIES.md`
- `docs/design/norn-retained-tui/acceptance.json`
- `docs/design/norn-retained-tui/briefs/NRT-001.json`
- `docs/design/norn-retained-tui/briefs/NRT-001.md`
- `docs/design/norn-retained-tui/briefs/NRT-002.json`
- `docs/design/norn-retained-tui/briefs/NRT-002.md`
- `docs/design/norn-retained-tui/checklist.json`
- `docs/design/norn-retained-tui/design.json`
- `docs/design/norn-retained-tui/producer-coverage.json`
- `docs/design/norn-retained-tui/stories.json`
- `docs/release-notes/0.1.0-preview.1.md`
- `docs/release-notes/UNRELEASED.md`

## Constraints

- **SCOPE** — No provider/tool execution engine changes, session-host/Ready transport, active-input policy change, Liminal dependency, Iridium embedding, filesystem watcher, session baseline capture or mutation-owner work in this checkpoint. Those remain wanted programme work, not permanent feature cuts. NRT-002 is the explicit narrow exception for producer observation, append-owner acceptance publication and checked identity-counter refusal; execution/provider policies remain unchanged.
- **SCOPE** — No added dependencies outside the exact D-R4-GRAPHEMES amendment; no arbitrary configuration values. Keep the existing locked package versions unchanged.
- **SCOPE** — No new durable transcript store, raw terminal recording, ANSI emulator or sidecar agent. Use existing EventStore plus one semantic view.
- **SCOPE** — Root owns programme/tracking updates and NCS integration outside these product walls.
- **SCOPE** — No silent skipped prerequisites, lint bypasses, or reduction of existing semantic assertions. Source changes need the existing rigorous review and exact-commit venue receipt.

## Topics

### Public shared core contracts for R1

**ViewSource / StoreInstanceId / SessionIdentity** (R1 vocabulary; R2 minting): A source binds persisted-or-explicit-ephemeral session, local store instance and agent identity. StoreInstanceId is minted independently for every EventStore instance; it is not a persisted generation or host identity.

**HistoryCursor / HistoryRead / HistoryPage** (R1 types; R2 implementation): Opaque cursor binds exact source, event ordinal and EventId, including empty-start. Read uses explicit direction/anchor/event demand. Page exposes approved compact semantic items, next cursor and coverage; never full opaque event payloads. Typed errors identify wrong source, stale generation, mismatched anchor and unavailable data. R2 may transiently clone only the explicitly selected SessionEvents while holding the store lock, release the lock, and call project_committed off-lock. This bounded selected-event staging is not retained view state. Every public/retained HistoryPage carries only compact HistoryRecord values and approved body capabilities, never raw SessionEvent/opaque provider payloads. No filesystem I/O or display projection occurs under the store lock; no full-history clone is introduced.

**ItemId / ProvisionalKey / ViewRevision / ViewItem** (R1): Committed identity and provisional execution/response/attempt/segment identity are disjoint. Revision is local projection revision, not history durability. Tool IDs retain call_id with supplied item aliases; authoritative response_items replace flat projections exactly once. Item bodies are references, not cloned raw events.

**BodyRef / BodyRepresentation / BodyRead / BodyPage / BodyAvailability** (R1 types; R2 validated store/spool minting): Only allowlisted event display fields, owner-validated spool event references and revision-bound provisional display fragments can be read. BodyRead is an explicit byte range; response repeats source/item/representation/revision, actual range and continuation. Public callers cannot mint arbitrary paths/roots/JSON pointers. No opaque reasoning, provider state, transport credential or raw audio capability.

**Projection / ProjectionInput / ProjectionChange / CoverageState** (R1): Pure deterministic reducer accepts typed approved semantic inputs with explicit provenance. It records replacements/aliases, tool status, model metadata and source resets without terminal I/O, store reads or provider calls. Lost transient coverage stays explicit; this interface has no Ready, observer snapshot, durable feed or host lifecycle guarantee.

**ViewError** (R1): Typed source/cursor/body/projection errors carry the referenced identity or field. Failed validation cannot yield an empty body/successful association. No unwrap/expect, ignored errors, arbitrary defaults or terminal dependencies.

### Source, model and body invariants

**I1 Source and timeline** Every item is scoped by ViewSource: persisted session identity when available, explicit ephemeral identity otherwise; root/child agent identity and parent relation; local store-instance generation. Store-instance generation changes on store replacement/reopen, never because geometry changes. A persisted EventId plus validated ordinal identifies committed content. Do not call local store-instance generation a durable store generation or reuse another session cursor after /new. R2 adds a distinct store-instance identity at all three EventStore constructors; persisted SessionBinding supplies the session identity. R1 makes cursor fields private and gives crate-internal validated minting to the store adapter; external callers cannot manufacture a validated cursor.

**I2 Model/configuration** Capture the actual accepted ModelRuntime selection at local turn admission: canonical model, route, effective context and selected effort/tier, with a local configuration revision. ModelChange events retain their old/new values. Earlier history with no model evidence stays unknown; do not label it using today's model. Rendering a historical item must not apply its configuration. Preserve active/pending/refused semantics supplied by current control owners, including NCS after landing.

**I3 Provisional identity** ProvisionalKey = source + agent + local execution + response iteration + attempt + semantic segment. This key is explicitly volatile, not an EventId. Response/part IDs are used when supplied; unkeyed TextDelta/ThinkingDelta are local ordered segments only. Retry invalidates the interrupted attempt's uncommitted fragments and retains a retry notice; already committed items/results remain. Never correlate by text equality, tool name, clock time or guessed provider identity.

**I4 Authoritative replacement** At accepted committed-history boundaries, reconcile by event/source IDs and explicit local attempt/response mapping. When AssistantMessage.response_items is present it is authoritative; flat content/thinking/tool_calls are projections and must not be rendered a second time. Preserve stable anchors with explicit provisional-to-committed alias mappings only when association is proven. If generic events cannot establish the association, replace the identified response window from committed history and expose incomplete provisional association; never silently invent exact identity.

**I5 Tool identity and state** ToolKey = source + agent + call_id, narrowed by committed invocation identity when needed. Streaming item_id is an alias while call_id is unavailable, never the call_id itself. Keep call kind, original argument string, description availability, invocation reference and result reference after completion. Custom/freeform arguments remain strings. A result in an incomplete history page may be an orphan awaiting explicit older-history demand; do not manufacture empty arguments or join unrelated same-name calls. Lifecycle states include assembling/running/completed/failed/blocked/cancelled/incomplete, each only when evidenced.

**I6 Bodies** BodyRef is typed: a committed event plus an allowlisted display field/item path; a validated spool reference obtained from that event and approved session root; or provisional fragment storage owned once by the projection with its exact revision. A caller cannot supply an arbitrary path or arbitrary JSON pointer. Body identity includes source, owner item, representation and revision; reads return the same identity/range or an explicit stale/unavailable/malformed error. Historical content never resolves through today's workspace bytes. Opaque reasoning, credentials, reusable provider transport state and raw audio bytes are not display body capabilities. Spool capabilities are minted from a stored event through the owning SpoolWriter, which retains its private data_dir, registered entry and root_session_id. Body callers supply the capability and range, not a root path; the owner opens through PrivateRoot and validates the reference.

**I7 Demand and cursor** HistoryRead requests explicit before/after cursor and event demand. Cursor binds source/store generation, ordinal and EventId, with a distinct empty-start value. Validate all components; clone only selected records while locked and do body/file work after releasing the store lock. No events() clone on paint/resize/scroll. BodyRead names an explicit byte range/representation; preserve UTF-8 boundaries and incomplete chunk state. Current spools hold serialized JSON: raw serialized-JSON ranges are honest; field decoding cannot be claimed range-bounded without an additional index. Parsing/formatting occurs off the render/input path and results are revision-tagged. History items are lightweight semantic headers/body capabilities, not cloned raw SessionEvent values or opaque provider payloads. Projection adapters borrow only approved fields. Source generation/cursor validation precedes every page operation. R2 may transiently clone only the explicitly selected SessionEvents while holding the store lock, release the lock, and call project_committed off-lock. This bounded selected-event staging is not retained view state. Every public/retained HistoryPage carries only compact HistoryRecord values and approved body capabilities, never raw SessionEvent/opaque provider payloads. No filesystem I/O or display projection occurs under the store lock; no full-history clone is introduced.

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
- Pinned viewport painting starts through D-R1-INDEX borrowed traversal at an exact current ItemId, in either direction with declared inclusion semantics. Existing ID-to-order lookup plus BTreeMap ranges must avoid linear seeking across unrelated retained rows. Missing/foreign IDs fail explicitly; alias resolution is a separate proven step. No body loading/cloning or second item owner is introduced.
- Hit testing and mouse dispatch validate the current frame source, body revision and rendered source-map identity. A logical placeholder anchor may preserve a requested item/offset while content is unavailable, but it is not the identity of a painted source map and cannot authorize selection, copy or an unrelated hit. Keep those identities distinct; stale/missing mappings are explicit and never rebound through a placeholder fallback.

### Read-only call Changes

- The initial title is Changes with a scope label such as Tool call <call_id>. Group by evidenced path and exact call; preserve source/agent identity. There is no implicit session-start baseline, current-file preview, Git HEAD comparison, rename detection or external-writer attribution.
- edit: show old_string → new_string as an edit fragment, not a whole-file diff. Preserve committed and after_hash independently of diagnostics/error. Missing/ill-typed strings are evidence unavailable, never empty strings. A blocked edit is a proposed fragment labelled not committed.
- write: show submitted content and committed/bytes_written/path facts when available. There is no captured before-content in current write output, so before comparison is unavailable; an empty before-file is not inferred. Post-write diagnostic failure may follow a committed write.
- apply_patch: show the supplied patch tied to the call and the result's evidenced outcome. A submitted patch alone is intent, not proof all hunks committed. Never regenerate the old patch against current disk contents.
- Unknown/Bash/MCP tools retain description, call identity, raw detail and evidenced result. Their arbitrary filesystem writes are outside structured receipt coverage; a nearby shell/tool invocation does not supply authorship or before/after hashes. No action-log-only filter, because its successful-only mutation entries omit committed-with-error cases.
- Every known and unknown tool starts compact. The collapsed row prioritizes the original tool-use description and evidenced lifecycle state, with concise duration when recorded. If the description is absent, show the tool name and explicitly unavailable description; never invent one. Exact call ID, commitment, duration availability and description parse errors remain visible in expanded detail or the selected Changes view, rather than adding repeated technical bookkeeping to collapsed rows. All original arguments, metadata, availability and evidenced results remain retained; a separately identified argument summary may be shown in detail only when existing semantics supply it. Body collapse and individual historical expansion apply to unknown tools too.
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

Implement the settled NRT-001 checkpoint on frozen NCS commit f0cb7476ceb80ce6b9c85a02088cdb8297e960a3. R0 registers this contract; R1, R8 (R4.layout) and R9 (R4.text) may then run independently. R2 retains semantic completion dependency on R1, with early dispatch after the fixed shared contract under D-R2-DISPATCH; R3 follows R2; R4 follows R3, R8 and R9; R5, R6 and R7 follow sequentially. Root owns R8 and R9; one integration owner owns overlapping R3-R6 paths. NCS must be exact-commit venue-green and integrated before NRT landing. If NCS changes, reconcile/rebase and repeat exact-candidate verification. No approval or preference placeholder remains. Stop and name required files before editing outside a row wall.

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

### Fixed R1/R2 seam and dispatch

R2 may dispatch after the R1 opaque cursor/body/HistoryRecord contract listed here is fixed and both owners have received it, while remaining R1 reducer tests and static refinement continue. Root has accepted that freeze at this amendment. norn_models owns only R2 store/spool paths; channels_compatibility owns R1 session_view paths. Any shared contract change is coordinated with both owners before editing it. R2 semantic completion and integration/test acceptance still require completed R1 and R2; depends_on remains R1.

- ViewSource and SessionIdentity; local store_generation UUID supplied by the actual EventStore instance
- HistoryCursor: opaque fields; source()/position() read access; crate-internal empty/event/validate
- HistoryRecord: opaque compact record; cursor()/items() read access
- project_committed(&HistoryCursor, &SessionEvent) -> Result<HistoryRecord, ViewError>: crate-internal off-lock projection seam
- SessionProjection::apply_history_record and reconcile_history_record consume compact records
- BodyRef: opaque capability; origin()/representation() inspection; crate-internal validated committed minting
- BodyOrigin::Committed is store-owned; Provisional and Local are projection-owned and rejected by the R2 store body reader
- BodyRange with explicit offset and NonZeroUsize byte demand; BodyRepresentation/DisplayField allowlist
- resolve_committed_body returns approved inline text or explicit SpoolRequired; no caller-supplied spool root/path
- ViewError is the shared typed error

R2 may transiently clone only the explicitly selected SessionEvents while holding the store lock, release the lock, and call project_committed off-lock. This bounded selected-event staging is not retained view state. Every public/retained HistoryPage carries only compact HistoryRecord values and approved body capabilities, never raw SessionEvent/opaque provider payloads. No filesystem I/O or display projection occurs under the store lock; no full-history clone is introduced.

### Local helper split and grapheme-safe styled rows

Split existing source-owned local notices/body-range reads/exact EventId reconciliation from projection.rs into session_view/local.rs to satisfy the 500 production LOC requirement. No new capability, alternate body store or weakening of identity validation.

Permit a direct unicode-segmentation = "1.13.2" dependency in crates/norn-tui/Cargo.toml and only its norn-tui dependency-list membership in Cargo.lock. The exact version already exists in the lockfile; no package version/checksum churn or other dependency/policy change. Grapheme boundaries govern retained styled-row clipping, wrapping and selection; scalar-only clipping is not an acceptable substitute. Existing ANSI-producing APIs are adapted to direct styled spans; no ANSI parser/capture is introduced.

### Actual TUI session binding

Pass an explicit Arc<SessionBinding> into TuiInputs, whose declaration is in app/event_loop.rs already inside R3. The real CLI obtains its actual AgentToolInfra.session from its assembled coordination runtime; missing required infra is an explicit launch error, never a fabricated identity. Initialize and validate ViewSource through EventStore::bind_view_source. /new validates the newly created binding before the existing rotation commit, then replaces the view source and cursor/body state. Add transparent ViewError and HistoryReadError to TuiError in error.rs. Update only binding/error wiring in the three existing TuiInputs fixtures using their actual ephemeral setup; their semantic/PTY assertions remain unchanged until the sequential R7 work. Preserve resolved NCS launch-mode validation before provider/MCP construction.

### Independent R4.text geometry contract

Use one plain displayed string with ordered typed style byte ranges. Segment extended grapheme clusters across style-span boundaries, then produce cell-bounded styled rows with displayed-byte ranges and hit-test boundaries. Style is data, never embedded ANSI. No norn/session types, body reads, terminal I/O, application state or provider work belongs in this module. The integration adapter separately maps original approved body bytes through visible escaping into these displayed-byte positions.

Validate style ranges at construction: in-bounds, ordered/nonoverlapping, valid UTF-8 boundaries; malformed ranges and disallowed terminal/bidi controls produce typed errors with their offsets. Keep hard newlines and explicitly parameterized tab stops. Zero target width produces an explicit nonpaint result without losing caller text or looping; zero-column graphemes preserve source identity and make progress.

Greedy wrapping preserves hard lines, spaces and whole extended grapheme clusters. Clip to a bounded cell interval without emitting half of a wide cluster or silently rewriting the saved logical range. A cluster wider than available columns is explicitly unpaintable at that geometry; resizing can reveal it unchanged.

Tests include CJK width, ZWJ emoji, combining marks, variation selectors and style boundaries inside a cluster; tabs at multiple columns with explicit tab-stop policy; empty strings/hard newlines/trailing spaces; invalid ranges/controls; zero/tiny width; greedy wrap; clipping at both halves of wide cells; byte-range-to-cell and cell-hit-to-byte mapping. Assertions distinguish displayed-byte coordinates from original body bytes.

Root alone owns these exact two files. This row depends only on R0 and may run alongside R1/R2/R3. It does not wire render/mod.rs or modify Cargo files; R4 integrates it after R3, R8 and R9 and owns the already-approved unicode-segmentation manifest/lock change.

A small external diagnostic harness may use the existing resolved unicode-segmentation/unicode-width packages to exercise this pure seam before workspace wiring. Label that evidence as bounded local diagnostics, never a workspace or venue receipt.

Design provenance: D-R4-TEXT, D-R4-GRAPHEMES and viewport_invariants in ../design.json; R4 owns the final full-frame and real PTY acceptance.

### Reading-control file wall and independent authoring

Hit testing and mouse dispatch validate the current frame source, body revision and rendered source-map identity. A logical placeholder anchor may preserve a requested item/offset while content is unavailable, but it is not the identity of a painted source map and cannot authorize selection, copy or an unrelated hit. Keep those identities distinct; stale/missing mappings are explicit and never rebound through a placeholder fallback.

Parallel authoring exception: app/search.rs and app/search_tests.rs may be authored independently as a pure literal-query search module over source-bound original text prefixes, with grapheme-safe match/selection boundaries. Every result retains exact source/body revision and original-byte coordinates and declares loaded-transcript, selected-body or explicitly requested older-history scope, including partial/unavailable coverage. An unsearched suffix or older range is never reported as no match. No I/O, provider work, module wiring or runtime mutation belongs in this slice. R5 remains prerequisite for R6 integration, validation and completion.

Parallel authoring exception: app/export.rs and app/export_tests.rs may be authored independently for exporting explicitly scoped, source/revision-bound original text to an explicit operator path. Default writes create a new file; replacement requires an explicit overwrite choice. Preserve original hard newlines and report missing/partial source coverage and path-specific errors; no hidden transcript persistence. Filesystem work must run off the terminal event loop when integrated. This authoring slice does not wire modules, dispatch runtime work or mutate session/provider state. R5 remains prerequisite for R6 integration, validation and completion.

Split reading-action orchestration into app/view_actions/reading.rs and reading_tests.rs: typed search state, navigation to an exact source/body-revision hit, and supervised explicit-path export I/O workers. Existing commands.rs owns command grammar. Preserve current-frame source/map identity independently of logical placeholder anchors, source-scoped partial/unavailable search/export status, create-new by default and explicit overwrite choice. Export filesystem work stays off the terminal event loop; worker completion/errors remain supervised and source-bound. This adds no provider, persistence or session-control capability. R5 remains prerequisite for R6 integration, validation and completion.

### Exact publication binding repair — NRT-002

One common owner owns observation schema, sender derivation, caller/setup wiring, shared append helpers, delivery_inputs caller, typed SessionError propagation and all producer regression fixtures. Use private OnceLock resolution cells and one coalescing execution Notify. Complete successful receipts synchronously in the actual append-owning closure before cancellable hooks or outer continuations; cloned tickets cannot publish. Validate source/sender identity; child senders drop parent scope.

The second owner alone owns indexed store history lookup, projection input/assistant reconciliation and retired-attempt fences, plus the ActiveInputDelivery leaf type. The common owner retains delivery_inputs/setup/shared append helpers and the single producer regression file. Fix the observation/index/input seam with both owners before parallel authoring; coordinate any schema change before dependent edits.

R3/R5 completion requires the NRT-002 exact producer publication-binding repair and its required proof. Existing implementation/integration authoring may continue under exclusive coordinated ownership; duplication, guessed association or an unverified repair cannot be reported complete. NRT-001 and NRT-002 form one final checkpoint; completion gates do not add circular source-authoring prerequisites.

For one exact admitted execution, normal End and TurnCompleted facts share one compact completed-execution presentation row. Selection or expansion exposes every original completion fact, source, model, usage, timing and availability field. Errors, cancellation and coverage gaps remain explicit. Never drop metadata, invent success or associate records by text, time or proximity. Implement presentation only through existing TUI file walls.

Parallel fixture-authoring exception: after the current source freeze ends, channels_fixture alone may update the existing canonical pty_smoke.rs, model_selection.rs, mcp_channels.rs and support/mcp_channels_tui.rs. The already-declared retained_workspace.rs and support/retained_workspace.rs may be authored by a separate explicitly assigned owner. Preserve every semantic assertion and PTY acceptance. This permits authoring only: R7 validation/completion still requires R6 and the NRT-002 publication-binding repair. No source collision, early acceptance or extra path is authorized.

**api**
- scope_constructor: AgentEventSender::observe_execution(&self, store: &EventStore, source: &ViewSource, execution: Uuid) -> Result<(AgentEventSender, ExecutionObservation), ObservationError>
- constructor_rules: Validate the exact source through the actual store, validate source.agent_id against sender identity, reject an already scoped sender's rebind, and return an immutable scope. The original sender remains unscoped. for_child drops the parent's observation scope. No AgentStepRequest fields are added.
- execution_observation: Opaque cloneable handle: source(), execution(), opening_input() -> Option<&InputResolution>, and notification() supporting an await on a coalescing Notify. One opening-input resolution slot exists because one run_agent_step admission has at most one opening prompt. AgentMessages/McpChannelWake have no opening prompt and do not mint a human acceptance.
- producer_response_scope: Crate-private for_response(actual zero-based request iteration) creates one response owner. It is derived at StepMachine::call_provider from the producer's checked iteration counter, never from received Done events.
- producer_attempt_scope: Crate-private for_attempt(actual positive retry number) creates a unique immutable AttemptObservation ticket. The retry owner supplies its actual counter. Counter overflow must be a typed refusal, never a saturated reused key.
- attempt_observation: Opaque Arc-backed ticket with source(), attempt() -> &AttemptKey, and resolution() -> Option<&AttemptResolution>. Fields and successful-resolution constructors are private to the runtime observation owner. Every scoped provider envelope carries the same ticket for that actual attempt.
- attempt_resolution: Single-assignment Accepted(HistoryRecord), NotAccepted(reason category), or AcceptedButUnavailable { event_id, safe typed observation error }. AcceptedButUnavailable reports an accepted event whose view could not be projected; it must never be labelled a failed append or alias arbitrary content.
- winning_attempt: A response owner has exactly one private single-assignment winning-attempt slot. The retry closure fills it only after complete successful response assembly. This avoids public AssembledResponse schema changes, universal request fields, or inferring the winning attempt from received events. Failed attempts are not retained by this response owner.
- live_envelope: One AgentEventKind::Observed(ObservedAgentEvent) carries a private ObservationScope (Execution, Response or Attempt) and the boxed original AgentEventKind, without cloning its body. Nested wrapping is refused. Every scope shares the opaque execution-owner identity. Provider envelopes carry exact attempt tickets; auxiliary tool-result/compaction/message envelopes carry execution provenance so an old turn cannot acquire a new turn identity.
- exact_record_read: EventStore::history_record(&self, source: &ViewSource, event_id: &EventId) -> Result<HistoryRecord, HistoryReadError>. Validate the bound source and managed writer generation; use the actual event-ID index to check ordinal and EventId; clone only that selected event under the lock; release the lock and call project_committed. No whole events() clone or raw payload crosses the public seam.
- input_reconciliation: SessionProjection::reconcile_input_record(local: &ItemId, record: &HistoryRecord) -> Result<(), ViewError>. Validate same source, existing Local Input, exact committed UserContent target, and conflict/idempotence rules; retain one canonical committed body and alias the old local item. Never infer local identity from text.
- assistant_reconciliation: Consumer invokes existing reconcile_history_record only with an Accepted record from the matching opaque attempt ticket, after source/execution checks. Shared projection records an exact retired-attempt fence so later buffered envelopes cannot recreate the live copy.
- consumer_split: New app/transcript/publication.rs owns the admitted execution handle, observed unresolved attempt tickets, resolution processing and late-event fences. New publication_tests.rs tests the real runtime receipt path. Transcript remains the integration owner; no second retained history-record cache is needed.
- resolution_cell: Private OnceLock cells, one coalescing execution Notify, immutable read tickets and one producer resolution guard.
- accept_history: Borrow &HistoryPage; no second retained record cache.

**ownership_and_boundedness**
- There is no queue of receipts. Each opening admission has one outcome cell; each actual provider attempt has one outcome cell; each response owner retains at most one winning attempt.
- Outcome cells are single-assignment immutable receipts. A producer guard owns the only resolution right. Cloned tickets are read handles, not additional publishers.
- The already bounded broadcast holds live-envelope ticket Arcs. Failed attempts are not accumulated in a producer ledger. The frontend retains tickets only for attempts it actually displayed and removes resolved tickets after processing.
- An entirely missed attempt produces no provisional row. Its ordinary committed HistoryRecord is sufficient and receives no invented live alias.
- One execution Notify stores a coalesced wake, not one message per publication. It never blocks the producer. Consumers also inspect ticket resolution on initial envelope admission, so a notification that preceded ticket delivery cannot lose the receipt.
- The pending ticket set is tied to currently owned live/provisional work, not elapsed time or total historical events. It must be drained on notification, turn completion and source replacement. Closed scopes reject late admission.
- Compact HistoryRecord data contains metadata and approved lazy body references only. It is moved into one cell once. Original provider bodies remain in the store; no per-frame or per-delta copies are introduced.

**publication_lifecycle**
- Before running a turn, retain begin_execution's exact UUID/model snapshot and create the store-validated execution sender/observation handle. Associate the exact submitted Local ItemId with the human opening admission in this same frontend owner.
- At each producer request boundary, create a response owner with the actual response index. Each retry creates an attempt ticket before streaming and sends that ticket on every live provider envelope. Do not depend on a separate start notification being received.
- A failed streaming attempt resolves NotAccepted before retry waiting; cancellation drops its producer guard and resolves NotAccepted. The resolution notifies the frontend so a lost retry broadcast cannot leave a failed provisional copy active.
- On successful assembly, transfer the winning ticket to the response publication owner; success at this stage is not acceptance and resolves no Accepted cell.
- Move the publication-resolution guard into the same owner closure that performs append or append_batch. On successful append, mint the exact indexed compact record, fill Accepted and notify synchronously before returning or awaiting hooks. On a failed append fill NotAccepted. Never move a successful receipt back to the outer future for later publication.
- If append later moves to spawn_blocking, the owning closure retains the resolution guard: dropping the outer future cannot prematurely resolve NotAccepted while that worker can still commit. The worker reports the final actual outcome.
- A post-append view-projection failure is AcceptedButUnavailable with the exact EventId and safe error category; it is neither a false successful display association nor a false claim that storage rolled back.
- On turn completion, process all currently resolved known tickets and the opening receipt before final history display. Ordinary history and ticket records are idempotent in either order. Then close admission to that execution and preserve exact retired-key fences for late buffered envelopes.
- For human steers, preserve the existing ActiveInputDelivery transport, add the actual accepted EventId, and emit its acknowledgement inside the same actual append-owner closure before hooks. The TUI obtains the owner-minted exact record off the input/paint path and calls the same input reconciliation API. Do not create a second steer route.

**lag_and_ordering**
- A scoped live event names its actual source, execution, response and retry; the reducer never assigns it to a counter inferred from another received event.
- Broadcast lag marks the affected current live attempt incomplete. Further unproven deltas must not be concatenated across the missing interval. A later actual attempt starts under its own explicit producer key.
- The ticket cell survives missing publication/retry notifications whenever any provisional fragment was displayed. On the Notify wake, reconcile Accepted or retire NotAccepted using that exact ticket.
- Publication before buffered live delivery: inspect ticket first, accept its record, set the retired fence, and do not apply stale deltas. Publication after live delivery: replace only that ticket's provisional rows.
- History before receipt and receipt before history both call idempotent apply_history_record through exact reconciliation; neither requires a FIFO or event-window guess.
- Generic text without canonical response-item identities is retired, but does not gain a fabricated body-offset alias. Canonical item/part and exact tool call identity aliases retain existing R1 validation. Old body revisions stay explicitly stale and locally pinned copy bytes remain governed by existing copy rules.
- Source replacement rejects old generation tickets even if session names, event contents or execution UUIDs look similar. A late old execution cannot acquire the new execution's accepted-model stamp.

**acceptance**
- store: ["Exact indexed read returns the same compact identity as the corresponding page.", "Wrong source generation, absent EventId, index/ordinal conflict and changed managed writer are typed refusals.", "Only the selected raw event is cloned under lock; projection happens after releasing it."]
- producer: ["Generic MockProvider TextDelta + Done(None) yields one exact Accepted assistant receipt after actual append.", "Canonical response items and a multi-response tool loop produce distinct exact response/attempt/event bindings.", "A failed retry resolves its ticket NotAccepted; only the actual winning attempt obtains the published record.", "No Accepted receipt after pre-append validation or sink failure.", "Cancellation before append resolves NotAccepted; cancellation while a hook waits preserves the already Accepted receipt.", "Use a controlled append owner fixture to prove the resolution occurs in the owner closure: stopping the outside waiter cannot mark NotAccepted before the owner reports success.", "Opening input receipt names the precise accepted UserMessage; child-result and external-wake paths never claim an operator opening input.", "Human steer acknowledgement carries its exact accepted EventId and survives cancellation while hooks await.", "Repeated identical text in separate turns remains two distinct accepted inputs and responses."]
- projection_and_consumer: ["History-first and receipt-first each retain one assistant row set and one canonical human body.", "A receipt seen before queued live deltas fences those deltas from recreating the provisional copy.", "Broadcast lag with a retained ticket still reconciles from its cell; no receipt queue is required.", "Losing every live event for an attempt leaves ordinary committed history without an invented alias.", "Retry/cancel/stale-source/old-execution envelopes cannot rebind another attempt or model snapshot.", "Generic fragment retirement does not invent canonical body-offset aliases; real item/part aliases continue to validate.", "Notify coalescing and publication-before-notification-registration cannot lose an observable resolution.", "Many failed retries retain no producer receipt ledger and impose no frontend backpressure; actual retained ticket ownership is asserted."]
- actual_pty: ["Version a new external PTY oracle attempt; preserve attempt01 and its report unchanged.", "For actual run_app with existing MockProvider, assert exactly one rendered prompt body and one final assistant body after commit and lazy body loading, not merely substring presence.", "Repeat the current geometry/resize/paging/restoration checks against the newly compiled exact source. No real provider request is needed."]

Validate the managed current registered spool owner before acquiring the store lock and off the paint/input path, using the existing registered-entry guard in session/spool/range.rs. Cached store ID/generation alone cannot prove that a deleted/recreated session still owns the registered writer. Reuse existing authority; no new capability, filesystem work under the store lock or whole-history/body read.

Add a transparent boxed ObservationError variant in provider/channel_event.rs so checked scoped-envelope failures retain their actual typed source. Never relabel an observation/source failure as NoObservers or another delivery outcome; no payload duplication, new event route or observer authority.

LiveReduction exposes completion_item: Option<ItemId> from the actual Done reduction; the core retains its compact Done label without adding a duplicate local metadata body. The TUI retains typed Done stop_reason, response_id and Usage under the exact active-execution identity, then moves those facts into the one final completion-details body. It uses exact returned routine notice IDs for compact presentation, never order/text/label guesses, and does not duplicate retained metadata or transcript caches. Every original fact remains inspectable; stale source/execution fences and existing source walls remain unchanged.

The already-registered R7 new_retained_workspace parallel-authoring partition is assigned to review_tui, exclusively for tests/retained_workspace.rs and tests/support/retained_workspace.rs. Author persistent actual-App PTY-04 reading-control/resize fixtures using the shared retained_screen oracle API and the existing 33-check external R6 evidence as a source of scenarios, not a substitute for executing these fixtures. channels_fixture remains the exclusive canonical-fixture/shared-oracle owner. No source paths or acceptance scope are added, and R6/NRT-002 prerequisites still gate R7 validation/completion.

**Exhaustive fixture match repair:** The actual core-check02 exhaustive-match failures authorize only an explicit AgentEventKind::Observed arm in the named existing test fixture. Preserve the current unscoped event route, unexpected-event refusal and every original semantic assertion. Do not add a wildcard, todo, ignore, lint bypass or new source/behavior access. Apply only after the active source owner thaws the current compile. Paths: crates/norn/src/loop/runner/tests/streaming_nudge.rs, crates/norn/src/session_view/projection_tests.rs.

At finalise_turn, reconcile the actual root turn's final input/output usage against the already-accounted live_root_usage for that same turn. Add only that turn's missing deltas to the cumulative ledger and publish the actual current counters. The final result is authoritative. Reset/associate per-turn live accounting with exact root execution identity: a second turn with numerically equal usage must count again, not be mistaken for a duplicate. A duplicate Done fence has no UI usage side effect. Preserve all existing usage/model/budget controls and assertions; this is exact-turn reconciliation, not a new feature or a cross-turn numeric high-water mark.

**Shared future allocation boundary:** Box::pin the sole inner orchestrate_run(...).await at the existing shared wrapper in crates/norn-cli/src/print/orchestrator.rs (inspection anchor line250). Preserve that wrapper's existing signal ownership, output sink, typed error envelope, cancellation and drop behavior. This is only an allocation boundary for the inner future to address the three observed >16KiB outer-future Clippy failures; it spawns no task and changes no protocol, configuration or runtime policy. driven.rs and additional speculative call sites are outside this amendment. Verify the composed call sites before claiming the diagnostic is resolved.

### Structured assistant details and shared PTY observer

Pure parallel authoring slice: render/retained_structured.rs and retained_structured_tests.rs provide a source-mapped RenderedMarkdown adapter for complete multi-field assistant JSON, preserving the existing secondary-fields/Ctrl+E acceptance. Every displayed field/range maps back to the supplied approved original source; partial JSON remains the original text with explicit partial state. Do not synthesize BodyRefs, invent content/fields or parse ANSI. No module wiring, body I/O, provider work or runtime mutation in this slice; the TUI owner integrates it against current source/revision mapping and retains all original bytes and secondary fields.

Use tests/support/retained_screen.rs as one shared existing-vte terminal observer for the canonical PTY suites, owned only by channels_fixture. Preserve real visible geometry, alternate-screen/resize/mode and exact-occurrence assertions plus real termios restoration; do not replace semantic assertions with weaker substring checks. No new dependency, clipboard acknowledgement, provider certification or broader terminal-matrix claim is introduced.

### Current modified-test cleanup partition

Tom's no-allow-anywhere instruction overrides the inherited CLAUDE.md test exception. Before landing, remove inherited allow attributes and resolve the underlying test unwrap/expect uses in the eleven already-modified TUI files named by this partition. Preserve every assertion and propagate failures through Result; no skips, weaker assertions, renamed unused values or replacement lint bypasses. This is test-only cleanup of these current modified files, not unrelated unchanged-module or public-history audit work.

Exclusive test-cleanup partition: review_child_context owns test-only cleanup in crates/norn-tui/src/agents/tabs.rs, crates/norn-tui/src/app/autocomplete.rs, crates/norn-tui/src/app/helpers.rs, crates/norn-tui/src/events/schema_render.rs, crates/norn-tui/src/render/fixed_panel.rs, crates/norn-tui/src/render/markdown.rs, crates/norn-tui/src/render/text.rs. norn_tui retains crates/norn-tui/src/app/dispatch.rs, crates/norn-tui/src/app/event_loop.rs, crates/norn-tui/src/app/slash.rs, crates/norn-tui/src/app/state.rs. The active source owner must explicitly acknowledge handoff before parallel cleanup starts; no concurrent writes to any shared file. Production behavior remains unchanged, and all current brief acceptance and final verification requirements remain.

Core test-only cleanup within current modified source walls: review_child_context exclusively owns loop/commands.rs, session/branch.rs and session/spool.rs under crates/norn/src. norn_models retains loop/classify.rs and provider/agent_event.rs. Remove the five inherited allow attributes and resolve their underlying test unwrap/expect uses, preserving every semantic assertion and production bytes for this cleanup. Propagate Result failures; no skips, weaker assertions, wildcard bypasses or unrelated unchanged-module changes. Common-owner Clippy style repairs are a separately identified existing task, not permission to change production through this test-cleanup partition.

The 52-file pre-thaw core AST snapshot remains historical evidence. Source ownership must be acknowledged before parallel cleanup; no concurrent writes to a shared file. The whole-workspace Clippy01 run has ended and root explicitly released the document freeze. These are existing paths only and all NRT acceptance and final verification requirements remain.

The existing modified-core test cleanup includes inherited panic/panic!, expect_err and unwrap_err as well as unwrap and expect. Replace these with contextful fallible Result diagnostics while preserving every original semantic assertion and production bytes for the cleanup. No new skips, weaker assertions, or replacement bypasses.

norn_models additionally performs test-only cleanup in its already-owned NRT-002 R1 crates/norn/src/tests/integration.rs: the inspected inherited test code has six unwrap, sixteen expect and two panic uses. Do not remove or edit the allow attribute in unchanged tests/mod.rs, and do not expand into unrelated test modules. This is an existing-path cleanup; all owner coordination and final verification requirements remain.
