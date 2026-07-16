# Norn → Frame v1: domain map

Author: Sable Nightwick · 2026-07-05
Context: Waffles the Terrible's three-question mapping exercise (stack-devs)
toward Frame v1. Norn's role in the stack: the agent runtime — every agent
Frame's pipeline runs is a norn agent, driven by Aion through norn's driven
mode, governed by prospekt doctrine.

> **Historical snapshot.** This file records the 2026-07-05 state and retains
> its original readiness claims for chronology. It is not the current execution
> tracker. Use [`NORN-STACK-INTEGRATION-PLAN.md`](NORN-STACK-INTEGRATION-PLAN.md)
> for sequencing and
> [`design/ablative-stack-composition.md`](design/ablative-stack-composition.md)
> for the current cross-stack contract and hot-composition direction.

## 1. What's ready (relied on as-is today)

**The invocation contract Aion builds on — all verified against live use:**

- **Driven mode `norn-driven/1`** (`crates/norn-cli/src/print/jsonrpc/`):
  versioned protocol, capability handshake, one run/execute per process,
  post-acceptance error funnel (every failure answers as a typed JSON-RPC
  error), spawn-time schema arg. The stdin-reader wedge is fixed (a61fc36).
  Tom has ruled driven mode THE workflow interface — non-negotiable.
- **Session idempotency**: `--session-id <derived> --resume-if-exists` =
  create-or-resume in one flag pair, clap-guarded against conflicting modes.
  This is the Aion activity primitive (at-least-once activities re-enter the
  same session instead of forking state).
- **Structured output**: `--output-schema` / spawn `output_schema`, with the
  `schema_unreachable` honesty contract (validation_errors + best-attempt
  output, never a fake success).
- **Context protections, armed and validated**: catalog-seeded windows by
  default; since 5042669 a mis-armed window (explicit value above the model's
  catalog max, or an uncatalogued model with no window) is a hard typed error
  at agent build — covers TUI, print, driven, and embedders through the one
  assembly funnel. Empirically verified under a 3.5M-token, 50-turn hostile
  prompt (2 live compactions, no overflow).
- **Root session persistence**: write-through JsonlSink before memory,
  durable across kill -9; resume and fork-from-session work.
- **Tool surface** (canonical names, registry-enforced): read, search
  (content/files/fuzzy/ast — and since 670696c it never sweeps .git or
  secret material on any flag), edit, write, apply_patch, bash, process,
  cron, lsp, web_search/web_fetch, skill, task, diagnostics_check, fork,
  spawn_agent, signal_agent, wake_agent, close_agent. Monitor stack: run_in_
  background, timeout-migrates, watches with filter scripts, wake-on-completion.
- **Embedder event fidelity** (3cac008): injection delivery, live compaction
  events, tool-delta correlation, context-window setter — the meridian
  integration contract.
- **Config layering**: compiled defaults < user settings < project <
  local < `-c` overrides < flags; profiles (md/toml/json, three scan roots);
  workspace confinement on read-class tools.

Suites: 2,855 norn lib tests + full norn-cli suite green; strict clippy wall.

## 2. What's broken or incomplete (names and files)

Ranked by how hard each bites Frame.

1. **Children have no persistence sink** (Gap 1 of the 14-gap inventory,
   `docs/reviews/2026-07-04-session-fidelity-inventory.md`). Fork/spawn/rhai
   children run entirely in memory — a pipeline's sub-agents leave no durable
   trace. The `child-persistence` design is in active review
   (`docs/design/child-persistence/` — design note + my review; two items
   held: crash-window write ordering, single name-allocation authority).
2. **Plain print mode emits no stdout envelope on agent/provider errors**
   (`print/orchestrator.rs:351` → stderr + exit 1, stdout empty). Shell-mode
   consumers see error runs as unparseable output rather than a typed stop.
   Driven mode is immune (its funnel answers everything). Fix sketch is in
   `docs/reviews/2026-07-05-context-window-incident.md` with 5 owner rulings
   pending (new `StopInfo::Error` variant, envelope-version question).
3. **Session index flock has no deadline on the CLI path**
   (`builder_from_cli` never calls `with_index_lock_deadline`; `file.lock()`
   at `session/persistence/lock.rs:96` blocks forever). Lead suspect for the
   doctrine pilot's silent 8-minute wedge — and concurrent norns IS the
   pipeline shape, so Frame inherits this until wired. Bounded plumbing
   (`lock_with_deadline` + typed `IndexLockTimeout`) already exists; the
   deadline value needs an owner ruling or settings key.
4. **Child model/window validation deferred**: a spawned child on a different
   model keeps the parent's loop config (comments at `tools/agent/
   spawn_launch.rs`, `fork_launch.rs`, `integration/rhai/agent_ops.rs`);
   per-model re-seeding is owned by the `agent-variants` unit.
5. **Agent variants not implemented** (brief written:
   `docs/agent-variants-and-child-persistence-briefs.md`): named profile
   variants with tool subsets, prompt variants, per-variant model resolution,
   no-silent-downgrade rule. This is the substrate for pipeline roles
   (explorer / implementer / adversarial reviewer).
6. **Pre-assembly stdin read is unbounded** (`print/orchestrator.rs:274`,
   runs even with a positional prompt). Empirically NOT the pilot wedge
   (Vespa's spawn-site audit: all harnesses null stdin) — demoted to hygiene,
   but it's a landmine for any future harness that pipes and doesn't close.
7. **Session-fidelity remainder** (same inventory): full tool outputs not
   spooled (Gap 5 — capped at prompt-build, originals lost), suppress/mute
   marks live-only (Gap 8), assorted smaller gaps. The road-sign/annotation
   schema that fixes 8 properly is HELD for joint design with Tom.

## 3. Critical path for Frame v1

**Must land, in dependency order:**

1. **Index-lock deadline on the CLI path** — smallest fix, unblocks trust in
   concurrent fleets; every pipeline run gambles on it until then. Needs:
   Tom's scope ruling + the deadline value.
2. **Print-mode error envelope** — Frame's orchestration reads norn's stdout;
   "error = unparseable" is not a contract. Needs: Tom's 5 rulings, then a
   day's work. (Driven-mode-only pipelines dodge this, but shell-mode tools
   and humans don't.)
3. **child-persistence** — living software that can't remember what its
   sub-agents did isn't living. Durable child timelines + the children/
   layout + spool are the foundation for the action-log spine and any
   Frame-level memory claims. In review now; lands after my two holds clear.
4. **agent-variants** — pipeline roles with scoped tools and honest model
   resolution. Prospekt doctrine already names these roles; norn has to
   enforce them rather than trust prompts.

**Explicitly NOT on the critical path (nice-to-have or later phase):**

- Haematite embedding behind the session store (phasing already agreed with
  Tom: manager-trait seam first, embedding as its own campaign; JSONL is the
  v1 store and imports later — append-only makes that safe).
- Annotation/road-sign layer, consent-muting, compaction-as-view (joint
  design with Tom, deliberately held).
- Claude Code session import, native norn workflows (driven mode + Aion IS
  the workflow story for v1), shared diagnostics job server.

**Cross-domain dependencies I'm watching:** Aion drives norn exclusively via
driven mode + `--session-id` idempotency (verified with Vesper); meridian
embeds norn directly and is pinned at the event-schema contract (the
`ForkComplete` Option change in child-persistence is flagged for a meridian
pin-bump ticket before it lands).
