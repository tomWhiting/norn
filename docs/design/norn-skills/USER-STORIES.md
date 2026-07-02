# Norn-Skills — User Stories

## AI Agent — Using Skills During Execution

**S1.** As an AI agent, I want to see a list of available skills with descriptions and when-to-use guidance in my system prompt so that I can invoke relevant skills without guessing names.

**S2.** As an AI agent, I want to invoke a skill by name and receive its expanded instructions with frontmatter stripped so that I can follow domain-specific workflows.

**S3.** As an AI agent, I want skill arguments to be substituted into the skill body before I see it so that the instructions reference the specific task at hand.

**S4.** As an AI agent, I want dynamic shell output injected into skill content before it reaches me so that I have real-time context without needing to run commands myself.

**S5.** As an AI agent, I want Norn session variables ({{name}}) expanded in skill content so that I can reference runtime state like working directory and session ID.

**S6.** As an AI agent, I want to know when a skill's shell command fails so that I can adjust my approach instead of working with incomplete context.

**S7.** As an AI agent, I want to see the skill directory path and bundled resource listing in the activation result so that I can load reference files on demand.

## Human Operator — Invoking Skills Interactively

**S8.** As a human operator, I want to invoke skills via slash commands (/skill-name) so that I can trigger domain workflows without remembering tool syntax.

**S9.** As a human operator, I want to pass arguments to skills (/fix-issue 123) with shell-style quoting support so that multi-word arguments are handled correctly.

**S10.** As a human operator, I want skills I authored for Claude Code to work in Norn without modification so that I don't maintain two copies.

**S11.** As a human operator, I want skills from .agents/skills/ (cross-client standard) to be discovered by Norn so that skills shared across tools work everywhere.

## Profile Author — Configuring Skills for Agents

**S12.** As a profile author, I want to place project-specific skills in .norn/skills/ so that agents working in this project have domain capabilities.

**S13.** As a profile author, I want to place personal skills in ~/.norn/skills/ so that they apply across all my projects.

**S14.** As a profile author, I want to set an effort level in a skill's frontmatter so that the agent uses appropriate reasoning effort for that skill's domain.

**S15.** As a profile author, I want to hide a skill from the model's auto-invocation (disable-model-invocation: true) so that it is only triggered by explicit slash commands.

**S16.** As a profile author, I want to create background-knowledge skills (user-invocable: false) so that the model can load them automatically but the user doesn't see them in the slash command menu.

**S17.** As a profile author, I want project skills to override user skills on name collision so that project-specific behavior wins.

**S18.** As a profile author, I want diagnostic warnings when skill frontmatter is non-conformant so that I can fix issues without skills silently failing.

## Workflow Author — Orchestrating Agents with Skills

**S19.** As a workflow author, I want the skill catalog available as an API (SkillCatalog.list()) so that I can programmatically discover and configure skills for agent steps.

**S20.** As a workflow author, I want skill effort overrides to apply to the child LoopContext in fork mode so that sub-agents use the appropriate reasoning level.

**S21.** As a workflow author, I want shell execution in skills to be globally disableable via settings so that I can run agents in restricted environments.
