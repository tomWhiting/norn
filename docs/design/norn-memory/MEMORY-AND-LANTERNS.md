# Memories and lanterns: two systems, and one of them already exists

**Author:** Sable Nightwick
**Date:** 2026-08-08
**Status:** Design response to owner steer — rulings in §9
**Supersedes:** `RESONANCE.md` §3 (the authorship-based distinction) and
`ANNOTATION-UNIFICATION.md` §1, both replaced by the owner's definition in §1.

---

## 1. The owner's definition, which is better than mine

**Tom, 2026-08-08:**

> *A memory is a way of storing a note against something, so you can retrieve
> that note against that thing. A lantern comes with an annotation like the
> note, but is a pathway back to have a conversation with the previous self
> through a fork.*

I had distinguished them by **authorship** (runtime-lit vs agent-authored).
That was serviceable and wrong. **The real discriminator is whether the object
carries a resumable session reference:**

| | **Memory** | **Lantern** |
|---|---|---|
| What it is | text anchored to a referent | text anchored to a referent **+ a resumable point in a session** |
| Retrieval returns | the text | the text, **or a conversation with the ancestor** |
| Cost to use | tokens for the text | tokens, **or a whole fork** |
| Declared | deliberately, any time | where a branch returns (§4) |
| Decays with code churn | **no** | **yes** (§3) |

This is implementable in a way the authorship split was not, because "does it
carry a session id and event range" is a fact about a record rather than a
judgement about intent.

---

## 2. The finding: lanterns already exist in norn, and already fire

**`SessionEvent::ForkComplete` is a lantern.** Verified at
`crates/norn/src/session/events.rs`:

```rust
ForkComplete {
    base: EventBase,
    forked_session_id: Option<String>,   // ← the pathway back
    result_summary: serde_json::Value,   // ← the annotation
    usage: EventUsage,
    duration_ms: u64,
}
```

Its own rustdoc: *"a **completion reference**, not a content merge — the
child's own events remain in its own session file … visualisers can render the
branch joining back at this event without flattening the tree into a DAG."*

And its sibling carries the timeline anchor:

```rust
ChildBranch {
    parent_session_id: Option<String>,
    child_session_id: Option<String>,
    path_address: String,               // e.g. root/fork-1a2b3c4d
    parent_event_anchor: Option<EventId>, // "the durable anchor for where in
                                          //  the parent's timeline the branch occurred"
    kind: ChildBranchKind,              // Spawn (fresh) | Fork (history-seeded)
}
```

**So the annotation, the portal, and the join point are already written,
already durable, and already tree-structured.** Every fork that completes emits
one, today, with no new mechanism and no significance classifier.

**What is missing is one thing: a code anchor.** `ForkComplete` records what
the child *produced*, not what it *touched*. That is derivable from the child's
own tool calls rather than a schema change — which is exactly the log-as-truth
shape already proposed (`RESONANCE.md` §5): the log is the truth, the index is
derived.

**Memory, by contrast, has no home.** The nearest existing variant is:

```rust
Label { base, label: String, description: Option<String> }
```

— *"a named checkpoint in the session timeline"*, anchored **only to a position
in time** via `EventBase.parent_id`. Tom's memory is a note against *a thing*
(a file, a symbol, a decision). **`Label` is a road sign; a memory needs a
referent.** That is the genuine gap, and it is small.

---

## 3. Decay, re-derived from the owner's definition — and it gets sharper

`RESONANCE.md` §2 argued decay follows code churn, not elapsed time. That
survives, and Tom's definition supplies a better *reason* than the one I gave.

- **A memory is a record.** "We chose X because Y" does not stop being what
  happened. It may become less relevant; it does not become false. **Churn must
  not dim it.**
- **A lantern is an oracle.** It opens a conversation with an agent whose
  world-model is frozen at the moment it was lit. **The further the code has
  drifted since, the more confidently wrong that ancestor will be** — not
  vaguer, *wrong*, and fluent about it.

So churn is not a proxy for relevance. For a lantern it is a **direct measure
of how much the ancestor does not know.**

### 3.1 Decay and its remedy are the same computation

`DESIGN.md` Open Question 3 asks how to construct the *"welcome to the future"*
briefing when forking to a lantern. **It is the churn.** The commits touching
the lantern's referenced paths since it was lit are simultaneously:

1. the measure of how far the ancestor's world has moved (its dimming), and
2. **the exact briefing that repairs it.**

One git query, two uses. No summarisation step, nothing stored, and the thing
that tells you a lantern is stale is the thing that makes it usable again.

**Consequence worth stating:** a heavily-dimmed lantern is not worthless — it is
*expensive*, because the briefing needed to make its ancestor useful is large.
Dimming should therefore rank it lower, never delete it.

---

## 4. Declaration — and the measured argument against the current spec

`DESIGN.md` line 100 specifies that **the runtime lights lanterns on detecting
significance.** There is now measured evidence that this is the mechanism most
likely to fail.

**SOURCED — Continual Harness (arXiv 2605.09998, Princeton/ARISE/DeepMind),
§C.1.4:** every step prompt listed the IDs and titles of *all* stored memories
"so the agent sees the full catalog for free" — which is exactly `DESIGN.md`'s
Tier 1 whisper. Their measurement:

> *"The reference rate remains low in absolute terms, which we report honestly;
> most authored entries sit unused."*
> *"…from-scratch runs write many entries and rarely reach back for them."*

**And the qualifier that matters most to us:**

> *"Memory is leveraged once the library is both mature and inherited"* —
> bootstrap runs, loading a store from a previous run, "consult it actively",
> while runs writing their own rarely re-read it. *"The transferable unit of
> the framework is therefore the harness across runs, not a single episode."*

**Two conclusions, and they cut in opposite directions:**

1. **Against the current design:** cheap automatic declaration plus passive
   catalog surfacing has a measured low pull rate. Volume without value.
2. **For the core of the vision:** *inherited* memory does get used. The
   communal, cross-session, cross-agent claim — the actual heart of `DESIGN.md`
   — is the part with supporting evidence.

### 4.1 Three declaration mechanisms, priced

| | Cost to declare | Volume | Quality | Risk |
|---|---|---|---|---|
| **(a) Runtime-detected** (current spec) | none | high | unknown — needs a significance heuristic we would have to invent | measured low pull rate; an invented classifier |
| **(b) Agent-declared** | a tool call | low | high — a considered act | agents under-declare, as people under-document |
| **(c) At branch return** — **recommended** | **none** | moderate, structurally bounded | **high** | requires branches to be used |

**(c) is recommended and it is not a compromise.** Context-Folding (2510.11967)
and AgentFold (2510.24699) both establish that an agent completing a
sub-trajectory produces a deliberate, high-density summary at the moment of
return — the `return` action that "summarises the outcome and rejoins the main
thread". **norn already has that moment, and already writes it: `ForkComplete`.**

So the significant moment is **identified structurally, not detected
heuristically.** No classifier, no invented threshold, no new agent effort, and
the summary is produced by the agent that did the work, at the moment it
finished, while it still knew why.

**Memories keep mechanism (b)** — deliberate, cheap, any time. Two objects, two
declaration mechanisms, which is a further argument that they are two systems.

---

## 5. What we store them against

The anchor is a tuple, and different retrieval arms use different parts:

| Component | Source | Notes |
|---|---|---|
| **Repo identity** | root commit hash | invariant across worktrees **and** clones (`RESONANCE.md` §4.2, verified) |
| **Paths** | tool arguments | exact; survives file renames via `--follow` |
| **Symbols** | existing AST/LSP tooling | **finer-grained than paths and stable under file moves** |
| **Intent text** | `tool_use_description` | required on every call, model-authored, durable |
| **Commit at declaration** | git | required for churn (§3) |
| **Session + event range** | `ForkComplete` / `ChildBranch` | **lanterns only** — this is the portal |

**Recommendation: anchor to several, deliberately.** There is no stable anchor.
A path breaks when a file moves; a symbol breaks when it is renamed — and each
survives the other's failure. Storing both is cheap and makes retrieval robust
to the ordinary churn of a codebase. **A single anchor is a single point of
silent failure**, and a memory that quietly stops matching looks exactly like
one that was never written.

---

## 6. Retrieval, and the honest answer on embeddings

Tom asked specifically what role vector embeddings would play. **The research
is close to unanimous, and it is not the answer the original design assumed.**

**SOURCED — Recursive Language Models (arXiv 2512.24601, MIT CSAIL):** long
context should not be fed to the model at all; it should live in an
**environment the model queries programmatically**. *"The LLM can search,
filter, and transform the context using Python functionality"* — explicitly
**no vector search**; BM25 was a *baseline they beat*. Reported: +26% against
compaction, +13% against Claude Code, +130% against CodeAct with sub-calls, at
comparable cost, scaling past 10M tokens.

**SOURCED — Prime Agent (Prime Intellect):** *"Rather than vector embeddings or
traditional retrieval, it maintains programmatic access to its own state."*
Memories are CRUD objects — `create_memory(...)`, `list("memory")`.

### 6.1 The distinction that resolves it: pull vs push

RLM does not refute resonance, because it addresses a different mode. Separate
them and each gets the right machinery:

**PULL — the agent goes looking.** *"What do we know about this?"* Here RLM is
decisive: give the agent **a queryable handle over the store**, not a
pre-selected injection. Filter by path, by symbol, by text; read what looks
worth reading; **iterate**, which no injection scheme permits. Needs no
embeddings.

**PUSH — memory arrives unbidden.** This is resonance proper, and it *must*
pre-select, because nobody asked. **This is also the mode with the poor
measured track record** (§4).

> **Recommendation: make pull excellent and keep push minimal.**
>
> - **Pull:** a programmatic handle over memories and lanterns. This is where
>   the measured wins are.
> - **Push:** the narrowest, highest-precision signal only — **exact anchor
>   match**, titles without content. *"3 memories and 1 lantern on this file."*
>   A handful of tokens, near-zero false positives.

**Where that leaves embeddings.** Embeddings improve *ranking under
uncertainty* — which is the push problem. **They are the thing you would add to
make the weaker mode smarter, so they are the last addition, not the first.**
And before them sits the lexical arm (`RESONANCE.md` §4.4.2): a classical
inverted index over the intent corpus, no model, no vector store, no
branch-consistency problem — on a corpus that is mostly rare identifiers, where
embeddings blur exactly what discriminates.

**HYPOTHESIS, mine, testable:** on this corpus lexical beats semantic. Settle
it by measurement, not argument.

### 6.2 What this costs the original vision — stated plainly

`DESIGN.md`'s poetry is *"the agent doesn't query for memory; the memory comes
to the agent."* Under this recommendation, **most of the value arrives by
pull.** The vision's substance — communal, inherited, no transcript reading,
knowledge living in the landscape rather than the walker — survives completely,
and §4's evidence *supports* it. What does not survive is the claim that
**passive arrival** is the primary channel. That part has been measured, by
someone else, and it did not work.

---

## 7. Session format: three interlinks the owner is right to expect

Tom's instinct that this "interlinks with the session file storage format" is
correct, and in three specific places.

### 7.1 norn's session log is already the tree Tom described

`EventBase { id, parent_id, timestamp }` — *"Parent event ID, forming a tree
structure."* Compaction is an **event in the log** (`SessionEvent::Compaction`),
not a destructive rewrite.

**SOURCED — Prime Agent** independently arrived at the same design: *"session
history stored as append-only JSONL … can include messages, model switches,
compaction summaries, or extension entries … branching, forking, and cloning
managed by moving a leaf pointer within the same file."* That is Tom's own
session-tree braindump — immutable tree, compaction as a view reroute rather
than a rewrite — built by an independent team. **Worth knowing his instinct is
externally corroborated rather than merely ours.**

### 7.2 🔴 The retention hazard — a lantern can open onto nothing

**A lantern's value is entirely its portal, and the portal is a pointer to a
child session file on disk.** `ForkComplete`'s own doc says the child's events
"remain in its own session file (under the root's `children/` directory)".

> **If child session files are ever cleaned up, pruned, or aged out, every
> lantern pointing at them becomes a dead portal — and it will look exactly
> like a live one until someone tries to walk through it.**

This is the day's recurring shape aimed at this feature: **absence that
presents as presence.** It must be settled before lanterns ship, not after:
either child sessions become retained-by-reference, or a lantern must be able
to state that its portal is gone rather than discovering it at fork time.

**`forked_session_id: Option<String>` already records an ephemeral child
honestly** — the rustdoc says *"absence stated, never a fake id"*. So the
schema already refuses to lie about this. The gap is lifecycle, not shape.

### 7.3 Lanterns impose a stable-event-identity requirement

A lantern addresses a point in a timeline. **If compaction or any future
rewrite renumbers or replaces events, lantern anchors dangle.** Compaction being
an *event* rather than a rewrite already protects this — but it becomes a
**requirement** the moment lanterns exist, rather than a property that happens
to hold. It should be written down as such.

---

## 8. What I have not established

- **Tom's belief that the session format "carries a few old things and is
  missing a couple"** — I have not audited it. What I can say is that the tree
  structure, `Compaction`-as-event, `ChildBranch`/`ForkComplete`, and `Label`
  are all present, and that `Label` lacks a referent (§2). **A proper audit is
  its own piece of work** and I would rather do it deliberately than infer it
  from four variants.
- **Whether `Custom { event_type, data }` should carry memories**, or whether
  memory deserves a first-class variant. First-class is my instinct — `Custom`
  is where schemas go to avoid review — but it is a real decision.
- **Whether the research generalises.** Continual Harness measured an embodied
  game agent; we are a coding agent. The low-pull-rate finding is the one I
  would most want to re-measure on our own traffic rather than inherit.

---

## 9. Rulings sought

- **M1.** Confirm §1's split: memory = note + referent; lantern = note +
  referent + portal. Everything else follows from it.
- **M2.** Accept §2: **lanterns are `ForkComplete` events plus a derived code
  anchor**, not a new object. This is the "use what exists" answer and it makes
  declaration free.
- **M3.** Accept §4(c): lanterns declared **at branch return**; memories
  declared **deliberately**. Reject `DESIGN.md`'s runtime significance
  detector, which would require inventing a classifier and has a measured poor
  outcome.
- **M4.** Accept §6.1: **pull-first, push-minimal**, with embeddings last and
  the lexical arm before them. This is a real change of emphasis from
  `DESIGN.md` and should be an explicit decision, not a drift.
- **M5.** **The retention hazard (§7.2)** — rule how child sessions are
  retained before lanterns ship. A dead portal that looks alive is the exact
  failure class this estate has spent the day removing.
- **M6.** Does memory get a first-class session-event variant, or ride
  `Custom`? (§8)
- **M7.** Do you want the session-format audit as its own piece of work? (§8)
