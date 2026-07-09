---
type: design
cluster: norn-skills
title: "Norn Skills: Discoverable, Templated Agent Capabilities"
---

# Norn Skills: Discoverable, Templated Agent Capabilities

## Intention

When this work is done, a Norn agent can discover, load, and execute skills from the filesystem using the Agent Skills open standard (agentskills.io). Skills are modular capability packages: markdown documents with structured frontmatter that the runtime scans, catalogs, and presents to the model. The model invokes skills by name (via the SkillTool) or through slash commands. Skills support string substitution, dynamic shell-evaluated context, argument passing, and reasoning effort overrides.

The experience should feel like plugins that load on demand: the agent always knows what skills are available (via the catalog in the system prompt), loads full instructions only when invoked, and can reference companion files for deep reference material. Skills authored for Claude Code should work in Norn without modification. Skills authored for other Agent Skills clients (Cursor, Codex, Gemini CLI, etc.) should work via the standard's portable core.

## Problem

Norn has a `SkillTool` (`tools/skill.rs`) that loads SKILL.md files by name. It discovers files in configured search directories and returns raw content. But:

- **Never wired.** `SkillSearchPaths` is never installed on the `ToolContext` in norn-cli. The tool errors immediately if invoked.
- **No frontmatter parsing.** The tool returns the entire file including YAML frontmatter. The model sees raw YAML mixed with instructions.
- **No string substitution.** `$ARGUMENTS`, `$N`, `${CLAUDE_SESSION_ID}` are not expanded. The model sees literal dollar-sign placeholders.
- **No dynamic context.** The backtick-bang syntax is not processed. Shell commands in skill templates are not executed.
- **No argument passing.** The SkillTool accepts only a `name` parameter. There is no way to pass arguments to a skill.
- **No catalog.** The model has no way to discover what skills are available without invoking the tool and failing. Claude Code's pattern of listing all skill descriptions in the system prompt is not implemented.
- **No invocation control.** No `disable-model-invocation` or `user-invocable` gating.
- **No effort override.** Skills cannot override the profile's reasoning effort.
- **No slash command registration.** Skills are not registered as slash commands.
- **No shell execution safety.** No global disable, no trust gate, no output caps.

Additionally, three modules in the codebase have independent implementations of YAML frontmatter splitting: `profile/loader.rs`, `rules/parser.rs`, and skills would need a fourth.

## Solution

### Design Principles

1. **Agent Skills standard compliance is the baseline.** The standard's required fields (`name`, `description`) are required. Optional standard fields (`license`, `compatibility`, `metadata`) are stored.
2. **Claude Code compatibility is the target.** A SKILL.md file that works in Claude Code must work in Norn. This means supporting hyphenated field names, string-or-list YAML shapes, `when_to_use`, and the full substitution set.
3. **Progressive loading.** Metadata always visible, instructions on demand, resources on reference.
4. **Visible failures.** Shell execution failures in skill templates produce visible error markers, never silent drops.
5. **Lenient parsing.** Warn on non-conformant fields (name mismatch, exceeded length), still load the skill. Skip only on missing description or unparseable YAML.

### Prerequisite: Shared Frontmatter Utility

**This prerequisite is shared with the norn-context cluster. Delivered by NC-001 (norn-config cluster).**

#### D0: Shared `util::frontmatter` module

A `crates/norn/src/util/frontmatter.rs` provides a single `split_frontmatter()` function. Profile loader, rules parser, and skills loader all call it. Delivered by NC-001; this cluster depends on it.

### Group 1: Skill Discovery and Catalog

#### D1: Skill search path ordering

Skills are discovered from an ordered list of directories. First match wins on name collision. Project-level paths precede user-level paths so project-specific skills win.

1. `{cwd}/.norn/skills/` — Norn project skills (highest priority)
2. `{cwd}/.agents/skills/` — cross-client interoperability (Agent Skills standard convention)
3. `{cwd}/.claude/skills/` — Claude Code project skills
4. `~/.norn/skills/` — Norn user skills
5. `~/.agents/skills/` — cross-client user skills
6. `~/.claude/skills/` — Claude Code user skills
7. `{cwd}/.meridian/skills/` — Meridian-integrated workspaces (lowest priority)

The `.agents/skills/` paths follow the standard's "adding support" guide recommendation for cross-client skill sharing. Scanning `~/.claude/skills/` ensures personal Claude Code skills are available in Norn.

Within each directory, discovery follows the Agent Skills standard: subdirectories containing `SKILL.md`. Norn additionally supports flat `<name>.md` files as a convenience extension (not standard-defined).

The search path list comes from norn-config settings. The default is the seven-tier ordering above.

When two skills share the same name across directories, the first-found wins. A diagnostic is recorded when a skill is shadowed (per D13).

#### D2: Frontmatter parsing and SkillMetadata

Each discovered SKILL.md is parsed via `util::frontmatter::split_frontmatter()`. The YAML is deserialized into `SkillMetadata` using `#[serde(rename_all = "kebab-case")]` to handle the hyphenated field names in the standard.

| Field | Type | Default | Source |
|-------|------|---------|--------|
| `name` | `Option<String>` | Directory name | Agent Skills standard (required) |
| `description` | `Option<String>` | First paragraph of body | Agent Skills standard (required) |
| `when_to_use` | `Option<String>` | None | Claude Code |
| `license` | `Option<String>` | None | Agent Skills standard |
| `compatibility` | `Option<String>` | None | Agent Skills standard |
| `metadata` | `Option<HashMap<String, String>>` | None | Agent Skills standard |
| `argument_hint` | `Option<String>` | None | Claude Code |
| `arguments` | `StringOrList` | Empty | Claude Code |
| `disable_model_invocation` | `bool` | `false` | Claude Code |
| `user_invocable` | `bool` | `true` | Claude Code |
| `allowed_tools` | `Option<StringOrList>` | None | Agent Skills standard (experimental) |
| `model` | `Option<String>` | None | Claude Code |
| `effort` | `Option<SkillEffort>` | None | Claude Code |
| `context` | `Option<SkillContext>` | None | Claude Code |
| `agent` | `Option<String>` | None | Claude Code |
| `paths` | `Option<StringOrList>` | None | Claude Code |
| `shell` | `Option<SkillShell>` | None (defaults to Bash) | Claude Code |
| `hooks` | `Option<serde_json::Value>` | None (reserved) | Claude Code |

**`StringOrList` custom deserializer:** Claude Code frontmatter accepts both space-separated strings and YAML lists for `arguments`, `allowed-tools`, and `paths`. A custom serde deserializer (`StringOrList`) handles both shapes:

```yaml
arguments: issue branch          # string form → ["issue", "branch"]
arguments:                       # list form → ["issue", "branch"]
  - issue
  - branch
```

**Lenient validation (per the standard's "adding support" guide):**
- Name doesn't match directory name: warn via diagnostics, load anyway
- Name exceeds 64 characters: warn, load anyway
- Description missing or empty: skip the skill, record diagnostic
- YAML unparseable: skip the skill, record diagnostic
- Unknown fields: silently ignored for forward compatibility

`SkillEffort` enum: `Low`, `Medium`, `High`, `XHigh`, `Max`.
`SkillContext` enum: `Fork`.
`SkillShell` enum: `Bash`, `PowerShell`.

#### D3: SkillCatalog

A `SkillCatalog` scans all search paths at startup, parses frontmatter for every discovered skill, and holds metadata in memory.

Public API:
- `scan(dirs: &[PathBuf])` — discover all skills, populate catalog
- `list()` — all `(name, description)` pairs
- `get(name: &str)` — full `SkillMetadata` for a skill
- `system_prompt_listing()` — formatted listing for system prompt injection
- `is_empty()` — true when no skills were discovered

The listing includes only skills where `disable_model_invocation == false`. For each skill, `description` and `when_to_use` are concatenated (separated by a space) in the listing, matching Claude Code's behavior. Format:

```
# Available Skills

The following skills provide specialized instructions for specific tasks.
When a task matches a skill's description, call the skill tool with that
skill's name to load its full instructions.

- skill-name: Description text. When to use text.
```

When no skills exist, the listing is empty and the section is omitted entirely. The SkillTool is not registered when the catalog is empty (per the standard's "adding support" guide: "don't register a skill tool with no valid options").

The listing is part of the base system instruction (`system_sections[0]`), byte-stable for prefix caching.

#### D4: Slash command registration

Each skill where `user_invocable == true` is registered in the `SlashCommandRegistry` on `LoopContext`. Invocation: `/skill-name [args]`. The handler calls SkillTool internally with the arguments.

Skills where `user_invocable == false` are not registered as slash commands but remain invocable by the model via the SkillTool (they appear in the system prompt listing).

### Group 2: Template Processing

#### D5: Three-stage template expansion

When a skill body is loaded (after frontmatter extraction), three expansion stages run in order:

**Stage 1: Dynamic context (backtick-bang).** Find `` !`command` `` inline patterns and ` ```! ` fenced blocks. Execute via `sh -c` (or the shell from the `shell` frontmatter field) with a 5-second timeout. Replace with trimmed stdout. On failure, replace with `[skill shell command failed: {error}]`.

Standard markdown code blocks (without `!`) are never executed.

**Shell execution safety:**

- **Global disable:** When `disableSkillShellExecution` is set in norn-config settings, all `!` commands are replaced with `[shell command execution disabled by policy]` instead of being executed. Bundled skills (if any) are exempt.
- **Working directory:** Commands run from the agent's working directory (cwd), not the skill directory. The skill uses `${CLAUDE_SKILL_DIR}` to reference bundled scripts.
- **Stdout cap:** Output is truncated at 32KB with a `[truncated — output exceeded 32KB]` marker appended.
- **Stderr:** On success, stderr is discarded. On failure, the first 1KB of stderr is included in the failure marker.
- **Trust:** Project-level skill shell executions are logged at `tracing::info!` level. Trust gating (requiring user confirmation for project skills from untrusted repos) is a norn-config concern and not implemented in the skill module itself.

**Stage 2: Dollar-sign expansion.** Resolve: `$ARGUMENTS` (full args string), `$N` / `$ARGUMENTS[N]` (positional), named `$name` (from `arguments` frontmatter), `${CLAUDE_SESSION_ID}`, `${CLAUDE_EFFORT}`, `${CLAUDE_SKILL_DIR}`.

Escaping: `$$` produces a literal `$`. Unrecognized `$name` references where `name` does not match an argument or built-in are left as-is.

**Stage 3: Mustache expansion.** Resolve `{{name}}` via the `VariableStore` on `LoopContext`. This is the existing `expand()` function from `integration/variables.rs`.

**Rescanning semantics:** Each stage is single-pass over the full text. Replacement text produced by stage N is visible to stage N+1. This means shell output containing `$ARGUMENTS` will be dollar-expanded. Shell output containing `{{name}}` will be mustache-expanded. This is intentional — it allows skills to compose shell output with variable references. If literal dollar signs or braces are needed in shell output, the shell command should produce `$$` or the content should use escaping.

#### D6: Argument handling

The SkillTool gains an optional `arguments` parameter (string). Arguments are parsed using shell-style quoting: double-quoted and single-quoted strings are single arguments (quotes stripped), unquoted words split on whitespace. Backslash escapes within quotes.

Named arguments from the `arguments` frontmatter list are mapped positionally.

If `$ARGUMENTS` does not appear in the skill body, the raw arguments string is appended as `ARGUMENTS: <value>` (Claude Code auto-append behavior).

### Group 3: Runtime Integration

#### D7: Effort override

The `effort` frontmatter field maps to `ReasoningEffort`:

| Skill effort | ReasoningEffort |
|-------------|-----------------|
| `low` | `Low` |
| `medium` | `Medium` |
| `high` | `High` |
| `xhigh` | `XHigh` |
| `max` | `Max` |

When a skill with `effort` is invoked, the LoopContext's `reasoning_effort` is overridden. The override applies to the provider call that consumes the skill content (the next model turn after activation). The previous value is restored after that turn completes. For fork/subagent mode, the child's LoopContext gets the override; the parent is unaffected.

#### D8: `allowed-tools` as stored hint

**Deliberate compatibility deviation.** Claude Code treats `allowed-tools` as permission pre-approval: listed tools can be used without prompting the user. The Agent Skills standard says it is "experimental" and means "pre-approved tools the skill may use."

Norn does not have Claude Code's permission system. The `allowed-tools` values are often permission patterns (`Bash(git add *)`) that do not map to Norn's tool-name-level filtering.

Decision: **parse and store, do not enforce.** The `allowed_tools` field is stored in `SkillMetadata` for future use. When Norn gains a permission system, the field will be wired. A diagnostic is recorded noting that `allowed-tools` is stored but not enforced (per D13). This is honest — we don't pretend compatibility we can't deliver.

#### D9: Fork/subagent mode

When `context: fork` is set, the `agent` field selects the subagent configuration (system prompt, tools, permissions). The expanded skill body becomes the subagent's **task input**, not its system prompt. This matches Claude Code's behavior where the agent type provides the system prompt and the skill content is the task.

Depends on norn-agents Group 3. The skill module defines the interface and metadata fields; actual fork execution is wired when agent infrastructure is complete.

#### D10: Per-skill hooks (deferred)

The `hooks` frontmatter field is parsed and stored in `SkillMetadata` as raw JSON but not acted upon. Implementation deferred to Phase 2 pending config-driven hook system.

#### D11: Path-scoped activation (deferred)

The `paths` frontmatter field is parsed and stored in `SkillMetadata` but not enforced. In Claude Code, `paths` limits automatic skill activation to matching file glob patterns. Implementation deferred — the field is stored so existing SKILL.md files parse correctly, but path filtering is not enforced in Phase 1.

### Group 4: Wiring and Diagnostics

#### D12: SkillSearchPaths wiring

During `build_runtime`, `SkillSearchPaths` is constructed from the configured search path list and installed on the `ToolContext`. The `SkillTool` is registered in the tool registry only when the catalog is non-empty.

#### D13: SkillCatalog construction and injection

During `build_runtime`, a `SkillCatalog` is constructed by scanning search paths. The catalog's system prompt listing is included in the base system instruction. The catalog is stored on the `ToolContext` as an extension (via `Arc<SkillCatalog>`) so the SkillTool can access metadata at invocation time.

The SkillTool activation result includes the skill directory path and a listing of bundled resources (files in the skill directory other than SKILL.md), capped at 20 entries. This supports progressive disclosure tier 3 — the model can see what resources exist without loading them.

#### D14: Skill diagnostics

A `SkillDiagnostics` collector (using the existing `DiagnosticCollector` pattern) records:

- Skipped: unparseable YAML, missing description
- Warning: name mismatch with directory, name exceeds 64 chars, shadowed by higher-priority skill
- Info: `allowed-tools` stored but not enforced, `paths` stored but not enforced, `hooks` stored but not acted upon, shell execution disabled by policy
- Error: shell command failures (with stderr excerpt)

Diagnostics are surfaceable via a future `/doctor` command or debug output.

## Goals

G1. Skills discovered from `.norn/skills/`, `.agents/skills/`, `.claude/skills/`, `~/.norn/skills/`, `~/.agents/skills/`, `~/.claude/skills/` with first-match-wins precedence.

G2. SKILL.md frontmatter parsed into structured metadata covering Agent Skills standard fields (`name`, `description`, `license`, `compatibility`, `metadata`) and Claude Code extensions (`when_to_use`, `arguments`, `allowed-tools`, `model`, `effort`, `context`, `agent`, `paths`, `shell`, `hooks`).

G3. Available skills listed in the system prompt with description + when_to_use so the model can auto-invoke based on description matching.

G4. Template expansion processes backtick-bang, dollar-sign arguments, and mustache variables in a defined three-stage order with safety controls.

G5. Existing Claude Code SKILL.md files work in Norn without modification (hyphenated field names, string-or-list shapes, substitution variables).

G6. Skill effort overrides the profile's reasoning effort for the activation turn.

G7. Shell execution has safety controls: global disable, stdout cap, stderr handling, cwd specification.

## Non-Goals

- **Per-skill hooks implementation.** Reserved in schema, deferred to Phase 2.
- **Path-scoped activation.** Reserved in schema, deferred to Phase 2.
- **`allowed-tools` enforcement.** Stored but not enforced until Norn has a permission system.
- **Skill persistence across compaction.** Auto-compaction module handles re-injection; the skill module tags activated content for identification.
- **Skill authoring or creation tools.** CLI/workflow concern, not runtime.
- **Remote skill repositories.** Local filesystem only.
- **Skill versioning.** Files versioned via git; no in-system versioning.
- **Model override.** The `model` frontmatter field is parsed but not implemented until provider-switching mid-session is supported.
- **Legacy `.claude/commands/` compatibility.** Flat command files in `.claude/commands/` are not scanned. Skills supersede commands.

## Structure

```
crates/norn/
├── src/
│   ├── util/
│   │   ├── mod.rs              — pub mod + re-exports (D0, NC-001)
│   │   └── frontmatter.rs      — split_frontmatter() (D0, NC-001)
│   ├── skill/
│   │   ├── mod.rs              — pub mod + re-exports
│   │   ├── types.rs            — SkillMetadata, StringOrList, enums (D2)
│   │   ├── loader.rs           — skill file loading, frontmatter parsing (D2)
│   │   ├── catalog.rs          — SkillCatalog: scan, enumerate, listing (D3)
│   │   └── template.rs         — three-stage expansion with safety (D5, D6)
│   ├── tools/
│   │   └── skill.rs            — MODIFY: accept args, query catalog, resource listing (D6, D13)
│   ├── profile/
│   │   └── loader.rs           — MODIFY: use util::frontmatter (D0, NC-001)
│   ├── rules/
│   │   └── parser.rs           — MODIFY: use util::frontmatter (D0, NC-001)
│   └── loop/
│       └── loop_context.rs     — MODIFY: effort override (D7)

crates/norn-cli/
├── src/
│   └── runtime/
│       └── builder.rs          — MODIFY: wire SkillSearchPaths, SkillCatalog (D12, D13)
```

## Current Inventory

| Component | File | Status |
|-----------|------|--------|
| SkillTool | `tools/skill.rs` | Exists — loads raw content, no parsing/expansion |
| SkillSearchPaths | `tools/skill.rs` | Exists — type defined, never installed |
| split_frontmatter() | `profile/loader.rs` | Complete — NC-001 extracts to util |
| split_front_matter() | `rules/parser.rs` | Complete — NC-001 replaces |
| expand() / VariableStore | `integration/variables.rs` | Complete |
| run_prompt_command() | `loop/loop_context.rs` | Complete — shell execution model |
| SlashCommandRegistry | `loop/commands.rs` | Complete |
| SystemPromptBuilder | `system_prompt/builder.rs` | Complete |
| LoopContext | `loop/loop_context.rs` | Complete — composable sections |
| ReasoningEffort | `provider/request.rs` | Complete — None/Low/Medium/High/XHigh/Max |
| DiagnosticCollector | `integration/diagnostics.rs` | Complete — reusable pattern |

## Constraints

- CO1: No `.unwrap()` or `.expect()` in library code.
- CO2: All files under 500 lines of code (excluding tests, comments, whitespace).
- CO3: `#[serde(rename_all = "kebab-case")]` on SkillMetadata for hyphenated YAML field names.
- CO4: Custom `StringOrList` deserializer for `arguments`, `allowed-tools`, `paths`.
- CO5: Three-stage expansion order: backtick-bang, then dollar-sign, then mustache. Each stage is single-pass; replacement text visible to subsequent stages.
- CO6: Backtick-bang failures produce visible error markers, not silent drops.
- CO7: Unknown frontmatter fields silently ignored for forward compatibility.
- CO8: `$ARGUMENTS` auto-appended when absent from skill body.
- CO9: `$$` escapes to literal `$`.
- CO10: Shell execution respects `disableSkillShellExecution` setting. Stdout capped at 32KB.
- CO11: SkillTool not registered when catalog is empty.
- CO12: Skills with missing description are skipped, not loaded with empty description.
