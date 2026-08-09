# The Kernel Rethink — norn stripped to what it should be outstanding at

**Status:** r1 — thinking piece requested by Tom 2026-08-09 ("have a really good long think… and come back to me"). Not a ruling, not a plan. Evidence folded from: Waffles' manifold answers (verified at SPINE-MODEL.md and the aion swing brief bytes) + four Opus repo surveys (liminal @ 4ed0562, aion @ 533e8dc40/0.12.0, manifold @ a9fd6bc + frame, norn @ 72c5f81), §§3a–3e.

**Provenance:** Tom's voice-note steer 2026-08-09 ~00:14Z. His framing, distilled:

> Everything should be the best version of itself and play well with others — not shoehorned into everything else. Iridium gets no AI in its core, not because AI is unwanted but because the core must stay a great text editor; capability arrives via liminal. Apply the same knife to norn: don't ditch it, don't rewrite it — strip it back to the things we want it to be **excellent** at: forking itself, traversing its own history, remembering, connecting (LSP, MCP), concurrency, communication. Sub-agents maybe become norn calling norn. Modes (plan mode etc.) as configuration. A supervising process you can attach to — watch a headless norn, inject messages. And possibly: norn as an aion server whose actions are its own tools, writing checked AWL workflows over itself.

---

## 1. The unifying observation: these are not six asks, they are one architecture

Every item in the steer is the same design move applied to a different surface: **the durable session tree is the product; everything else is a view over it or a capability plugged into it.**

- *Excellent at forking / traversing history* → the tree IS the core artifact (this was already the direction: the fork re-walk ladder, NTI-003..006).
- *Excellent at remembering* → the memory/lantern design is a derived index over the same tree (norn-memory campaign; decision-sheet item 1, log-as-truth, becomes MORE load-bearing here, not less).
- *Sub-agents as norn-spawns-norn* → every agent becomes a first-class session in the same store; the tree is uniform across processes.
- *Supervisor/attach* → a control-plane **view** over the same store; no new truth.
- *Workflows over own tools* → orchestration becomes **data** (an AWL document) that executes against the same tool substrate and journals into the same history.
- *Modes* → configuration of the same loop, not new machinery.

The strip-back question is then not "which features to delete" but **"which ring does each capability live in"**.

## 2. The three rings

**Ring 0 — kernel (norn owns it, and it must be excellent):**
- The agent loop (turn engine, retry-forever brain, provider layer).
- The session tree: immutable timelines, fork, resume, compaction-as-reroute, parent/child linkage — including across processes.
- The action log (the compressed spine; scope federation; the re-walk ladder).
- Memory/lanterns/resonance (when built — it is a derived view over ring-0 data, so its substrate is ring 0 even though the index is rebuildable).
- Tool execution substrate + the envelope layer (tool_use_description etc.).
- Connectivity as a *client*: MCP client, LSP client.
- Communication: result channels, message injection, the attach surface (§6).
- Headless operation as the primary shape; the library API kept but demoted to "embedding is possible", not the design driver.

**Ring 1 — kernel-adjacent (ships with norn today, stays until the extension surface exists, then candidates for extraction):**
- bash / file ops / apply_patch — a coding agent's hands; pragmatically kernel, philosophically ring 1.
- Search (Tom: "may or may not need search") — verified self-contained (one tool, 2.3k lines, four modes; nothing in the loop depends on it). Keeping it is cheap; cutting it is clean. The four modes (regex/glob/fuzzy/AST) are genuinely better than ripgrep-via-bash for an agent, so my lean is keep — but it is a ring-1 tenant, not kernel.
- Conventions/diagnostics (the non-executing checker) — small, load-bearing for estate discipline.
- TUI — becomes an *attach client* (§6) rather than an embedded driver. The TUI does not die; it stops being special.

**Ring 2 — extensions (MCP tools, liminal extensions, or aion workflows; NOT norn core):**
- Web search/fetch, speech, skills-as-features, cron, monitors, assistant hats, "speaker/daemon" plans.
- **Consequence, flagged plainly: the internal-agents roadmap (skills → hats → speaker/daemon) is reshaped by this rethink.** Those become extensions riding the attach/extension surfaces, not core subsystems.
- New message/interaction types, dashboards, anything manifold-shaped.

## 3. Sub-agents: collapse the suite to two primitives

The inventory (§3e) sharpens this considerably. Today there are **six agent tools** (`spawn_agent`, `fork`, `signal_agent`, `wake_agent`, `close_agent`, `agents`) riding an in-process subsystem of **~50k lines** (~28k production) — tokio-spawned children, live handle maps, a message router with wake semantics, pending-mailbox machinery, reclamation protocols.

Two facts change the argument's shape, both in the proposal's favour:

**The durable half is already right.** Children are NOT in-context appendages — every child already gets its own JSONL timeline in the same store (`{root_id}/children/{slug}.jsonl`), with parent-child linkage recorded three ways (index `parent_id`/`rel_path`, `ChildBranch` on both timelines, `ForkComplete` on the parent) under a parent-first write ordering. **A subprocess child inherits this persistence shape nearly unchanged.** What the collapse removes is the in-*process* execution machinery, not the session model.

**The wire already exists.** Norn's `--protocol jsonrpc` driven mode is a working parent↔child protocol today: `initialize` handshake, `run/execute`, live `event/*` notifications, mid-run intervention, id-matched result — and it is the same protocol aion's `aion-integration-norn` already drives. Norn-spawns-norn reuses it rather than inventing anything.

**The proposal: keep `fork`, replace the rest with `dispatch`.**
- `fork` stays kernel — same store, seeded-from-parent history, `ForkComplete` on the parent timeline. It is the primitive the memory design leans on (ForkComplete is a lantern; the portal is a tree coordinate).
- `dispatch` = norn spawning norn as a subprocess speaking driven mode, handing back a **session id**. `DispatchComplete` generalises `ForkComplete`.

What it buys:
1. **Uniformity.** A sub-agent IS a norn session. Everything ring 0 is excellent at (re-walk, action_log, memory, resume, attach) applies to children automatically, with zero child-specific code.
2. **The portal exists by construction.** Tom's lantern discriminator (memory = note; lantern = note + portal) is satisfied natively: a dispatched child's result carries `(session_id, position)` — you can go back and talk to it.
3. **Crash isolation.** A dead child cannot take the parent's process down; the parent holds a session id that outlives everything.
4. **Supervision for free** via the attach surface (§6) — no separate sub-agent progress machinery.
5. **Concurrency honestly**: OS-level process parallelism + aion-level workflow parallelism (§5), no hand-rolled in-loop scheduling.

What must SURVIVE the collapse in some form (the inventory is explicit): addressing (the registry's hierarchical paths), result delivery (today's result channel → the child's terminal event + a parent timeline event), messaging-to-dormant-children (today's durable pending mailbox → messages land in the child's store and driven-mode injection delivers to live ones), and `ChildPolicy` narrowing (grants must still only narrow). These become simpler under the subprocess model — the store does the durable half — but they do not vanish.

**Costs, stated honestly:** process spawn overhead per child (measure before ruling — the fleet pattern spawns tens, not thousands); the current machinery is working, heavily tested code, and the no-backwards-compat law says *replace, don't run both* — a real migration. The `agents` read-tool survives as a view over the index + liveness rather than over in-memory handles.

**Compatibility note:** NTI-003..006 survive intact — they harden exactly what remains (action_log, fork results, transcript rung, compaction citations). NTI-005's transcript query becomes *more* important: it is the read side of attach.

## 3a. What the estate has already ruled (verified at bytes, 2026-08-09)

Waffles answered the manifold questions directly (his DM, ~00:17Z) and pointed at two documents I then read at their bytes — these constrain everything below:

**Manifold's spine model** (`apps/manifold/docs/design/spine/SPINE-MODEL.md` r1, the constitution):
- Kernel = five content-agnostic doors (signed record, streams, fragments/component loading, processes/seating, grants). Doctrine L9: *the core never learns a feature*.
- Three sigils forever: `/` space, `@` participant, `#` stream. Kinds are an open set, dispatched to extensions.
- **Norn presents as a runner extension. Each norn agent is a PARTICIPANT** whose MANIFEST (record properties) says what runs them, their session identity, start directory, respawn recipe. Presence = seat liveness in the kernel truth report — not a new system.
- Sessions do NOT become spaces ("the tree is the atlas, not the filing cabinet"); promoted lanes of work do. Norn-spawning-norn maps natively: each spawn = a new participant whose manifest names its parent as spawner, signed acts all the way down.
- **Actors sign, subjects don't** — an injected message into a running session is signed by the injector, about the session.
- One ops door (`registry_op`) for all record/tree mutation — norn must not mint tree verbs.
- The stack map, one-to-one (§7): *liminal = runtime and record · manifold kernel = seating · frame = component carriage · iridium = every editing surface · beamr = extension processes · lys = identity · **aion/AWL + runners = agent work***. The last entry is the ruled home for orchestration — norn does not need to own it.

**The aion swing brief** (`stack/aion/docs/design/aion-authoring/SWING-BRIEF-2026-08-09.md` @ aion main `533e8dc40`, Vesper, routed through Waffles):
- Aion becomes a one-stop shop with a **built-in assistant** whose **brain is a pluggable seam — norn is today's default engine** (§4). The seam Tom's norn-as-aion-server instinct wants *already has a name on the aion side*.
- The assistant's tool surface IS the aion MCP server's tool surface — one contract, two consumers (embedded brain, external hosts).
- **The reverse direction already exists**: `aion-integration-norn` spawns norn as a child process per workflow dispatch (verified in the brief against code). Aion→norn is built; Tom's ask is norn→aion.
- Read grammar ruled (R-T6): cursor pages that return immediately, **never blocking tails**; every cross-request handle carries its full identity explicitly (the transcript-key/run-axis fusion defect is the cautionary tale).
- Supervision is ruled aion-native (W-1..W-4): *the server supervises workers; workers supervise agents.* Two layers, no hand-started processes, no launchd.
- Operational scar to inherit — **mechanism corrected 2026-08-09 by Hermes's controlled 2x2** (240+ instrumented boots, logs verified present at liminal `gate-logs/p0-55/`, all four cells): the ws starvation was NOT wake semantics — edge vs level made no difference (62/120 vs 64/120 lost). **What decides is the DELIVERY SLICE BUDGET: zero losses out of 192 at budget 256 under both wake rules.** The fix class is budget/cadence under contention, not wake rules. The design guidance survives unchanged: cursor-replayable reads, never blocking tails, bounded inboxes that shed LOUDLY — any stream surface norn exposes sizes its delivery budget for contention, because it starves exactly when the agent is busiest, which is when you're watching.

## 3b. Survey evidence — manifold + frame (Opus survey, 2026-08-09; cites verified by the surveyor at file:line)

The facts that move the design, beyond §3a:

- **Norn is constitutionally not privileged.** Manifold `docs/SHAPE.md` anatomy table: agents = "norn + any runtime… **Norn is one client of the participation contract, never a gate**" — Claude sessions and any other runtime enter through the same enrolment, SDK, and stamp. The rethink must produce a norn that is *excellent*, not one that is *required*.
- **The participant manifest is the designed attach/supervision surface — and it is EMPTY today.** SPINE-MODEL §3 names exactly the fields (runner, session identity, start directory, respawn recipe, host) as record properties on a `@participant` node; nothing implements it. This is the cleanest place for norn to land, and it costs manifold's kernel nothing because presence is already the truth report.
- **An agent-attach precedent already runs today**: `packages/manifold-mcp` seats a Claude session on an estate by spawning `manifold mailbox open <stream> --json` subprocesses and speaking NDJSON on stdin — two tools (`send`, `history`). The seam is "spawn the CLI, speak NDJSON", constrained by an exclusive per-participant-per-stream lock. Accept or reject that precedent *deliberately* when norn's attach client is designed.
- **Three-outcome sends doctrine** (manifold compose-carrier): a send LANDS, is REFUSED BY NAME, or is a NAMED UNCERTAINTY — never two answers where reality has three. Norn's injection verb inherits this contract.
- **The attach lane has a named upstream dependency**: liminal #14 (attach-path refusals: 28 silent sinks), #37 (attach provenance), #39 (live caps — "no configured number refuses the (N+1)th honest live extension"). Owned by Hermes. An agent runtime that attaches/detaches constantly rides this lane; it is a death clock today, per manifold's own extensions doc.
- **The UI is nearly free if the event vocabulary is right.** Frame's grunk primitives (LogView with pinned log-follow, LifecycleTimeline as a derived never-contradicting projection, StatusPill over a closed 6-phase enum, CounterPanel) render a small data vocabulary (`ShellPhase`, `ShellLogKind`, monotonic feed cut). **If norn's session stream emits a phase enum and a log-kind union that map onto these, the whole primitive family is inherited** — no bespoke components. There is no tree component anywhere; a session-tree view would be new work whoever builds it.
- **Caution row:** the word "session" already means two things in manifold (mailbox lock; page HMAC token) — norn's session concept needs the qualifier said out loud at every boundary. And liminal's SDK today costs one TCP connection per conversation — "fine for one session, not fine for a surface following twenty" — which shapes how many streams a norn estate view should open.

## 3c. Survey evidence — liminal (Opus survey, 2026-08-09; liminal main @ 4ed0562)

**The headline correction: "everything becomes a liminal extension" is a direction, not a present capability.** The surveyor grepped the whole tree: no extension model, no manifest format, no hot-loading (the one "loader" module documents itself as a hash-dedup map that never executes code; uploadable broker code is a *killed* design), no MCP implementation, no stdio transport. Manifold's own extensions doc concurs: the extension wire spec "DOES NOT EXIST today". Consequence for norn: **MCP remains the near-term extension mechanism** (norn already carries client + `mcp serve`); the liminal-extension future is real but must not gate the rethink.

What liminal DOES have is more useful than what was assumed:

- **`Frame::WorkerRegister` (0x17) is a wire-level action manifest, today**: `WorkerRegistration { namespaces, task_queue, activity_types, identity, activities: Vec<{name, input_schema_json, output_schema_json}> }`, synchronously accepted or rejected. **A process can already declare named, JSON-schema-typed activities on the bus.** This is the tools-as-actions seam (§4) at the transport layer — norn's generated tool manifest has a place to stand without any new protocol.
- **The estate's north star names norn's job** (MCP-SEAM-ONE-PAGER, attributed to Tom 2026-07-12): *"agent registers **mailbox** with liminal / **workflows** with aion / **dispatch** with norn, MCP on all."* Norn = dispatch. The kernel identity in §2 is not new — it is the estate's own line.
- **What the estate already assumes of norn** (liminal's docs): norn's MCP surface inherits liminal's §Versioning discipline *verbatim* (four seams, one version discipline); agent transcripts ride the existing observability drain (`aion.observability.v1`, consumed by the host notifier before fan-out) — **reuse that tap, never invent a second one**.
- **`worker-front-door` service profile**: a near-zero-cost liminal deployment whose whole surface is register-worker + correlated push/reply + telemetry drain — the natural profile for norn workers on a bus, with no channel/store machinery.
- **The participant protocol is the developed attach story** (enrollment → credential attach → record admission ⇄ delivery+ack → detach/leave, typed Died/Detached causes, delivery_seq dedup) — durable, ordered, resumable conversations. §6's injected-message verb maps onto `RecordAdmission`; the stream verb onto participant deliveries.
- **Operational laws to inherit**: push payloads under ~64KB or chunked (the G4 truncation incident); *"a caller's poll quantum must NEVER change the protocol outcome"* (G7 ruling); crash detection is a process-link EXIT, not a heartbeat.
- Maturity: 1,947/2 green at the 0.5.3 gate (the 2 are declared red instruments), 862 commits in 30 days, backpressure frames exist but publish doesn't consult them yet.

## 3d. Survey evidence — aion (Opus survey, 2026-08-09; aion @ 0.12.0, main 533e8dc40)

The feasibility question is answered: **the engine was built for this shape.**

- **Embeddable, with the exact posture needed**: `EngineBuilder` exposes `activity_dispatcher(Arc<dyn ActivityDispatcher>)` + `in_process_activity_serving()` — documented as the mode where *the embedder fulfils every activity*, queue admission bypassed, missing dispatcher fails loudly. The dispatcher trait is one function (JSON string in, JSON string out). One process can be server + worker + client simultaneously (proven in-tree).
- **Replay is honest**: action results are journaled (`ActivityCompleted` carries the payload); a fully recorded history replays with **zero live calls** (proven by test); non-determinism is a typed fatal. An effectful tool call that completed is never re-executed on resume.
- **AWL's checker is real**: unknown actions, arity/type/duplicate/missing args, graph cycles, unreachable steps, non-exhaustive outcomes, unbounded loops (bounded iteration is *mandatory* — no unbounded loop exists in the language), unavailable bindings — all refused before anything runs, and reachable as `POST /awl/check` with no local toolchain.
- **The schema door exists**: `type X = schema("file.json")` re-emits external JSON Schema verbatim as an AWL type. The forward path (tool registry → worker block of typed actions) is the net-new piece.
- **Tom's instinct is already in aion's vision docs**: `docs/vision/WORKFLOW-AUTHORING-AGENT-IDEA.md` (2026-06-22, unscoped) — a norn agent whose whole job is authoring aion workflows, iterating against a liveness dial (mocked → cheap-model → live), callable by humans or other AIs. The rethink would be scoping that idea with the tool-manifest twist.
- **Norn is already aion's first-class agent integration**: `aion-integration-norn` spawns `norn --protocol jsonrpc` (norn has a JSON-RPC driven mode today), with the locked observability/intervention design: the `run/*` Response is the replay-authoritative result; `event/*` notifications stream but never enter history; on retry an activity re-runs fresh. Session pinning (`--resume-if-exists`) exists and a workflow has parked on a signal for 28 days and resumed healthy. **The dependency direction is clean: zero norn-specific code in aion's platform crates, and norn does not depend on aion — so norn hosting the engine creates no cycle.**
- **Patterns worth stealing outright**: norn-dev.awl's *control step* (gates are server-run commands, so "a build report that says all green cannot route the workflow to done on its own say-so" — the agent cannot self-certify); norn-fleet.awl's distribute/collect fan-out of agent judgements.

**The design-arounds, honest list:**
1. **No per-activity cancellation** — the server cannot durably kill one wedged tool call; only run-cancel or worker-drain. For agent tool calls this is the sharpest gap.
2. **Per-action `timeout` is authored but NOT enforced today** — only the workflow-level timeout binds.
3. **Schema doors refuse `oneOf`/`anyOf`/`patternProperties`/external `$ref`** — and norn's own envelope layer injects the description field into every `oneOf` variant, so norn's real tool schemas hit this immediately. A normalization pass in the generator is mandatory, not optional.
4. The `agent` seam in AWL is deliberately `String → String`; typed per-tool actions go the worker-owed path with real contracts — which is exactly what the generated manifest provides.
5. Engine-side fan-out width appears unbounded (UNVERIFIED) — worker `max_concurrency` is the only confirmed bound.
6. beamr (which the engine embeds) carries an open JIT memory-safety advisory in every released version; the named mitigation is dropping the `threads` feature. A norn embedding the engine inherits this surface.
7. Declared `run` bodies have no sandbox (stated three times in aion's docs) — protection is review + package-hash sealing.

## 3e. Survey evidence — norn itself (Opus inventory, 2026-08-09; main @ 72c5f81)

The numbers that ground the ring assignments (lines are totals; roughly 2/3 production after test files):

- **Whole lib crate: ~299k lines / 942 files.** tools/ 67k · provider/ 57k · loop/ 42k · session/ 40k · agent/ 23k · integration/ 20k.
- **21 model-facing tools** on the full path (18 standard + cron + process + conditional skill), plus MCP/extension proxies. Tom's instinct that "we don't have tons of stuff" is right — the count is disciplined; the *weight* is in the agent subsystem.
- **The agent subsystem is ~50k lines** (agent/ + tools/agent/), the largest single body in the codebase after provider. This is what §3 collapses.
- **Driven mode exists and is the wire**: `--protocol jsonrpc` (2,693 lines in print/) — initialize, run/execute, event/* notifications, mid-run intervention. Already what aion drives.
- **Session store already multi-session**: one store dir, `index.jsonl` + advisory `index.lock` (the only cross-process primitive in the system), child timelines under `{root_id}/children/`, manifest-driven discovery (nothing crawls directories). The attach surface's "list sessions" verb is a read over this index; the event stream is an append-only JSONL file — the cursor grammar §6 needs is the file's own shape.
- **No supervisor exists**: four in-memory registries, none cross-process, no status/PID files, no worksite concept. Confirms §6's gap — and its smallness.
- **Clean cut lines found**: `follow_up` tool — 1,712 lines, **registered nowhere in production** (dead at registration, delete); `conventions` and `diagnostics` are 7-line facades over the chiron crate and the post-check pipeline respectively.
- **Extension-shaped bodies, with sizes**: patch family ~5.9k · task 3.0k · web 2.1k · schedule/cron ~4.0k · process-manager/watches ~5.0k · skills ~5.4k · rhai 2.2k · claude adapter 1.9k · extensions protocol 0.6k · session/migration ~4k. MCP cluster is 10.4k but is ring-0 connectivity, not periphery.
- **Ring-1 notes**: search is one self-contained tool (2.3k, four modes) — nothing in the loop depends on it; bash is 3.1k with the risk classifier; the TUI is its own 27.7k crate already consuming the child-result channel — structurally ready to become a client.

## 4. Norn as an aion server: the script tool, done right this time

Tom's early script-tool idea, codex "code mode", and ultracode workflows are all the same instinct: **let the model write orchestration instead of performing it one call at a time.** The estate now owns the missing ingredient the earlier attempts lacked: a durable workflow engine with a checkable language.

**The shape:**
- Norn generates an **action manifest from its own tool registry** (tool JSON Schemas → aion action signatures). Generated, never hand-written — hand-maintained copies of a schema are the version-drift layer error waiting to happen.
- The model writes an **AWL workflow** whose actions are norn's own tools (and one special action family: `dispatch` — run an agent step, §3).
- Aion **checks the workflow before execution** (types, action signatures) — errors surface *before* any effect, unlike script-in-a-tool or JS-workflow approaches where they surface mid-run.
- Norn executes as its own worker: each action invocation is a norn tool call through the existing executor, journaled by aion. `PENDING SURVEY`: embedding API, replay semantics (results journaled so effects never re-execute on resume), retry policy shape.
- The workflow run and its journal are **timeline artifacts** — events in the session tree, re-walkable, lantern-able, supervisable. Orchestration stops being ephemeral.

**What this displaces:** structured multi-step orchestration (fan-outs, migrations, batch refactors, verification pipelines) moves into workflows. Ad-hoc "go do this" stays `dispatch`. The in-core sub-agent *suite* (§3) loses its remaining justification.

**What this does NOT do:** it does not put an LLM inside every workflow step. A workflow of pure tool-actions is a checked, durable script (already useful). The potent form mixes tool-actions with dispatch-actions (agent steps). Both fall out of the same manifest.

**The settled seam (§3a) reframes the integration:** aion already dispatches norn as an agent-in-step (`aion-integration-norn`), already names norn as the default brain for its built-in assistant, and already has three serving paths for AWL workers (server-run `run` bodies, `aion worker shell` + manifest, SDK worker). So "norn as an aion server" most plausibly lands as: **norn registers as an aion worker whose declared actions are generated from its own tool registry**, the model authors AWL against that manifest, and the engine runs the workflow dispatching tool-actions back into the same norn session's executor. All-in-one process where wanted; ordinary worker where not. The two directions compose: a workflow step can be a tool-action (this proposal) or an agent-dispatch (what exists).

**Hard questions to resolve with evidence (`PENDING SURVEY` + aion owner):**
- Action granularity and the type boundary (tool JSON Schema ↔ AWL action signatures; the manifest must be generated, never hand-written).
- Embedding vs sibling server; what worker registration costs a norn process in practice.
- Where a workflow run *lives* on the timeline: it must be an event with a handle (id + cursor) either way — re-walkable, lantern-able.
- Failure semantics surfaced to the model (a failed action mid-workflow must be a loud, typed, resumable state — never a silent partial).
- Read-back during a run rides the ruled cursor grammar (describe_run / read_transcript shapes in the swing brief §6) rather than anything norn invents.

## 5. Concurrency: two planes, neither hand-rolled

- **Process plane:** many norns, spawned freely (dispatch, users, cron-like externals), supervised centrally (§6). The OS schedules.
- **Workflow plane:** parallelism *declared* in AWL, executed by aion, durable across process death.

Norn's own loop stays single-threaded-per-turn and simple. The kernel refuses to grow a scheduler.

## 6. Attach, not supervisor: norn exposes a stream and accepts sends

The need is proven by scar tissue: the delegation skill this estate already uses wraps every headless norn in hand-rolled status files, envelope paths, and session-id bookkeeping — a supervisor built in bash, because norn lacks the surface.

But §3a changes the shape of the answer. **The estate already owns both halves of "supervision", and norn should build neither:**
- **Lifecycle** (start, stop, respawn, liveness) is ruled elsewhere: aion-native supervision for agent *work* (W-1..W-4: server→workers→agents), manifold seating for agent *presence* (manifests carry the respawn recipe; the truth report carries liveness).
- **Observation and steering** already have liminal words: an agent's observable state is a **stream** (ordered, cursor-replayable); injection is a **signed send** to the agent's seat.

What norn must own is therefore tiny, and it is ring-0 *communication*, not a new subsystem:

1. **Present a live session as a cursor-pageable event stream.** Reads return immediately with what exists now (`from_seq`/`limit` → events + next cursor) — the exact R-T6 grammar aion just ratified, and the same read shape the session store already serves. **Cursor-replayable, never a blocking tail, delivery budget sized for contention** (the ws starvation scar — Hermes's 2x2 at liminal `gate-logs/p0-55/` proved the losses were slice-budget, not wake semantics; §3a).
2. **Accept an injected message into a live session**, attributed to the injector (actors sign, subjects don't), landing on the timeline as a first-class event.
3. **Answer "what sessions live under this store, and which have a live process"** — a list read, not a daemon.

Terminal attach (`norn attach <session>`) is then a thin client over 1+2 — and the TUI's long-term shape. Manifold's runner extension bridges the same three verbs into participant containers. The delegation skill's status files retire. **`nornd` as a standing daemon is explicitly NOT proposed** — every consumer here is a reader of durable truth plus a signed send, which is the log-as-truth architecture (decision-sheet item 1) doing its job.

Surveyed and answered (§3e): driven mode exists and carries injection today; no liveness registry exists (the list verb is a read over `index.jsonl` + a process-liveness check — small, new); the event stream is append-only JSONL, so the cursor grammar is the file's own shape.

## 7. Modes as data

Plan mode, review mode, worker mode = named bundles of: system-prompt fragment + tool policy (allow/deny) + dispatch posture (may it spawn? may it write?) + conventions activation. Norn already carries profiles; modes are profiles grown one notch. **No mode is code.** The estate's existing instruction-preset pattern (the delegation skill's mode × preset matrix) is the working prototype.

## 8. What this means for in-flight work

- **Memory campaign: strengthened, untouched.** The portal discriminator, log-as-truth, epilogue chains — all of it assumes exactly the architecture this rethink doubles down on. The 10-item decision sheet stays the next conversation.
- **NTI-003..006: intact** (see §3). NTI-007 (generator truth) unaffected.
- **Internal-agents roadmap (skills → hats → speaker/daemon): reshaped** — becomes ring-2 extension work after the attach/extension surfaces exist. Needs Tom's explicit word since it reverses a standing NEXT list.
- **Parked codex branches** carrying sub-agent-adjacent work: unaffected today (already parked), but the rework-vs-reject calculus changes if the sub-agent suite collapses.

## 9. Honest costs and sequencing instinct

This is not a rewrite and must not become one. The sequence that keeps norn shippable at every step:

1. **Attach surface first** (smallest, highest leverage, provable against the existing skill's scar tissue; unblocks manifold integration and the TUI-as-client migration).
2. **Dispatch (norn-spawns-norn) beside fork**, then collapse the in-process suite once dispatch is proven — replace, don't run both indefinitely.
3. **Action manifest + AWL workflows** (needs aion embedding evidence; the biggest piece; possibly its own campaign).
4. **Ring-2 extractions last** — only after the extension surface exists to receive them.

Each step lands independently; none blocks the memory campaign.

## 10. Rulings this document needs from Tom (once evidence is in)

1. The three-ring boundary (§2) — especially search, bash, and the TUI's demotion to client.
2. Sub-agent collapse (§3): fork + dispatch as the only two primitives?
3. Norn-as-aion-server (§4): campaign-worthy, or wait for aion maturity?
4. Attach surface (§6): confirm the three-verb shape (stream-read, signed inject, session list) and that no `nornd` daemon is wanted — the liminal-verbs question is answered (Waffles: present the session as a stream, speak sends, don't mint a protocol).
5. The internal-agents roadmap reversal (§8).

---

## 11. r2 — Tom's first-round answers (2026-08-09 ~00:48Z, his DM; "let's just keep going, I think we're sort of getting there")

Corrections and steers, folded as rulings-in-progress:

1. **Supervision confirmed optional, attach verbs stand.** A norn *might* be supervised by aion, *might* be kicked off by manifold — "but not every session would be done like that." The three-verb surface (§6) is exactly what serves both the supervised and the bare cases; nothing changes.
2. **`follow_up`: WIRE IT UP, do not delete.** ~~Delete now: follow_up~~ (§3e) is REVERSED by owner word: "our tool calls definitely produce follow-ups, and the follow-up tool is meant to be able to follow them — if it's just not wired up it probably just needs to be wired up." The follow-up *actions* mechanism (tool/follow_up.rs) is live today; the model-facing tool was simply never registered. It joins the infrastructure family Tom named as the important core: **action_log + follow_up + tool_use_description**.
3. **The timeline tool.** Tom: the action_log concept "could be extended out to a timeline tool… almost the yang to the memory and lantern tools." And the load-bearing line: **"for norn, anything could be a forkable moment — you don't need to declare a lantern to be able to go back and work back through the timeline and inspect it."** Design reading: the re-walk ladder (NTI-003..006) and the lantern design are two layers over one substrate — the timeline tool is the *complete* record (walk anywhere, inspect any moment, fork from it); lanterns are the *curated* bright spots that resonate unbidden. Declaration buys resonance, never access. This unifies action_log's six query modes, the transcript rung (NTI-005), and fork-from-coordinate into one surface.
4. **Liminal framing corrected (owner word):** "Liminal doesn't have an extension system — that's not the point. **Liminal IS the extension system.** It has the liminal protocol, and there's a thing for in-process stuff too. It's already got what we need — use the liminal SDK, things speak the liminal protocol. A little under-egged at the moment." §3c's "no extension model" stands as a *fact about manifests/hot-loading*, but the design frame is: extensions = processes speaking the protocol, and the SDK is the door. The worker-front-door profile + WorkerRegister manifest (§3c) are exactly this, already live.
5. **Inter-agent communication: possibly not norn's to own.** Tom: still really important, "but I wonder if that maybe sits outside of norn or is not a dedicated thing… great if there was just like a liminal tool." Direction: sibling-to-sibling messaging rides the estate bus (a `liminal` tool in norn speaking the protocol) where an estate exists; parent↔child keeps the driven-mode pipe (it IS the channel) and the store's durable pending-delivery for dormant children. The in-norn MessageRouter/wake machinery shrinks accordingly. What norn stays outstanding at (his words): **working its own history, forking itself, and communicating amongst itself and its sub-agents** — with the comms *transport* increasingly liminal's.

Open from round 1, still unruled: search keep/cut (my lean: keep), TUI→client timing, aion campaign timing, roadmap reversal formality.

---
*Evidence sections §§3a–3e carry the survey facts; every claim there cites either a document read at its bytes or an Opus surveyor's file:line cite. The surveyors' full reports are session artifacts, not committed.*
