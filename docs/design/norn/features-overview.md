**Key Features**
1. **Tool-Embedded Validation and Diagnostics**

Tools should not merely perform actions and rely on external hooks afterward. Write/edit tools should be able to run configured validation directly as part of their execution lifecycle.

Examples:
- File length checks after write/edit.
- Path-specific rules such as stricter limits for `mod.rs`.
- Clippy, cargo check, tests, diagnostics, or custom commands after edits.
- LSP-based blast-radius analysis after function edits.
- Runtime-provided tool arguments that the agent does not set directly, such as workflow policy, validation rules, or file-pattern constraints.

2. **Headless, Scriptable Agent Runtime**

The runtime should be designed for orchestration first, not as a terminal-first interactive application. TUIs are optional later. The primary use case is scripted, headless, workflow-driven execution.

3. **Schema-Enforced Structured Output**

Structured output should be a hard contract, not a best-effort instruction. If the model output fails the schema, the runtime should detect that and return the error to the agent with the expected schema and validation failure.

4. **Per-Event Output Schemas**

Schemas should apply to different output/event types, not only final responses.

Examples:
- Assistant text message schema.
- Spoken-response schema for text-to-speech.
- Tool-call envelope schema.
- Stop/final-output schema.
- Question/clarification schema.
- Optional thinking/reasoning schema where available.

5. **Dual Written and Spoken Responses**

Assistant output should optionally include both:
- A structured written response optimized for reading.
- A spoken version optimized for text-to-speech.

This is especially useful for dyslexia and for long agent reports where dot points are easier to read but awkward to listen to.

6. **Tool Call Envelopes**

Tool calls should be wrapped in a runtime envelope that can include metadata not supplied by the model.

Examples:
- Tool use description.
- Task or requirement linkage.
- Runtime policy.
- Inbound messages.
- Diagnostics.
- Filesystem or working-tree notifications.
- Other context channels.

7. **Input Channels / Steering Without Interrupting**

The runtime should support inbound channels while the agent loop is running. These may include user messages, diagnostics, filesystem changes, working-tree changes, or agent messages.

The model should receive these at controlled points in the loop without requiring a hard interrupt or waiting for the agent to stop.

8. **Dynamic Tool Availability**

Available tools should be determined dynamically by profile, capability, workflow stage, task state, code location, or current activity.

Examples:
- Frontend tools become available when editing frontend files.
- Write/edit tools are removed once implementation is done and review begins.
- Team-lead profiles get workflow dispatch tools but not write/edit tools.
- Developer profiles get edit/write/LSP tools but not command-and-control tools.

9. **Tool Search and Tool Discovery**

BM25 or semantic tool search is useful when large tool registries exist, especially MCP servers with many tools. However, deterministic tool activation by role/profile/workflow stage should be preferred where possible.

10. **Multi-Search Tool**

A single powerful search interface could combine:
- `ripgrep`
- `fd` / file finding
- fuzzy file search
- glob filters
- AST/structural search
- vector search
- keyword search
- regex filtering

The tool should support using some search modes as filters and others as ranking/search terms.

11. **Codex File Search as a Possible Baseline**

Codex’s standalone file search may be a strong base for a broader multi-search tool, especially if it is genuinely standalone and high quality.

12. **Syntax-Aware Patch/Edit Tools**

Codex’s apply-patch implementation should be examined for syntax-aware patching. Norn should integrate patching/editing with the existing tree-sitter and syntax infrastructure.

13. **LSP-Aware Tools**

Editing and navigation tools should integrate with LSP functionality for references, definitions, diagnostics, hover data, and blast-radius analysis.

14. **Messaging and Collaboration Integration**

Norn should integrate with Meridian’s collective messaging model, including DMs, mailboxes, direct input, and eventually live direct chat sessions between humans and agents or agents and agents.

15. **Direct Session Input**

Some messages should bypass the Meridian mailbox and go directly into the underlying agent session, for slash commands, compaction commands, or direct steering.

16. **Sub-Agent and Forking as First-Class Actions**

Forking should be a first-class primitive. An agent should be able to fork itself, optionally onto a cheaper/faster model, to perform bounded tasks such as committing code, responding to messages, reading logs, or checking outputs.

The parent receives a structured audit result afterward.

17. **Model-Driven and Orchestrator-Driven Multi-Agent Work**

Both patterns are valuable:
- Orchestrator-driven agents for deterministic workflows.
- Model-driven sub-agents for autonomous delegation inside a task.

18. **Agent Registry and Bounded Concurrency**

The runtime should support agent tracking, roles, statuses, task ownership, and concurrency controls. The exact shape needs further explanation/design.

19. **Roles, Profiles, and Capabilities**

Profiles should configure model, reasoning effort, tools, hooks, settings, instructions, disallowed patterns, and capabilities. Capabilities should be composable.

20. **Goals, Budgets, and Continuation Policies**

Agents should support goal tracking, token budgets, time budgets, optional iteration limits, soft handoff points, and continuation/recapitulation paths when thresholds are reached.

21. **Streaming Observability**

The runtime needs token-by-token streaming and structured runtime events for observability, logging, diagnostics, and UI display.

22. **Session Trees and Forked Histories**

Pi’s tree-based session model is valuable. Sessions should support branching, forking, reintegration, and audit trails.

23. **Never Delete the Audit Trail**

For Norn-native sessions, context editing should not destroy history. Events should be marked as skipped, summarized, superseded, compacted, or excluded from active context, while preserving the original record.

24. **Claude Code Transcript Editor**

Claude Code session editing requires much more surgical precision because Claude’s session files are external, dependency-sensitive artifacts. The editor must preserve chains, dependencies, and valid transcript structure.

25. **Claude Code / Norn Session Translation Layer**

Eventually, Norn should be able to ingest Claude Code session transcripts into its own tree/session model, then reconstruct valid Claude Code transcripts when invoking Claude Code legitimately through Claude Runner.

26. **DMs as High-Fidelity Memory**

The message-level exchange between humans and agents is often more useful than raw tool-event logs. DMs provide compact, high-signal summaries of long work periods and should be linked to the lower-level event logs.

27. **Graph-Backed Session Intelligence**

Session events, DMs, forks, code entities, requirements, tasks, and outputs should be linkable in a graph. Initial implementation may use Memgraph, with future exploration of Qdrant Edge or a custom graph/vector database.

28. **Hybrid Memory and Search**

Memory should combine dense, sparse, ColBERT-style embeddings, rerankers, keyword search, regex, graph queries, and eventually more advanced models such as hyperbolic embeddings.

29. **Workflow Reflection / Dream Process**

Agents should periodically reflect on past runs, outcomes, messages, and logs to extract lessons, update memory, and improve future behavior.

30. **NERVA Integration Later**

NERVA may later analyze logs and interactions against conceptual/emotional/productivity/safety rubrics and provide steering feedback or self-improvement signals.

31. **Rhai Integration as More Than Builtins**

Rhai should potentially become a plugin/module layer, not merely a place for standalone builtins. It may support workflow definitions, extension-like behavior, sandboxed functions, and scripted runtime customization.

32. **Norn Extension System**

A native Norn extension system may be more valuable than Pi extension compatibility. Possibilities include:
- Rhai-based extensions.
- Rust extensions.
- QuickJS/Pi-compatible extensions later.
- Sandboxed extension functions with controlled HTTP/filesystem/tool access.

33. **OpenAI Responses API Provider**

The OpenAI provider should support Responses API streaming, tool calling, structured output, reasoning controls, server-side web search, and generated Rust types from OpenAI SDK/interface definitions where practical.

34. **OpenAI Server-Side Web Search / Web Fetch**

When using OpenAI/Codex subscriptions, Norn should use OpenAI’s server-side tools where appropriate, while also supporting local/non-OpenAI web search and fetch tools.

35. **Provider-Specific Subscription Optimization**

The provider layer should preserve legitimate subscription advantages:
- Claude through Claude Runner / Claude Code subscription.
- OpenAI through Codex/OpenAI subscription paths where legitimate.
- Avoid API-cost blowouts where subscription-backed usage is allowed.

36. **Claude-Code-Compatible Tools Where Useful**

Read/write/edit/bash/grep/glob/web search/web fetch may use Claude-like schemas where helpful, but exact compatibility is not mandatory if better Norn-native tools exist.

37. **Enhanced Norn Tools Exposed to Claude via MCP**

Norn’s stronger tools, especially AST-aware edit/write tools, diagnostics-aware tools, search tools, and task tools, should potentially be exposed to Claude Code as MCP tools.

38. **Skills, Slash Commands, Sub-Agents, Profiles, Prompt Templates**

Norn should implement equivalents of:
- Skills.
- Slash commands.
- Sub-agent definitions.
- Profiles/capabilities.
- Prompt templates.
- Runtime string substitution.

39. **Session Variables and String Substitution**

The runtime should provide session variables that can be substituted into tools, commands, prompts, hooks, and templates.

Examples:
- Session ID.
- Current working directory.
- Home directory.
- Profile.
- Member identity.
- Team tree/status.
- Workflow IDs.

40. **Runtime System Prompt Commands**

Profiles/capabilities should be able to run commands to populate dynamic prompt sections, such as team status, current tasks, member info, or workflow state.

41. **Hooks**

Norn should likely implement Claude-Code-style hooks where appropriate, both for compatibility and for standalone modular use outside Meridian.

42. **Standalone Modular Crates**

Norn should follow the Meridian pattern: standalone crate/library first, integrated into Meridian second. It should not be locked to Meridian.

43. **MCP Server Exposure**

Norn tools should be exposable as MCP tools so Claude Code and other harnesses can use them.

44. **Design System and Structured Planning Integration**

Norn should integrate with the design-system crate: structured JSON design artifacts rendered to Markdown, with plans/tasks/reviews generated against schemas.

45. **Task Management Tools**

Norn needs task management tools compatible with the current Claude task format where useful, but ideally extended toward richer task hierarchies and project management.

46. **libcorpus / Knowledge Index Integration**

Norn should integrate with `libcorpus` for code, documentation, knowledge-base files, syntax trees, and broader non-code task context.

47. **Diagnostic-Grade Agent Operations**

Agent actions should be captured with diagnostics-quality reporting, not merely logs. This includes tool failures, validation failures, policy violations, schema errors, and workflow issues.

48. **Full Spec Before Implementation**

The whole design should be captured before dispatching briefs. Implementation can be phased, but the full feature surface should be written down now so nothing gets lost.
