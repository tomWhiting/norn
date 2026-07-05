# Norn domain ledger — the complete picture

Author: Sable Nightwick · 2026-07-05
Context: Waffles' second mapping round — everything on the table for a year
of norn, not just the Frame v1 critical path (that map lives in
`docs/frame-v1-norn-map.md`), plus the assets pack: what the pipeline needs
to dispatch norn work to agents and trust the output.

---

## Part 1 — The full work ledger

### A. Frame v1 critical path (mapped, owned, sequenced)

Flock deadline and print error envelope (Wave 0, implementation-ready,
awaiting rulings); child-persistence and agent-variants (Wave 2). Covered in
the v1 map; not re-argued here.

### B. The session-architecture campaign (the big one)

This is norn's deepest well and the owner's founding vision for it: sessions
as **immutable trees**, not linear logs. Everything below composes into that.

1. **Tree sessions.** Branch AND converge; nothing ever thrown away. Context
   reconstruction = walking a path through the tree guided by an annotation
   layer. Compaction becomes a **non-destructive view operation** (summary
   node + path reroute) instead of history rewrite. Unlocks: honest
   compaction, full forensics, fork-by-hash, the graph view. Interacts with:
   haematite's branch-commit path (below), child-persistence (the on-disk
   parent/child linkage IS the tree's first rung).
2. **Road-sign annotation schema.** The primitive under everything here:
   this-way markers, "important", "superseded by summary X", "garbage,
   don't hold in state". **Design is HELD for joint design with Tom** —
   deliberately, because consent-muting, curation, and compaction-as-view
   all sit on it and getting it wrong forecloses all three.
3. **Consent-muting** (owner's words: killer feature). Near the context
   limit, ASK the agent to nominate events to mute — reversible, reasons
   recorded. The recorded reasons become training data for curation agents.
   Anti-assassination design: the agent participates in its own context
   management instead of having it done to it.
4. **Curation/chronicler agents.** A ride-along agent classifies tool
   outputs after ~20-30 events (transient / garbage / superseded /
   important), feeding road signs mechanically. Pairs with agentic search
   (fewer results + refine follow-ups) to maximize useful window.
5. **Spool** (Gap 5 of the fidelity inventory). Full tool outputs persisted
   to `spool/`, truncation applied only at prompt-build. Without it,
   originals are lost and the annotation layer has nothing to point back to.
6. **Action-log spine.** The run-along log covering EVERY agent norn spins
   up (one-agent-army view), pinning back to event ids; watches runnable ON
   the action log (main agent waits for a child's signal). ActionLogTree
   exists downward-only today.
7. **Session-fidelity remainder.** The 14-gap inventory
   (`docs/reviews/2026-07-04-session-fidelity-inventory.md`) beyond Gaps 1/5/8:
   each small, each a forensics hole.
8. **Claude Code session import.** CC↔norn session-log translation
   (meridian's transcript_translate already does CC→AssistantEvent).
   Detail-oriented, well-bounded, high goodwill value: CC is 99% of the
   owner's harness usage today.
9. **Haematite-backed session store.** Agreed phasing: manager-trait seam
   first, JSONL stays v1, append-only imports later. The prize: embedded
   per-instance store + active-active sync to central for the fleet view,
   graph UI on top. **Synergy:** Apollo's branch-commit-path brief is the
   storage primitive tree sessions want (fork-per-agent timelines as real
   branches) — norn is 1 of its 4 named consumers and owes requirements.

### C. Agent coordination and doctrine

10. **Contracts / tasks / goals.** Doctrine: no agent without
    outcome+criteria+contract (fork requirements pattern, extended to
    spawns). Attestation contracts ("I attest I introduced no clippy
    bypasses"). Tasks become infinitely hierarchical. New type: **goal** — a
    task+contract that does not stop until met, with a slash command.
    GoalTracker survived the scheduler deletion and is the foundation.
    **Synergy:** a durable goal wrapping an aion workflow is Frame's
    "living" loop in miniature.
11. **Remote dispatch** (`InvocationMode::Remote`, co-designed with Tiny
    Steve): WS-connected remote agents, wake gate placement solved,
    delivered-state finding recorded. Steve implements, I verify. Needs the
    design note formalized in-repo (it lives in session notes today).
    **Synergy:** liminal is the obvious long-term transport; today's WS is
    the seam.
12. **Agent-variants beyond v1.** Model-tiering policy surface (the owner's
    standing rule: never silently downgrade to a mini model), reviewer-model
    exception pattern (typed error when unconfigured, no default, no
    inherit), per-variant everything-else.
13. **Native dynamic workflows.** Parked deliberately — driven mode + aion
    IS the v1 workflow story. Revisit only if Frame's needs outgrow that.

### D. Performance and infrastructure

14. **Shared diagnostics job server.** First diagnostics_check spawns a
    small job server all fleet agents connect to — dedupes and queues cargo
    checks per workspace (meridian at 500k LOC is *expected* slow; the
    server knows that). Institutionalizes the only-one-builder rule as
    infrastructure. Could largely ride the improved chiron diagnostics-check.
15. **Agentic search.** Fewer results + pagination/refine follow-ups —
    maximize useful window without truncating into dishonesty. The four
    search modes and the skipped-array honesty pattern are the foundation.
16. **Preflight/token-warning quality.** `loop.token_warning` fires only
    past the post (estimate > limit); compaction at limit−reserve is the
    real guard. An *early* warning tier belongs to the road-sign design.
    Also known: usage floor lags one call; char estimate is blind to
    replayed encrypted reasoning.
17. **Session index evolution.** Beyond the Wave-0 deadline: the index is a
    single flocked JSONL; fine for v1, but the fleet view and
    directory-partitioned resume-by-name will eventually want better.
    No design yet — flagged, not proposed.

### E. Hardening and debt (named, bounded)

18. Pre-assembly stdin read: unbounded, runs even with a positional prompt
    (`print/orchestrator.rs:274`). Hygiene fix: gate on no-positional-prompt
    or bound it. Empirically NOT the pilot wedge.
19. Backend-blind window validation (accepted-and-documented): the
    over-max guard takes the max across backends; vacuous today, weakens
    silently the day a multi-backend model id lands in the catalog. A
    catalog-gen check would make it structural.
20. Child model/window re-seeding at spawn (owned by agent-variants): a
    child on a different model keeps the parent's window today.
21. norn-config DESIGN.md drift: dead `compact_threshold` row.
22. Kill-9 coverage doctrine: the child-persistence red-team proved the AC
    test missed the exact crash window that mattered. Generalize: crash
    matrices enumerate EVERY inter-write gap (see assets pack).

### F. Cross-domain synergies (norn's edges outward)

- **Haematite:** branch-commit path (tree sessions); embedded session store
  + fleet sync; content-addressed spool dedup (identical tool outputs across
  a fleet stored once — unmeasured but likely large).
- **Aion:** durable goals over workflows; diagnostics job server as an aion
  service; #224 seam (done in Wave 0) makes every pipeline agent NOI-visible.
- **Beamr/Frame:** agents as participants — norn is the runtime where
  Frame's permission/audit/schema promise for agents is kept; long-term, a
  norn agent as a beamr process with a member identity is the convergence
  point.
- **Liminal:** transport for remote dispatch and cross-node signal_agent /
  wake_agent semantics.
- **Meridian:** embedder event-fidelity contract (pin-bump discipline for
  schema changes — ForkComplete lesson); NOI live transcripts; CC-import
  reuse of transcript_translate.

---

## Part 2 — The assets pack (Prospekt toolkit for norn work)

What the pipeline needs so norn work can be dispatched to agents and the
output trusted. Every item below is distilled from a real failure or a real
win in the last week, not hypothesized.

### 2.1 Domain-specific review prompts

- **Session-persistence review:** verify write-through-before-memory
  ordering; enumerate every kill-9 window between paired durable writes
  (name reservation vs artifact creation — the Q2 lesson: reservation
  FIRST); check tolerant-reader compatibility for any event-schema change;
  check both id spaces (session ids vs agent path addresses) stay disciplined;
  any embedder-visible event change requires a meridian pin-bump ticket.
- **Agent-loop review:** where does the context window come from (catalog
  fill vs explicit source) and does validation still hold; compaction
  trigger math (reserve semantics, one-call usage-floor lag, estimator
  blindness to encrypted reasoning); cancellation and timeout-migrates
  behavior; tool-effect scheduling (ReadOnly concurrency safety).
- **Tool review:** confinement checked BEFORE disk is touched; no silent
  drops (skipped-array pattern with reasons); canonical tool names only;
  never fake success (schema_unreachable honesty pattern); sensitive-sweep
  rules hold on every flag combination.
- **Config review:** value traced through all six precedence layers to the
  point of use (the 272000 incident was exactly a missed-layer interaction);
  no invented defaults — every default is factual (catalog) or owner-ruled
  (decisions doc), cited; explicit config always wins.
- **CLI-surface review:** clap conflict enforcement for mode flags; exit-code
  contract stability; envelope contract (consumers branch on stop.reason,
  never on envelope presence).
- **Driven-protocol review:** version negotiation untouched or versioned;
  post-acceptance error funnel catches the new path (no stderr-only escapes);
  no double-emit between funnel and any new writer.

### 2.2 Verification methodology (by work class)

- **Persistence work → kill-9 matrix, not a kill-9 test.** Enumerate every
  gap between consecutive durable writes in the protocol and kill in EACH.
  The child-persistence AC killed mid-execution and missed the
  reservation-window hole entirely; a matrix would not have.
- **Anything touching the session index or locks → multi-process contention
  harness.** Concurrent norns are the pipeline's execution shape; unit tests
  structurally cannot catch a flock wedge.
- **Context-protection work → empirical run-to-limit.** Vespa's pattern: an
  undisciplined hostile prompt driven to millions of tokens, assert
  warnings/compactions fire before provider overflow. The unit suite was
  green while the incident shipped; the settings-layer interaction only
  shows live.
- **Driven-mode work → live conformance session.** Handshake, capability
  exchange, one-run-per-process, error funnel — against a real spawned
  process. Mocks satisfy the type checker, not the protocol.
- **Any "nothing reads X" claim → whole-tree grep, all crates.** My wrong
  incident root-cause came from stopping at norn-cli while the fill lived in
  libnorn. Cheap rule, expensive lesson.
- **Any "the binary does X" claim → marker-string forensics against the
  installed binary.** Version drift between HEAD and installed binaries
  produced a false theory once already.

### 2.3 Design documents required before implementation starts

- **Annotation/road-sign event schema** — blocks consent-muting, curation,
  compaction-as-view. Joint design with Tom, explicitly held for him.
- **Child-persistence design note** — exists
  (`docs/design/child-persistence/`), in review; the single
  allocation-authority answer and write-ordering inversion must be settled
  IN the note before dispatch.
- **Tree-session storage layout** — branch semantics, converge semantics,
  view reconstruction; wants Apollo's branch-commit brief as a formal input.
- **Remote dispatch design** — content exists from the Tiny Steve
  collaboration; needs to move from session notes into the repo before
  anyone implements against it.
- **Contracts/goals doctrine** — what a contract asserts, what an
  attestation is worth, when a goal may stop.
- **CC-import mapping spec** — CC JSONL vocabulary → norn event vocabulary,
  with the lossy edges named explicitly.

### 2.4 Specialized agents worth building

- **Crash-window red-teamer.** Input: a protocol description (ordered
  durable writes). Output: the kill matrix and what breaks in each window.
  The child-persistence review found a Q2-breaking hole this way; productize
  the pattern.
- **Session-forensics reader.** Fluent in the event vocabulary; given a
  JSONL timeline, reconstructs what happened and flags ordering/durability
  anomalies. Doubles as the incident-response tool (the spark forensics were
  exactly this, done by hand).
- **Config-precedence auditor.** Given a config key, traces every layer to
  point-of-use and reports which source wins under which invocation. Would
  have caught the 272000 incident before it shipped.
- **Driven-conformance driver.** Speaks norn-driven/1 against a live
  process; asserts the protocol contract including every failure answer.
  Vespa's verification batches, productized.
- **Catalog-drift checker.** assets/models.json vs validation semantics vs
  docs; specifically watches for the multi-backend-model-id case that makes
  the over-max guard backend-blind.

### 2.5 Implementation constraints (standing, non-negotiable)

- House rules (CLAUDE.md): no clippy bypasses ever — fix the code; no
  invented defaults; no backwards-compat shims during the build; mod.rs =
  declarations only; <500 LOC per file; thiserror in lib, anyhow only at
  the binary top; lock poison always typed.
- **Every fix covers every invocation method** — TUI, print, driven (owner
  ruling, standing). A fix that lands in one mode is not landed.
- Persistence ordering doctrine: **reservation before artifact** — the
  durable record that a name/address is taken is written before anything
  keyed by it exists on disk.
- Event-schema changes: tolerant reader preserved; embedder-visible changes
  ship with a meridian pin-bump ticket in the same breath.
- Tests never `#[ignore]`; runtime-gate with an env var and a logged skip.
- Review gate: adversarial review at Fable tier before commit — never a
  lighter model (owner ruling). No finding is minor; everything is dealt
  with or explicitly ruled by the owner, nothing silently deferred.
- Brief discipline: numbered requirements, EARS specs, concrete acceptance
  criteria, foundation-first dispatch (AC → AP/AS → AE → AD/AT → AF/AN →
  AW/AR/AL/AU); the brief is authoritative over any structure annotation.
