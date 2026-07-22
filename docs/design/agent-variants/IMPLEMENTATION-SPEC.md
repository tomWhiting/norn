# agent-variants — implementation spec (2026-07-07)

Derived from `docs/agent-variants-and-child-persistence-briefs.md` (brief
`agent-variants`, R1–R6) against the code as of main `39bf32b`. The
child-persistence brief has LANDED — path addresses (`BranchedChild.path_address`),
`slugify_name_stem`, and `mint_child_name` are real and used below.

House law applies in full: no invented defaults, no compat shims (model/role
arg changes REPLACE the old schema), no `#[allow]` outside `#[cfg(test)]`,
files < 500 LOC (split modules rather than squeeze), thiserror in the library,
every failure typed and loud.

## Ground-truth map (verified 2026-07-07)

- Config: `NornSettings` all-`Option` sections; keyed sections are
  `BTreeMap<String, T>` merged wholesale-by-name later-layer-wins
  (`merge_mcp_servers`, `config/merge/collection_sections.rs:117-136`).
  Validation: `validate_settings` sequence in `config/validate.rs`,
  `ConfigError::InvalidConfig`.
- Merged settings do NOT reach `ToolContext`; derived projections are
  installed as extensions at assembly (`install_permission_policy`,
  `runtime_init/extensions.rs:87`).
- Spawn (`tools/agent/spawn.rs`): `SpawnAgentArgs { task, model: String
  (required), role: String (required), profile, tools, path, output_schema,
  child_policy }`. Child system prompt today: profile instructions, or the
  literal `"You are a sub-agent. Task: {task}\n\nComplete the task and stop."`
- Fork (`tools/agent/fork_tool.rs` + `fork_context.rs`/`fork_launch.rs`/
  `fork_seed.rs`): static `FORK_SYSTEM_PREAMBLE` (`agent/fork.rs:139`) +
  `ParentSystemInstruction` extension that NOTHING publishes (consumer
  `fork_tool.rs:425`, forwarder `fork_context.rs:161-163`, zero production
  `::new` sites — verified dead source).
- `ChildPolicy` (`agent/child_policy.rs`): `grant_for_child` narrows;
  gating is call-rejection only. NO registry-level filtering exists. The only
  assembly-level strip today is `signal_agent` when `messaging == None`
  (spawn.rs:337-342, fork_tool.rs:471-475).
- Children share the parent's `Arc<ToolRegistry>`; subsetting = the
  `allow_list: Option<Vec<String>>` fed to both `SubAgentExecutor::new` and
  `provider::surface::collect_function_definitions`.
- Child context window: `arm_auto_compaction` fills from
  `smallest_context_window_for_model`; `validate_context_window` runs ONLY on
  the root build path. Comments at `arming.rs:166-170` and
  `spawn_launch.rs:379-383` explicitly defer child-path validation to THIS
  unit — close that gap.
- Standard tool set (`tools/registry_builder.rs`): read, write, edit, bash,
  apply_patch, search, lsp, task, tool_search, action_log, web_fetch,
  web_search, spawn_agent, fork, signal_agent, wake_agent, close_agent,
  agents (+ cron/process on builder paths).

## 1. Config surface (R1)

`NornSettings` gains (after `tools`, before `mcp_servers` — update the field-
order comment and any snapshot tests):

```rust
/// Agent variant definitions keyed by variant name. See `VariantSettings`.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub variants: Option<BTreeMap<String, VariantSettings>>,
```

```rust
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VariantSettings {
    pub description: Option<String>,
    /// Inline prompt text. Mutually exclusive with `prompt_file`.
    pub prompt: Option<String>,
    /// Path to a prompt file (UTF-8). Relative paths resolve against the
    /// agent's working directory (the working_dir the builder threads to
    /// the catalog build), NOT the process working directory. Whether
    /// user-level settings should anchor elsewhere is an open owner item.
    /// Mutually exclusive with `prompt`.
    pub prompt_file: Option<String>,
    /// Tool-name allowlist. Absent = inherit the parent's registry surface.
    pub tools: Option<Vec<String>>,
    /// Model id. Absent = inherit the parent's model — UNLESS
    /// `model_required` is true, in which case absence is a typed error at
    /// spawn time (the reviewer ruling). Never a hardcoded fallback.
    pub model: Option<String>,
    /// When true, spawning this variant without a model (from this field or
    /// an explicit spawn-time `model`) is a typed error naming the missing
    /// config key. Ships true on the built-in `reviewer` only.
    pub model_required: Option<bool>,
    /// Reasoning effort: one of the `provider::request::ReasoningEffort`
    /// serde names ("none","low","medium","high","xhigh","max"). Validated at
    /// config-validate time, parsed to the typed enum at catalog build.
    pub reasoning_effort: Option<String>,
}
```

(All fields `#[serde(default, skip_serializing_if = "Option::is_none")]` per
house pattern.)

**Merge (layers):** `merge_variants` in `collection_sections.rs`, exact copy
of the `merge_mcp_servers` shape — wholesale by-name, later layer wins.
Register in `merge/settings.rs`. This is D3 verbatim.

**Built-ins are NOT a merge layer.** They overlay per-FIELD at catalog build:
`resolved = configured.field.or(builtin.field)` for each field. Rationale
(recorded): the reviewer ruling's own wording — "spawning `reviewer` without
`variants.reviewer.model` configured (settings)" — requires that a user
setting ONLY `variants.reviewer.model` produces a working reviewer. Wholesale
replace at the built-in boundary would silently discard the built-in
adversarial prompt the moment a user sets the model key, which is exactly the
silent-loss failure the house rules forbid. Cross-layer semantics stay
wholesale per the brief; the built-in boundary is field-fallback.

**Validation (`validate_variants`):** name non-empty/clean
(`check_nonempty_clean`); `prompt` XOR `prompt_file` (both set = error naming
the variant); `model` if present non-empty; `tools` entries non-empty;
`reasoning_effort` if present must parse as a `ReasoningEffort` serde name
(match the strings; error lists the valid set).

## 2. Built-in variants (R2)

Module `crates/norn/src/agent/variants/` (mod.rs = declarations only):
- `catalog.rs` — `VariantCatalog`, `ResolvedVariant`, `VariantCatalogError`
  (thiserror), catalog build (overlay + prompt-file loading + effort parse).
- `builtin.rs` — the three built-in definitions as data
  (`fn builtin_variants() -> &'static [...]` or const structs), prompts via
  `include_str!("prompts/{explorer,reviewer,implementer}.md")`.
- `prompts/*.md` — the prompt texts (drafted per FLEET-PLAYBOOK doctrine;
  Fable reviewer will judge them against it).

Built-in data:
- `explorer`: tools `[read, search, lsp, tool_search, action_log, agents,
  web_fetch, web_search]`; model None (inherit); prompt: wide-search,
  read-only, report-structure guidance.
- `reviewer`: tools `[read, search, lsp, tool_search, action_log]`;
  `model_required: true`, model None — NO default, NO inherit (ruled);
  prompt: adversarial review per FLEET-PLAYBOOK (brief + intent + diff,
  nothing deferred, patient-records standard).
- `implementer`: tools None (full parent registry); model None (inherit);
  prompt: complete-and-verify-before-done guidance.

`ResolvedVariant { name: String, description: Option<String>,
prompt: Option<String> /* loaded text */, tools: Option<Vec<String>>,
model: Option<String>, model_required: bool,
reasoning_effort: Option<ReasoningEffort> }`

`VariantCatalog { variants: BTreeMap<String, ResolvedVariant> }` with
`get(name) -> Option<&ResolvedVariant>` and `names() -> impl Iterator` (for
the unknown-variant error listing). Built once at assembly:
`VariantCatalog::build(settings.variants.as_ref(), cwd) -> Result<Self, VariantCatalogError>`
— reads `prompt_file`s eagerly (fail loud at startup, not at spawn time).
The settings trust boundary rejects `prompt_file` in both working-directory
settings layers before this build step; inline repository prompts remain
supported, while eager prompt-file reads require trusted user configuration.

**Install:** `install_variant_catalog(ctx, &settings, cwd)` in
`runtime_init/extensions.rs` (PermissionPolicy pattern) publishing
`Arc<VariantCatalog>`. Forward the extension to children in
`spawn_context.rs` and `fork_context.rs` (grandchildren spawn variants too).
The catalog installs ONLY via the runtime-base seam
(`install_runtime_base_extensions`, `agent/assembly.rs` — the one
production call site, reached by the CLI and by any builder that sets
`load_runtime_base`). Embedded builders WITHOUT `load_runtime_base` get no
catalog: variant use there — built-ins included — fails with the typed
no-catalog error. Decision recorded 2026-07-07; revisit if an embedded
consumer needs built-ins without the runtime base.

## 3. Spawn honours variants (R3) — `spawn.rs`

Schema changes (REPLACE, no compat):
- `variant: Option<String>` — new.
- `model: String` → `Option<String>`.
- `role: String` → `Option<String>`.
- `required` drops to `["task"]`; schema docs updated; guidance
  markdown (`guidance/spawn_agent.description.md`) updated to teach variants.

Resolution (typed failures via `ToolError`, messages name exact config keys):
1. `variant` and `profile` both set → error (mutually exclusive surfaces).
2. Variant lookup in `VariantCatalog`; unknown → error listing available
   names (builtin ∪ configured, sorted).
3. `role` = `args.role` → else variant name → else error ("role or variant
   required").
4. `model` = `args.model` → else `variant.model` → else if
   `variant.model_required` → error: `variant '<name>' requires a model: set
   variants.<name>.model or pass model explicitly` → else PARENT'S model.
   Parent model must come from runtime ground truth — the agent registry
   entry for `infra.agent_id`, or (if the root turns out not to carry a model
   there — verify, don't assume) a small `AgentModel(String)` extension
   published at every assembly site from the actual launch model. NEVER from
   re-read settings, NEVER a literal. If no source exists → typed error.
   Same rule with no variant at all: `args.model` else parent model.
5. Prompt plan: every child receives compiled child-agent policy at System
   authority and the task separately at User authority. A built-in variant
   prompt is an additional System fragment; a configured variant prompt is an
   additional User fragment. On the profile path, trusted operator-profile
   guidance is Developer and workspace-profile guidance is User. No variant or
   profile leaves only the compiled child policy in the stable plan.
6. `reasoning_effort`: thread `variant.reasoning_effort` into the child
   launch → the child loop's provider requests. **Owner ruling 2026-07-07
   ("reasoning effort inherited"):** resolution is variant effort → (a
   profile-set effort, on the profile path) → the PARENT's ACTIVE effort →
   None. The parent's active effort is read from the same live per-step
   {model, reasoning_effort} stamp (`AgentModel` extension, refreshed at
   every step start by `run_agent_step` from the step's actual request
   inputs) that parent-model inheritance reads — one extension, so model
   and effort can never be observed out of sync. Never a re-read of
   settings, never an invented literal; a parent with no effort passes
   None through unchanged. Forks inherit the forker's active effort the
   same way (they have no variant/effort surface of their own).
7. Descriptor disclosure: `role` = resolved role, `model` = resolved model,
   `profile` = `Some(variant_name)` when variant used, else `args.profile`.
   `name_stem` = `slugify_name_stem(&variant_or_role_label, "spawn")` so the
   R4 child name carries the variant.

## 4. Effective tools = allowlist ∩ policy (R6)

New helper (in `tools/agent/delegation.rs` beside `grant_child_policy`):

```rust
pub(crate) fn effective_child_tools(
    parent_registry: &ToolRegistry,
    base_allowlist: Option<Vec<String>>,   // args.tools ∨ variant.tools ∨ profile tools
    granted: &ChildPolicy,
) -> Option<Vec<String>>
```

Rules: start from `base_allowlist` (None = full registry surface — in that
case materialise the registry names so subtraction is explicit, matching how
the signal_agent strip already materialises); remove `signal_agent` when
`granted.messaging == MessagingScope::None` (centralise the two existing
copies); remove `spawn_agent` AND `fork` when
`granted.delegation.remaining_depth == 0` (a leaf must not SEE delegation
tools — assembly-level, not call-rejection). Result feeds BOTH
`SubAgentExecutor::new` and `collect_function_definitions` at every child
assembly site (spawn, fork, rhai). The call-rejection path stays as
defence-in-depth (registry re-validation unchanged).

## 5. Fork identity enrichment (R4) — `agent/fork.rs` + `fork_tool.rs`

Replace the static-const-only composition with a structured preamble builder:

```rust
pub struct ForkIdentity<'a> {
    pub parent_agent_id: &'a str,
    pub path_address: &'a str,          // from BranchedChild.path_address
    pub requirement_slugs: &'a [String],
    pub granted: &'a ChildPolicy,
}
pub fn build_fork_preamble(identity: &ForkIdentity<'_>) -> String
```

Content: the existing preamble intent (you are a fork, split at this point,
complete requirements, stay focused) PLUS: who forked you (parent agent id),
your position (path address), your requirements contract (the slugs, which
are already schema-forced), and your delegation rights — remaining depth,
max concurrent children, messaging scope — because "a limit an agent doesn't
know about is an assassination" (brief R4 verbatim; tell the child its
budget). D8 installs the built preamble as a `ForkAgentPolicy` System fragment
in the inherited `ParentPromptPlan`; `combine_system_instruction` remains only
as a legacy embedder helper. Golden-file test: render with fixed inputs, `assert_eq!` against
`testdata/fork_preamble.golden.md` checked into the repo (no snapshot crate;
plain file + assert).

## 6. Wire ParentPromptPlan (R5 — D8 completion)

The live root/spawn/fork path publishes a source-aware `ParentPromptPlan` that
preserves every inherited System/Developer/User fragment. Root assembly
captures its installed stable plan; spawn publishes the child's own plan; fork
adds its compiled fork preamble to the inherited plan and publishes that child
plan for the next generation.

`ParentSystemInstruction` remains an input-only bridge for legacy embedders.
When no typed plan is present, fork maps that exact legacy text to
`EmbedderPolicy` System authority and immediately continues on the typed path.
D8-assembled agents never publish the legacy extension, so a flattened
Developer/User plan cannot be re-armed as System authority.

## 7. Child context-window validation (closes the deferred gap)

At every child launch path (spawn_launch.rs `arm_auto_compaction` site, fork
launch, rhai `agent_ops.rs:467`): run `validate_context_window` for the
child's model before arming, mirroring the root build path. Errors propagate
typed (abort the child launch loudly — never launch with a lying window).
Delete the "deferred to agent-variants" comments — this IS that unit.

**Owner ruling 2026-07-07 ("context window comes from the model,
overrideable"):** the child's window resolves explicit override → catalog →
typed error. `ChildLoopConfig` carries `context_window: Option<u64>` (a
loop-config field, NOT a narrowing axis — same exemption as the other
loop_config fields), suppliable via the spawn/fork `child_policy.loop_config`
argument and the envelope/rhai grant. The override has the root's
explicit-window semantics: above a catalogued model's ceiling → rejected at
launch (never a silent clamp); a deliberate uncatalogued child model accepts
the explicit value. With no override, an uncatalogued child model is a typed
error naming `child_policy.loop_config.context_window` (child remedies only —
the root knobs do not exist on the child path). This resolves the previously
recorded root/child asymmetry.

## 8. Rhai path (`integration/rhai/agent_ops.rs`)

Accept optional `variant` key in the spawn config map. Resolve via the
`VariantCatalog` extension from the host context if present (absent catalog +
variant requested = typed error). Applies: model resolution (same rules incl.
reviewer model_required), prompt (base instruction), reasoning_effort,
descriptor.profile. Tools stay `&[]` (documented known boundary — grant is
observability-only there; do NOT silently pretend variant.tools applies).

## 9. Acceptance tests (from the brief, mapped)

- Merge-layer tests: variant defined in user, overridden wholesale by
  project; built-in field-fallback (user sets ONLY reviewer.model → resolved
  reviewer keeps built-in prompt/tools, gains model).
- Spawned `explorer` provider-facing tool list (collect_function_definitions
  output) contains NO write/edit/bash/apply_patch — registry-level assertion.
- Leaf child (granted remaining_depth == 0): tool list contains neither
  `spawn_agent` nor `fork`.
- Child with no variant model: launch model == parent's model, asserted
  against the launch/provider config, not the descriptor.
- `reviewer` with no model anywhere → error text contains
  `variants.reviewer.model`.
- Fork preamble golden test: contract slugs + depth budget present.
- Child window validation: oversized explicit window on a child rejected
  (mirror `oversized_explicit_window_is_rejected_at_build`).
- ParentPromptPlan: root and child contexts carry their own source-aware stable
  plans; fork inheritance preserves each source and authority without
  publishing `ParentSystemInstruction`.

## 10. File budget

`spawn.rs` is already ~800 lines: put resolution logic in a NEW
`tools/agent/variant_resolve.rs` (owns steps §3.1–3.4) rather than growing
spawn.rs. Preamble builder lives in `agent/fork.rs` (check budget; split
`agent/fork_preamble.rs` if it would cross 500).

## 11. P0 authority addendum (2026-07-11)

Variant merge precedence does not grant file-read authority. Both project and
local settings layers are rejected if a variant declares `prompt_file`; inline
repository prompts remain supported. Trusted user prompt files must use an
absolute path and, when physically beneath the workspace, are normalized against
the immutable launch root and read through the Unix no-follow workspace API.
Eager reads must never follow a repository symlink or re-resolve a mutable CWD.

Variant/profile resolution must also keep model-selected authority separate from
operator selection. A profile selected through a model-facing spawn request is
rejected when it carries `prompt_commands`, including a trusted user profile.
This is an intentional confused-deputy closure, not a same-name fallback. Static
profile/variant prompt text remains available. D8 later resolved `ROLE-01`:
compiled built-in variants are System authority, while every prompt supplied
through configured variant settings is User authority. A configured prompt is
never promoted merely because its settings layer or variant name is trusted.
