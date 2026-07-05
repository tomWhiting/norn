# Campaign briefs: agent variants & child persistence (2026-07-04)

Owner direction (Tom, Meridian DM session 2026-07-04): start with fork/spawn system
instructions and agent profiles, then child persistence + storage layout. Naming note
(Tom ruling, same day): briefs carry NAMES, not version numbers — version-style labels
teach agents to defer work to "the next one." Everything in this document ships; there
is no "later" bucket except the explicit parked list. Full gap
evidence in `docs/reviews/2026-07-04-session-fidelity-inventory.md`. Dispatch per
`docs/FLEET-PLAYBOOK.md`: implement → adversarial Fable review → fix → re-verify → commit.

Both briefs follow existing norn patterns. Flagged owner decisions are marked **[RULING]**
— recorded here, silence-is-ship per the DECISIONS-2026-07 convention unless marked HOLD.

---

## Brief `agent-variants` — Agent variant profiles + child system instructions

### Intent

Children today get almost nothing: forks receive only `FORK_SYSTEM_PREAMBLE`, spawns a
near-empty prompt, and `ParentSystemInstruction` is half-wired (never published anywhere).
Meanwhile `SubagentDescriptor` already carries `role`, `model`, and `profile` fields.
This brief turns those dormant fields into a real variant system: named agent variants
(à la Claude Code subagent types) with a prompt block, tool subset, and default model —
defined in config, honoured by fork and spawn, disclosed to the parent.

### Requirements

- **R1 — Variant definitions in settings.** A `variants` section in NornSettings (the
  existing config surface: `crates/norn/src/config/types.rs`, merge in `config/merge/`,
  validation in `config/validate.rs`). Each variant: `name`, `description`,
  `prompt` (inline string or file path), `tools` (allowlist; absent = inherit parent's
  registry), `model` (absent = **inherit the parent's model — never a hardcoded
  fallback, never a silent downgrade**), `reasoning_effort` (optional). Merge semantics:
  by name, project overrides user (same as MCP servers, D3 of norn-config DESIGN).
- **R2 — Built-in variants.** Ship `explorer` (read-only tool subset, wide-search prompt
  guidance), `reviewer` (adversarial-review prompt per FLEET-PLAYBOOK briefing patterns,
  read-only + diagnostics), `implementer` (full tool set, verify-before-done guidance).
  Built-ins are data (embedded variant definitions), not code paths — user/project
  settings can override any of them by name. **[RULING agent-variants-a]** these three names/scopes;
  prompts drafted by implementer, reviewed by Fable against FLEET-PLAYBOOK doctrine.
  **Reviewer-model exception (ruled after Dr. Spaceman's Q3, 2026-07-04):** the built-in
  `reviewer` variant has NO default model and does NOT inherit the parent's — same-tier
  review of the code that wrote it is the playbook's "broken reviewer." Spawning
  `reviewer` without `variants.reviewer.model` configured (settings) or an explicit
  spawn-time model = typed error naming the missing config. No invented pin (norn's
  catalog is provider-dependent, a hardcoded tier would be arbitrary); the review tier
  is an owner-level config value. Explorer/implementer keep parent-inherit.
- **R3 — Spawn honours variants.** `spawn` tool accepts `variant: Option<String>`;
  resolves against merged settings; unknown variant = typed error listing available
  variants (no silent fallback). Resolved variant populates the child system prompt
  (via `system_prompt/builder.rs` child path), tool registry subset, model, and the
  existing `SubagentDescriptor.role/model/profile` fields so `subagent.started` Customs
  disclose the variant durably.
- **R4 — Fork identity enrichment.** Forks keep full history inheritance, but
  `FORK_SYSTEM_PREAMBLE` grows structured identity: who forked you, your requirements
  contract (already forced), your position (path address once `child-persistence` lands; parent agent id
  until then), and what delegation rights you hold (from `ChildPolicy::grant_for_child`
  depth budget — the child is TOLD its budget; a limit an agent doesn't know about is
  an assassination).
- **R5 — Wire or delete `ParentSystemInstruction`.** It is currently designed-never-wired.
  Either it becomes the carrier for parent-authored variant prompts (published in the
  fork/spawn pipelines) or it is deleted — no zombie code. Implementer proposes; Fable
  reviewer rules with evidence.
- **R6 — Delegation disclosure hygiene.** Leaf agents (depth budget exhausted) must not
  be shown fork/spawn tools they cannot use; tool registry filtering happens at child
  assembly, not at call-rejection time. Precedence: delegation policy WINS over a
  variant's `tools` allowlist — effective tools = allowlist ∩ policy, computed at
  assembly. An `explorer` granted spawn in its allowlist at depth 0 simply doesn't see
  spawn; never a silently-empty result at call time. (Ruled after Dr. Spaceman's Q3
  rider, 2026-07-04.)

### Acceptance criteria

- Variant resolution: unit tests across all merge layers incl. project-overrides-builtin.
- A spawned `explorer` cannot see write tools (registry-level, verified by tool list in
  its first provider call payload, not by call rejection).
- A spawned child with no variant model runs on the parent's model — asserted against
  the actual provider config, not the descriptor.
- Fork preamble contains contract slugs + depth budget; golden-file test.
- `cargo clippy --workspace --all-targets -- -D warnings` + `cargo fmt --check` clean;
  no `#[allow]` outside `#[cfg(test)]`.

### Key files

`crates/norn/src/config/{types,validate}.rs`, `config/merge/`,
`crates/norn/src/system_prompt/builder.rs`, `crates/norn/src/agent/prompt_install.rs`,
`crates/norn/src/agent/fork.rs`, `crates/norn/src/tools/agent/{spawn.rs,fork_pipeline.rs,fork_seed.rs}`,
`crates/norn/src/loop/loop_context.rs` (ChildPolicy).

---

## Brief `child-persistence` — Child persistence, storage layout, path addressing

### Intent

Close inventory Gaps 1/2/3/5/6 (and 11 by consequence): children persist, the storage
layout adopts the Claude Code shape, and agents get filesystem-like addresses. Identity
vs address: ids remain storage identity (default UUIDv4 per R8); paths are the
human/coordination layer.

### Requirements

- **R1 — Layout.** `session_data_dir()/<sluggified-project-dir>/<root-uuid>.jsonl` for
  the root; sibling dir `<root-uuid>/` containing `children/<path-slug>.jsonl` (one per
  fork/spawn, recursively) and `spool/` for full-size tool outputs. Migration: existing
  flat session files remain readable (index self-heal already tolerates layout drift —
  verify, don't assume). **[RULING child-persistence-a]** exact slugification scheme = same as Claude
  Code's (replace `/` with `-`) for interop with the planned CC session import.
- **R2 — Children get sinks.** `resolve_fork_store` / `resolve_child_store` / rhai
  `agent_ops.rs` mint sink-equipped stores via SessionManager (or a new branch
  primitive on it). Grandchildren too — the fix must be depth-recursive. `--no-session`
  keeps sink-less children (explicit choice propagates down); in that mode the
  branch-point event carries a typed `session: None` — absence stated honestly,
  never a fake id (keeps R3 consistent; ruled after Dr. Spaceman's Q1, 2026-07-04).
- **R3 — Durable linkage.** Branch-point event on the PARENT timeline at fork/spawn
  (revive `SessionEvent::Fork` or a new Custom; must carry: child session uuid, child
  path address, parent event id anchor). `ForkComplete.forked_session_id` must point at
  a session file that exists (kill the registry-UUID fallback lie,
  `fork_pipeline.rs:511-514`).
- **R4 — Path addressing.** Root = primary line, permanently marked. Children named
  `<variant-or-role>-<short-random>`; full address = `root/reviewer-kestrel/verifier-moss`.
  Uniqueness is **for-all-time within a parent** (append-only per-parent name registry),
  NOT merely among live siblings: addresses are recorded durably (branch-point events)
  and used for coordination, so a dead child's name is never re-minted under the same
  parent — an old persisted address must never resolve to a different agent. (Ruled
  after Dr. Spaceman's Q2, 2026-07-04; refines Tom's live-siblings phrasing — names stay
  just as short in practice since scope is one parent's lifetime children.) Address
  recorded in the child's session header event + the parent's branch-point event +
  `subagent.started`. Coordination tools (`signal_agent`) accept path addresses as
  aliases for agent ids. File naming consequence for R1: the flat `children/` dir is
  keyed on the FULL path slug (e.g. `reviewer-kestrel--verifier-moss.jsonl`), since
  sibling-scoped names alone collide across different parents' grandchildren.
- **R5 — Spool for full tool outputs (Gap 5).** `append_tool_result` persists the FULL
  output; the capped projection happens at prompt-build/replay time. Over-budget
  payloads write `spool/<event-id>.bin` (or inline if small) with a durable reference
  in the event. Action-log Level-2 detail reads the spool.
- **R6 — Root stop-reason events (Gap 6).** `loop.timed_out` / `loop.cancelled` /
  `loop.max_iterations` Customs appended on the respective exits (timeout append goes
  after the inner future drops, beside `ensure_tool_results_complete`,
  `loop/runner/entry.rs:306`).
- **R7 — SessionTree verdict.** Production-wire it as the in-memory index over the new
  on-disk tree, or delete it. No dead code remains either way.

### Acceptance criteria

- Kill -9 a run mid-fork; resume; the child's session file exists on disk with its
  events up to the kill, and the parent's branch-point event references it.
- Grandchild `Sent` audits reach disk (Gap 11 regression test).
- A 200k-char tool output round-trips: full in spool, capped in prompt, full in
  action-log detail after restart.
- Resumed root session after step timeout shows the `loop.timed_out` event.
- 500-LOC compliance maintained (spawn.rs and fork_pipeline.rs are already large —
  expect module splits, not squeezing).

### Key files

`crates/norn/src/session/{store,manager,tree}.rs`,
`crates/norn/src/tools/agent/{fork_pipeline,spawn,fork_seed,lifecycle}.rs`,
`crates/norn/src/tools/agent/coord/signal_agent.rs`,
`crates/norn/src/integration/rhai/agent_ops.rs`,
`crates/norn/src/loop/{tool_dispatch.rs,runner/entry.rs,runner/machine.rs}`,
`crates/norn/src/tool/output_budget.rs`, `crates/norn/src/config/paths.rs`.

- **R8 — Session identity (RULED, Tom 2026-07-04).** Session NAME becomes a first-class
  resume handle alongside the id (like coord sessions). Session ids are enforced-valid:
  any UUID version, or a fully custom opaque string supplied by the embedder — validated
  at the boundary, never silently coerced. **Default generation switches to UUIDv4**:
  v7's shared timestamp prefix defeats git-style short-prefix eyeballing, and "created
  at" tells you nothing about which session was active last. Sessions are partitioned by
  project directory (R1 layout), so resume-by-name resolves within the current project
  only — no cross-project collisions, no "500 stacked dev-scout" pileups. Display form
  everywhere: `name (shortid)` — name plus first 8 chars of the id. Spawned children:
  collision-safe id + generated name (R4 scheme); callers may supply either.

### Explicitly out of scope (parked, not dropped)

Haematite embedding (own campaign, after its dev interface exists); road-sign
annotation events + consent-muting (rides on this layout, next campaign); durable
suppression marks (Gap 8 — belongs to the annotation-event design); live/persisted
compaction field asymmetry (Gap 9 — fold into annotation design); CC session import.

---

## Brief `config-polish` — Config surface follow-ups (RULED, Tom 2026-07-04)

Extends the shipped norn-config cluster (NC-001..005). Four requirements:

- **R1 — Bare `norn init`.** The `init conventions`-but-no-`init` asymmetry dies. Plain
  `norn init` initializes a project `.norn/` (settings file skeleton, gitignore entry for
  `settings.local.*`), and reports what it created. Subcommands (`conventions`, future
  ones) remain, but the bare command works.
- **R2 — Auto-initialization.** `~/.norn/` is initialized on first run (not just first
  write — revisits norn-config D11 for the user level ONLY; project `.norn/` is still
  never auto-created by mere reads, only by `norn init` or first project-level write).
- **R3 — Settings format preference: TOML or JSON.** Loader accepts `settings.toml` OR
  `settings.json` at each layer (error if both exist at one layer — no ambiguity).
  `norn init` asks or takes a flag. Merge/validate layers are format-agnostic (they
  already operate on deserialized types).
- **R4 — Inline JSON settings flag.** `--settings '<json>'` on the CLI: a full
  NornSettings fragment injected as the highest file-layer (below `-c` single-key
  overrides). This is WHY JSON stays supported: you can't inline TOML on a command
  line. Matches the Claude Code pattern Tom uses daily.

Acceptance: round-trip tests for both formats at every layer; both-formats-present
error path tested; `norn init` idempotent (second run changes nothing, says so).

## Dispatch notes

- `agent-variants` and `child-persistence` are independent until agent-variants R4 wants
  path addresses — `agent-variants` lands with agent-id placeholders if `child-persistence`
  hasn't merged. Parallelizable across two implementers with strict
  file ownership (only overlap: `fork_pipeline.rs`/`spawn.rs` — sequence those edits).
- Sequential cargo builds only (shared target dir); both norn target dirs are cold as
  of 2026-07-04 late — first build is slow, expect it.
- Every unit: Fable adversarial review with the brief + intent + diff, re-verify after
  fixes, then commit. No `cargo ... | tail` (masks exit codes).
