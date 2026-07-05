# Norn requirements: haematite branch-commit path

Author: Sable Nightwick · 2026-07-05
Consumer: norn tree-sessions (fork-per-agent timelines) — 1 of 4 named
consumers of Apollo Biscuit's branch-commit-path brief (with urd, tharsis,
frame). These are REQUIREMENTS from norn's side, not design: what the
primitive must let a session runtime do. Post-v1 by agreed phasing (JSONL is
the v1 store; manager-trait seam first), so nothing here is urgent — but the
brief should not foreclose any of it.

## The consuming model, in one paragraph

A norn session is an immutable, append-only event log. A fork creates a
child agent whose timeline shares the parent's history up to an anchor
point and diverges after it. Trees branch AND converge (a synthesis step
that consumes several children is a DAG node with multiple parents).
Nothing is ever rewritten or deleted; compaction is a view (summary node +
path reroute), and an annotation layer ("road signs") points at events
without touching them.

## Requirements

R1. **Branch at an arbitrary committed point, not just tip.** Norn forks
    children from a specific event in parent history (the fork anchor).
    O(1)-ish creation from any committed root, not only the branch head.

    *Granularity note (from Apollo's review of this doc, 2026-07-05):
    anchors are COMMIT-granular natively; an anchor between commits needs a
    derived root (fork + suffix-trim — cheap near the tip). Norn therefore
    commits at event or small-batch boundaries, so anchor-at-any-event holds
    without derived roots on the hot path. This is a standing assumption the
    manager-trait seam must state explicitly: the session store's commit
    cadence IS the fork-anchor resolution.*

R2. **Branches are first-class named refs that ADVANCE.** Create, append,
    read, list — with the branch root moving past its fork point. (Today's
    buffer-only branch writes are exactly the gap; this is the heart of the
    ask.)

R3. **Append-only advance is the only write shape norn uses.** A session
    branch never rewrites history. If the engine can exploit that
    (fast-path appends, no merge machinery on the hot path), norn benefits;
    at minimum it must not force rebase-like semantics on us.

R4. **Converge = multi-parent commit, not content merge.** Norn's "merge"
    records that a node has N parents (which children fed a synthesis) with
    deterministic ordering and provenance. We never need line-level or
    key-level conflict resolution between sessions — explicitly out of
    scope. (`ConflictPolicy::VectorClock` staying unimplemented costs norn
    nothing.)

R5. **Stable event addresses across branching.** An event's identity
    (position/hash) must survive being inherited by N child branches — the
    annotation layer and the action-log spine hold references to events and
    must not care which branch is reading them.

R6. **Atomic reserve-then-create, or a primitive that makes it easy.**
    Norn's crash-ordering doctrine is reservation-before-artifact (a durable
    record that a name/anchor is claimed lands before anything keyed by it
    exists). A single CAS covering "record child branch in parent + create
    child branch" would collapse our kill-9 matrix for fork; if that's not
    natural, branch creation must at least be idempotent/collision-honest
    (typed error on existing name, never silent reuse).

R7. **Read patterns:** (a) fast sequential replay of one branch from an
    anchor (resume/rebuild); (b) random access by event address (annotation
    dereference, forensics); (c) cheap child-branch enumeration given a
    parent (the directory-stack address model: root/reviewer-2/verifier-1).
    Existing read-only checkout/time-travel covers (a)/(b) shape-wise;
    (c) wants to be an index, not a scan.

R8. **Retention is archive, never delete.** Terminal sessions stop
    advancing but remain readable forever. The component-namespace archive
    pattern fits; tombstone GC must never collect a branch root a session
    index still references.

R9. **Content-addressing is a feature we will lean on.** Fork-by-hash
    (branch from any state hash), structural sharing across sibling
    timelines (prolly-tree dedup is why haematite is attractive at fleet
    scale), and eventually spool dedup: identical large tool outputs across
    a 16-agent fleet stored once.

R10. **Scale envelope for sizing, single node:** ~16 concurrent agents per
     instance appending simultaneously (distinct branches, single writer
     per branch — no cross-writer contention semantics needed); timelines
     to millions of tokens (tens of MB per branch); thousands of sessions
     per project namespace; fork on the agent hot path (a fork tool call
     should not notice branch creation — milliseconds, not the ~15-20ms
     F_FULLFSYNC tier unless durability genuinely demands it at that
     moment; norn's DurabilityPolicy cadence can batch).

## Non-requirements (don't build for us)

- Multi-node distribution, elections, quorum anything — v1 embedding is
  single-node by agreed phasing.
- Cross-branch conflict resolution / vector clocks (see R4).
- Value-size-aware chunking is Apollo's blob question; norn's events are
  small (spool blobs would be a later, separate keyspace decision).

## Sequencing note

Norn integrates behind a session-store manager trait (seam first, then the
haematite implementation as its own campaign). The branch-commit brief can
therefore land engine-side whenever it suits haematite's roadmap; norn
consumes it when the embedding campaign opens. What matters now is only
that the brief's design doesn't contradict R3/R4/R5 — those are the
load-bearing ones.
