# Norn-Config — User Stories

## AI Agent — Operating Under Configuration

**S1.** As an AI agent, I want my tool permissions to be configured in a settings file so that the operator does not need to pass --allowed-tools on every invocation.

**S2.** As an AI agent, I want reasoning effort to be set in settings so that I operate at the configured reasoning level without per-invocation flags.

**S3.** As an AI agent spawning sub-agents, I want profile resolution to work from within libnorn so that child agents can load profiles by name at runtime.

## Human Operator — Configuring Norn Globally

**S4.** As an operator, I want to set my preferred model in ~/.norn/settings.json so that every Norn session uses it without me passing --model.

**S5.** As an operator, I want to define MCP server connections in my settings file so that they are available in every session.

**S6.** As an operator, I want to set permission deny rules in my user settings so that dangerous tool patterns are blocked across all projects.

**S7.** As an operator, I want to configure hooks in my settings file so that they fire automatically without me registering them programmatically.

**S8.** As an operator, I want to override the provider timeout and retry settings in my settings file so that I do not need to pass -c flags on every invocation.

## Human Operator — Configuring Norn Per-Project

**S9.** As an operator, I want to place a .norn/settings.json in my project so that project-specific settings apply automatically when I run Norn in that directory.

**S10.** As an operator, I want project settings to override my user settings so that each project can tailor Norn without changing my global config.

**S11.** As an operator, I want a .norn/settings.local.json that is gitignored so that I can have personal overrides in a shared project without committing them.

**S12.** As an operator, I want to add project-specific skill search paths in .norn/settings.json so that project skills are discovered automatically.

**S13.** As an operator, I want project hooks to extend my user hooks, not replace them, so that project-specific automation adds to my personal setup.

## Human Operator — Overriding Configuration at Invocation

**S14.** As an operator, I want CLI flags to override settings file values so that I can make one-off changes without editing files.

**S15.** As an operator, I want NORN_HOME to redirect all Norn state to a custom directory so that I can run isolated instances for testing or CI.

## Human Developer — Maintaining the Codebase

**S16.** As a developer, I want path resolution to be in the norn crate so that I do not need to depend on norn-cli to resolve ~/.norn/ paths.

**S17.** As a developer, I want a single split_frontmatter function so that I do not maintain duplicate parsing logic across profiles, rules, and skills.

**S18.** As a developer, I want settings types to derive Default with all-None fields so that tests can construct partial settings without specifying every field.

## Workflow Engine — Launching Norn Programmatically

**S19.** As a workflow engine, I want to load merged settings from libnorn so that I can construct a RuntimeBundle without going through the CLI.

**S20.** As a workflow engine, I want per-tool config (write limits, edit config) to be expressible in settings so that workflow-level tool policies do not require custom code.
