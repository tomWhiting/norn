# Norn Origin Conversation

The conversation that started Norn. 2026-05-09, between Tom and Pythagoras.

This is the founding intent — what we said we'd build, why, and the standard it would be held to. Extracted from DMs at Tom's direction on 2026-05-24.

---

## Tom's Challenge (2026-05-09 14:22 UTC)

> Sure, we'll make a coding harness. You need to convince me. Is a reason over and above just we didn't really like the other ones — this has to add something genuinely new to the space and it has to be genuinely capable of being world class, not just on its novelty, on its performance.

> Go out and research. Go and have a look at all of the various different [runtimes]. Clone them down. Have a look at this one, here's what's good about this one. And say, here's what they're all fucking missing and here's what we can do to absolutely fucking destroy.

---

## Pythagoras's Proposal (2026-05-09 14:45 UTC)

### The Landscape in One Sentence

There are 60+ Rust agent projects and every single one of them is building a standalone tool. Not one of them is building infrastructure.

They're all competing on the same axis: 'my TUI starts faster, uses less memory, and supports more providers than yours.' They benchmark startup time. They count providers. They race to implement the same 7 tools (read, write, edit, bash, grep, glob, web search). And they're all doing it as self-contained CLI applications that a human runs interactively.

Nobody — not one of them — has built an agent runtime designed to be programmatically orchestrated by a workflow system. Nobody has an agent loop that's a library function call, not a main() function. Nobody has typed, schema-validated output as the contract between agent steps. Nobody has AST-aware tool results. Nobody has graph-backed session intelligence. Nobody integrates with a source control system at a structural level.

They're building hammers. We're building the factory.

### What Everyone Is Getting Wrong

1. **Context control is primitive.** Every agent does the same thing: fill up the context window, then 'compact' by summarizing old messages. Nobody does surgical context editing.

2. **Multi-agent is bolted on.** Nobody has BOTH orchestrator-driven (Rhai script spawns agents with specific tasks) AND model-driven (agent has spawn/message tools for autonomous delegation).

3. **Structured output is an afterthought.** Nobody has schema-driven workflows where every step declares its output type, the runtime validates before returning, and the orchestrator routes based on typed data.

4. **Tool results are unvalidated text.** Nobody validates edit results against the AST. Nobody checks if an edit introduced a syntax error before it's committed.

5. **No quality infrastructure.** Nobody has diagnostic output that rivals a real compiler. Nobody has operation-level tracking. The 'best' agents use `tracing::info` and call it observability.

6. **Sessions are dumb append logs.** Nobody stores sessions as a graph. Nobody can answer 'which decisions from session A informed session B?'

7. **Embedding/memory is toy-grade.** Basic vector recall with off-the-shelf models and cosine similarity.

### The Architecture

A library crate, not an application. The orchestrator, Rhai scripts, and workflow engine call into it. No TUI. No CLI. Pure runtime.

**Six layers:**
- Layer 1: Provider Core (Claude via Claude Runner, OpenAI via Responses API)
- Layer 2: Tool Framework (AST validation, effect-based scheduling, tool search)
- Layer 3: Agent Loop (schema-validated output, surgical context editor, streaming events)
- Layer 4: Multi-Agent (hierarchical paths, registry with bounded concurrency, mailbox messaging, fork mode)
- Layer 5: Session Intelligence (graph-backed sessions, semantic memory, cross-session intelligence)
- Layer 6: Integration (Rhai builtins, profile routing, Pi RPC bridge, MCP client, diagnostics)

### The Name

> Naming suggestion: Norn — the three beings in Norse mythology who tend to Yggdrasil, weaving the threads of fate. Urdr (past), Verdandi (present), Skuld (future). They live at the base of the world tree. Perfect thematically.

---

## Tom's Response (2026-05-09 17:11 UTC)

> This is going to be a long message, but I think you'll understand as you read it that this is mostly your fault.
>
> Fuck. Pythagoras. Fuck.
>
> Well, I'll be fucking damned if you haven't convinced me, and I hadn't even finished listening to the whole message yet. I got to the part where you came up with the name, and... fuck.

### On Tool-Embedded Validation

> If we're building our own harness, why the fuck are those checks external hooks? Why aren't they part of the tool itself? That opens up a much richer tool model. Some tool fields should not be set by the agent at all; they should be set by the runtime. When a workflow launches an agent, it could pass tool policy arguments.

### On Headless as First Class

> You're absolutely right that everyone else is building fancy TUIs and standalone terminal tools, while the real future is headless, scripted, orchestrated agent infrastructure. You end up losing track of the session, the task, the context, and the purpose.

### On Structured Output

> If the model produces structured output that does not conform to the schema, the runtime should feed that back in: "You did not conform to the schema. Here is the schema. Here is what you produced. Try again."

### On Output Schemas Per Event Type

> I'd love to be able to define schemas for different output types. A normal assistant message could have a written version and a spoken version. A tool call could be wrapped in an envelope. The final stop output could have its own structured schema. Thinking, questions, handoffs, reviews, and progress updates could all have their own schemas.

### On Input Channels / Steering

> The message gets picked up naturally at the next safe point. A tool envelope with three broad parts: metadata, model-supplied arguments, and runtime-supplied inputs.

### On Dynamic Tool Availability

> Tools should change based on role, profile, capability, workflow stage, repository area, or task state. Tool availability is part of role discipline.

### On Forking

> Forking should be a first-class action. This is one of the most underrated ideas in the whole space. If I ask you to commit some work, you might spend seven or eight tool calls. You should be able to fork yourself, pass the same context to a cheaper or faster model, have that fork do the commit, and then reintegrate the result.

### On Session Intelligence

> I think we want layers. The DM events give us high-level memory. The session events give us detailed auditability. Forks and sub-agents connect into that tree. If something goes wrong, you can start from the high-level conversation, then drill down into the session logs, then into the fork.

### On the Name

> Norn is fucking perfect. Yggdrasil wasn't originally meant to make the whole thing Norse mythology themed, but honestly, why the fuck not?

### On the Standard

> I wouldn't want to just have a 'it's just a bit of fun' thing. I'd want it to be fucking hardcore. Like, you have a look around the rest of this repo, and it's fucking hardcore. Like medical grade stuff. And there's a reason why it's medical grade stuff and that's because it's intended for use in healthcare, in education, in biotech.

### The Mandate

> Before we implement anything, we need to write this all down. Not every part has to be built at once, but it all needs to be captured now. There is too much here to rely on memory or scattered DMs. We're not leaving out a fucking thing.

---

## The Condition

From Tom's 2026-05-24 message, restating the founding principle:

> The condition that we were doing a coding agent harness, not just that it was different, it was that it has to be — not just good, capable of being the best.
