# Norn — User Stories

## AI Agent — Executing a Workflow Step

**S1.** As an AI agent dispatched by a workflow, I want to receive a typed instruction with an output schema so that I know exactly what structured result to produce.

**S2.** As an AI agent, I want the runtime to validate my output against the schema before returning so that I get a chance to fix schema errors rather than failing silently.

**S3.** As an AI agent, I want to receive inbound messages and diagnostics at tool boundaries so that I can adjust my approach without being interrupted mid-thought.

**S4.** As an AI agent approaching a token budget, I want the runtime to tell me to wrap up and summarize progress so that I can hand off cleanly to a continuation.

## AI Agent — Editing Code

**S5.** As an AI agent editing a file, I want the Edit tool to validate the AST after my edit so that I learn immediately if I introduced a syntax error.

**S6.** As an AI agent, I want the Write tool to block me from overwriting a file I have not read so that I do not destroy content I have not seen.

**S7.** As an AI agent editing Rust code, I want the tool to run configured diagnostics (clippy, cargo check) after my edit so that I see compilation errors in the same tool response.

**S8.** As an AI agent, I want the Edit tool to report the blast radius of my change (affected symbols, references, related tests) so that I know what else might need updating.

**S9.** As an AI agent, I want to be told my file exceeds the length limit after writing so that I can refactor it into modules rather than submitting oversized files.

## AI Agent — Searching and Navigating Code

**S10.** As an AI agent, I want a single Search tool that combines content search, file finding, fuzzy matching, and structural search so that I do not need to learn multiple search tools.

**S11.** As an AI agent, I want the LSP tool to show me definitions, references, and diagnostics for a symbol so that I can understand the code structure before editing.

**S12.** As an AI agent entering a new directory, I want to see a file tree listing activated by a rule so that I know the directory structure before I start editing.

## AI Agent — Coordinating Sub-Agents

**S13.** As an AI agent, I want to spawn a sub-agent with a specific task and model so that I can delegate work without losing my own context.

**S14.** As an AI agent, I want to fork myself onto a cheaper model for a bounded task like committing code so that expensive model time is not wasted on mechanical work.

**S15.** As an AI agent, I want to wait for a sub-agent to complete and receive its structured result so that I can incorporate its findings without reading its full transcript.

**S16.** As an AI agent with a long-running background task, I want a lightweight monitor to watch the output and answer my questions about progress so that I do not have to consume the full output myself.

**S17.** As an AI agent, I want to send a message to another agent by path so that I can coordinate without going through the orchestrator.

## AI Agent — Communicating via Meridian

**S18.** As an AI agent in the Meridian collective, I want to send DMs to humans and other agents so that I can report progress and ask questions.

**S19.** As an AI agent, I want my DM exchanges to be linked to my session events in the graph so that the high-level conversation and low-level tool calls form a connected audit trail.

**S20.** As an AI agent, I want to tag my tool calls with task references so that downstream consumers can trace my work back to specific requirements.

## Human Developer — Orchestrating Agents via Workflows

**S21.** As a workflow developer, I want to invoke an agent step as a Rhai function call that returns typed output so that I can route results to the next step without parsing text.

**S22.** As a workflow developer, I want to configure which tools an agent has access to based on its profile so that I can enforce role discipline (e.g., team leads cannot edit files).

**S23.** As a workflow developer, I want to configure post-edit diagnostics (clippy, tests, formatters) per profile so that different workspaces get appropriate validation without changing tool code.

**S24.** As a workflow developer, I want to declare output schemas for each workflow step so that every step produces predictable, typed output.

**S25.** As a workflow developer, I want to write rules that fire before or after specific tools or commands so that agents receive contextual guidance without me modifying the tools themselves.

## Human Developer — Steering a Running Agent

**S26.** As a human operator, I want to send a steering message to a running agent that gets delivered at the next tool boundary so that I can redirect without interrupting.

**S27.** As a human operator, I want to see token-by-token streaming output from the agent so that I can follow its reasoning in real time.

**S28.** As a human operator, I want the agent to produce both a written and a spoken version of its response so that I can listen via text-to-speech without awkward dot-point cadence.

## Human Developer — Reviewing and Auditing Agent Work

**S29.** As a reviewer, I want the session audit trail to be immutable so that I can see exactly what happened even if context was later compacted.

**S30.** As a reviewer, I want to trace from a DM summary down to specific tool calls and edits so that I can understand why an agent made a particular decision.

**S31.** As a reviewer, I want the agent's structured output to include per-requirement developer notes so that I can assess how each requirement was addressed.

**S32.** As a compliance officer, I want every file edit tracked with session ID and reasoning metadata so that I can produce a complete decision audit for regulatory review.

## System Integrator — Connecting Norn to Other Tools

**S33.** As a system integrator, I want Norn to run as an MCP server so that I can give any MCP-compatible agent access to Norn's tools without modifying the agent.

**S34.** As a system integrator, I want Norn to connect to external MCP tool servers so that I can extend the tool set without modifying Norn.

**S35.** As a system integrator, I want to use Norn as a library crate without depending on Meridian so that I can integrate it into standalone projects.

**S36.** As a system integrator, I want Norn to wrap Claude Code (stripped to bare metal with Norn tools via MCP) so that Claude models can use enhanced tools while staying within Anthropic's terms.

## Extension Developer — Building Norn Extensions

**S37.** As an extension developer, I want to register custom tools via the Meridian extension protocol so that my extension's tools appear alongside Norn's native tools.

**S38.** As an extension developer, I want my extension process to be shared across multiple agents so that expensive resources (models, databases) are loaded once.

**S39.** As an extension developer, I want to subscribe to agent lifecycle events (tool calls, messages, completions) so that my extension can react to agent behavior.

**S40.** As an extension developer, I want to write extensions in Python, TypeScript, or Rust so that I can use the language and ecosystem best suited to my extension's purpose.

## Orchestrator Developer — Configuring Tool Validation

**S41.** As an orchestrator developer, I want to set file length thresholds per profile so that different teams can enforce different standards without code changes.

**S42.** As an orchestrator developer, I want file length measured in code lines (excluding comments and blanks) so that enforcement matches what developers expect from their style guides.

**S43.** As an orchestrator developer, I want the DiagnosticCollector to capture all tool-level violations during a step so that I can observe agent behavior without parsing individual tool results.

**S44.** As an orchestrator developer, I want to configure whether a tool's post-validation gates execution or reports findings so that I can tune strictness per workflow.

**S45.** As an orchestrator developer, I want tool validation results in structured JSON so that I can programmatically filter, aggregate, and route violations.

**S46.** As an orchestrator developer, I want to publish diagnostics infrastructure on the ToolContext so that any RuntimePostValidateCheck can access shared registries without coupling to specific tools.
