---
type: design
cluster: norn-context
title: "Norn Context: Project Files, Rule Loading, and Context Layering"
---

# Norn Context: Project Files, Rule Loading, and Context Layering

## Intention

When this work is done, a Norn agent has layered project context that mirrors how Claude Code handles CLAUDE.md and rules, adapted for Norn's infrastructure. A `NORN.md` file at the project root provides always-on project conventions. Rule files in `.norn/rules/` provide conditional guidance that activates on path globs, bash commands, or tool invocations. Nested `NORN.md` files in subdirectories activate lazily when the agent works in those directories. The context layering is deterministic and integrates cleanly with the existing system prompt builder, rules engine, and prefix caching.

The experience should feel invisible: the agent simply knows the project's conventions, learns file-type-specific guidance as it touches code, and never receives stale context from files that changed mid-session.

## Problem

Norn has a complete rules engine (`rules/` module — 7 files, fully tested) that evaluates triggers, tracks presence, executes shell sources, and produces rule injections. But rules must be loaded programmatically — there is no file discovery that scans `.norn/rules/` and populates the engine.

Norn has no equivalent of CLAUDE.md. There is no mechanism to load project-level or user-level context files into the system prompt. Profile `system_instructions` provide per-role context, but project conventions (coding standards, architecture notes, build commands) have no home.

The rules parser (`rules/parser.rs`) only accepts Norn's native format (with `triggers:`, `delivery:`, `timing:` fields). Rule files authored for Claude Code (with `globs:` and `description:` fields) cannot be loaded without manual conversion.

Three modules have independent frontmatter splitters: `profile/loader.rs`, `rules/parser.rs`, and the skills cluster would add a fourth.

## Solution

### Design Principles

1. **Context is what the agent always knows. Rules are what it learns when it touches something specific.** The boundary is activation: context files are always-on, rules are conditional.
2. **Claude Code compatibility for rules.** A rule file with `globs:` frontmatter should work when dropped into `.norn/rules/`.
3. **Mtime-based staleness, not file watchers.** Simple stat checks per iteration. No external dependencies.
4. **Additive layering.** All applicable context files concatenate. No override mechanism — user-level provides defaults, project-level provides specifics, nested directories provide local additions.

### Prerequisite: Shared Frontmatter Utility

**This prerequisite is shared with the norn-skills cluster.**

#### D0: Shared `util::frontmatter` module

A `crates/norn/src/util/frontmatter.rs` provides a single `split_frontmatter()` function. Profile loader, rules parser, and skills loader all call it. The existing implementations are replaced. (Full description in the norn-skills DESIGN.md.)

### Group 1: NORN.md Context Files

#### D1: NORN.md discovery

Context files are discovered from a fixed set of locations, loaded in order, and concatenated.

**Always-on context (loaded at session start):**

1. `~/.norn/NORN.md` — user-level conventions (applies to all projects)
2. `{cwd}/NORN.md` — project-root conventions (committed to repo)

Both are loaded if they exist. User-level first, project-root second (project-root content appears later, giving it higher effective precedence since the model reads it more recently).

**Lazy-activated context (loaded on directory access):**

3. `{subdir}/NORN.md` — subdirectory conventions (activated when the agent reads a file in that directory)

Nested files are not loaded at startup. They activate when a `RuntimeEvent::PathChanged` occurs for a file in a directory (or descendant) that contains a `NORN.md`. Once activated, the content is delivered as a rule injection via `DeliveryMode::SystemContextAppend` — it persists in context until compacted, and re-activates on next directory access.

#### D2: Context file loading and injection

Always-on context files are read at session start. Their content is appended to the base system instruction (`system_sections[0]`), after the Norn base prompt and profile instructions, before the skill catalog listing.

The full layering order for `system_sections[0]`:

1. Norn base prompt (from `build_system_prompt()`)
2. Profile `system_instructions`
3. User-level `NORN.md` (`~/.norn/NORN.md`)
4. Project-root `NORN.md` (`{cwd}/NORN.md`)
5. Skill catalog listing (from norn-skills D3)

This is byte-stable for prefix caching. Dynamic content (rule injections, prompt commands, nested NORN.md) goes in `system_sections[1..]`.

#### D3: Mtime-based staleness detection

At the start of each iteration (after `clear_dynamic_sections()`, before `evaluate_prompt_commands()`), stat the always-on context files. If any file's mtime differs from the last-seen mtime, re-read the file and rebuild `system_sections[0]`.

Cost: two `stat()` syscalls per iteration (user-level and project-root). If neither exists, the stat returns `NotFound` immediately.

The staleness check is a method on the context loader, called from the agent loop alongside `evaluate_prompt_commands()`.

#### D4: Nested NORN.md as synthetic rules

When the context scanner discovers a `{subdir}/NORN.md` file during directory scanning, it creates a synthetic `Rule` and adds it to the `RuleEngine`:

- **ID:** `norn-md:{relative-path}` (e.g. `norn-md:src/api`)
- **Trigger:** `PathGlob { pattern: "{subdir}/**" }`
- **Delivery:** `SystemContextAppend`
- **Timing:** `After`
- **Body:** The file content
- **Shell source:** None

This reuses the entire rules infrastructure — trigger evaluation, presence tracking, re-injection after compaction — without building a parallel activation system.

The synthetic rule is created lazily: the first time the agent reads a file in a directory, the context scanner checks if that directory (or any ancestor up to the project root) has a `NORN.md`. If found and not already registered, the synthetic rule is added to the engine.

### Group 2: Rule File Discovery

#### D5: Rule directory scanning

Rule files are discovered from ordered directories:

1. `{cwd}/.norn/rules/` — project rules (highest priority)
2. `~/.norn/rules/` — user-level rules
3. `{cwd}/.claude/rules/` — Claude Code compatibility
4. `{cwd}/.meridian/rules/` — Meridian-integrated workspaces

All `.md` files in each directory are parsed and added to the `RuleEngine`. Rule IDs are derived from the file stem (e.g. `rust-conventions.md` becomes `RuleId("rust-conventions")`). If the same ID exists in multiple directories, first-found wins (project rules override user rules).

Rule directories come from norn-config settings (The Count's norn-config cluster). The default is the four-tier ordering above.

#### D6: Claude Code rule format compatibility

The rules parser detects the format by examining frontmatter keys:

- **Norn format detected:** `triggers:` key present. Parsed via the existing parser path.
- **Claude Code format detected:** `globs:` or `paths:` key present. Auto-mapped to Norn types.
- **Both present:** Parse error (ambiguous format).
- **Neither present:** Parse error (no trigger source).

Claude Code format mapping:

| Claude Code field | Norn equivalent |
|-------------------|-----------------|
| `globs:` (string or array) | One `TriggerCondition::PathGlob` per pattern |
| `description:` | `Rule.name` |
| (implicit) | `DeliveryMode::SystemContextAppend` |
| (implicit) | `TriggerTiming::After` |

A rule file with only `globs: "**/*.rs"` is mapped to a single PathGlob trigger with SystemContextAppend delivery and After timing — matching Claude Code's behavior where rules persist once activated.

The `paths:` key is treated as an alias for `globs:` (Claude Code uses both interchangeably in different contexts).

### Group 3: Context Layering Integration

#### D7: Context layering order

The complete system prompt assembly:

**Base instruction (`system_sections[0]`, cached):**

| Layer | Source | When loaded |
|-------|--------|-------------|
| Norn base prompt | `build_system_prompt()` | Session start |
| Profile instructions | `Profile.system_instructions` | Session start |
| User NORN.md | `~/.norn/NORN.md` | Session start |
| Project NORN.md | `{cwd}/NORN.md` | Session start |
| Skill catalog | `SkillCatalog.system_prompt_listing()` | Session start |

**Dynamic sections (`system_sections[1..]`, cleared per iteration):**

| Layer | Source | When loaded |
|-------|--------|-------------|
| Prompt commands | Profile `PromptCommand` shell output | Each iteration |
| Active rule injections | `RuleEngine.process_event()` | On trigger match |
| Nested NORN.md | Synthetic rules from D4 | On directory access |
| Active skill body | SkillTool invocation | On skill invocation |

Dynamic sections are cleared at the top of each iteration (`clear_dynamic_sections()`). Rules re-fire based on the presence set. Nested NORN.md files re-activate via their synthetic rules.

#### D8: Compaction survival

When auto-compaction fires:

- **Always-on context files** survive — they are part of `system_sections[0]` which is not compacted.
- **Active rules** (including nested NORN.md synthetic rules) are lost from dynamic sections but re-activate on the next trigger match via the rules engine's presence tracking.
- **Skill catalog** survives — it is part of `system_sections[0]`.
- **Prompt commands** re-execute on the next iteration.

No special compaction logic is needed. The existing split-prompt architecture (base in `[0]`, dynamic in `[1..]`) handles this naturally.

### Group 4: Wiring

#### D9: Context loader construction

During `build_runtime`:

1. Read `~/.norn/NORN.md` and `{cwd}/NORN.md` if they exist.
2. Concatenate their content and append to the base system instruction.
3. Record initial mtime for both files for staleness detection (D3).
4. Scan rule directories (D5) and load all rules into the `RuleEngine`.
5. Start with no synthetic rules for nested NORN.md — they are created lazily (D4).

#### D10: RuntimeEvent emission for nested NORN.md

The agent loop already emits `RuntimeEvent::PathChanged` after tool executions that touch files. The context scanner registers a callback (or integrates into the rule evaluation pipeline) that checks for NORN.md files in the path's directory ancestry when a PathChanged event occurs for a directory not yet scanned.

This is not a new event type — it uses the existing RuntimeEvent infrastructure that the rules engine already consumes.

## Goals

G1. Always-on project context via `NORN.md` at project root and `~/.norn/NORN.md` at user level.

G2. Rule files discovered from `.norn/rules/`, `~/.norn/rules/`, and optionally `.claude/rules/`.

G3. Claude Code rule format (with `globs:` frontmatter) auto-mapped to Norn rule types.

G4. Nested `NORN.md` files activate lazily on directory access via synthetic rules.

G5. Context staleness detected via mtime stat checks per iteration.

G6. Context layering is deterministic: base instruction is byte-stable for prefix caching; dynamic sections clear and rebuild each iteration.

G7. No new compaction logic needed — split-prompt architecture handles survival naturally.

## Non-Goals

- **File watchers.** Mtime stat checks are sufficient and dependency-free.
- **Context file authoring tools.** CLI or workflow concern.
- **Context file import syntax.** Claude Code's `@file` import is a Claude Code feature. Norn context files are standalone. If import is needed, it is a future concern.
- **Context override mechanism.** All layers are additive. If override is needed, it belongs in the profile's `system_instructions`, not in the context system.
- **Nested rules directories.** Rule discovery is flat within each rules directory. Subdirectories within `.norn/rules/` are not recursively scanned (the rule ID would be ambiguous).

## Structure

```
crates/norn/
├── src/
│   ├── util/
│   │   ├── mod.rs              — pub mod + re-exports (D0, shared with norn-skills)
│   │   └── frontmatter.rs      — split_frontmatter() (D0, shared with norn-skills)
│   ├── context/
│   │   ├── mod.rs              — pub mod + re-exports
│   │   ├── types.rs            — ContextFile, ContextLayer (D1)
│   │   ├── loader.rs           — file discovery, reading, mtime tracking (D1, D2, D3)
│   │   └── scanner.rs          — directory scanning, nested NORN.md detection (D4, D10)
│   ├── rules/
│   │   ├── parser.rs           — MODIFY: Claude Code format compat (D6), use util::frontmatter (D0)
│   │   └── engine.rs           — MODIFY: accept synthetic rules from context scanner (D4)
│   ├── profile/
│   │   └── loader.rs           — MODIFY: use util::frontmatter (D0)
│   └── loop/
│       └── loop_context.rs     — MODIFY: mtime check integration (D3), context in base instruction (D2)

crates/norn-cli/
├── src/
│   └── runtime/
│       └── builder.rs          — MODIFY: load context files, scan rule dirs (D9)
```

## Current Inventory

### Rules Engine (COMPLETE)

| Component | File | Status |
|-----------|------|--------|
| Rule, RuleId, TriggerCondition | `rules/types.rs` | Complete |
| parse_rule_file() | `rules/parser.rs` | Complete — needs Claude Code compat extension |
| evaluate_triggers() | `rules/triggers.rs` | Complete — PathGlob, BashCommand, ToolInvocation |
| RuleEngine | `rules/engine.rs` | Complete — presence tracking, shell_source, diagnostics |
| RulePresenceSet | `rules/lifecycle.rs` | Complete |
| DeliveryMode implementations | `rules/delivery.rs` | Stub — needs delivery logic |

### System Prompt and Context

| Component | File | Status |
|-----------|------|--------|
| SystemPromptBuilder | `system_prompt/builder.rs` | Complete |
| LoopContext | `loop/loop_context.rs` | Complete — composable sections, split prompt |
| PromptCommand execution | `loop/loop_context.rs` | Complete |
| VariableStore / expand() | `integration/variables.rs` | Complete |
| ContentTag / PromptView | `loop/context.rs` | Complete |
| HookRegistry | `integration/hooks.rs` | Complete |

### Frontmatter

| Component | File | Status |
|-----------|------|--------|
| split_frontmatter() | `profile/loader.rs` | Complete — extraction target |
| split_front_matter() | `rules/parser.rs` | Complete — replacement target |

## Constraints

- CO1: No `.unwrap()` or `.expect()` in library code.
- CO2: All files under 500 lines of code (excluding tests, comments, whitespace).
- CO3: Shared frontmatter utility replaces all existing implementations.
- CO4: Always-on context files are part of `system_sections[0]` (cached, byte-stable).
- CO5: Dynamic context (rules, nested NORN.md, prompt commands) uses `system_sections[1..]`.
- CO6: Mtime-based staleness detection — stat check per iteration, no file watchers.
- CO7: Claude Code rule format detection by frontmatter key presence (`triggers:` vs `globs:`).
- CO8: Nested NORN.md files are synthetic rules — they use the rules engine, not a parallel system.
- CO9: Context layering is additive — no override mechanism.
