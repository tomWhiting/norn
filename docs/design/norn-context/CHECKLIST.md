# Norn-Context — Checklist

## Shared Frontmatter Utility

- [ ] **C1** — util/mod.rs exists with pub mod frontmatter and re-exports.
- [ ] **C2** — util/frontmatter.rs exports a split_frontmatter() function that splits YAML frontmatter from markdown body.
- [ ] **C3** — split_frontmatter() handles the empty-frontmatter edge case (consecutive --- delimiters with no YAML content).
- [ ] **C4** — split_frontmatter() handles Windows line endings.
- [ ] **C5** — split_frontmatter() returns an error when the opening --- delimiter is missing.
- [ ] **C6** — split_frontmatter() returns an error when the closing --- delimiter is missing.
- [ ] **C7** — profile/loader.rs calls util::frontmatter::split_frontmatter() instead of its own implementation.
- [ ] **C8** — rules/parser.rs calls util::frontmatter::split_frontmatter() instead of its own split_front_matter().
- [ ] **C9** — No other split_frontmatter or split_front_matter function exists in the codebase outside util/frontmatter.rs.

## NORN.md Context Files

- [ ] **C10** — context/mod.rs exists with pub mod declarations and re-exports only.
- [ ] **C11** — context/types.rs defines ContextFile with path, content, and mtime fields.
- [ ] **C12** — context/loader.rs reads ~/.norn/NORN.md when it exists.
- [ ] **C13** — context/loader.rs reads {cwd}/NORN.md when it exists.
- [ ] **C14** — context/loader.rs concatenates user-level content before project-root content.
- [ ] **C15** — context/loader.rs records initial mtime for both context files.
- [ ] **C16** — Always-on context file content is appended to system_sections[0] (base instruction).
- [ ] **C17** — Always-on context appears after profile system_instructions and before the skill catalog listing in the base instruction.

## Mtime Staleness Detection

- [ ] **C18** — At the start of each iteration, the context loader stats both always-on context files.
- [ ] **C19** — If a context file's mtime differs from the last-seen mtime, the file is re-read.
- [ ] **C20** — When a context file changes, system_sections[0] is rebuilt with the new content.
- [ ] **C21** — If a context file does not exist, the stat check completes without error.

## Nested NORN.md

- [ ] **C22** — context/scanner.rs detects NORN.md files in directory ancestry when a PathChanged event occurs.
- [ ] **C23** — A nested NORN.md creates a synthetic Rule with a PathGlob trigger matching the directory and its descendants.
- [ ] **C24** — Synthetic rules use DeliveryMode::SystemContextAppend and TriggerTiming::After.
- [ ] **C25** — Synthetic rule IDs use the format norn-md:{relative-path}.
- [ ] **C26** — A nested NORN.md is only registered as a synthetic rule once (not re-registered on subsequent PathChanged events in the same directory).
- [ ] **C27** — Nested NORN.md content re-activates after compaction via the rules engine's presence tracking.

## Rule File Discovery

- [ ] **C28** — Rule files are discovered from {cwd}/.norn/rules/, ~/.norn/rules/, and optionally {cwd}/.claude/rules/.
- [ ] **C29** — All .md files in each rules directory are parsed and added to the RuleEngine.
- [ ] **C30** — Rule IDs are derived from the file stem (e.g. rust-conventions.md becomes RuleId("rust-conventions")).
- [ ] **C31** — If the same rule ID exists in multiple directories, the first-found wins.

## Claude Code Rule Format Compatibility

- [ ] **C32** — rules/parser.rs detects Norn format when the triggers: key is present in frontmatter.
- [ ] **C33** — rules/parser.rs detects Claude Code format when the globs: or paths: key is present in frontmatter.
- [ ] **C34** — Both triggers: and globs:/paths: present in the same file produces a parse error.
- [ ] **C35** — Claude Code globs: string maps to a single TriggerCondition::PathGlob.
- [ ] **C36** — Claude Code globs: array maps to one TriggerCondition::PathGlob per pattern.
- [ ] **C37** — Claude Code description: maps to Rule.name.
- [ ] **C38** — Claude Code format rules default to DeliveryMode::SystemContextAppend.
- [ ] **C39** — Claude Code format rules default to TriggerTiming::After.

## Context Layering and Wiring

- [ ] **C40** — system_sections[0] contains: Norn base prompt, profile instructions, user NORN.md, project NORN.md, skill catalog listing — in that order.
- [ ] **C41** — system_sections[1..] contains: prompt commands, rule injections, nested NORN.md, skill bodies — as dynamic sections.
- [ ] **C42** — Context files and rule files are loaded during build_runtime.
- [ ] **C43** — Always-on context survives compaction (part of system_sections[0]).
- [ ] **C44** — Active rules re-activate after compaction via the rules engine's presence tracking.
- [ ] **C45** — cargo clippy --workspace --all-targets -- -D warnings passes clean.
- [ ] **C46** — cargo fmt --check passes clean.
