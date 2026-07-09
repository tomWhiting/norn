# Norn-Skills — Checklist

## Shared Frontmatter Utility

- [ ] **C1** — util/mod.rs exists with pub mod frontmatter and re-exports.
- [ ] **C2** — util/frontmatter.rs exports a split_frontmatter() function that splits YAML frontmatter from markdown body.
- [ ] **C3** — split_frontmatter() handles the empty-frontmatter edge case (consecutive --- delimiters with no YAML content).
- [ ] **C4** — split_frontmatter() handles Windows line endings (\r\n) in both the opening and closing delimiters.
- [ ] **C5** — split_frontmatter() returns an error when the opening --- delimiter is missing.
- [ ] **C6** — split_frontmatter() returns an error when the closing --- delimiter is missing.
- [ ] **C7** — profile/loader.rs calls util::frontmatter::split_frontmatter() instead of its own implementation.
- [ ] **C8** — rules/parser.rs calls util::frontmatter::split_frontmatter() instead of its own split_front_matter().
- [ ] **C9** — No other split_frontmatter or split_front_matter function exists in the codebase outside util/frontmatter.rs.

## Skill Discovery

- [ ] **C10** — skill/mod.rs exists with pub mod declarations and re-exports only.
- [ ] **C11** — skill/types.rs defines SkillMetadata with serde rename_all = kebab-case and all fields from D2 (name, description, when_to_use, license, compatibility, metadata, argument_hint, arguments, disable_model_invocation, user_invocable, allowed_tools, model, effort, context, agent, paths, shell, hooks).
- [ ] **C12** — skill/types.rs defines SkillEffort enum with Low, Medium, High, XHigh, Max variants.
- [ ] **C13** — skill/types.rs defines SkillContext enum with Fork variant.
- [ ] **C14** — skill/types.rs defines SkillShell enum with Bash and PowerShell variants.
- [ ] **C15** — skill/types.rs defines a StringOrList custom deserializer that accepts both a space-separated string and a YAML list.
- [ ] **C16** — skill/loader.rs parses SKILL.md frontmatter into SkillMetadata using util::frontmatter::split_frontmatter().
- [ ] **C17** — skill/loader.rs defaults name to the directory name when the name field is absent from frontmatter.
- [ ] **C18** — skill/loader.rs silently ignores unknown frontmatter fields.
- [ ] **C19** — skill/loader.rs discovers skills via <name>/SKILL.md and <name>.md patterns in search directories.
- [ ] **C20** — skill/loader.rs skips skills with missing or empty description and records a diagnostic.
- [ ] **C21** — skill/loader.rs warns via diagnostics when name does not match directory name or exceeds 64 characters, but still loads the skill.

## Skill Catalog

- [ ] **C22** — skill/catalog.rs defines SkillCatalog with scan(), list(), get(), system_prompt_listing(), and is_empty() methods.
- [ ] **C23** — SkillCatalog.scan() discovers skills from an ordered list of 7 directories with first-match-wins on name collision.
- [ ] **C24** — SkillCatalog.system_prompt_listing() excludes skills where disable_model_invocation is true.
- [ ] **C25** — SkillCatalog.system_prompt_listing() concatenates description and when_to_use per skill entry.
- [ ] **C26** — SkillCatalog.system_prompt_listing() includes a behavioral instruction line telling the model when and how to call the skill tool.
- [ ] **C27** — SkillCatalog.system_prompt_listing() returns empty string when no skills exist.
- [ ] **C28** — Each skill where user_invocable is true is registered in the SlashCommandRegistry.

## Template Processing

- [ ] **C29** — skill/template.rs implements three-stage expansion in order: backtick-bang, dollar-sign, mustache.
- [ ] **C30** — Stage 1 finds and executes inline !`command` patterns, replacing with trimmed stdout.
- [ ] **C31** — Stage 1 finds and executes ```! fenced blocks, replacing with trimmed stdout.
- [ ] **C32** — Stage 1 does not execute standard markdown code blocks (without the ! marker).
- [ ] **C33** — Stage 1 replaces failed commands with [skill shell command failed: {error}], not silent drops.
- [ ] **C34** — Stage 1 respects the shell frontmatter field (Bash or PowerShell) when executing commands.
- [ ] **C35** — Stage 1 enforces a 5-second timeout per command.
- [ ] **C36** — Stage 1 respects disableSkillShellExecution setting — replaces commands with policy-disabled marker.
- [ ] **C37** — Stage 1 runs commands from the agent's working directory (cwd), not the skill directory.
- [ ] **C38** — Stage 1 truncates stdout at 32KB with a truncation marker appended.
- [ ] **C39** — Stage 1 on failure includes the first 1KB of stderr in the failure marker.
- [ ] **C40** — Stage 2 resolves $ARGUMENTS to the full arguments string.
- [ ] **C41** — Stage 2 resolves $N and $ARGUMENTS[N] to positional arguments (0-based).
- [ ] **C42** — Stage 2 resolves named $name arguments from the arguments frontmatter list.
- [ ] **C43** — Stage 2 resolves ${CLAUDE_SESSION_ID}, ${CLAUDE_EFFORT}, and ${CLAUDE_SKILL_DIR}.
- [ ] **C44** — $$ escapes to a literal $ in stage 2.
- [ ] **C45** — Unrecognized $name references that do not match an argument or built-in are left as-is.
- [ ] **C46** — Stage 3 resolves {{name}} via the VariableStore using the existing expand() function.
- [ ] **C47** — If $ARGUMENTS does not appear in the skill body, the raw arguments string is appended as 'ARGUMENTS: <value>'.

## Argument Handling

- [ ] **C48** — SkillTool accepts an optional arguments parameter (string).
- [ ] **C49** — Arguments are parsed using shell-style quoting (double-quoted and single-quoted strings as single arguments, unquoted split on whitespace).
- [ ] **C50** — Named arguments from the arguments frontmatter list are mapped positionally.

## Runtime Integration

- [ ] **C51** — SkillEffort maps to ReasoningEffort: low->Low, medium->Medium, high->High, xhigh->XHigh, max->Max.
- [ ] **C52** — Skill effort overrides LoopContext.reasoning_effort for the activation turn, then restores the previous value.
- [ ] **C53** — The allowed_tools field is parsed and stored in SkillMetadata but not enforced.
- [ ] **C54** — The hooks frontmatter field is parsed and stored in SkillMetadata but not acted upon.
- [ ] **C55** — The paths frontmatter field is parsed and stored in SkillMetadata but not enforced.
- [ ] **C56** — Fork mode: the agent field selects the subagent configuration; the expanded skill body becomes the task input, not the system prompt.

## Wiring and Diagnostics

- [ ] **C57** — SkillSearchPaths is constructed and installed on the ToolContext during build_runtime.
- [ ] **C58** — SkillTool is registered in the tool registry during build_runtime only when the catalog is non-empty.
- [ ] **C59** — SkillCatalog is constructed by scanning search paths during build_runtime.
- [ ] **C60** — SkillCatalog is stored on the ToolContext as an Arc<SkillCatalog> extension.
- [ ] **C61** — The skill catalog listing is included in the base system instruction.
- [ ] **C62** — SkillTool activation result includes the skill directory path and a listing of bundled resources.
- [ ] **C63** — Skill diagnostics record: skipped (unparseable/missing description), warning (name mismatch/shadowed), info (deferred fields).
- [ ] **C64** — cargo clippy --workspace --all-targets -- -D warnings passes clean.
- [ ] **C65** — cargo fmt --check passes clean.
