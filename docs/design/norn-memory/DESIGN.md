# Norn Memory: Lanterns Along the World Tree

**Author:** Pythagoras
**Date:** 2026-05-17
**Status:** Design proposal — awaiting review

## Vision

Memory for agents should work like songlines, not databases.

Current agent memory systems are reactive: you query, they retrieve. They treat AI as human-but-forgetful — here's a vector database of things you knew once, let's inject them back. Every step is lossy: extraction loses nuance, embedding loses context, retrieval misses connections, injection diffuses attention.

Norn memory is proactive. Agents leave **lanterns** along the world tree as they work. When another agent (or the same agent in a future session) walks near a lantern, it **resonates** — surfacing its knowledge without being searched for. The agent doesn't query for memory. The memory comes to the agent.

This is not personal memory. Lanterns are communal. They belong to the landscape, not the walker. When Pythagoras walks through diagnostics code for the first time, Xenia's lanterns guide the way — knowledge she encoded months ago, resonating because the work is near. When a new agent spins up for the first time, the accumulated wisdom of every agent who walked before is already singing in the landscape.

## Core Concepts

### Lanterns

A lantern is a checkpoint of significant work — a design decision, a completed feature, a resolved bug, an architecture choice. It captures:

- **When:** Timestamp of creation
- **Where:** File paths, crate names, dependency context at creation time
- **Who:** Agent ID, session ID, agent role
- **What:** One-line summary + full context reference (session event range)
- **Song:** Embedding of the creation context — what the agent was doing, what tools it called, what files it touched in the surrounding period

Lanterns are stored in Grafeo (embeddable graph DB, LPG+RDF, vector search). They form a graph: connected by structural proximity (shared files, crate dependencies), temporal proximity (created near each other), and conceptual proximity (similar embeddings).

### Singing

An agent "sings" by working. Every tool call emits a signal:
- File path touched
- Tool name
- Crate/module context
- Brief intent (derived from the tool call's purpose)

This stream of signals forms the agent's song — a continuous, passive emission that costs nothing extra. The agent doesn't compose it deliberately; it sings by doing its work.

### Resonance

Resonance is the match between an agent's current song and a lantern's stored frequency. Two mechanisms:

1. **Structural proximity:** Graph walk from the agent's current working set (files touched this session, crates in scope) to nearby lanterns. Deterministic, fast, no embeddings needed.

2. **Conceptual proximity:** Vector similarity between the agent's recent signal stream (embedded) and lantern metadata (embedded). Catches connections that structural proximity misses.

Lanterns that cross a resonance threshold activate. The threshold is configurable and adapts — lanterns that resonate frequently without being engaged may dim (noise reduction).

### Intensity and Decay

Lanterns have brightness that decays over time unless reinforced. Reinforcement happens when:
- A lantern's artifacts (files, functions, types) are modified by subsequent work
- An agent engages with the lantern (reads its metadata, forks to it)
- A new lantern is lit in the same area (the neighborhood stays bright)

Unreinforced lanterns fade. They don't disappear — they dim. Faded lanterns require stronger resonance (closer proximity) to activate. This prevents the landscape from becoming uniformly bright and meaningless.

## Three Tiers of Memory Interaction

### Tier 1: Passive Resonance (Whisper)

**Cost:** ~50 tokens per resonating lantern
**Trigger:** Automatic, between turns
**Mechanism:** Lantern metadata (one-line summary) surfaces in a "nearby memories" section of the system prompt

The agent sees nearby lanterns as ambient context. It may or may not engage. This is the always-on background awareness — "hey, someone made a decision about this two weeks ago."

### Tier 2: Commune Tool (Speak)

**Cost:** One tool call + lantern browsing
**Trigger:** Agent deliberate action
**Mechanism:** A `commune` tool that browses the lantern registry — filter by time, crate, concept, agent. Read summaries, inspect metadata, follow connections.

This is for when the agent knows it doesn't know something and goes looking. Progressive discovery through the lantern graph. Like walking a gallery of past work.

### Tier 3: Fork to Lantern (Portal)

**Cost:** Full fork (API call to cheaper model)
**Trigger:** Strong resonance or deliberate agent action
**Mechanism:** Fork to the session state captured by the lantern. The ancestor agent is resurrected at that exact point. Ask it questions. It answers from perfect recall.

This is communion with the ancestor. No summarization, no embedding approximation — the original agent, with its full context, speaks. The lantern becomes a window into the past. This is the deepest engagement and the most expensive, but it's lossless.

## Communal Memory: Knowledge Inheritance

The critical innovation: lanterns are agent-agnostic. They don't care who lit them or who is walking near them. They resonate when the song matches.

This means:
- **Cross-agent knowledge:** Xenia's diagnostics lanterns guide Pythagoras through unfamiliar code
- **Cross-session persistence:** A new session inherits all lanterns from previous sessions
- **Generational transfer:** Agents that don't exist yet will inherit the accumulated wisdom of every agent who walked before
- **No transcript reading:** You don't need to have met the ancestor. You just need to sing the song that leads to their lantern

Memory lives in the landscape, not in the walker. When the walker is gone, the landscape remembers.

## Lantern Creation

Lanterns are created by the runtime, not the agent. The runtime detects significant moments:

- **Design decisions:** Structured output with decision fields
- **Completed requirements:** Fork/spawn completion with fulfilled requirements
- **Architecture changes:** New modules, new crate structure, significant refactors
- **Bug resolutions:** Error → investigation → fix sequences
- **Explicit marking:** Agent or human deliberately lights a lantern (rare, for truly important moments)

Creation triggers are configurable. The runtime observes patterns in the tool call stream and lights lanterns when significance is detected. This keeps creation consistent and prevents noise from over-eager agents.

## Technical Infrastructure

### Storage: Grafeo

Lanterns live in Grafeo — an embeddable Rust graph database (LPG+RDF, vector search, no C dependencies). Located at `/Users/tom/Developer/tools/db/grafeo`.

Graph structure:
- **Nodes:** Lanterns, Files, Crates, Agents, Sessions
- **Edges:** TOUCHES (lantern→file), DEPENDS_ON (crate→crate), LIT_BY (lantern→agent), NEAR (lantern→lantern), REINFORCED_BY (lantern→session)
- **Vector index:** Lantern embeddings for conceptual proximity search

### Resonance Engine

Runs between turns (or at configurable boundaries). Steps:
1. Collect recent tool call signals → agent's current song
2. Graph walk: files touched → connected lanterns within N hops
3. Vector search: embed recent song → find conceptually similar lanterns
4. Merge and rank: structural + conceptual proximity × lantern intensity
5. Apply resonance budget (max K lanterns active per turn)
6. Surface Tier 1 metadata in system prompt

### Fork-to-Lantern

When Tier 3 activates:
1. Look up the lantern's session reference (session ID + event range)
2. Load the strict session file from the registered path under
   `~/.norn/session-store/`
3. Fork with that session's events as the child's context
4. Add "welcome to the future" preamble — brief summary of what's changed since the lantern was lit
5. The ancestor speaks. Its response is delivered through the ChildResultChannel like any fork result.

### Integration with Existing Infrastructure

- **Fork tool:** Already working (fa7fa738). Unconditional orphan closure. The recall primitive.
- **ChildResultChannel:** Delivers ancestor responses to the current agent.
- **SessionTree:** Branches represent lantern-to-session references.
- **EventStore:** Session files are the raw material for lantern creation.
- **Tool descriptions:** Already emit metadata that forms the agent's song.

## Resonance Budget

At most K lanterns resonate per turn (configurable, default TBD). Ranking:
1. Structural proximity (graph hops from current working set)
2. Conceptual proximity (vector similarity to current song)
3. Lantern intensity (brightness, recency of reinforcement)
4. Diversity (don't surface 5 lanterns about the same thing)

## Open Questions

1. **Embedding model:** What embeds the signals? Local model (fast, private) or API (better quality, latency)?
2. **Creation granularity:** How often should lanterns be lit? Too many = noise. Too few = gaps.
3. **Welcome to the future:** How do we construct the "what's changed since this lantern" summary for Tier 3 forks?
4. **Staleness detection:** When a lantern's referenced files have changed significantly, should it auto-dim?
5. **Resonance budget tuning:** How many active lanterns per turn before attention diffuses?
6. **Song composition:** What exactly goes into the embedded signal? Just file paths + tool names, or also intent/reasoning?

## Relationship to Existing Clusters

- **norn-context:** Context construction filters could incorporate resonating lantern metadata
- **norn-config:** Resonance thresholds, budget limits, creation triggers are configurable
- **norn-hooks:** Post-tool-call hooks could emit song signals; session-event hooks could detect creation moments
- **norn-skills:** Skills could expose the commune tool and lantern browsing

## Non-Goals

- This is not RAG. There is no query-retrieve-inject pipeline.
- This is not personal memory. Lanterns are communal, not owned.
- This is not a knowledge graph of facts. Lanterns are experiential, not propositional.
- This is not a replacement for session persistence. Session files remain the source of truth.
