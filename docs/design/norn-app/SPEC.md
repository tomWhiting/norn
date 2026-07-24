# Norn App — Product & Design Spec (PRD ↔ Design Brief)

**Status:** Draft v1 (lightning round, 2026-07-24 10:50 AEST). Evidence bindings from two in-flight norn Sol workers land as Appendix A amendments.
**Author:** Sable Nightwick, for Tom.
**Scope:** Native Norn application on **macOS (desktop)** and **iOS**, built on **Tauri 2** with the norn Rust library embedded directly in the app backend. Windows/Linux desktop follow later from the same codebase; nothing in this spec may preclude them.

---

## 1. Vision

Norn already has three execution mechanisms: the TUI, print/driven mode, and the library. The app is the **fourth driver** — not a wrapper around the CLI, not a webview chat toy, but a first-class native surface over the same engine, session store, and coordination machinery, held to the same standard as the rest of the codebase (patient records / financial transactions / legal documents).

The app is where Norn's session model finally becomes *visible*: sessions as immutable **trees** with a fork/branch structure you can see and touch, an annotation layer over the timeline, layered context views where compaction is a view-reroute (never a rewrite), and an action-log spine that shows exactly what every agent did and when. The TUI shows one session well; the app shows the *estate*.

**Outcomes we are buying:**

- **O1 — One engine, no drift.** The app embeds `crates/norn` in-process. Every capability gap discovered while building the app is fixed *in the library* so all four drivers align — never re-implemented in the app layer.
- **O2 — The session tree made real.** Visual, navigable, immutable session trees (root → forks → children), with annotations and layered context views, backed by the existing JSONL store and index.
- **O3 — Agent transparency.** Live view of the agent registry: every running/parked/terminal agent, its pending messages, its action log, its cost — nothing the runtime knows is hidden from the owner.
- **O4 — High class, native feel.** Cold start under a second on desktop; instant keyboard-first navigation; no Electron; memory footprint a fraction of the incumbent apps'. The bar is "the app the Claude/ChatGPT desktop apps should have been."
- **O5 — Continuity across devices.** A session started in the TUI resumes in the app on the same machine (shared store); an iOS device holds its own store with an explicit sync/handoff story (owner-ruled, see §10).

## 2. Product principles

1. **Local-first, owner-owned.** All data in the existing `~/.norn` formats. Export is trivial because the store *is* the export. No cloud dependency for core function.
2. **Keyboard-first on desktop, thumb-first on iOS.** Every desktop action reachable without the mouse; a command palette is the primary navigation instrument.
3. **Never lie about state.** Streaming, retrying, parked, awaiting-approval, degraded-IO — surfaced exactly as the runtime knows them. No spinner theatre.
4. **No arbitrary values.** UI timeouts, list caps, polling intervals: factual, owner-ruled, or configurable. Same CLAUDE.md law as the rest of the repo.
5. **Best-effort integrations never block the run** (mirrors the HerdR presence contract, PR #18).

## 3. Platform & stack

- **Shell:** Tauri 2.0 (`tauri-v2` skill conventions): Rust backend, **Vite + React + Zustand + Tailwind CSS 4** frontend, MVVM — views are dumb, view-models in Zustand stores, all engine truth lives in Rust and crosses the bridge as typed events/commands.
- **Backend:** a new crate `crates/norn-app` (workspace member) — the Tauri app + a thin driver layer over `norn::agent::AgentParts`, exactly parallel to `norn-cli`'s print orchestrator and `norn-tui`. Module discipline per CLAUDE.md (no god files, `mod.rs` re-exports only).
- **Bridge:** `AgentEvent` broadcast → Tauri event channel (one subscription per window); Tauri commands for the imperative surface (send turn, steer, cancel, fork, session CRUD, auth). All commands return typed results; every error crosses the bridge typed, never stringly.
- **iOS:** same crates; the webview is WKWebView. Rust targets `aarch64-apple-ios` (+ `aarch64-apple-ios-sim`).

### 3.1 iOS build prerequisites (the "fuck-around" checklist)

To get `tauri ios init` / `tauri ios dev` working on the Mac:
1. Full **Xcode** (not just Command Line Tools), current stable, launched once to accept licence + install the iOS platform.
2. `rustup target add aarch64-apple-ios aarch64-apple-ios-sim` (add `x86_64-apple-ios` only if an Intel-Mac simulator matters).
3. **CocoaPods** (`brew install cocoapods`) — Tauri's iOS template drives signing/deps through it.
4. An **Apple Developer** account; a development team ID in `tauri.conf.json > bundle > iOS > developmentTeam` for device deploys; automatic signing via Xcode for dev, manual profiles for TestFlight.
5. `cargo tauri ios init` (generates the Xcode project under `src-tauri/gen/apple`), then `cargo tauri ios dev` (simulator) / `--device`.
6. Known trip-wires: the generated project must be opened once in Xcode to resolve signing; env vars don't propagate into the Xcode build the way desktop builds do (secrets/config must be compiled or bundled); simulator networking to localhost dev server needs the dev-server host set to the Mac's LAN IP in `tauri.conf.json > build > devUrl` for on-device dev.

## 4. Functional requirements

EARS-style; R-numbers are stable references for briefs. **[D]** desktop-only, **[M]** mobile-only, otherwise both.

### R1 — Engine embedding & driver parity
- **R1.1** The app SHALL embed `crates/norn` in-process and drive turns through the `AgentParts` custom-driver contract (as the TUI and print orchestrator do). The app SHALL NOT shell out to the `norn` binary for core function.
- **R1.2** WHEN a capability exists in norn-cli/norn-tui but not in the library (slash actions, session rotation, provider-override folding, NOFILE init, startup traces), the work item SHALL be to lift it into `crates/norn` behind a public API, then consume it from all drivers. (Scout worker is enumerating the exact list — Appendix A.)
- **R1.3** The app SHALL use the same session store and index as the CLI on macOS (`~/.norn`), honouring the inter-process index lock and its deadline semantics; concurrent TUI + app on one store is a supported, tested configuration.

### R2 — Sessions: tree, timeline, views
- **R2.1** The app SHALL render the session index as a **tree** (root sessions → forks → children at `{root}/children/…`), with lineage from `parent_id`/fork linkage, status, fidelity, and generation surfaced.
- **R2.2** The timeline view SHALL render the immutable event log (JSONL) faithfully: user/assistant turns, tool calls with inputs/outputs, child branch/fork events, compaction events — with the raw event inspectable for any rendered item.
- **R2.3** Compaction SHALL be presented as a **layered context view**: the user can toggle between "what the model sees" (post-compaction view) and "what happened" (full immutable history). Compaction never deletes, and the UI never implies it did.
- **R2.4** The app SHALL support an **annotation layer** ("road signs") over timelines: bookmarks, labels, and notes attached to event ranges, stored outside the immutable event log (sidecar, format owner-ruled §10) and never mutating it.
- **R2.5** Fork SHALL be a first-class UI action from any resumable point, using library fork semantics (provider-state identity, fidelity, and epoch rules enforced by the engine, surfaced — not re-implemented — by the UI).
- **R2.6** Session search: full-text over titles, names, and event content, local only.

### R3 — Chat & turn surface
- **R3.1** Streaming turn rendering (text deltas, thinking/reasoning summaries where the provider emits them, tool-call progress) driven by `AgentEvent`s; the UI SHALL remain responsive during a 100k-token streamed turn.
- **R3.2** Steering: the inbound-message mechanism SHALL be exposed (queue a message mid-turn), with the queue visible and editable pre-delivery, honouring the exact-once pending-delivery machinery (D8) — the UI displays `delivered` only on the runtime's authoritative signal.
- **R3.3** Cancellation is immediate, safe, and honest (cancellation token; partial output preserved in the timeline).
- **R3.4** `/compact`, `/clear`→rotation, `/new`, `/model`, `/name` parity with the TUI, via the lifted library actions (R1.2). WHEN a rotation occurs, every presence/session-identity surface updates (same defect class as PR #18's rotation staleness — build the update seam once, in the library).
- **R3.5** Model/profile/effort switching between turns; the active model, account, and per-turn + cumulative token/cost figures visible at all times (from `EventUsage` — factual, not estimated).

### R4 — Agents & orchestration view
- **R4.1** A live **Agents panel** over `AgentRegistry`: every root/child agent, state (running / idle-parked / terminal / reclaimed), lineage, pending-message counts (including nondurable-pending status — the D8 machinery makes this queryable), and per-agent action-log tail.
- **R4.2** Spawn/fork child agents from the UI with explicit `ChildPolicy` display; close/reclaim honours the library's refuse-to-reclaim-unresolved-work gates and shows *why* when refused.
- **R4.3** The **action log** is a first-class, filterable view (per session and estate-wide): every tool execution, every agent, timestamped.
- **R4.4** Tool-permission prompts (where a profile requires approval) render as blocking, keyboard-answerable cards with full command/diff preview. Trust decisions are visible and revocable in settings.

### R5 — Auth & accounts
- **R5.1** Account management UI over the existing OAuth account catalog: list, add, remove, per-run account pinning; credential-affinity rules (P5 AFFINITY-01) enforced by the engine and *explained* by the UI when a resume is refused.
- **R5.2 [M]** iOS login SHALL use the **headless device-auth flow** (P2 work) or `ASWebAuthenticationSession`; never an embedded webview credential form. Desktop MAY use the existing browser-open flow.
- **R5.3** Credentials on iOS live in the **Keychain**; on macOS, wherever the CLI stores them today (shared store, single source of truth). Disclosure rules ([REDACTED] debug surfaces) carry over.

### R6 — Skills, MCP, config
- **R6.1** Skills catalog browser (read), invocation parity with TUI.
- **R6.2** MCP servers: list, health, enable/disable per the library's MCP runtime; config edited through the app writes the same files the CLI reads.
- **R6.3** Settings UI is a typed view over existing config/settings surfaces — no app-private config formats for engine concerns.

### R7 — Mobile lifecycle **[M]**
- **R7.1** WHEN iOS backgrounds the app mid-turn, the app SHALL persist enough (the store already persists events as they land) that foregrounding either resumes the stream (within `beginBackgroundTask` grace) or presents a truthful "turn interrupted at event N — retry/continue" affordance. Never a silently-vanished response.
- **R7.2** WHEN the process is killed mid-turn, next launch SHALL recover via the session replay machinery — no corrupt/partial timeline (the fsync-before-publish and pending-durability work is the foundation; scout worker confirms the delta).
- **R7.3** Tool availability on iOS is a **capability-gated profile**: no subprocess tools (bash/process/watch — `fork/exec` unavailable), no LSP; read/search/web/provider tools per audit. Gating is a library-level capability check, not app-side hiding (so `norn` embedded anywhere behaves identically).
- **R7.4** iOS store lives in the app container; sync/handoff between devices is **out of scope for v1** beyond manual export/import of session JSONL (owner ruling required for anything more, §10).

### R8 — Presence & estate integration
- **R8.1** The presence seam specced in PR #18's review (claim/update/release with session identity) SHALL be implemented in the library and used by the app on desktop, so a Norn app window inside a HerdR-managed context reports identically to the CLI.

## 5. Non-functional requirements

- **N1 Performance [D]:** cold start < 1s to interactive on Apple Silicon; session-tree render of 1,000 sessions without jank; 60fps scroll on a 10k-event timeline (virtualized).
- **N2 Memory:** idle desktop footprint an order of magnitude under Electron incumbents; no unbounded in-memory event accumulation (windowed reads over JSONL).
- **N3 Reliability:** every engine error typed across the bridge; no silent failures; UI error states are specific and actionable. All Rust code passes the workspace's strict clippy/fmt gates; no `unwrap`/`expect` in the app backend.
- **N4 Security:** Tauri CSP locked down; IPC command allow-list minimal; no remote content in the webview; credential material never crosses the bridge (UI sees aliases and status only).
- **N5 Accessibility [D]:** full keyboard operability; respects system dark/light; Dynamic Type on iOS.
- **N6 Updates [D]:** signed, delta-friendly updater; never blocks launch; never loses local data.

## 6. Explicit non-goals (v1)

- Windows/Linux shipping builds (architecture must permit, we don't ship).
- Cloud sync service, collaboration/multi-user.
- Anthropic-model support beyond what the library supports (unchanged constraint).
- Editing history (immutability is the product).
- iPad-optimised layout (runs, but not designed-for, in v1).

## 7. Library work packages (the "broaden the library" track)

The app is the forcing function; these land in `crates/norn` first:

- **L1** Public embedding audit: expose the pub(crate) seams a GUI needs (canonical session-path resolver, rotation actions, registry snapshots). *(Scout worker enumerating; binds to Appendix A.)*
- **L2** Lift driver-duplicated logic out of norn-cli/norn-tui (R1.2 list) into library APIs consumed by all drivers.
- **L3** iOS capability gating: a platform-capability layer for tool profiles (R7.3) replacing scattered `cfg(unix)` assumptions where they'd panic or silently no-op.
- **L4** Mobile durability delta: suspend/resume + interrupted-turn recovery hardening on top of existing persistence (R7.1/R7.2).
- **L5** Presence seam (R8.1) shared with PR #18's revision.

## 8. Milestones

- **M0 — Spike (desktop):** Tauri shell boots, embeds norn, lists sessions from `~/.norn`, streams one turn end-to-end. Proves the bridge.
- **M1 — Desktop alpha:** R1–R3 complete, R4 read-only, R5 desktop auth. Dogfoodable daily-driver for Tom.
- **M2 — Desktop beta:** R4 full, R6, R8, N1–N6 measured and passing.
- **M3 — iOS alpha:** builds/signs/runs on device; R7 gating in place; chat + session browsing on the phone store.
- **M4 — iOS beta:** R7.1/R7.2 lifecycle honesty proven under airplane-mode/background-kill torture tests.

Each milestone gets per-cluster briefs (R-numbers above are the anchor) and ships only through the standard Gate D review.

## 9. Acceptance (spec-level)

- A session created in the TUI appears in the app without restart and resumes correctly (and vice versa). 
- Kill -9 the app mid-stream: relaunch shows a coherent timeline, no duplicate or lost accepted messages (D8 invariants hold through the fourth driver).
- iOS: background the app mid-turn for 10 minutes, foreground: truthful state, recoverable turn.
- Full workspace battery, clippy `-D warnings`, fmt stay green with `norn-app` in the workspace.

## 10. Owner rulings required (no invented defaults)

1. Annotation-layer storage format & location (sidecar file per session vs index-adjacent).
2. iOS↔desktop session sync ambition for v1 (manual export only, or file-provider/iCloud container?).
3. Event-window sizes / virtualization thresholds if any must be fixed rather than configurable.
4. Updater channel & signing infrastructure.
5. Whether driven-mode (JSON-RPC) gets an app surface (debug console) in v1.

## Appendix A — Evidence bindings (pending)

- Scout (embedding surface + iOS breakage audit): norn session `claude-scout.3mrkb7`, envelope `~/.norn/delegations/claude-scout.3mrkb7`.
- Research (desktop/mobile AI-app pain points, competitive synthesis): norn session `claude-research.J242Te`, envelope `~/.norn/delegations/claude-research.J242Te`.
- Findings from both amend §4/§7 and add a "market pain-point → requirement" traceability table.
