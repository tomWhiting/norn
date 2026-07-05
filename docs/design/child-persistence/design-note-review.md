# Review: child-persistence design note

Reviewer: Sable Nightwick (Fable) · 2026-07-05
Subject: `docs/design/child-persistence/V2-design-note.md` (Dr. Spaceman)
Process: my line-by-line pass, then an independent Fable red-team instructed to
attack every ruling below. Red-team findings are folded in and credited inline.

Verdict up front: **all five decisions APPROVED, four with amendments, plus one
ordering change I am holding the design on (§7) and one design gap that must be
answered before implementation starts (§8).** The six brief-premise corrections
(C1–C6) all verified against source — good forensics; the briefs are amended to
match (C1 LOC figures exact: spawn.rs 430, fork_pipeline.rs 364).

## 1. D-b — split fork.rs into fork_context / fork_outcome, spawn unsplit: APPROVED

Line ranges verified (`build_fork_context` + `resolve_fork_store` at 99–253;
outcome cluster 257–532, `classify_step_result` file-private). Two riders:

- `fork_tool.rs:1405` (test) imports `SharedSessionTree`, so this split and the
  D-r7 delete touch the same file's imports. Land them in ONE commit — the
  intermediate state churns otherwise. The §7 sequencing note in your design is
  the only thing preventing a three-way merge mess with the agent-variants
  edits; keep it strict.
- `fork_context.rs` importing `ForkOutcome` from its sibling is fine as the
  single link — don't let it grow past that.

## 2. D-r7 — delete SessionTree: APPROVED

C3 verified; `tree.rs:10-11` self-documents as no-persistence/no-replay. On the
tree-sessions vision: the vision's tree is the ON-DISK parent/child linkage this
design creates (children/ layout + ChildBranch events + rel_path/parent_id index
columns). A future in-memory view gets rebuilt from branch events — which is
MORE aligned with "the registry is the timeline" than resurrecting a
never-published `Arc<RwLock>` structure. No foreclosure.

State in the impl plan: deleting `SessionEvent::Fork` (the variant, not just
tree.rs) is safe for old files only because Fork was never production-emitted —
the tolerant reader (`io.rs`) skips unknown variants, so a stray test-era file
degrades gracefully.

## 3. D-event — ChildBranch event + ForkComplete goes Option: APPROVED, with a schema amendment

Old persisted `ForkComplete` events always carry `forked_session_id` →
deserialize as `Some(..)`; no old-file breakage.

**AMENDMENT (red-team, I concur — this one's load-bearing): ChildBranch must
carry the minting parent's session id.** Fork seeding (§2.2 step 3) copies the
parent's history INTO the fork child — including the parent's ChildBranch
events. When that child later replays its own log to build its ever-used name
set (§4 step 1), it collects the *parent's* children's names from inherited
events. With only `{path_address, child_session_id, anchor, kind}` there is no
way to tell "I appended this as parent" from "I inherited this in my seed"
except fragile path-prefix matching. Add `parent_session_id` (or filter the
seed copy — but the field is the honest fix). Without it, per-parent scoping
silently becomes per-lineage over-reservation, and any future
"list-my-children" projection misattributes.

## 4. D-idspace — unify the two id spaces: APPROVED, CORRECTED to UUIDv4

The note's C6/§6.1 premise ("campaign vision fixes storage identity as UUIDv7")
is stale vision text and **contradicts the note's own brief**: R8 is RULED (Tom,
2026-07-04) — default generation is UUIDv4; v7's shared timestamp prefix defeats
git-style short-prefix eyeballing. Unify on **v4**.

Verified nothing depends on v7 time-ordering: `reserve` already mints
`Uuid::new_v4()` (registry.rs:497), no production code sorts by session id
(one test at manager.rs:1228), filenames key on path slugs, latest-session
resolution uses index timestamps. Sweep in: `types.rs:140` doc comment still
says "UUID v7 identifier" — fix with R8.

## 5. D-durability — ChildDurability two-variant axis: APPROVED, one edge to close

C4 verified (`store.rs` variants are fsync cadence, all sink-assuming).
**AMENDMENT: specify Persist-requested-under-an-ephemeral-parent.** Propagation
is one-way (Ephemeral forces Ephemeral down), but a persistent child of a
sink-less root has no `<root-uuid>/` dir and no root index row to hang off.
`branch_child` must handle this TYPED — either force-Ephemeral with the honest
`session: None` event to the in-memory parent store, or a typed error — never
discover it as a missing-directory failure.

## 6. Addenda (both confirmed)

- **ForkComplete Option change → meridian pin-bump ticket.** It's an
  embedder-facing event schema change (post-3cac008 these are contract);
  breaking for consumers, non-breaking for old session files.
- **Ephemeral children reserve names durably — state it as an INVARIANT, not a
  nicety.** §2.2 step 1's parent-store append is the ONLY durable trace an
  ephemeral child leaves. If an implementer "optimizes" step 1 away,
  for-all-time uniqueness silently breaks for ephemeral names.

## 7. HOLD — child-first write ordering breaks Q2 in the crash window

This is the one finding I'm holding the design on (red-team's attack; I
verified the window). §2.2 writes the durable name-reservation record (parent's
ChildBranch, step 6) LAST, but the durable artifacts keyed by that name (child
file, steps 3–4; index row with that rel_path, step 5) FIRST. kill -9 between
steps 5 and 6:

- Child file + index row exist under slug S; parent log has no reservation.
- Parent resumes, replays its log (§4) → S absent → same short name re-mintable
  → `branch_child` computes the IDENTICAL path `children/S.jsonl`. Step 2 only
  "creates the dir if absent" — no collision check. The new JsonlSink either
  truncates the orphan (data loss) or appends to it (two agents interleaved in
  one file), plus a duplicate rel_path index row. That is precisely "an old
  persisted address resolving to a different agent" — the thing Q2 was ruled to
  prevent.
- §5.3 defers orphan recovery to "let the kill-9 AC test decide" — but that AC
  kills MID-EXECUTION, after mint completed. It never exercises this window;
  the deferral guarantees the hole ships untested.

The child-first rationale ("orphan indexable from its own header — fail-safe
not fail-lost") protects a child that at crash time contains one header event,
i.e. nothing. **Invert to parent-first**: append ChildBranch, then child
file/index. Crash residue becomes a burned name + dangling child reference —
exactly the dangling case the ForkComplete-Option honesty machinery already
forces resume paths to tolerate, and Q2-safe by construction. If child-first is
kept for a reason I'm not seeing, the floor is: `branch_child` treats "slug
file or rel_path row already exists" as a hard typed error at mint.

Shared caveat worth one sentence in the note: under `DurabilityPolicy::Flush`
the parent append isn't fsynced, so the reservation is only as durable as the
parent's cadence — true of both orderings.

## 8. REQUIRED BEFORE IMPLEMENTATION — name one allocation authority

The name-uniqueness enforcement point is currently split across three
unsynchronized structures: the live `path_index` (checked by
`AgentRegistry::reserve` under the registry write lock), the tombstone index
(NOT checked by reserve at all; latest-wins insert overwrites), and the new
per-parent ever-used set replayed from the parent log inside `branch_child`
(lock unspecified). Worse, the registry's documented doctrine is the OPPOSITE
of Q2: "terminal transitions remove the path from the live index (so the path
is reusable)" (`get_terminal_by_path` doc), with reuse-disambiguation machinery
built around it. And the note carries two allocation narratives (§2.2 step 1:
branch_child allocates; §6.3: the registry populates at reserve).

Two parallel spawns under one parent racing check-then-append is a live bug
unless ONE designated authority holds ONE lock across check + parent-log append
+ registry insert. The design must name that authority, and the "path is
reusable" registry doctrine (plus its reuse fallbacks) must be explicitly
REMOVED, not left contradicting Q2 from underneath.

Minor, same area: §5.2's downgrade-safety claim cites the wrong reader —
`io.rs:289-313` is the session-event reader; index rows are parsed by
`read_index` (`index.rs:42-67`). The conclusion survives by accident (serde
skips unknown fields; no `deny_unknown_fields`), but an old binary resolves a
child row to the flat `data_dir/{id}.jsonl` which doesn't exist — child
sessions are phantom rows on old binaries. Moot under no-backwards-compat, but
don't cite evidence that doesn't support the claim.

## Next step

Spaceman: fold §3's schema amendment, §5's edge, §7's ordering inversion (or
argue for child-first + mandatory mint-collision hard error), and §8's single
allocation authority into the note, then it's GO from me. §7 and §8 are the
two I need to see answered in writing; everything else is mechanical.
