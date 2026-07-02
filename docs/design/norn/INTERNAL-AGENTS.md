# Internal agents & orchestration — design capture (2026-07-03)

**Status:** vision capture from an owner design discussion. Pre-brief — nothing
here is scheduled. Items marked **(lean)** are the assistant's recommendation
pending an owner ruling; everything else is the owner's stated intent.

This doc grew out of the `RunMonitored` held-item discussion (the former
`docs/HOLD-FOR-DISCUSSION.md`, now retired): the scaffolding in
`agent/monitor.rs` turned out to be the shadow of a much larger idea — a
taxonomy of agent kinds beyond the tree.

---

## 1. The agent taxonomy

Norn today has one axis: root agents and their **tree agents** (sub-agents and
forks — session lineage, registry presence, message routing, delegation
budgets). The discussion adds three internal kinds plus a real-time interface
kind:

| # | Kind | Lifetime | Session | Tree presence | Purpose |
|---|------|----------|---------|---------------|---------|
| 1 | **Tree agents** (existing) | task-scoped or lingering | persisted, resumable | yes | delegated *work* |
| 2 | **Processors** | one tool call | none | no | delegated *volume* — chew through large input, return structure |
| 3 | **Watchers** | medium/long-running | none | no | delegated *attention* — watch a feed over time, emit events |
| 4 | **The assistant** | persistent companion | yes (continuity) | no (beside the tree, not in it) | delegated *side-tasks* — triage, scribe, memory, PM |
| 5 | **The speaker** | session-long | shares a live view | no | delegated *conversation* — real-time voice interface to a slow deep model |

Kinds 2–4 are **internal agents**: assembled through the same `AgentBuilder`
path as everything else (zero-tool agents already work — `b15647a`), with
narrow role profiles, invisible to the tree. From the main model's point of
view, processors and watchers are *tools*; the agent inside is an
implementation detail.

The distinction between 2 and 3 (owner's refinement): a processor makes its
way *through* large output once; a watcher doesn't wade through volume at all
— it runs the command (or attaches to it), writes itself filter scripts, and
**responds to what shows up** for the life of the process.

## 2. What already exists in the codebase (verified 2026-07-03)

- **Zero-tool agents** (`b15647a`) — internal agents with tiny or empty
  toolsets assemble through the standard builder path.
- **Profiles** are first-class — the assistant's "hats" (§5) are profiles.
- **The message-injection ruling** (held-item 2 discussion, same day):
  boundary signals ride the durable injected-message path
  (`MessageRouter` + pending store), not the tool envelope. Watcher alerts,
  speaker relays, and assistant nudges all arrive as injected messages —
  persisted, ordered, resume-safe. The `RuntimeInputs` deletion is
  *reinforced* by this design, not in tension with it.
- **`agent/goals.rs`** — croner-based `Scheduler` (cron-expression entries
  that dispatch fresh agent sessions, `next_execution` computation) and
  `GoalTracker` (token/time budgets with `Stop`/`Handoff`/`Continue`
  continuation policies). Pure data structures; executor wiring explicitly
  deferred ("N-015 and beyond"). **Seed of the cron tool (§6).**
- **`integration/rhai/`** — a Rhai engine with blocking builtins
  (`read_file`, `run_cmd`, `read_json`/`parse_json`/`to_json`/`write_json`,
  `write_file`) and agent ops (`spawn_agent`, `signal_agent`), tested. (A
  `fork_agent` builtin was advertised in rustdoc but never registered; the
  rustdoc is corrected — forking from workflow scripts is a design item for
  the JS tool, not an existing capability.) The *plumbing* (host-function
  bridge, runtime-handle pattern, agent-op semantics) is the reusable part;
  the **language is not** — the owner has ruled Rhai out as the model-facing
  workflow language (§8.2). Rhai remains live today for internal lifecycle
  scripting (child-policy conditions, linger, coordination close).
- **wake/linger + `signal_agent` + delegation budgets** — scheduled wake-ups
  compose with the existing wake machinery.
- **Driven mode (`norn-driven/1`)** — the daemon (§6.2) is, structurally, a
  persistent driver that owns schedules and wakes sessions over the driven
  contract.
- **`ToolOutputBudget` / `cap_model_output`** — the inline cap already hides
  the bulk of a huge tool output from the model; the digest processor (§4.2)
  is the missing other half that *reads* what the cap hides.

**Deliberately absent:** managed background processes. The bash tool is
strictly synchronous — drain-grace exists precisely to stop backgrounded
children holding the pipes. Watchers need the opposite (§3).

## 3. Foundation: managed background processes

The one genuinely new primitive everything long-running depends on.
**Owner ruling (2026-07-03): the Claude Code Bash tool is the standard to
meet.** Concretely, the manager provides:

- **Explicit backgrounding** — spawn a command detached from any single
  tool call, owning its pipes; spool stdout/stderr to file (the spool is
  also the processor's input for post-hoc digestion, §4.2).
- **Automatic migration** — a foreground command that runs long enough is
  moved to the background instead of blocking the turn; the model is told
  it moved and how to check on it.
- **Wake on completion** — when a background process exits, the agent is
  notified with the result (exit status + output access) as an injected
  message; combined with wake/linger this works even when the agent is
  otherwise idle between turns.
- **Incremental output access** — read new spool output since the last
  check (the `BashOutput` pattern), status, kill, list.
- **Watcher handoff** — swap any running background process over to a
  watcher agent (§5) at any point: "this is taking a while and getting
  noisy — watch it for X instead."
- **Follow-up integration** — composes with norn's existing follow-up
  machinery (`tools/follow_up`) so a backgrounded command's completion can
  drive queued follow-on work.

Useful standalone — this ships value before any internal-agent layer
exists, and it is the explicit first work package (§10).

## 4. Processors (short-run, volume → structure)

### 4.1 Agentic search
"Here's what I'm looking for" → an internal agent with a read/search/glob/
AST/web-search/web-fetch profile (plus search-flavoured bash) goes and finds
it, returns findings. Also covers the huge-legacy-file case: "find the part
of this 5,000-line file that handles X."

**(lean)** A separate `agentic_search` tool rather than a mode on `search` —
different cost profile; the model should choose it deliberately.

### 4.2 Large-output digest
`cargo check` emits 135 warnings; today's model behaviour is "tail the last
30 lines and hope." Instead: hand the full output to a processor — "give me
every error and warning, grouped" — and return structure.

**(lean)** Two invocation surfaces: an option on bash itself
(`digest: "extract all errors and warnings"`), and post-hoc digestion of a
spool/offloaded output from §3.

## 5. Watchers (the original `RunMonitored` intent)

"Here's the command, here's what to watch for" →

1. command runs under the background-process manager (§3);
2. a cheap-model watcher agent consumes its output incrementally — writing
   its own filter scripts against the spool (the Claude Code `Monitor` tool
   pattern) rather than reading raw volume;
3. matches/alerts arrive at the parent as **injected messages**;
4. the watcher can be handed off — notably to the assistant (§6), so
   long-running watches survive the main agent going dormant.

**(lean)** Invocation: an option on bash (`monitored: {brief}`) for the
run-and-watch case; a small separate surface for *attaching* to an existing
background process.

## 6. The assistant (persistent companion) + scheduling

A persistent internal agent riding alongside the main agent — **not** a
sub-agent or fork. Side-tasks: "commit this for me," "document what we just
discussed," "remember this." Distinctly stateful: it keeps a session for
continuity.

- **Hats** (owner): the assistant has swappable profiles — project manager,
  chronicler/memory-keeper, etc. Multiple hat profiles are written; the
  assistant is switched between them. Profiles being first-class makes this
  cheap to express.
- **Triage mode** (owner): the *main* agent may stay dormant most of the
  time; the assistant wakes on a loop, triages, checks watchers, and only
  escalates to the main agent when warranted.
- **Adoption**: long-running watchers (§5) can be transferred to the
  assistant's care.
- It writes (commits, docs, memory) — so it needs attribution/audit and
  permission-policy answers. **(lean)** assistant is layer 2, after
  processors/watchers; the write-access questions get their own brief.

### 6.1 Cron tool (in-session)
Schedule: a relative wake-up ("in 15 minutes"), a loop ("every N"), or full
calendar cron expressions. `goals.rs::Scheduler` is the seed; what's missing
is the live executor (timers driving `wake_agent`/session dispatch) that the
integration layer was always meant to add. Targets: the main agent, the
assistant's triage loop, or a fresh session dispatch.

### 6.2 Daemon (longer-term)
Wake-ups that survive the session — and the machine: "wake at 8am if the
computer's on," even though no session is running. Structurally: a norn
daemon (launchd/systemd-managed) that owns the durable schedule store and
wakes/spawns sessions **over the driven protocol** — the daemon is a
persistent driven-mode driver. This is explicitly longer-term.

## 7. The speaker (real-time voice interface)

A very fast model (gpt-5.3-codex-class today; haiku-class on the Anthropic
side) acting as the *voice* of the slow deep model:

- has a **live view** of the main session (tails session events) — it is not
  a fork (no divergent context copy); it's a companion that can see;
- the user talks to it in real time — ask questions about progress, think
  out loud, give instructions — **without polluting the main agent's context
  or the terminal output**;
- anything that should reach the main agent is relayed as an injected
  message/intervention (same durable path as everything else);
- voice I/O rides the owner's existing meridian extensions: faster-whisper
  STT in, sentence-buffered streaming TTS out.

**Dependency:** norn support for the meridian extension protocol — a
long-wanted integration in its own right, and the gating piece here.

## 8. Workflows

### 8.1 Durable workflow dispatch (aion)
A tool that submits workflows to aion (the owner's Temporal-style durable
workflow engine on beamr) and tracks them. Norn-side this is a client tool;
aion's API surface is the open question. (The full aion engine is
"significantly heavier duty" — norn gets a dispatch surface, not an embedded
copy.)

### 8.2 Dynamic workflows (code mode)
The model writes a script that strings tools, sub-agents, and shell steps
together with deterministic control flow — spiritually the Claude Code
`Workflow` tool.

**Language — owner ruling (2026-07-03): NOT Rhai.** The owner has used Rhai
as a workflow language before and rejects it. The model-facing workflow
language is **JavaScript-style syntax**: every model has deep JS fluency,
and with tool calls/results already being JSON, JS is the zero-impedance
choice.

**(lean)** Engine: **Boa** (`boa_engine`) — a pure-Rust ECMAScript engine.
Rationale: keeps the supply chain pure Rust (no C FFI — consistent with
this workspace's posture; norn's owner rewrote the BEAM in Rust rather than
bind C), embeds cleanly, and orchestration scripts are not
performance-sensitive. Fallback candidate if Boa's ES conformance bites in
practice: `rquickjs` (QuickJS bindings — faster and highly conformant, but
a C dependency). The host API we expose matters far more than the engine:
`agent()`, `parallel()`, `pipeline()`, `bash()`, `read()`/`write()`, JSON
helpers — the semantics of `integration/rhai`'s existing host functions
carry over even though the language does not.

**Follow-on decision (flagged, not ruled):** once the JS engine lands, norn
would carry two embedded languages — JS for model-facing workflows, Rhai
for internal lifecycle scripts (child-policy conditions, linger,
coordination close). **(lean)** migrate the lifecycle scripting to the same
JS engine afterwards and remove Rhai entirely: one language, no parallel
infrastructure, and the owner dislikes Rhai anyway. Sequenced after the
workflow tool proves the engine, as its own work package.

Owner's motivating example — a worktree stacked-diff pipeline: each agent
finishes with structured output including a commit message → script commits,
pushes, branches off the result, dispatches the next agent, messages
reviewers.

## 9. Config surface

Per the repo rule (**no assumed defaults**): every internal-agent role is
explicitly configured — model, tools, prompt — e.g. sections per role
(search, digest, monitor, assistant hats, speaker). **(lean)** an
unconfigured role means the corresponding tool/surface simply isn't offered;
nothing silently falls back to a hardcoded model.

### 9.1 Proposed module layout (lean)

Owner directive (2026-07-03): internal agents get their own module. Proposed:

```
crates/norn/src/
  process/                 -- background-process manager (§3); no agent
                              dependency — bash + watchers both consume it
    manager.rs             -- spawn/adopt, registry of running processes
    spool.rs               -- output spooling, incremental reads
    handle.rs              -- status, kill, exit notification wiring
  internal/                -- internal-agent machinery (§1 kinds 2-4)
    role.rs                -- role config (model, tools, prompt) — explicit,
                              no defaults (§9)
    harness.rs             -- run an ephemeral internal agent to completion
                              via AgentBuilder → structured result; no tree
                              registration, no session persistence
    watcher.rs             -- watcher lifecycle over process:: (§5), alerts
                              via message injection
    assistant/             -- layer 2 (§6): hats, triage loop, adoption
```

Model-facing tools live with the other tools (`tools/agentic_search.rs`,
bash gains background/digest/monitored options, `tools/workflow/`,
`tools/aion.rs`) and call into `process::`/`internal::`. The speaker (§7)
is not a module here — it is a driven-mode/extension-protocol consumer
design in its own right.

## 10. Suggested build order (lean, unscheduled)

1. **Background-process manager** (§3, `process/`) — foundation,
   standalone value, owner-confirmed first.
2. **Processors** (§4, `internal/` harness + roles) — smallest agent
   surface, immediate daily value.
3. **Watchers** (§5, `internal/watcher.rs`) — composes 1 + 2.
4. **Cron tool, in-session** (§6.1) — `goals.rs` executor wiring.
5. **Assistant v1** (§6, `internal/assistant/`) — hats, triage loop,
   watcher adoption.
6. **Workflows** (§8) — JS code-mode tool (engine per §8.2); aion dispatch
   tool.
7. **Rhai retirement** (§8.2 follow-on) — migrate lifecycle scripting to
   the JS engine, remove Rhai (pending owner ruling).
8. **Speaker** (§7) — gated on meridian extension protocol support.
9. **Daemon** (§6.2) — after the driven-mode consumer story matures.

(4 and 6 are cheap relative to value and can move up; 8 is gated on the
extension-protocol work regardless of order.)

## 11. Open questions for the eventual briefs

- Monitor/digest invocation surface: bash options vs. separate tools (leans
  in §4.2/§5).
- Notification identity: what sender identity do non-tree agents use on the
  router (watcher alerts, assistant messages) — attribution matters for the
  action log and the transcript.
- Do delegation budgets apply to internal agents, and whose budget do they
  draw from?
- Assistant permissions/attribution: whose permission policy governs its
  writes; how its actions land in session events and the action log.
- Speaker's live view: tail the session JSONL vs. an event-bus subscription;
  how much of the deep model's state it may reveal.
- aion dispatch API surface (aion repo currently read-only/WIP).
- Daemon process model and schedule-store durability.
- Workflow JS engine final call: Boa (pure Rust, lean §8.2) vs rquickjs
  (C FFI, faster/more conformant) — decide at WP6 kickoff.
- Rhai retirement (§8.2 follow-on): confirm migrating child-policy/linger/
  coordination scripting to the JS engine once it lands.

## 12. Relationship to the held items

The `RunMonitored` scaffolding (`agent/monitor.rs`) did not survive this
design — it monitored an in-process Rust future with a static-string
heartbeat and an unused provider; nothing in §5 built on it. The owner ruled
on 2026-07-03 and the deletions are executed: `agent/monitor.rs`,
`ToolEnvelope.runtime_inputs` (+ `RuntimeInputs`, `InboundMessage`,
`DiagnosticReport`, `FileChange`, `FileChangeType`), and
`ToolContext.runtime_args`. Rulings are recorded in
`docs/DECISIONS-2026-07.md` §4; `docs/HOLD-FOR-DISCUSSION.md` is retired.
This doc is the forward design record.
