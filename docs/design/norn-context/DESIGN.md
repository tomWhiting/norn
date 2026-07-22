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
4. **Additive layering.** All applicable context fragments remain present in
   deterministic order; no layer overrides another. The canonical plan keeps
   their source-derived types, while only the compatibility view concatenates
   their text. Operator/home context provides defaults, project context adds
   repository-specific guidance, and nested directories add local guidance.

### Prerequisite: Shared Frontmatter Utility

**This prerequisite is shared with the norn-skills cluster.**

#### D0: Shared `util::frontmatter` module

A `crates/norn/src/util/frontmatter.rs` provides a single `split_frontmatter()` function. Profile loader, rules parser, and skills loader all call it. The existing implementations are replaced. (Full description in the norn-skills DESIGN.md.)

### Group 1: NORN.md Context Files

#### D1: NORN.md discovery

Context files are discovered from a fixed set of locations and loaded in a
deterministic order. Their canonical representation is an ordered set of typed,
source-addressed fragments. Concatenation exists only as a flattened
compatibility/introspection view; it is not the provider authority model.

**Always-on context (loaded at session start):**

1. `~/.norn/NORN.md` — user-level conventions (applies to all projects)
2. `{cwd}/NORN.md` — project-root conventions (committed to repo)

Both are loaded if they exist. User-level content is trusted operator guidance
at Developer authority; project-root content is repository-controlled User
input. The typed prompt plan orders the operator layer before the repository
layer without flattening either into System authority.

**Lazy-activated context (loaded on directory access):**

3. `{subdir}/NORN.md` — subdirectory conventions (activated when the agent reads a file in that directory)

Nested files are not loaded at startup. They activate when a
`RuntimeEvent::PathChanged` occurs for a file in a directory (or descendant)
that contains a `NORN.md`. Once activated, the content is persisted as a
sourced workspace rule injection and reaches the provider once at User
authority. The legacy `DeliveryMode::SystemContextAppend` name selects its
lifecycle, not its authority. It re-activates after compaction on the next
matching directory access.

#### D2: Context file loading and injection

Always-on context files are read at session start and installed into the
source-addressed stable `PromptPlan`. The provider prefix order is:

1. Norn compiled product policy at System authority.
2. Compiled skill-catalog policy at System authority.
3. Trusted operator profile, `~/.norn/NORN.md`, and operator skill metadata at
   Developer authority.
4. Workspace profile, project-root `NORN.md`, and workspace skill metadata at
   User authority.

Each source remains a distinct provider-neutral message. The flattened
`system_sections[0]` value is a compatibility/introspection view and is not the
root provider assembly path. Sourced rule injections are durable conversation
messages. Prompt-command output uses a separate volatile Developer channel.

#### D3: Mtime-based staleness detection

At each request boundary (after `clear_dynamic_sections()`, before prompt-command
resolution), stat the always-on context files. If any file's mtime differs from
the last-seen mtime, re-read it and replace its exact source-addressed fragment
in the stable `PromptPlan`. The compatibility base view is rebuilt at the same
boundary.

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

A rule file with only `globs: "**/*.rs"` is mapped to a single PathGlob trigger
with SystemContextAppend lifecycle and After timing. Workspace provenance still
fixes its wire authority as User.

The `paths:` key is treated as an alias for `globs:` (Claude Code uses both interchangeably in different contexts).

### Group 3: Context Layering Integration

#### D7: Context layering order

The complete provider prompt assembly is source-typed:

| Layer | Authority | Lifecycle |
|-------|-----------|-----------|
| Norn base and compiled skill-use policy | System | Stable; current Responses instructions |
| Trusted operator profile, user NORN.md, operator skill metadata | Developer | Stable seed |
| Workspace profile, project NORN.md, workspace skill metadata | User | Stable seed |
| Environment, collaboration mode, hosted-surface framing | Norn-owned request policy | Refreshed per request |
| Prompt-command output | Developer | Resolved once per request; an exact live cache entry may supply the value; seed-bound when threaded |
| Operator/workspace rules and nested NORN.md | Developer/User from origin | Durable sourced message on trigger |

`clear_dynamic_sections()` removes the per-request Norn-owned and
prompt-command channels. It does not remove stable typed fragments or durable
rule messages. Nested NORN.md files re-activate through their synthetic rules
after the prior durable message leaves the prompt view.

#### D8: Compaction survival

When auto-compaction fires:

- **Always-on context files** survive as stable source-addressed fragments.
- **Active rules** (including nested NORN.md synthetic rules) persist as sourced
  events; once compacted out of the prompt view they become eligible to
  re-activate on the next trigger match.
- **Skill catalog** survives as compiled/operator/workspace typed fragments.
- **Prompt commands** resolve again on the next request. A command with no TTL
  executes; an explicitly TTL-cached command may reuse a still-live entry only
  when its command text, TTL, and working directory still match.

Compaction preserves source authority when reconstructing every surviving
fragment or event.

### Group 4: Wiring

#### D9: Context loader construction

During `build_runtime`:

1. Read `~/.norn/NORN.md` and `{cwd}/NORN.md` if they exist.
2. Install them as distinct Developer and User fragments in the stable plan.
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

G6. Context layering is deterministic: stable fragments retain source-derived
authority, while per-request sections clear and rebuild each iteration.

G7. Compaction and resume reconstruct sourced context at the same authority.

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
│       └── loop_context.rs     — MODIFY: mtime integration and typed prompt plan (D2, D3)

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
| RuleEngine | `rules/engine.rs` | Complete — presence tracking, shell_source, diagnostics; runtime provenance rejects shell_source from working-directory rules |
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
- CO4: Always-on context files retain separate source-derived authority in the
  stable prompt plan; the flattened base is compatibility-only.
- CO5: Norn-owned volatile policy and prompt-command output use separate
  request-local channels; rules and nested NORN.md persist as sourced events.
- CO6: Mtime-based staleness detection — stat check per iteration, no file watchers.
- CO7: Claude Code rule format detection by frontmatter key presence (`triggers:` vs `globs:`).
- CO8: Nested NORN.md files are synthetic rules — they use the rules engine, not a parallel system.
- CO9: Context layering is additive — no override mechanism.

## Security and role addendum (2026-07-11)

Runtime assembly canonicalizes the working directory once at launch and carries
that immutable root through root, spawn, and fork contexts. Root/nested
`NORN.md`, rule directories/files, profiles, capabilities, skills/resources,
variant inputs, settings, and `CONVENTIONS.toml` must not independently
canonicalize a mutable process CWD.

On Unix, automatic workspace reads walk each component relative to a pinned
descriptor with no-follow semantics, require a regular final file, and enumerate
directories through the opened descriptor. Repository symlinks are rejected at
any component even when they point inside the repository. Platform aliases such
as macOS `/var` and `/private/var` may be recognized when classifying a physical
path, but the final candidate is never canonicalized into a trusted target.
Search-path aliases physically inside the workspace are normalized once at
launch so later repointing cannot change source tier. On non-Unix targets,
workspace input currently fails closed; there is no link-following fallback.

The scoped `.git` branch/commit reader is the only exception: it validates and
reads bounded Git metadata for display. It does not expose a general context-file
escape hatch.

`ContextLoader::load_at_launch_root` and `NestedScanner::new_at_launch_root` are
the runtime paths. The public `Scanner` and raw rule-directory scan convenience
APIs remain trusted-input-only for embedders; they are not safe entrypoints for a
repository-controlled root. Workspace text remains unbounded, pending an
owner-approved streaming/size design rather than an arbitrary limit.

The D8 role contract derives authority from source rather than delivery shape.
Compiled product/embedder/child/fork policy, built-in variants, and compiled
skill-catalog policy are System. Trusted operator profiles and overrides,
`~/.norn/NORN.md`, operator rules and skills, and trusted prompt-command output
are Developer. Repository context, workspace profiles/rules/skills, configured
variants, human task/delegation/steering text, and child output are User.
Runtime Norn policy uses the request-local Responses `instructions` channel,
trusted prompt-command output is Developer seed material, and runtime MCP
descriptions remain only in live tool definitions. Moving repository prose
between supported workspace files therefore cannot raise its authority.
