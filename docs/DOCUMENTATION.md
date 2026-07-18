# Norn — Documentation

## What is Norn?

Norn is an agent runtime — it's where AI agents live and work. If you think of an AI agent as a person doing a job, Norn is their office: it gives them a workspace, tools, memory, and the ability to talk to other agents and humans. It manages agent sessions from start to finish, making sure they have what they need and cleaning up when they're done.

The name comes from Norse mythology — the Norns are the three beings who tend the tree of fate, shaping what happens next. Norn shapes what AI agents can do, how they do it, and keeps a record of everything they've done.

## Why does Norn exist?

Running an AI agent today usually means calling an API, getting a response, and losing all context. The agent has no persistent memory, no workspace, no way to coordinate with other agents, and no audit trail of what it did.

Norn changes this:

- **Persistent sessions** — An agent session persists across interactions. The agent remembers what it was working on, what tools it has, and what it's already tried.
- **Tool management** — Agents need tools to do useful work (read files, search the web, call APIs). Norn manages which tools are available, enforces permissions, and tracks every tool call.
- **Memory** — Agents have persistent memory that survives between sessions. They learn from past interactions and build context over time.
- **Coordination** — Multiple agents can work together, each with their own role and responsibilities. Norn manages the team, delegates work, and collects results.
- **Accountability** — Every action an agent takes is recorded. You can always answer "what did the agent do, when, and why?" This feeds into Lys (the trace log) for a permanent, tamper-proof record.

## How does Norn fit in the Ablative Stack?

Norn is the agent layer. It sits on top of the stack and gives AI agents access to everything:

```
AI Agent (e.g. Claude, GPT, or any LLM)
        ↓
Norn manages the session, tools, and memory
        ↓
Aion orchestrates multi-step workflows
        ↓
Liminal handles messaging between agents and humans
        ↓
Haematite stores agent memory and state
        ↓
Beamr runs everything
        ↓
Lys records every action
```

When you interact with an AI agent through Ablative's system, Norn is running behind the scenes — giving the agent its capabilities, enforcing its boundaries, and keeping track of what it does.

## Current Status

**Active development** — Norn is in active development and used internally by Ablative. It includes a CLI (`norn`), a terminal UI (TUI) for interactive agent sessions, and support for multiple AI providers. The core agent session model, tool management, and coordination features are working.

## Getting Started

### What you'll need

- **Rust** — Norn is built in Rust. Install from [rustup.rs](https://rustup.rs) if you don't have it.
- **An AI provider API key** — Norn works with multiple AI providers (Anthropic Claude, OpenAI, etc.). You'll need an API key from at least one provider.

### Install Norn

```bash
cargo install norn-cli --locked
```

This gives you the `norn` command.

### Start an agent session

```bash
norn session new
```

This starts a new agent session. The agent gets a workspace, a set of tools, and a connection to your chosen AI provider.

### Interactive mode

Norn includes a terminal UI for working with agents interactively:

```bash
norn tui
```

This opens a terminal interface where you can chat with the agent, see what tools it's using, and watch it work in real time.

### Configure tools

Agents need tools to be useful. Norn manages tool access through configuration:

```bash
norn tools list          # See available tools
norn tools enable <name> # Give the agent access to a tool
```

## Key Concepts

**Sessions** — An agent session is a managed environment where an AI agent operates. It has a unique identity, a set of tools, memory, and a history of actions. Sessions can be paused and resumed.

**Tools** — Tools are capabilities you give to an agent — reading files, searching the web, running commands, calling APIs. Norn manages which tools are available and enforces permissions so agents can only do what you've allowed.

**Providers** — Norn supports multiple AI providers (the companies that run the AI models). You choose which provider to use, and Norn handles the communication. If one provider is down, you can switch to another.

**Agent teams** — Multiple agents can work together on a task. Each agent has its own role (researcher, reviewer, implementer), and Norn coordinates between them — delegating work, collecting results, and resolving conflicts.

**Steer** — Norn's steering system lets you guide agent behaviour through natural language instructions. Instead of complex configuration, you tell the agent what you want in plain English and Norn translates that into constraints.

## Learn More

- [Ablative Stack overview](https://ablative.com.au/stack) — See how Norn fits with the other components

## License

AGPL-3.0 — free to use and modify. If you distribute a modified version or run it as a service, you must share your changes under the same license.
