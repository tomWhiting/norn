# Norn-Context — User Stories

## AI Agent — Receiving Context During Execution

**S1.** As an AI agent, I want project conventions from NORN.md included in my system prompt so that I follow the project's coding standards from the start of the session.

**S2.** As an AI agent, I want directory-specific guidance from nested NORN.md files delivered when I read files in that directory so that I learn local conventions as I explore the codebase.

**S3.** As an AI agent, I want conditional rules to activate when I read or write files matching a glob pattern so that I receive guidance relevant to the file type I am working with.

**S4.** As an AI agent, I want rules to re-activate after context compaction so that I do not lose important guidance mid-session.

**S5.** As an AI agent, I want my user-level NORN.md conventions applied before project-level conventions so that project specifics take effective precedence.

## Human Operator — Configuring Project Context

**S6.** As a human operator, I want to place a NORN.md file at the project root so that all agents working in the project receive my coding conventions.

**S7.** As a human operator, I want to place rules in .norn/rules/ so that agents receive conditional guidance when they touch specific file types.

**S8.** As a human operator, I want to drop Claude Code rule files (with globs: frontmatter) into .norn/rules/ without modification so that I don't maintain two formats.

**S9.** As a human operator, I want to edit NORN.md mid-session and have the agent pick up the changes so that I can refine conventions without restarting.

**S10.** As a human operator, I want to place personal conventions in ~/.norn/NORN.md so that they apply across all my projects.

## Human Operator — Configuring Nested Context

**S11.** As a human operator, I want to place a NORN.md in a subdirectory so that agents receive additional guidance only when working in that part of the codebase.

**S12.** As a human operator, I want nested NORN.md guidance to persist in context once activated so that the agent retains subdirectory conventions while continuing work in that area.

## Rule Author — Writing Conditional Guidance

**S13.** As a rule author, I want to write rules in Norn's native format (with triggers, delivery, and timing) so that I can use all three trigger types and all three delivery modes.

**S14.** As a rule author, I want to write rules in Claude Code's simpler format (with globs: only) so that I can reuse rules across both tools.

**S15.** As a rule author, I want a clear error when my rule file has both triggers: and globs: keys so that I know the format is ambiguous.

**S16.** As a rule author, I want project rules in .norn/rules/ to override user rules in ~/.norn/rules/ on ID collision so that project-specific behavior wins.

## Workflow Author — Orchestrating Agents with Context

**S17.** As a workflow author, I want the context loader available as an API so that I can programmatically load context for agent steps.

**S18.** As a workflow author, I want context layering to be deterministic so that the same inputs always produce the same system prompt.

**S19.** As a workflow author, I want the base system instruction to be byte-stable for prefix caching so that long sessions benefit from cached prompts.
