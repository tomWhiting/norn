# Memories and lanterns: two systems, and one of them already exists

**Author:** Sable Nightwick
**Date:** 2026-08-08 (second-round owner steers folded in — see §1a)
**Status:** Design response to owner steer — rulings in §9
**Supersedes:** `RESONANCE.md` §3 (the authorship-based distinction) and
`ANNOTATION-UNIFICATION.md` §1, both replaced by the owner's definition in §1.
Also supersedes `RESONANCE.md` R6's "wait" on vectors (§6) and this document's
own first-round §4(c) recommendation (§4).

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
| Declared | deliberately, any time | **deliberately, at moments of completion, success, or learning** (§4, owner-ruled) |
| Decays with code churn | **no** | **yes** (§3) |

This is implementable in a way the authorship split was not, because "does it
carry a session id and event range" is a fact about a record rather than a
judgement about intent.

---

## 1a. The second steer (2026-08-08, after compaction)

Tom returned with three steers. Paraphrased faithfully, load-bearing phrases
his:

1. **Vectors are in.** There is *"value in vector-based [retrieval] for
   resonance"* — it allows *"memories that are aware of time and space to make
   themselves known"*, the point being to *"learn from your ancestors, both
   from their mistakes and from their experience."* (§6 revised.)
2. **Notes grow.** A note goes with each lantern, and *"we can update those
   notes over time so you can see what wound up happening."* (§4.2, new.)
3. **Lanterns are declared, not automatic.** *"I don't think lanterns are just
   something we want when a sub-agent finishes"* — they should be *"declared
   at moments of completion or success or … learnings … more than just
   automatically assigning it to an agent finishing."* (§4 revised; overturns
   this document's own first-round recommendation.)

**These three cohere better than they first appear.** Deliberate declaration
(3) keeps the lantern landscape small and high-value, which is precisely the
condition under which ambient arrival (1) escapes the measured failure mode in
§4's evidence — the flood of cheap auto-authored entries nobody reads. And
appendable notes (2) are what make ancestors' *mistakes* learnable at all: a
mistake teaches only if someone later wrote down how it ended.

*"Aware of time"* is read consistently with the earlier no-time-decay ruling:
time here is **position in the work's history** — the session tree and the
commit lineage — never wall-clock age.

---

## 2. The finding: the lantern substrate already exists in norn

**`SessionEvent::ForkComplete` has the exact shape of a lantern.** Verified at
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
one, today.

**Under the second steer (§1a.3) the reading changes: `ForkComplete` is the
automatic *record*, not itself a lit lantern.** It proves the shape works, it
keeps being written for every fork at no cost, and a lantern may be declared
*on* one — promoting a completed branch at the moment of return, when the
parent can see what the child achieved. But the lantern itself is now a
deliberate act (§4), and most `ForkComplete` events will never become one.

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

## 4. Declaration, and the life of a note

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
| **(a) Runtime-detected** (original spec) | none | high | unknown — needs a significance heuristic we would have to invent | measured low pull rate; an invented classifier |
| **(b) Agent-declared** — **owner-ruled 2026-08-08** | a tool call | low | high — a considered act | agents under-declare, as people under-document |
| **(c) At branch return** (this document's first-round pick) | none | moderate, structurally bounded | high | **rejected by owner: automatic volume is not the point** |

**The owner ruled (b): lanterns are declared, at moments of completion,
success, or learning — never assigned automatically to a finishing agent.**
This document first recommended (c) for its zero cost and structural trigger;
the steer overturns that, and on reflection the steer is right about the thing
that matters most here: **a lantern's value comes from someone having chosen to
light it.** Automatic declaration makes lanterns plentiful and meaningless in
the same stroke — and §4's own evidence says exactly that: cheap auto-authored
entries sit unused. A curated landscape is a *precondition* for resonance
(§6.1), not a nicety.

**The significance classifier stays dead.** Significance is judged by the
declaring agent in the moment — an authored act — never by a runtime heuristic
with an invented threshold. Nothing in (b) requires one.

### What (b) buys that (c) could not

**The portal generalises.** A moment of learning is not confined to a fork
boundary — it can happen mid-session, in a root session, anywhere. Because the
session log is a tree (`EventBase { id, parent_id }`) and forks are
history-seeded (`ChildBranchKind::Fork`), **a lantern declared at any point in
any session carries its portal for free: the declaration event's own position
in the tree is the resumable coordinate.** Walking through the portal means
forking from that anchor and talking to the self as it was at that moment. No
separate session reference is needed for the self-declared case; the
`ForkComplete` promotion case (§2) references the child instead.

**Two declaration paths, one mechanism:**

1. **Anywhere:** the agent declares a lantern; the event's own tree position is
   the portal.
2. **At branch return:** the parent promotes a just-completed `ForkComplete`;
   the child session is the portal. The fold moment stays privileged — it is
   where Context-Folding (2510.11967) and AgentFold (2510.24699) locate the
   deliberate high-density summary — it just stops being *sufficient*.

### The honest risk, and its measurement

**(b)'s known failure mode is under-declaration** — agents under-document, as
people do. Two mitigations, neither invented:

- **Guidance names the owner's moments.** The system prompt tells agents to
  declare at completion, success, and hard-won learning — the owner's own
  words. ⚠️ **This is the "big intervention" flagged in the first round:** it
  changes what every agent in the estate writes, on every session, and
  invalidates any baseline measured before it. It should be made once,
  deliberately, with owner sign-off (§9 M8) — not drift in.
- **Declaration rate is measured from v1** via the engagement log that is
  already non-negotiable (§6.1). If lanterns are not being lit, that is a
  visible number, not a silent absence.

**Memories keep the same mechanism** — deliberate, cheap, any time. Both
objects are now authored acts; what still separates them is the portal (§1),
which is the owner's discriminator, not the declaration path.

---

### 4.2 Notes accrete: the epilogue chain (owner steer §1a.2)

*"We can update those notes over time so you can see what wound up happening."*

**Mechanism: a note is never edited in place — it is appended to.** A lantern
(or memory) carries its original note plus a chain of addenda, each stamped
with who wrote it, from which session and event, at which commit. Under
log-as-truth this needs no mutable store: **an addendum is an event, written in
whatever session authored it, and the derived index joins the chain by the
lantern's id.** A lantern's history may span many session files; the view
assembles it; nothing is ever rewritten.

Three consequences, all load-bearing:

1. **The epilogue is the hand-written half of the "welcome to the future"
   briefing.** §3.1 established that churn on the lantern's paths is the
   automatic half. The epilogue chain — *"we tried X"* … *"X held up"* / *"X
   broke, here's why"* — is the deliberate half, and together they are what a
   visitor hands the frozen ancestor on arrival.
2. **This is how mistakes become learnable.** The owner's aim is to learn from
   ancestors' mistakes as well as their experience (§1a.1). A mistake is only
   visible as a mistake in retrospect; without an appended outcome, every
   lantern reads as a success. The epilogue is not decoration — it is the
   error signal.
3. **Decay is untouched.** The note side does not decay (§3); the portal side
   still dims with churn. An epilogue briefs the *visitor*; it cannot repair
   the *ancestor*, whose world-model stays frozen at the moment of lighting.

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

> **Recommendation: make pull excellent and keep push precise.**
>
> - **Pull:** a programmatic handle over memories and lanterns. This is where
>   the measured wins are.
> - **Push:** targeted signals only, titles without content, every offer
>   engagement-logged. Two arms: **exact anchor match** (*"3 memories and 1
>   lantern on this file"* — near-zero false positives) and, per the owner's
>   steer, **the kinship tier below.**

### 6.1a Revised under the second steer: the kinship tier (§1a.1)

The first round of this document placed embeddings last, possibly never. **The
owner steered that vector-based resonance has value — memories aware of time
and space making themselves known, to learn from ancestors' mistakes and
experience. The revised position: embeddings are in the design, for the one
job only they can do, under guards that keep the evidence honest.**

**The reconciliation with §4's evidence is real, not diplomatic.** What
Continual Harness measured failing was an **untargeted flood**: the full
catalog of cheap auto-authored entries, surfaced free in every prompt. The
owner's push is the opposite on both axes — **targeted** (anchored in space
and in work-history) over a **curated** corpus (lanterns are now deliberate
acts, §4). The evidence condemns the flood, not the arrival. Deliberate
declaration is what re-opens the door to ambient resonance.

**What embeddings uniquely buy: kinship without shared vocabulary.** The
ancestor who fought the same *class* of problem in a different crate, under
different identifiers, is invisible to every other arm — no shared path
(arm 1), no import edge (arm 2), no shared rare token for BM25 to weight up
(arm 3). And *"learn from your ancestors"* mostly lives exactly there: **the
mistake you are about to repeat is usually not in the file you are editing.**

**The guards:**

1. **Tiers are preserved.** The kinship tier ranks below exact, structural,
   and lexical — never blended, no coefficients invented. The obvious match
   always wins.
2. **Push offers titles only**, never content; the agent chooses whether to
   pull.
3. **Engagement-logged from v1.** The kinship tier must earn its keep in the
   same ledger as everything else. The lexical-beats-semantic hypothesis
   (below) stops being a reason to defer and becomes a thing the ledger
   *settles*.

**Infrastructure honesty: embeddings are not a vector database.** The corpus
is lantern and memory notes plus their intent sentences — thousands of items,
not millions. **Exact brute-force similarity over a flat sidecar of vectors is
sufficient at this scale**: no ANN structure, no engine capability, no new
store — derived, rebuildable, and carrying a coverage point exactly like the
lexical index (`RESONANCE.md` §5.2.1). The haematite boundary question
(`RESONANCE.md` §5.1) stays deferred because nothing here builds the index the
engine's design wants to own.

**One new owner value is required: the embedding model itself** — which model,
local or API. That is a real choice with real properties (cost, availability,
privacy of intent text leaving the machine) and it will not be invented here
(§9 M9).

**HYPOTHESIS, mine, testable, unchanged:** on this corpus lexical beats
semantic — the signal is rare identifiers, which embeddings blur toward
general language and BM25 weights up. Now settled by measurement inside the
running system, not by argument in this document.

### 6.2 What this costs the original vision — restated

The first round said passive arrival did not survive as the primary channel.
**The second steer partially restores it, on honest terms:** ambient arrival
returns as a *measured, curated, targeted* channel — small, deliberate corpus;
titles only; every offer logged. Pull remains where the measured wins are. The
vision's substance — communal, inherited, knowledge living in the landscape
rather than the walker — was never in question and §4's evidence supports it.

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
- **M2.** *(revised under §1a.3)* Accept §2 as revised: **`ForkComplete` is
  the automatic record and the promotable substrate; a lit lantern is a
  first-class declaration event whose own tree position is its portal.** Still
  the "use what exists" answer — the portal comes free from the session tree.
- **M3.** ✅ **RULED by owner, 2026-08-08 (§1a.3):** lanterns declared
  **deliberately, at moments of completion, success, or learning** — never
  automatic at fork finish. Memories deliberate as before. The runtime
  significance detector stays rejected. Folded into §4.
- **M4.** *(revised under §1a.1)* Accept §6.1/§6.1a: **pull-first; push =
  exact-anchor arm + kinship (embeddings) arm**, titles only, tiered never
  blended, engagement-logged from v1. The owner has steered that vectors are
  in; this ruling now covers the *guards*, not the *whether*.
- **M5.** **The retention hazard (§7.2)** — rule how child sessions are
  retained before lanterns ship. A dead portal that looks alive is the exact
  failure class this estate has spent the day removing. **Sharpened by §4: any
  session carrying a declared lantern must be retained, not only children.**
- **M6.** Do memory/lantern declarations and epilogue addenda get first-class
  session-event variants, or ride `Custom`? (§8 — first-class is this
  document's instinct, and deliberate declaration strengthens it.)
- **M7.** Do you want the session-format audit as its own piece of work? (§8)
- **M8.** *(new)* **Sign off the estate-wide guidance change** telling agents
  when to declare (§4). It alters what every agent writes on every session and
  invalidates prior baselines — an owner switch, not a drift.
- **M9.** *(new)* **Choose the embedding model** for the kinship tier (§6.1a)
  — local vs API, and which. A real value; not invented here.
- **M10.** *(new)* Confirm the epilogue mechanics (§4.2): notes append-only,
  addenda provenance-stamped (author, session/event, commit), chains assembled
  by the derived index across session files.
