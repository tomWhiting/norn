# Resonance: how lanterns are found

**Author:** Sable Nightwick
**Date:** 2026-08-08
**Status:** Design response to owner steer — awaiting rulings in §7
**Supersedes:** parts of `DESIGN.md` (Pythagoras, 2026-05-17) as marked

`DESIGN.md` was written before haematite existed and before this codebase had
the session log it now has. Its vision holds. Three of its mechanisms do not,
and the owner's steer of 2026-08-08 corrects two of them directly. This
document works the third — **access** — which the owner named as the real
open problem.

---

## 1. Two premises checked before building on them

### 1.1 The per-call intent signal does not exist — CONFIRMED

`DESIGN.md` §Singing (line 37) assumes every tool call emits "brief intent
(derived from the tool call's purpose)", and §Integration (line 147) states
flatly:

> **Tool descriptions:** Already emit metadata that forms the agent's song.

**This is false.** Verified in the tree:

- `Tool::description()` (`tools/bash/tool.rs:268`) returns
  `include_str!("../guidance/bash.description.md")` — a **static per-tool**
  string. Identical for every call. It carries no information about *this*
  invocation, so as a resonance signal its discriminating power is zero.
- `BashArgs` (`tools/bash/tool.rs:49-64`) has `command`, `timeout`,
  `working_dir`, `run_in_background`, `watch`. **No description or intent
  field.** The same holds across the file and search tools.
- The only agent-authored intent string anywhere in the tool surface is
  `WatchSpec.brief` (`tools/bash/tool.rs:70`), scoped to background watches.
- `action_log` is a **query** tool (`tools/action_log.rs:94-109`) — it reads
  the log, it is not a place agents write intent.

This is the fourth instance today of a now-familiar shape: **prose asserting a
capability the mechanism does not implement.** It belongs with `MissingNode`,
`Lww`, and haematite's `field order` comment. See
[`haematite-write-discipline`] in the coordinator's memory for the class.

**Consequence:** the "song" as specified cannot be built. Either resonance
runs on signals that *do* exist (§4), or a per-call intent field is added to
the tool schemas — a real change, touching every tool, and an owner decision
(R3).

### 1.2 Grafeo remains unverified

`DESIGN.md` §Storage names Grafeo as the store. Its concurrency and
embedded-writer properties **have never been checked by this seat.** haematite
did not exist when that choice was made. §5 argues v1 needs neither.

---

## 2. Decay: the owner's inversion, and why it is also the cheaper design

`DESIGN.md` line 53: *"brightness that decays over time unless reinforced."*

**Owner steer:** decay must not be a property of time. It should follow
revisits and further work, with version control integrated. Returning to the
earliest work, untouched since, it should *still resonate* however long ago it
was.

That is correct, and the reason is that **dimming was never a claim about
truth — it is noise control for ranking.** `DESIGN.md` says so itself (line
58: "prevents the landscape from becoming uniformly bright and meaningless").
Once dimming is understood as ranking rather than decay, the right variable is
not age but **supersession**: has later work made this lantern describe a world
that no longer exists?

And supersession has a direct, measurable proxy already sitting in the repo:
**commits touching the lantern's referenced paths since it was lit.**

### 2.1 Properties

| Situation | Commits touching those paths | Result |
|---|---|---|
| Earliest work, never revisited | 0 | **Full brightness, forever** |
| Area rewritten repeatedly | many | Dims — it describes a vanished state |
| Area actively worked right now | recent | Dims, but engagement (§2.3) lifts it |

The owner's requirement — *old untouched work still resonates* — is satisfied
**by construction**, not by tuning. There is no half-life to pick.

### 2.2 Three consequences worth stating

1. **Nothing is stored and nothing is written.** Brightness is computed at read
   time from git plus the session log. This preserves the position already
   argued in `ANNOTATION-UNIFICATION.md` §4 (decay computed, not stored) and
   strengthens it: the highest-frequency mutation path in the whole memory
   design disappears, and with it the entire haematite A4 single-writer
   concern for this feature.
2. **It contains no invented number.** Per `CLAUDE.md`, a half-life or decay
   constant would be an arbitrary value. A count of commits touching a path is
   a **measured fact**. This is the difference between a default that is
   factual and one that is guessed.
3. **`DESIGN.md` already had the right idea, filed as a minor question.** Open
   Question 4 asks "when a lantern's referenced files have changed
   significantly, should it auto-dim?" The owner's steer promotes that from a
   footnote to *the* mechanism and deletes the time-based headline.

### 2.3 Two further inputs, both derivable from the log

- **Engagement brightens.** An agent that reads a lantern's detail or forks to
  it has demonstrated the lantern was worth surfacing.
- **Surfaced-and-ignored dims.** `DESIGN.md` line 49 already proposes this.
  It is a **weaker** signal and should be treated as such: an agent may ignore
  a lantern because it has already absorbed it, which is success, not noise.
  Recorded here as honestly weak rather than quietly weighted.

### 2.4 Where the churn proxy misleads — stated, not papered over

- **Reformatting, renames and file moves are churn without staleness.**
  Rename-following (`git log --follow`) and whitespace-insensitive comparison
  (`-w`) mitigate this. A mass reformat still counts and will wrongly dim.
- **A decision can be superseded by a change somewhere else entirely.** An
  architecture lantern is invalidated by a change in a file it never
  referenced. Path churn cannot see this. **This is a real gap with no
  proposed fix** — flagged rather than hidden.

Churn is a good proxy. It is not the thing itself.

---

## 3. Notes and lanterns: related, not the same

**Owner ruling 2026-08-08:** they are not the same thing, but they are
related. This lands **against** the unification proposed in
`ANNOTATION-UNIFICATION.md` §1, which is recorded there.

The distinction that survives scrutiny is **authorship**:

- A **lantern** is *involuntary*. The runtime lights it on detecting a
  significant episode (`DESIGN.md` line 100: "created by the runtime, not the
  agent"). It asserts: *significant work happened here.*
- A **note** is *voluntary*. An agent or person writes it deliberately. It
  asserts: *I want to tell you something.*

**The relation:** a note needs a place in space and time — the owner's own
framing. A lantern is exactly that place. So **notes attach to lanterns.** A
lantern may carry no notes (a purely runtime-detected episode). A note is never
free-floating; the lantern supplies its coordinates.

### 3.1 The argument that decides it — from the owner's own decay steer

If notes and lanterns were one object they would share one decay rule, and one
of them would be wrong:

- **Churn should dim a lantern.** It describes a state of the code, and the
  code moved.
- **Churn must not dim a note.** A note explaining *why* a thing was done is
  often **more** valuable after a rewrite, not less — it is the only surviving
  record of the reasoning the rewrite may have discarded.

The owner's two instincts support each other: separating notes from lanterns is
what makes the churn-decay rule safe to apply. Merging them would have forced
churn onto exactly the content that should outlive churn.

---

## 4. Access — the actual problem

### 4.1 Signals that exist today (verified)

| Signal | Status | Strength |
|---|---|---|
| Repo-relative path | **Exists**, robust (§4.2) | **Strong** — exact |
| Tool name | Exists | Weak alone, useful as a filter |
| Import / dependency graph | Exists via `tools/ast.rs`, `tools/lsp/` | **Strong** — exact, structural |
| Session log (engagement, sequence) | Exists | Strong for §2.3 |
| Per-call agent intent | **Does not exist** (§1.1) | — |

### 4.2 Identity normalisation across worktrees and clones — VERIFIED

The owner flagged that this must survive worktrees and different directories.
Measured directly, by creating a second worktree and comparing:

```
                    main checkout                worktree
--show-toplevel     …/stack/norn                 …/stack/norn/.worktrees/probe   (differs — correct)
--git-dir           .git                         …/.git/worktrees/probe          (differs — WRONG key)
--git-common-dir    .git                         …/stack/norn/.git               (same within a clone)
root commit         f01f2e3a…                    f01f2e3a…                       (same, always)
```

**The rule:**

- **Repo identity = the root commit hash** (`git rev-list --max-parents=0`).
  Invariant across worktrees *and* across clones — which matters, because this
  estate had two clones of norn until one was deleted last week, and lanterns
  lit in one must resonate in the other.
- **Location = path relative to `--show-toplevel`.** Verified: a file in the
  worktree and the same file in the main checkout both relativise to
  `crates/norn/src/lib.rs`.
- **Do not use `--git-dir`** — it differs per worktree and would split a
  worktree's lanterns from its own repository's.

**Edge cases, named:** a repository with multiple root commits (grafted or
merged histories) yields more than one candidate and needs a deterministic
choice; a history rewrite changes the identity and orphans every lantern. Both
are real; neither is solved here.

### 4.3 The retrieval arms, in order of cost

1. **Exact path match.** Lanterns lit on the file being edited. No model, no
   index tuning, no threshold. If an agent is editing `lock.rs`, the lanterns
   lit on `lock.rs` are relevant — this needs no cleverness and no argument.
2. **Structural neighbourhood.** Same directory, same crate, and — the useful
   one — **what this file imports**, available exactly from the existing AST
   and LSP tooling. This is better than vector similarity for the near case
   *and* it is exact rather than approximate.
3. **Conceptual similarity (vectors).** Buys the genuinely different case:
   *someone solved this class of problem elsewhere in the tree.* Real value,
   but second, and it carries a consistency problem (§5.1).

**Arms 1 and 2 need no embedding model and no vector store.** They can be built
now. Arm 3 is a separate decision.

---

## 5. Storage: v1 needs no new engine

If location is a repo-relative path and brightness is computed from git, then a
lantern is: an anchor (repo, path, commit), a summary, and a session reference.
That is a small amount of structured data, and the session log already records
the events it is derived from.

This is the **log-as-truth** position already put to the owner
(`ANNOTATION-UNIFICATION.md` §5): the log is the source of truth and any index
is a **derived, rebuildable view**. Under it:

- Grafeo is not required for v1.
- haematite is not required for v1 either — it becomes an **optional
  accelerator**, not the store of record.
- Rebuilding the index is always available as the repair path, because nothing
  authoritative lives only in it.

This is the same architecture the owner already has in front of him. The two
questions should be ruled together, not separately.

### 5.1 If vectors are added later, the boundary problem must be named first

An external vector index does not participate in haematite's branch, fork and
merge semantics:

- **Fork a branch** → the index does not fork; the child retrieves neighbours
  for records its branch does not contain.
- **Merge** → the index has no merge; it is rebuilt or it drifts.
- **Time-travel to a pinned root** → the index is still at HEAD, returning
  neighbours that do not exist in the pinned state.

All three are one shape: **a derived view that does not honour the boundary its
source honours.** Referred to haematite's owner (Apollo Biscuit) with the
question put as *is branch-consistent similarity search something haematite
could coherently own, or is it structurally the wrong place for it?* — because
an ANN index is global over all vectors, and "global" and "per-branch" are
close to contradictory. If the answer is "wrong place", arm 3 proceeds with the
violation **bounded and documented**, not discovered later.

---

## 6. The budget problem, and a factual answer to it

`DESIGN.md` line 151: *"At most K lanterns resonate per turn (configurable,
default TBD)"*, and line 49: an adaptive threshold. Under `CLAUDE.md` both are
**invented values** and this seat may not pick them. But leaving the feature
silently disabled is equally forbidden.

There is a factual source. Express the budget not as a count of lanterns but as
a **share of the context window** — and the context window is a fact from the
generated model catalog, which `CLAUDE.md` names explicitly as a legitimate
factual default. A token allowance is measurable, scales correctly across
models, and degrades honestly: when lanterns do not fit, fewer surface, and the
count was never a magic number.

The **share** itself still needs an owner ruling (R5). It is one number, and
it is his to set rather than mine to guess.

---

## 7. Rulings sought

- **R1.** Confirm §3: notes and lanterns are separate objects, notes anchored
  to lanterns, and **churn-decay applies to lanterns only.** (Owner has ruled
  the separation; the decay asymmetry is the consequence and needs confirming.)
- **R2.** Accept §2: decay computed from commit churn on referenced paths,
  never from elapsed time, never stored. Accept the §2.4 gap as a known limit.
- **R3.** Add a per-call intent field to the tool schemas, or build resonance
  on path and structure alone? §1.1 shows the "song" cannot be built as
  specified without this. **Recommendation: build arms 1 and 2 first without
  it** — the intent field costs tokens on every call estate-wide and should be
  justified by a retrieval gap actually observed, not assumed.
- **R4.** Accept §5: log-as-truth, no new storage engine for v1, index derived
  and rebuildable. **This is the same ruling already pending on
  `ANNOTATION-UNIFICATION.md` §5 and should be made once, for both.**
- **R5.** The resonance budget as a share of the context window — set the
  share.
- **R6.** Vectors: defer arm 3 pending haematite's owner (§5.1), or commit now
  to an external index with the boundary violation documented?

## 8. Recommendation

Build arms 1 and 2 — exact path match and structural neighbourhood — with
churn-based brightness computed at read time, notes as separate objects
anchored to lanterns, and no new storage engine. That is a complete, useful
feature with no embedding model, no invented constants, and no consistency
problem. Then measure engagement from the log, which is the only real evidence
that any of this works, and let the observed gap decide whether arm 3 and the
intent field are worth their cost.
