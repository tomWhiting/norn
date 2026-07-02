# Norn-Agents — User Stories

## AI Agent — Delegating Work to Sub-Agents

**S1.** As an AI agent, I want to spawn a sub-agent with a named profile so that the child has the right system instructions, tool access, and reasoning config for its role.

**S2.** As an AI agent, I want spawn to return immediately so that I can continue working while the child executes in parallel.

**S3.** As an AI agent, I want to wait for a child agent reactively so that I consume zero CPU while idle instead of polling every 50ms.

**S4.** As an AI agent, I want to wait on multiple children and react to whichever finishes first so that I can process results as they arrive.

**S5.** As an AI agent, I want to send a steering message to a running child so that I can redirect its approach without killing and respawning it.

**S6.** As an AI agent, I want to close a parent agent and have all its descendants shut down recursively so that I do not leave orphaned agents running.

**S7.** As an AI agent, I want to be notified via mailbox when a child completes so that I do not have to poll for completion.

## AI Agent — Running as a Sub-Agent

**S8.** As a sub-agent, I want to see the tools available to me so that I can call them by name rather than guessing.

**S9.** As a sub-agent, I want profile-derived system instructions so that I understand my role and constraints.

**S10.** As a sub-agent, I want to receive steering messages from my parent at tool boundaries so that I can adjust my approach without interruption.

**S11.** As a sub-agent, I want to emit structured progress updates via per-event schemas so that my parent can observe my work without me explicitly reporting.

**S12.** As a sub-agent, I want to break my work into subtasks so that my parent can see the task tree and track progress.

## AI Agent — Managing Tasks

**S13.** As an AI agent, I want to create a named task group that persists to disk so that my work plan survives across sessions.

**S14.** As an AI agent, I want to create subtasks under a parent task so that I can decompose work into a hierarchy.

**S15.** As an AI agent, I want to claim a task atomically so that no other agent picks up the same work.

**S16.** As an AI agent, I want to list child tasks of a parent so that I can see what sub-work exists.

**S17.** As an AI agent, I want parent task status to roll up from children so that a parent shows InProgress when any child is active and Completed only when all children are done.

**S18.** As an AI agent, I want to search available tools by keyword so that I can discover capabilities I may not know about.

## Human Developer — Configuring Agent Profiles

**S19.** As a human developer, I want to write agent profiles as markdown files with YAML frontmatter so that the format matches our existing Meridian profiles.

**S20.** As a human developer, I want profiles at ~/.norn/profiles/ so that they are consistent with the rest of the ~/.norn/ directory structure.

**S21.** As a human developer, I want workspace-level profiles to override user-level profiles so that I can customise agent behaviour per project.

**S22.** As a human developer, I want to use the 31 existing Meridian profiles with Norn sub-agents so that I do not have to rewrite profile definitions.

## Human Developer — Observing Agent Activity

**S23.** As a human developer, I want to see a task hierarchy that shows what each agent is working on so that I understand the state of a multi-agent session at a glance.

**S24.** As a human developer, I want task groups to persist across sessions so that I can review what was done and resume work later.

**S25.** As a human developer, I want sub-agent progress visible through tool_use_descriptions so that I can see what a child agent is doing without structured reporting overhead.
