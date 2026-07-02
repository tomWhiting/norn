# BRIEF R1 — Assembly Unification

**Cluster:** Norn hardening campaign, Wave 3
**Branch:** `hardening/final-state` (Wave 1 committed at `27df51d`; Wave 2 editing rules/session/loop/context internals — **do not touch those or any code; this brief writes only this file**)
**Status:** Ready for an Opus implementer
**Owner sign-off required on the Open Decisions (§7) before step 3 lands.**

---

## 1. Goal (sharpened, not relitigated)

Collapse Norn's runtime-assembly paths onto **one** library-owned assembler. Today there are four:

| # | Path | Entry point | Assembles via |
|---|------|-------------|---------------|
| A | Library / embedded | `norn::agent::AgentBuilder::build()` (+ `.load_runtime_base()`) | `AgentBuilder::build` → `agent/assembly.rs` + `runtime_init` |
| B | CLI print | `norn-cli` `runtime::build_runtime` → `RuntimeBundle` → `run_agent_step` | `runtime/builder.rs` (2857 L) + `runtime/wiring.rs` (758 L) |
| C | CLI jsonrpc driven | `print::driven::execute_driven` → same `build_runtime` → `orchestrate` | shares path B |
| D | CLI TUI | `tui/driver.rs::drive` → same `build_runtime` → `TuiInputs` → `norn_tui::run_app` | shares path B, then hand-wires coordination |

Path **A already is the assembler.** Paths B/C/D duplicate it against a parallel `RuntimeBundle` shape that never constructs an `Agent`. R1 makes B/C/D go through `AgentBuilder`:

- **`AgentBuilder` IS the assembler** — no new type.
- **`Agent::into_parts()`** hands custom drivers (the TUI's multi-turn REPL; the print step-loop) the assembled fields they need.
- **`builder_from_cli(cli, provider, profile, settings) -> Result<AgentBuilder, BuildError>`** — a thin (~150 L) setter-mapping in `norn-cli`.
- **Delete** `build_runtime`, `RuntimeBundle`/`RuntimeInputs`, and the `runtime/wiring.rs` coordination copies (~1650–1800 L gross from `norn-cli`).
- **Lock** `AgentBuilder::build`'s assembly helpers and `Agent`'s fields to `pub(crate)` so drift is structurally impossible.
- **Land the conformance test FIRST** — assert a CLI-assembled bundle and a library-assembled bundle are field-for-field equivalent for identical inputs; keep it green through the migration; retire the dual-path comparison only in the deletion step.

**Meridian** (`/Users/tom/Developer/ablative/meridian`, git dep pinned at `fde31ce`) is the reference consumer: it *already* assembles every one of its four production agent paths through `AgentBuilder::new(provider)…​.load_runtime_base()…​.build()`. R1 does not migrate Meridian's builder call sites (they are already correct); it makes five clusters of norn-internal logic that Meridian currently *copies* (§6) deletable, and it fixes latent bugs that bite Meridian **today** (embedded skill tool missing, session-lifecycle hooks never firing, provider-blind prompt — §5).

---

## 2. Re-verification against the post-Wave-1 tree

Wave 1 (`27df51d`) already collapsed several drifts the original design flagged. Verified against the current tree:

**Already unified by Wave 1 (do NOT re-add to R1 scope):**

- **Hook assembly** is one function. `norn::runtime_init::assemble_hook_registry(programmatic, &hook_settings, profile, cwd)` is called by BOTH `load_runtime_base` (`runtime_init/base.rs:205`) and CLI `build_runtime` (`runtime/builder.rs:196`). `load_hooks_from_settings(&NornSettings)` is infallible and takes `&NornSettings` (base.rs:204, builder.rs:194). H13 (programmatic-first merge) holds on both.
- **Workspace-root validation** is one function: `norn::agent::validate_workspace_root` — used by `AgentBuilder::build` (builder.rs:295) and `build_runtime` (builder.rs:240).
- **Permission policy, tool-output budget, runtime extensions, skill infra, context-search paths, agent handles** are all shared installers in `runtime_init/extensions.rs` (`install_permission_policy`, `install_tool_output_budget`, `install_runtime_extensions`, `install_skill_infra`, `install_context_search_paths`, `install_agent_handles`) called by both paths.
- **`register_standard_tools`** moved to `norn::tools::registry_builder` and is re-exported by `norn-cli` (`runtime/mod.rs:14`).
- **Session resume** yields `ReplayArtifacts` (single traversal): `restore_session_state` (assembly.rs:488) uses `crate::session::ReplayArtifacts::from_events`. The CLI's `install_action_log` (wiring.rs:610) uses `rebuild_action_log` over `store.events()` — same primitive.
- **Disallowed-tools / permission / skill-shell / ToolCategory** changes all landed and are read the same way on both paths.

**Still duplicated / still drifting (R1's real target):**

1. **`build_runtime` (builder.rs, ~590 code L excl. tests) reconstructs `LoopContext` + gated `ToolRegistry` by hand** via `build_loop_context` (builder.rs:567) instead of `from_profile` inside `AgentBuilder::build`. Parallel to `assembly.rs`'s `populate_loop_context` + `from_profile`.
2. **`apply_system_prompt` (builder.rs:390) is deliberately provider-blind** — its own doc says "Full parity lands when this assembly path converges onto `AgentBuilder` (Phase 3 R1)." `AgentBuilder::install_system_prompt` (assembly.rs:227) IS provider-aware (`reframe_prompt_entries(…, self.provider.capabilities())`, builder.rs:461-472). **Real drift.**
3. **Coordination wiring is entirely hand-rolled in the CLI**, duplicating `assembly.rs::install_agent_infra`:
   - `runtime/wiring.rs`: `cli_coordination_envelope` (:102), `install_agent_tool_infra` (:176), `install_pending_agent_messages_for_loop` (:240), `install_child_result_sender` (:257), `install_headless_reclamation` (:281), `install_shared_agent_event_channel` (:298), `run_session_start`/`run_session_end` (:315/:327).
   - `print/orchestrator.rs:344-394` and `tui/driver.rs:126-222` each re-run the envelope + child-result channel + root registration + infra install + event channel + session hooks by hand.
   `AgentBuilder::build` does all of this from `.agent_registry()` + `.child_policy()` + `.child_result_capacity()` + `.event_channel_capacity()` + `.inbound_capacity()` (builder.rs:519-565), publishing the identical extensions.
4. **The CLI reconstructs `iteration_monitor_from_profile` and `IterationMonitorSpec`** (wiring.rs:519-561) — a byte-for-byte copy of `runtime_init/base.rs:410-446`. `load_runtime_base` already produces the monitor; the CLI copy exists only because `build_runtime` does not call `load_runtime_base`.
5. **The CLI mints the diagnostic collector, task store, skill catalog, provider defaults independently** (`build_diagnostic_collector`, `build_shared_task_store`, `build_skill_search_paths`, `build_skill_catalog` in wiring.rs / builder.rs) — all of which `load_runtime_base` already produces on `LoadedRuntimeBase`.

**Confirmed NOT yet fixed by Wave 1 (bugs R1 must close — see §5):** embedded skill tool missing; session-lifecycle hooks never fire on the library/embedded path; CLI system prompt provider-blind; terminal-reclamation asymmetry between print and TUI cannot be expressed on `AgentBuilder`.

---

## 3. Current signatures (read from the tree — the migration's fixed points)

**`AgentBuilder` fields** (`agent/builder.rs:91-131`) — 40 `pub(super)` fields incl. `provider`, `profile`/`profile_name`/`model`, `system_prompt`/`append_system_prompt`, `reasoning_effort`/`service_tier`, `capabilities`, `working_dir`/`workspace_root`, `bash_drain_grace`, `allowed_tools`/`without_tools`, `extra_tools`, `lsp_backend`/`lsp_workspace`, `execution_mode`, `agent_config`, `retry_policy`, `session`/`session_request`, `event_channel_capacity`, `cancel`, `inbound_capacity`/`inbound`/`inbound_tx`, `agent_id`, `hooks`/`rules`/`diagnostics`/`diagnostic_infra`, `additional_post_checks`, `agent_registry`/`child_policy`/`child_result_capacity`, `extensions`, `load_runtime_base`, `task_group_slug`.

**`Agent` fields** (`agent/instance.rs:33-50`): `provider: Arc<dyn Provider>`, `registry: Arc<ToolRegistry>`, `loop_context: LoopContext`, `config: AgentLoopConfig`, `model: String`, `tool_defs: Vec<ToolDefinition>`, `event_store: Arc<EventStore>`, `event_sender: Option<AgentEventSender>`, `events_tx: Option<broadcast::Sender<AgentEvent>>`, `cancel: CancellationToken`, `inbound: Option<InboundChannel>`, `inbound_tx: Option<InboundSender>`, `id: Uuid`, `info: Arc<ResolvedAgentInfo>`, `session_entry: Option<SessionIndexEntry>`, `replay: Option<ReplaySummary>`.

**`TuiInputs`** (`crates/norn-tui/src/app/event_loop.rs:63-113`) needs: `provider: Arc<dyn Provider>`, `executor: Arc<dyn ToolExecutor>`, `store: Arc<EventStore>`, `registry: Arc<RwLock<AgentRegistry>>`, `loop_context: LoopContext`, `agent_config: AgentLoopConfig`, `model: String`, `tools: Vec<ToolDefinition>`, `history`, `status_bar`, `root_id: Uuid`, `initial_prompt`, `data_dir`, `session_id`, `root_event_sender: AgentEventSender`, `agent_event_rx: broadcast::Receiver<AgentEvent>`, `root_inbound: Option<InboundChannel>`.

**Mapping `TuiInputs` ← `Agent::into_parts()`** (the whole reason `into_parts` exists):

| `TuiInputs` field | Source from `AgentParts` |
|---|---|
| `provider` | `parts.provider` |
| `executor` | `parts.registry.clone() as Arc<dyn ToolExecutor>` |
| `store` | `parts.event_store` |
| `registry` (AgentRegistry) | **caller-held clone** of the `Arc<RwLock<AgentRegistry>>` it passed to `.agent_registry()` (not in `AgentParts` — see D2) |
| `loop_context` | `parts.loop_context` |
| `agent_config` | `parts.config` |
| `model` | `parts.model` |
| `tools` | `parts.tool_defs` |
| `root_id` | `parts.id` |
| `root_event_sender` | `parts.event_sender.expect(...)` (present because `.event_channel_capacity()` set) |
| `agent_event_rx` | `parts.events_tx.expect(...).subscribe()` |
| `root_inbound` | `parts.inbound` |
| `session_id`/`data_dir` | `parts.info.session_id` / caller's session data dir |

**Meridian's superset of setters actually used** (proves the builder surface is sufficient today): `working_dir`, `cancel_token`, `event_channel_capacity`, `inbound_capacity`, `agent_id`, `session`, `load_runtime_base`, `task_group_slug`, `profile`/`profile_name`/`model`, `reasoning_effort`, `allowed_tools`/`without_tools`, `capabilities`, `system_prompt`/`append_system_prompt`, `max_iterations`, `step_timeout`, `diagnostics`, `lsp_backend`/`lsp_workspace`, `output_schema`, `extension`, `inbound_sender()`. **Meridian wires no coordination** (`.agent_registry()` appears at zero call sites) — spawn/fork is a CLI-only feature today.

---

## 4. Requirements (foundation-first, deletion-last, five green commits)

Each step compiles and passes `cargo clippy --workspace --all-targets -- -D warnings` + `cargo fmt --check` + the full test suite independently.

### Step 1 — Foundation: `into_parts` + conformance harness (green commit 1)

**R1.1 — `Agent::into_parts()`**
*Files:* `crates/norn/src/agent/instance.rs`, `crates/norn/src/agent/mod.rs`.
Add:
```rust
/// Every assembled field of a built agent, for custom drivers (the TUI's
/// multi-turn REPL, the print step-loop) that run the agent-step loop
/// themselves instead of calling `Agent::run`.
pub struct AgentParts {
    pub provider: Arc<dyn Provider>,
    pub registry: Arc<ToolRegistry>,
    pub loop_context: LoopContext,
    pub config: AgentLoopConfig,
    pub model: String,
    pub tool_defs: Vec<ToolDefinition>,
    pub event_store: Arc<EventStore>,
    pub event_sender: Option<AgentEventSender>,
    pub events_tx: Option<tokio::sync::broadcast::Sender<AgentEvent>>,
    pub cancel: CancellationToken,
    pub inbound: Option<InboundChannel>,
    pub inbound_tx: Option<InboundSender>,
    pub id: Uuid,
    pub info: Arc<ResolvedAgentInfo>,
    pub session_entry: Option<SessionIndexEntry>,
    pub replay: Option<ReplaySummary>,
}

impl Agent {
    /// Decompose the agent into its assembled fields. Consumes the agent;
    /// mutually exclusive with `run`. The returned `event_store` is the
    /// same `Arc` the loop persists into; `registry` is the same `Arc`
    /// spawn/fork children dispatch through.
    #[must_use]
    pub fn into_parts(self) -> AgentParts { /* move every field */ }
}
```
Re-export `AgentParts` from `agent/mod.rs`.
*Deletes:* nothing.
*Acceptance:* `into_parts_returns_same_arcs` — `AgentBuilder::build()` then `into_parts()`; assert `Arc::ptr_eq(&parts.event_store, &<store held before>)`, `parts.id == builder-supplied agent_id`, `parts.events_tx.is_some()` iff `.event_channel_capacity()` was set, `parts.info.session_id` non-empty.

**R1.2 — Conformance harness (the safety net, landed FIRST)**
*Files:* new `crates/norn-cli/tests/assembly_conformance.rs`.
Assert that, for a fixed CLI invocation and the equivalent library inputs, the two assembly paths produce field-equivalent bundles. Compare, on the same `Cli` (e.g. `["norn","-m","gpt-5.5"]` under an isolated `NORN_HOME`):
- **Tool set:** sorted `registry.names()` equal.
- **System prompt:** `loop_context.system_sections[0]` equal (this will initially **differ** on hosted-tool framing — assert equal for a non-hosted provider so the test is green now, and widen to a hosted provider in step 3 once the CLI prompt becomes provider-aware; document the deferred assertion inline).
- **Agent-loop config:** `agent_config` equal (serde round-trip).
- **Extensions on the shared `ToolContext`:** presence of `PermissionPolicy`, `SharedTaskStore`, `SharedToolCatalog`, `DiagnosticInfra`, `HookRegistry` (when configured) — same set.
- **Retry policy, reasoning effort, service tier, iteration monitor** on `loop_context` equal.
The library side of the comparison calls `AgentBuilder::new(mock_provider).profile(cli_resolved_profile).working_dir(cwd).load_runtime_base().build()`; the CLI side calls `build_runtime` (still present until step 5).
*Acceptance:* the test compiles and passes at step 1 (it is the regression fence for steps 2–4).

### Step 2 — `builder_from_cli` + missing setters, coexisting with `build_runtime` (green commit 2)

**R1.3 — New `AgentBuilder` setters for CLI/embedder parity**
*Files:* `crates/norn/src/agent/builder_setters.rs`, `builder.rs` (fields + `build` consumption), `assembly.rs` (install points).
Add (each `#[must_use]`, each documented as no-assumed-default):
- `pub fn event_schemas(mut self, schemas: EventSchemaSet) -> Self` → `loop_context.event_schemas` in `build`. (`norn::agent_loop::event_schemas::EventSchemaSet`.)
- `pub fn variables(mut self, variables: Arc<VariableStore>) -> Self` → overrides the variable store `populate_loop_context` would mint; when set, its `session_id` must still reconcile with `open_session`/minted id (assert, do not silently diverge).
- `pub fn disallowed_tools(mut self, names: &[&str]) -> Self` → stored; `build` calls `registry.set_disallowed(...)` after `from_profile` gating (deny-wins, mirroring `build_runtime` builder.rs:250).
- `pub fn terminal_reclamation(mut self, enabled: bool) -> Self` → gates the unconditional `install_terminal_reclamation` inside `install_agent_infra` (assembly.rs:767). **Default when coordination is wired is an Open Decision (D3)** — do not pick a default; require the setter when `.agent_registry()` is set, or default `true`, per owner sign-off.
- **(decision-gated, D2)** `pub fn register_root(mut self, path: String, role: String) -> Self` → when set alongside `.agent_registry()`, `build` performs the `AgentRegistry::reserve(...).confirm()` the CLI does by hand (`orchestrator.rs:571`, `driver.rs:382`).

*Deletes:* nothing.

**R1.4 — `builder_from_cli`**
*Files:* new `crates/norn-cli/src/runtime/from_cli.rs`; re-export from `runtime/mod.rs`.
```rust
/// Map a resolved CLI invocation onto an `AgentBuilder`. Provider
/// selection, model-alias / provider-profile resolution, and profile
/// resolution + CLI overrides have already happened in the caller (they
/// are CLI config surface, not assembly); this function only translates
/// the resolved state into builder setter calls.
pub fn builder_from_cli(
    cli: &Cli,
    provider: Arc<dyn Provider>,
    profile: Profile,     // already carries CLI model/tool/reasoning overrides
    settings: &NornSettings,
) -> Result<AgentBuilder, BuildError>
```
Body (~150 L), in order:
1. `AgentBuilder::new(provider).profile(profile).working_dir(cwd).load_runtime_base()` — `apply_working_dir(cli)` runs in the caller (it mutates process CWD) exactly as `build_runtime` does today (builder.rs:88).
2. `.task_group_slug(session_slug)` when `--session-name` set (replaces `build_shared_task_store`'s slug derivation).
3. Tool gating: `.allowed_tools(&allowed)` / `.disallowed_tools(&disallowed)` from `AppliedOverrides` (`apply_cli_profile_overrides` already run by caller).
4. Agent-loop config: build `AgentLoopConfig` via the existing typed helpers (`apply_config_overrides_to_loop`, `apply_loop_config_overrides`) and pass `.agent_config(config)`. **Note the merge interaction:** `load_runtime_base` also derives an agent config from settings and `effective_agent_config`/`merge_agent_config` overlays the explicit one — the conformance test (R1.2) is the fence that this overlay reproduces `build_runtime`'s result.
5. `.event_schemas(merge_event_schemas(&profile, &cli.event_schema)?)`, `.variable_pairs(parse_kv-parsed --variables pairs)` (build() applies them to the store it mints with the resolved session id; the earlier `.variables(build_variable_store(...))` shape was removed in Wave 6 — it minted a store whose random session id build() rejects under open_session).
6. Session: translate `--no-session`/`--resume`/`--fork`/`--session-id`/`--resume-if-exists`/`--session-name` into either `.session(EventStore::new())` (`--no-session`) or `.open_session(SessionManager::new(session_data_dir()), spec, DurabilityPolicy::Flush)` where `spec` is the matching `SessionSpec` (`session_spec.rs:20`). **Adoption of `.open_session` is Open Decision D4.**
7. Coordination (CLI only): `.agent_registry(reg)` + `.child_policy(env.child_policy)` + `.child_result_capacity(env.child_result_capacity)` from `cli_coordination_envelope()`, `.event_channel_capacity(N)`, `.inbound_capacity(env.child_policy.inbound_capacity)`, `.register_root("/root","lead")` (D2), `.terminal_reclamation(false for TUI / true for print)` (D3).
8. `.execution_mode(mode)` and `.output_schema(schema)` when `-s` present.
9. LSP handles (`RuntimeInputs.lsp_workspace`/`lsp_backend`) → `.lsp_workspace` / `.lsp_backend`.
*Deletes:* nothing.
*Acceptance:* extend R1.2 to compare `builder_from_cli(...).build()` against `build_runtime(...)` for `-m`, `--allowed-tools`, `--disallowed-tools`, `-c max_turns=`, `--reasoning-effort`, `--event-schema`, `--variables`, `--session-name`. Every field asserted in R1.2 stays equal.

### Step 3 — Migrate print + jsonrpc-driven onto `builder_from_cli` (green commit 3)

**R1.5 — Print orchestrator + driven path use `builder_from_cli` → `Agent`**
*Files:* `crates/norn-cli/src/print/orchestrator.rs`, `print/driven.rs`, `print/session.rs`.
- `execute` (orchestrator.rs:185) and `execute_driven` (driven.rs) build the provider, resolve profile+overrides, then call `builder_from_cli(...)`, take `.inbound_sender()`/`.handle()` as needed, `build()`, and `into_parts()`. The step-loop in `orchestrate` (orchestrator.rs:242) runs against `AgentParts` (it must keep slash interception, `/compact`, driven `intervene/*`, and stream rendering — so it uses `into_parts()` + `run_agent_step`, **not** `Agent::run`).
- **Delete** the hand-wiring in `orchestrate` (orchestrator.rs:344-405): `cli_coordination_envelope()`, the child-result channel, `register_root_agent` (orchestrator.rs:571), `install_agent_tool_infra`, `install_pending_agent_messages_for_loop`, `install_child_result_sender`, `install_headless_reclamation`, `install_shared_agent_event_channel`, the manual `AgentEventSender::new`. These are now builder outputs on `AgentParts`.
- `run_session_start`/`run_session_end` (orchestrator.rs:390/550): **superseded by R1.7** (Agent-fired session hooks) — see D1. Until D1 is signed off, keep the explicit calls but feed them `parts.info.session_id` (never the empty string).
- The provider-aware prompt now flows automatically; **widen the R1.2 system-prompt assertion to a hosted-search provider** and assert the CLI prompt reframes `web_search` as provider-native (closes bug §5.4).
*Deletes:* ~300–400 net L from `print/`.
*Acceptance:* existing `crates/norn-cli/tests/jsonrpc_driven_mode.rs` stays green; add `print_agent_has_skill_tool_when_catalog_present` (closes §5.2) and `print_prompt_is_provider_aware` (closes §5.4).

### Step 4 — Migrate the TUI driver onto `builder_from_cli` → `into_parts` (green commit 4)

**R1.6 — TUI driver uses `builder_from_cli` → `Agent::into_parts()` → `TuiInputs`**
*Files:* `crates/norn-cli/src/tui/driver.rs`.
- `drive` (driver.rs:71) constructs the `AgentRegistry`, keeps one clone for `TuiInputs.registry`, passes the other to `builder_from_cli(...).agent_registry(reg).register_root("/root","lead").terminal_reclamation(false)…`, `build()`, `into_parts()`, and maps `AgentParts` → `TuiInputs` per the §3 table.
- **Delete** driver.rs:126-222 hand-wiring: `register_root_agent` (driver.rs:382), `cli_coordination_envelope`, the child-result channel, `install_agent_tool_infra`, `install_pending_agent_messages_for_loop`, `install_child_result_sender`, the manual broadcast channel + `AgentEventSender`, `install_shared_agent_event_channel`. The TUI **must not** get terminal reclamation (its status panel owns it) → `.terminal_reclamation(false)` (D3).
*Deletes:* ~250 net L from `tui/`.
*Acceptance:* the TUI still builds and `open_session`/rotation tests (driver.rs tests) stay green; add `tui_parts_carry_root_inbound_and_event_channel` asserting `parts.inbound.is_some()` and `parts.events_tx.is_some()`.

### Step 5 — Delete the old path; lock internals (green commit 5, deletion-last)

**R1.7 — Session-lifecycle hooks fire on the unified path** *(decision D1)*
*Files:* `crates/norn/src/agent/instance.rs` (or the runner), `crates/norn-cli/src/runtime/wiring.rs`.
Per D1 sign-off, either (a) `Agent::run` fires `on_session_start`/`on_session_end` with `self.info.session_id`, and `AgentParts` exposes `fire_session_start`/`fire_session_end` helpers for `into_parts` drivers; or (b) a documented explicit-fire contract. Remove the CLI's `run_session_start`/`run_session_end` copies once the library fires them. **Closes §5.1 (empty ids) and §5.3 (never firing embedded).**

**R1.8 — Skill tool registered on every assembly path** *(decision D5)*
*Files:* `crates/norn/src/agent/assembly.rs` (or `runtime_init`), `crates/norn/src/tools/registry_builder.rs`.
Register `SkillTool::with_config(skill_tool_config_from_settings(&settings))` inside the library assembly whenever skill infra is installed (i.e. the `load_runtime_base` extension path, `install_runtime_base_extensions` assembly.rs:175), matching the CLI's `if !skill_catalog.is_empty()` gate (builder.rs:163-173). **Closes §5.2 — a latent bug affecting Meridian today.**

**R1.9 — Delete `build_runtime` / `RuntimeBundle` / wiring copies; lock internals**
*Files:* delete/gut `crates/norn-cli/src/runtime/builder.rs` (`build_runtime`, `build_loop_context`, `apply_system_prompt`, `merge_discovered_rules` copy, `build_shared_task_store`, `warn_unmatched_tool_flag_names` if unused post-migration, `IterationMonitorSpec` copy), `crates/norn-cli/src/runtime/bundle.rs` (whole file), and from `crates/norn-cli/src/runtime/wiring.rs` remove `cli_coordination_envelope`, `install_agent_tool_infra`, `install_pending_agent_messages_for_loop`, `install_child_result_sender`, `install_headless_reclamation`, `install_shared_agent_event_channel`, `run_session_start`/`run_session_end`, `iteration_monitor_from_profile` + `IterationMonitorSpec`, `build_diagnostic_collector`. Keep the genuinely CLI-only helpers that map to builder inputs (`build_write_tool`/`length_limit_from_profile`, `build_slash_state_*`).
- **Lock internals:** the `agent/assembly.rs` phase helpers stay `pub(crate)` (they already are); confirm no `pub` leaks. `Agent`'s fields stay `pub(super)`. Remove the now-unused `pub use` re-exports from `runtime/mod.rs`.
- **Retire the dual-path comparison** in R1.2 (nothing to compare against once `build_runtime` is gone); convert it to assert `builder_from_cli(...).build()` against a committed golden snapshot of the assembled fields.
*Deletes:* ~1650–1800 gross L from `norn-cli`.
*Acceptance:* workspace builds with `build_runtime`/`RuntimeBundle` gone; full suite green; grep proves zero remaining references to the deleted symbols.

**Rough per-step LOC delta** (excl. tests): S1 `+~90` (into_parts) `+~200` (test); S2 `+~150` (from_cli) `+~120` (setters); S3 `−~350`; S4 `−~230`; S5 `−~1550` code + `−~700` dead tests, `+~120` (skill/hook wiring). **Net across both crates ≈ −1200 to −1350 L.**

---

## 5. Wiring bugs — fixed-for-free vs needs-explicit-work (verified against current tree)

| # | Bug | Current-tree status | R1 treatment |
|---|-----|---------------------|--------------|
| 5.1 | Session hooks fire with empty ids (`session_id.as_deref().unwrap_or("")`, orchestrator.rs:392, driver.rs:215) | Real. `populate_loop_context` (assembly.rs:445) always mints a non-empty id; `info.session_id` is always populated. | **Explicit (R1.7):** fire with `info.session_id`. Free once the source is `info` not the CLI's `Option<String>`. |
| 5.2 | Embedded skill **tool** missing | **Real, confirmed.** `norn::tools::registry_builder::register_standard_tools` does NOT register `SkillTool` (registry_builder.rs:65-97); only CLI `build_runtime` does (builder.rs:163). `AgentBuilder`-assembled agents (Meridian, all library callers) get skill *infra* published but no skill *tool*. | **Explicit (R1.8).** High-value — Meridian's four agent paths cannot invoke skills today. |
| 5.3 | Session-lifecycle hooks never fire on library/embedded path | **Real, confirmed.** The loop/runner and `Agent::run` (instance.rs:124) never call `run_session_start`/`on_session_end`; only the CLI drivers do (`norn-tui` fires none). Meridian's `agent.run(...)` therefore fires no session hooks. | **Explicit (R1.7)** + D1 sign-off. |
| 5.4 | CLI provider-blind system prompt | **Real, confirmed.** `apply_system_prompt` (builder.rs:390) has no `reframe_prompt_entries`; its doc defers parity to "Phase 3 R1". `AgentBuilder` is already provider-aware. | **Free in R1.5** — deleting `apply_system_prompt` and routing through `AgentBuilder::install_system_prompt` fixes it; widen R1.2 assertion to prove it. |
| 5.5 | Coordination-wiring asymmetry (print installs headless reclamation, TUI must not; both re-register root, re-mint envelope) | **Real.** `install_agent_infra` (assembly.rs:737) installs `install_terminal_reclamation` **unconditionally** — so a TUI using `AgentBuilder` would wrongly get reclamation. Root reservation is CLI-only and not done by `build`. | **Explicit:** R1.3 `.terminal_reclamation(bool)` (D3) + `.register_root()` (D2). Not free — the builder cannot express the two CLI variants today. |

---

## 6. Meridian: what each call site becomes, and what deletes

Meridian already funnels every path through `AgentBuilder…load_runtime_base().build()`. R1 does not rewrite these; it (a) fixes §5.2/§5.3 which they depend on, and (b) makes the copied clusters §A–§E deletable by exposing library-owned equivalents. Migrating Meridian is a **follow-up Meridian-side change (D6 scope)**, not part of this norn brief.

**Call sites (unchanged builder shape after R1):**
- **CS1 — Aion activity** `meridian-aion/src/activities/agent.rs:182` (`handle_norn_activity`): `.profile_name().working_dir().agent_id().session().event_channel_capacity(256).load_runtime_base()` + optional `.allowed_tools()`/`.output_schema()`. After R1: **gains the skill tool (§5.2) and session hooks (§5.3) for free**; `.session(NornSessionStore-built EventStore)` can become `.open_session(SessionManager::new(custom_root), spec, Flush)` once §A/§B migrate.
- **CS2 — Assistant session** `meridian-services/src/assistant/service.rs:1697` + `configure_norn_builder:246` + loop in `assistant/norn_session.rs:117`: single `agent.run(initial_prompt)` with `InboundSender` steering. After R1: session-lifecycle hooks fire around `run` (§5.3) — Meridian's `finalize_norn_session` (norn_session.rs:435) can stop hand-firing if it adopts the library contract.
- **CS3 — Rhai workflow step** `meridian-services/src/workflow/imperative_callbacks/norn_step.rs:316` (resume) and `:594` (fresh): `.task_group_slug("workflow").diagnostics().lsp_backend()/lsp_workspace().step_timeout()`. Unchanged; **gains skill tool + session hooks**.
- **CS4 — VM dispatch** `meridian-exchange/src/workspace/dispatch.rs:86` (`assemble_vm_norn_session`) finishing a factory-built builder + `EventStore::with_sink(JsonlSink)`. Unchanged.

**Meridian-side copies that R1 makes deletable (follow-up, not this brief):**
- **§A `NornSessionStore`** (`assistant/norn_session_store.rs`, 527 L) — reimplements New/Resume/Fork over `EventStore::with_sink` at a custom root. Deletable via `.open_session(SessionManager::new(custom_dir), SessionSpec::{Create|Resume|Fork|OpenOrResume}, Flush)` — `SessionManager::new(&data_dir)` already accepts a custom root (verified `builder_setters.rs:300`). Requires R1 to keep `.open_session` as the one session front door.
- **§B Workflow session-index copy** (`norn_step.rs:92,144`) — hand-builds `SessionIndexEntry` + `append_index_entry`/`resolve_session`. Same `.open_session` migration.
- **§C Capability-dir scan** (`service.rs:193 norn_capability_dirs`, `:204 resolve_norn_capabilities`) — copies norn capability discovery. Deletable only if R1 (or a follow-up) exposes a library capability-discovery helper; **out of R1 scope unless owner adds it (flag under D6).**
- **§D Provider-default copy** (`workflow/provider.rs:22 REQUEST_TIMEOUT=2m`, `MAX_RETRIES=2`, "Match the production default used by norn-cli (NC-003)") — copies CLI provider defaults. R1 does not touch provider construction (it is caller surface); if these should be shared, expose them from `runtime_init` (flag under D6).
- **§E Default-profile copy** (`configure_norn_builder:255 default_norn_profile()`) — copies the CLI "supply a default profile when none set" policy, because the library deliberately has no implicit model. This is intentional caller policy; R1 does not remove it.

---

## 7. Open Decisions — DO NOT GUESS; owner sign-off before step 3

- **D1 — Session-hook ownership.** Does `Agent::run` fire `on_session_start`/`on_session_end` itself (with `info.session_id`), and do `into_parts` drivers get explicit `fire_session_start`/`fire_session_end` helpers on `AgentParts`? This determines whether Meridian's `agent.run()` gets session hooks automatically and whether the TUI (multi-turn, no `Agent::run`) fires them explicitly. **Affects Meridian's session loop and `finalize_norn_session`.** Blocks R1.5/R1.7.
- **D2 — Root registry registration.** Is reserving the `AgentRegistry` "/root" entry mandatory when `.agent_registry()` is wired, and should `build()` do it via `.register_root(path, role)`? The TUI status panel needs a depth-0 root; Meridian wires no coordination and needs none. Keep it caller-side, or fold into `build`? Blocks R1.3/R1.6.
- **D3 — Terminal-reclamation control.** `install_agent_infra` installs `install_terminal_reclamation` unconditionally. Add `.terminal_reclamation(bool)`; what is the default when coordination is wired — `true` (headless), or required-explicit (no-assumed-default)? The TUI needs `false`. Blocks R1.3.
- **D4 — Session front door for the CLI.** Adopt `.open_session(SessionManager, SessionSpec, DurabilityPolicy)` at build time (opens during `build()`), replacing the CLI's post-build `open_session` + `.session(store)`? The print path currently opens after build (to set `debug_dump_file` from the resolved id and `cache_key`); `.open_session` already wires `cache_key = session_id` and surfaces `info.session_id`. Confirm the ordering is preserved, and how `--no-session` (no `SessionSpec` variant) maps (`.session(EventStore::new())`). Blocks R1.4 step 6.
- **D5 — Skill-tool registration gate.** Register `SkillTool` always in `register_standard_tools`, or only on the `load_runtime_base` path where the catalog + `SkillToolConfig` exist (matching the CLI's `!catalog.is_empty()` gate)? Library agents built without `load_runtime_base` have no catalog. Blocks R1.8.
- **D6 — Meridian migration scope.** Is deleting §A/§B (session store + index copies) in scope for this campaign or a separate Meridian PR? Are §C (capability discovery) and §D (shared provider defaults) library APIs the owner wants exposed now, or left as intentional Meridian copies? Determines whether R1 adds those library surfaces.
- **D7 — Library surface for `event_schemas` / `variables`.** These are today CLI-only `LoopContext` concepts. Adding `.event_schemas()` / `.variables()` to `AgentBuilder` expands the public library surface for features Meridian does not use. Confirm they belong on the builder vs. staying a CLI-side `into_parts` post-mutation.

---

## 8. Sequencing invariant

`R1.1/R1.2 (foundation + fence) → R1.3/R1.4 (add, coexist) → R1.5 (print/driven) → R1.6 (TUI) → R1.7/R1.8/R1.9 (fire hooks, skill tool, delete + lock)`. The conformance fence (R1.2) is green at every step until R1.9 retires the dual-path comparison. No step deletes `build_runtime` until every consumer has moved off it.
