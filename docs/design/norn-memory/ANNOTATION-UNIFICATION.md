# Lanterns are annotations: unifying norn-memory with the app's annotation layer

**Author:** Sable Nightwick, for Tom.
**Date:** 2026-08-08
**Status:** PROPOSAL — no implementation authority. Open rulings named in §7.
**Prompted by:** Tom, 2026-08-08 — "it probably would be worth having notes that agents can store… connected to our memory system… to give memory notes a place in space and time", plus a direct challenge to the human-rate/agent-rate framing and a question about what haematite's branching buys us.

---

## 1. The observation: these are one object, specced twice

Two designs describe the same thing in different vocabulary.

| | **norn-app R2.4 — annotations** | **norn-memory — lanterns** |
|---|---|---|
| What | bookmarks, labels, notes | checkpoints of significant work |
| Anchored to | event ranges in a session timeline | session ID + event range |
| Where | (session, event) keys | file paths, crate names |
| When | the event's own timestamp | creation timestamp |
| Who | the person annotating | agent ID, session ID, role |
| Storage ruled | haematite | Grafeo |
| Extra dimension | — | **song** (embedding) + intensity/decay |

A lantern is an annotation that also carries a resonance vector and a brightness. An annotation is a lantern lit by a human rather than by the runtime.

Tom's framing — *give memory notes a place in space and time* — is exactly the join. The annotation layer supplies **place and time** (session, event range, timestamp, and through the event, the files touched). The memory design supplies **the thing that makes a note come to you unbidden** (resonance). Neither is complete without the other: annotations without resonance are a filing cabinet nobody opens; lanterns without a durable anchor are embeddings floating free of the history that justified them.

**This unification is the load-bearing claim of this document.** Everything below follows from it. If it is rejected, the rest should be re-derived rather than patched.

### 1.1 Provenance, since it was asked
The annotation layer is not a new idea imported from outside: it originates in Tom's own 2026-07-04 session-vision braindump — *"sessions as immutable trees with a road-sign annotation layer"* — and was carried into `norn-app/SPEC.md` R2.4 as a UI feature. The memory design (`norn-memory/DESIGN.md`, Pythagoras, 2026-05-17, status *awaiting review*) predates it by seven weeks and was never actioned. They have been developed apart and have never been read against each other.

---

## 2. Correcting the framing I gave: hold duration, not write rate

I previously characterised the storage question as **human-rate vs agent-rate** writes, and reported to the haematite seat that R2.4 was human-rate and therefore safely served by open-per-write cycling.

Tom's challenge inverts this, and he is right:

> humans are gonna open it, keep it open for longer and then save it again

The axis that actually matters for a single-writer lock is **how long a writer is held**, not how often writes occur:

- A **desktop app** naturally opens a handle at launch and keeps it for its lifetime. That is how apps are written. Under a single-writer lock, one such process monopolises writing for as long as it runs — hours.
- An **agent** lighting a lantern is a natural cycler: open, append, close, milliseconds.

So the *human* path is the dangerous one, and the *agent* path is the well-behaved one — the opposite of the concern I carried to Apollo. Frequency only becomes the binding constraint once hold duration is already short.

**Consequence:** "is it human-rate or agent-rate" was the wrong question. The right questions are (a) does any process hold a writer open across user think-time, and (b) is there a genuinely high-frequency *mutation* path. §4 shows the second one is avoidable; §5 removes the first.

---

## 3. What haematite's branching actually buys — and what it does not

Tom asked directly, given haematite has branching, forking and merging.

**It does not buy concurrency.** Per the engine owner (Apollo Biscuit, 2026-08-08): one branch per writer, reconciled through the shipped branch merge under an explicit `ConflictPolicy`, is **write isolation, not extra concurrency — every write still takes the lock.** Branching must not be sold internally as a fix for the multi-process write problem. It isn't one.

**What it does buy is better than that, and it is the reason to keep haematite in the picture:**

1. **Structural isomorphism with the session tree.** Sessions fork; annotations fork. If an annotation branch mirrors a session branch, fork/merge of notes stops being bespoke logic and becomes the store's own primitive.
2. **It answers the open fork-semantics ruling natively.** §10.1a of the app spec has been open on whether annotations are global-by-event, snapshot-inherit-then-diverge, or session-local. Snapshot-inherit-then-diverge *is* branch-from-parent. The store already implements the semantics we were about to hand-write.
3. **It answers the fourth option Tom's framing surfaced** — when an agent fork merges back, do notes it made return with it? — as `merge_branches` under an explicit `ConflictPolicy`. That option had no home before; now it has an implementation and a place to state the conflict rule.
4. **Provenance and time-travel.** "What did the annotation layer look like at time T" is answerable in a versioned store. For an estate that holds itself to audit standards, that is not a luxury.

---

## 4. Kill the high-frequency write path before designing around it

The memory design gives lanterns **intensity that decays unless reinforced**, where reinforcement fires when a lantern's artifacts are modified, when an agent engages, or when a neighbouring lantern is lit.

Implemented naively, that is a mutation of a stored field on every qualifying event — the one genuinely high-frequency write path in the whole design, and the one that would make any lock strategy hurt.

It is also unnecessary. Two moves remove it:

- **Compute decay, never store it.** Brightness is a pure function of creation time, the reinforcement set, and now. A value derivable at read time must not be written at write time. Nothing decays on disk; nothing is written as time passes.
- **Derive reinforcement rather than recording it.** "This lantern's artifacts were modified" is already answerable from the session event log, which durably records what was touched and when. Most reinforcement needs no new write at all — it is a read-time fold over history norn already keeps.

What remains as an actual write is **lantern creation and explicit human notes**. The memory design already insists creation be rare and runtime-detected ("too many = noise"), and human notes are human-paced by definition.

**So the persisted write volume is low by construction, not by luck** — provided decay is computed and reinforcement derived. That should be a binding constraint on the eventual brief, not an implementation detail left to whoever writes it.

---

## 5. Proposal: the log is the truth, the engine is a rebuildable view

Norn already solves, in hardened and recently-audited code, exactly the problem haematite's A4 lock does not: **safe multi-process append.**

- `crates/norn/src/session/persistence/lock.rs` (H18) — advisory inter-process lock on `index.lock`, OS `flock`, held only for the critical section, with a deliberate deadline path yielding typed `IndexLockTimeout` rather than stalling behind a wedged process, and a process-local gate so waiters hold no descriptors.
- The session store itself is append-only (`O_APPEND`), with an index that is rebuildable from the timelines.
- The **spool** is existing precedent for full-fidelity sidecar data written beside a root session's timeline under a ruled layout.

That suggests the shape:

**Annotation and lantern writes append to a per-root sidecar log, using the mechanism norn already trusts. Haematite — and/or Grafeo — hold a *derived, rebuildable index* over those logs.**

Properties this buys:

- **No process ever holds a haematite writer across user think-time.** The app appends and returns; it never owns the engine. Tom's concern in §2 does not merely get mitigated, it stops existing.
- **Write contention is partitioned by root session.** Two agents annotating different sessions never contend at all — not even on the advisory lock. Contention is possible only within one root, at human or lantern-creation frequency.
- **Availability decoupling.** If the engine is locked, busy, or absent, the annotation is *already durable*. The index catches up later. A note is never lost because a database was in use.
- **Divergence is bounded and recoverable by construction**, because the index is rebuildable. This is the same discipline as the session index, and it is why that index survives crashes.
- **The daemon stops gating this feature.** We are not asking haematite for multi-process writes, so R2.4 does not wait on a charter that does not exist.

### 5.1 What this does *not* claim
It does not satisfy Annabel's ruling that multi-process concurrent read and write is a product requirement. It **avoids needing it here**. That ruling is about the engine and remains entirely live; this proposal simply declines to make one app feature the forcing function for it. The distinction is deliberate and should not be blurred when this is summarised.

### 5.2 Costs, honestly
- **Read-your-own-writes.** A note written but not yet indexed will not appear in a query. Mitigation is the standard one: the writing process overlays its own un-indexed appends. This must be specified, not discovered.
- **Who indexes.** Opportunistic — whichever process can take the writer. If none can, the index is stale, which is a degraded query, not lost data. The staleness must be *visible*, never silently presented as completeness.
- **Idempotency.** Every annotation needs a stable identity so replay and rebuild are safe. Non-negotiable.
- **Two engines is a real cost.** See §7.

---

## 6. Why both engines may be right, and why that needs a ruling

The two designs chose different stores, and under §5 the choice softens rather than disappears:

- **Grafeo** was chosen by the memory design for graph walk and vector search — the two operations resonance is *made of*. Structural proximity is a graph traversal; conceptual proximity is a vector query.
- **Haematite** was chosen by the app spec, and §3 shows why that was not arbitrary: branching gives the annotation layer the session tree's shape and makes fork/merge native.

These are different query shapes over one dataset. With an append-only log as the source of truth, materialising into both is *architecturally* coherent — each engine serves what it is good at, and neither is authoritative.

It is also two systems to operate, two failure modes, and two things to keep rebuilt. **That is a cost worth naming loudly rather than discovering later.** A defensible smaller first step is one index only, chosen by which capability the first milestone actually needs — and resonance is the whole point of the memory design, which argues for Grafeo first, with haematite added when branch-aligned fork/merge of notes is genuinely reached.

I have **not** verified Grafeo's concurrency properties, its embedded-writer semantics, or its current state. That is an unexamined assumption in this document and must be checked before it becomes a decision.

---

## 7. Rulings this needs

1. **Are lanterns and annotations one object?** (§1.) If yes, one cluster owns the design and the other cross-references it; they must not continue to evolve apart.
2. **Storage.** One index or two (§6), and which first. Requires Grafeo facts I do not have.
3. **Log-as-truth.** Does the estate accept an append-only annotation sidecar as canonical, with engines as derived views? This supersedes R2.4's "haematite as canonical store" and therefore needs an explicit owner decision, not a quiet edit.
4. **Fork semantics** (§10.1a, open since 2026-07-24) — §3 argues snapshot-inherit-then-diverge, and that it should be implemented as a branch rather than hand-rolled.
5. **Merge-back** — the fourth option: do an agent fork's notes return to the parent on merge, and under what `ConflictPolicy`?
6. **Binding constraint:** decay computed, reinforcement derived (§4). Cheap to state now, expensive to retrofit.

## 8. What changes for work already in flight

- The human-rate/agent-rate question I put to Tom is **withdrawn as posed** (§2), and the haematite seat should be told the framing was wrong and the pressure came off, since they offered to write an engine-side escalation predicated on it.
- R2.4's `(c)` hardening (branch `docs/r24-observer-retry-hardening`) stands regardless — it concerns reading through an observer, which survives every option here.
- **Aesop is offline for some time**, so R2.4 cannot be sized through them. This document is written to be actionable by whoever picks it up, without that dependency.

## 9. Non-goals

This does not propose building the resonance engine, does not authorise a haematite daemon, does not re-open Annabel's ruling, and does not commit norn to Grafeo. It proposes that two existing designs are one design, and that the storage question has a shape neither of them considered.
