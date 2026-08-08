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

### 1.1 RETRACTED IN FULL — the per-call intent signal **does** exist

> **🔴 THIS SECTION WAS WRONG. Owner correction, 2026-08-08.**
> Everything below the retraction line is preserved as the record of an error,
> not as a finding. **`tool_use_description` is a required, model-authored,
> per-call intent field on every tool call in norn**, and it is durably
> recorded. The corpus this design said did not exist has been accumulating
> all along.

**The mechanism, verified at the tree:**

- `tool/envelope.rs:22` — `pub const ENVELOPE_DESCRIPTION_KEY: &str =
  "tool_use_description"`, documented as **reserved across the whole tool
  surface**.
- `inject_envelope_fields` adds it to **every** tool's schema (and to every
  `oneOf` variant), typed `string`, described to the model as *"Brief
  description of what you are doing with this tool call and why."*
- **It is pushed into `required`.** The model must supply it on every call. This
  is not an optional annotation that agents may skip — it is mandatory,
  uniform, and already paid for.
- A sibling field `tool_use_metadata` is injected too: an optional object for
  *"tags, task references, or annotations"*. **A second signal this design had
  no idea existed.**
- `split_envelope_fields` strips both before the tool sees its arguments, and
  deliberately preserves a non-string description as its JSON rendering rather
  than dropping it (`envelope.rs:55-60`).
- Durability is covered by test:
  `child_tool_use_description_recorded_in_event_store`
  (`tools/agent/spawn/tests/permissions.rs:138`).

**Two real examples supplied by the owner**, which show the quality of the
corpus better than any description of it:

> *"Record implementation decisions, exact name substitution/validation,
> checker parity, gate results, and the out-of-fence stale-default test failure
> required by the brief."*

> *"Audit the final dirty tree, scope boundaries, diff size, and all untracked
> #136 deliverables before handoff."*

These state **purpose**, not mechanics. That is exactly the "song" `DESIGN.md`
posited — and `DESIGN.md:147` was **right** to say it already exists. The claim
retracted here is mine, not Pythagoras's.

**How I got it wrong, stated plainly because the shape matters.** I read
`BashArgs` — the per-tool, model-supplied argument struct — found no
description field, and concluded about the whole tool-call surface. But the
field is injected **at the envelope layer, generically**, so it cannot appear
in any per-tool struct and no amount of reading those structs would ever have
found it. **I measured one layer and stated a conclusion about another.** That
is the same *aboutness* error I made earlier the same day with haematite's
consumer set, and it reached a design document labelled CONFIRMED. The lesson
is not "check more carefully" — I checked carefully. It is **check that what
you measured is the thing you are about to make a claim about.**

**Consequence: R3 is withdrawn.** There is nothing to add and no token cost to
weigh. The signal is mandatory, free, high quality, and durable, and the design
should be built around it rather than around its absence.

---

<details>
<summary><b>Superseded text, retained as the record of the error</b></summary>

### The per-call intent signal does not exist — ~~CONFIRMED~~ **WRONG**

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

</details>

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
| **Per-call agent intent** (`tool_use_description`) | **Exists, REQUIRED on every call, durable** (§1.1) | **Strong** — model-authored purpose, free |
| Per-call open metadata (`tool_use_metadata`) | Exists, optional, **and the model is explicitly instructed to use it for "tags, task references, or annotations"** (§4.1.1) | **Potentially strong** — a task reference *is* a referent |
| Repo-relative path | Exists, robust (§4.2) | **Strong** — exact |
| Tool arguments (paths, commands, content) | Exists | Strong — the concrete referent of the intent |
| Import / dependency graph | Exists via `tools/ast.rs`, `tools/lsp/` | **Strong** — exact, structural |
| Tool name | Exists | Weak alone, useful as a filter |
| Session log (engagement, sequence) | Exists | Strong for §2.3 |

### 4.1.1 Two corrections found by auditing the field *next to* the one I got wrong

Method note, because it produced both of these in about two minutes: the
haematite seat's rule after any correction — ***audit the neighbouring field of
the same artifact, not the same field elsewhere, because the adjacent one is
where your attention isn't.*** Applied to §1.1's retraction, it immediately
found two things.

**(a) My claim about `tool_use_metadata` was wrong.** I wrote "nothing
populates it deliberately yet" — asserted, never checked, one field over from
the one I had just been corrected on. In fact `system_prompt/sections.rs:56-63`
**instructs the model to use it**:

> *"An optional `tool_use_metadata` object can carry **tags, task references,
> or annotations**."*

**A task reference is a referent**, which is precisely what a memory needs
(`MEMORY-AND-LANTERNS.md` §1). This may already be an anchoring channel. What
remains genuinely unknown is whether models *do* populate it in practice —
that is measurable against real session logs and has not been measured.

**(b) A framing effect on the intent corpus, which is the more important
find.** The same guidance tells the model what the description is *for*:

> *"briefly state what you are doing with this call and why. **This description
> is surfaced in the activity log and streaming indicator.**"*

**So the model writes it for a human watching a progress line.** §4.4.1 praises
this corpus as "purpose-shaped prose written at the moment of acting" — which it
is — but its shape is set by an audience it was told about, and that audience is
a live status display, not a future reader searching for prior work.

**Consequence for the design:** if intent text becomes the retrieval corpus,
either the guidance should say so, or we should expect status-update prose and
size our expectations accordingly. **This is not a defect; it is a fact about
what the corpus was optimised for**, and it was invisible to me because I
verified the field's *existence and mechanism* and never asked what it was
*for* — the adjacent property of the very field I had been corrected on.

**Do not change the guidance casually if we take this route.** Telling the model
its descriptions feed a memory system changes what it writes, estate-wide, on
every call — a larger intervention than it looks, and one that would invalidate
every measurement taken before it.

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

## 4.4 How resonance is actually measured

The owner's question. Answered in three parts: **what is compared**, **how the
comparison is scored without inventing weights**, and **how we know it works**.

### 4.4.1 What is compared

A lantern and the present moment are both, concretely, **a set of tool calls**.
Each call carries a required natural-language statement of purpose
(`tool_use_description`), the arguments it acted on, and the paths it touched.

So resonance compares **intent text against intent text**, anchored by
**paths against paths**. Not file contents, not diffs, not embeddings of code —
the sentences agents wrote about *why* they were doing what they did.

That corpus has properties worth noticing:

- **It is already written**, on every call, at no additional cost, and durably.
- **It is unusually clean.** Purpose-shaped prose, one or two sentences,
  written by the agent at the moment of acting, with no retrospection.
- **It is dense in rare, discriminating tokens** — `stale-default`, `scaffold`,
  `AWL`, `out-of-fence`, `handoff`, crate and symbol names.

### 4.4.2 A retrieval ladder, cheapest first

1. **Exact structural match.** Same repo-relative path. A fact, not a score.
2. **Structural neighbourhood.** Same crate; what this file imports (exact,
   from existing AST/LSP tooling).
3. **Lexical match over intent text.** A classical inverted index with BM25 or
   TF-IDF ranking over the `tool_use_description` corpus. **No embedding model,
   no GPU, no API, no vector store, no consistency problem** — and it is
   incrementally updatable, which suits an append-only source exactly.
4. **Semantic match (embeddings).** Buys paraphrase: *"fix the flaky test"*
   against *"stabilise the intermittent failure"*. Real, and the only arm that
   needs the machinery §5.2 describes.

**HYPOTHESIS, labelled as such and testable cheaply:** for *this* corpus,
lexical retrieval may outperform semantic retrieval. The discriminating signal
lives in rare technical tokens — identifiers, crate names, project jargon —
which embeddings tend to blur toward their nearest general-language neighbour,
and which BM25 weights *up* precisely because they are rare. This is a claim
about a specific corpus, not a general claim about retrieval, and the honest
way to settle it is to build arm 3 and measure §4.4.4 against arm 4 later.

**Consequence: the gap between "no similarity search" and "embeddings" is not
empty.** The earlier framing of this document treated arm 3 as the vector arm
and therefore as blocked. It is not. There is a substantial, cheap,
dependency-free retrieval arm sitting between them, and it operates on the
richest signal we have.

### 4.4.3 Scoring without inventing weights

A weighted score — `0.5 × structural + 0.3 × lexical + 0.2 × recency` — would
be three invented constants, which `CLAUDE.md` forbids and which nothing in the
system could ever justify.

**So do not combine them. Order them.** The arms are **categorical tiers**, not
addends:

> Surface exact structural matches first, then structural neighbours, then
> lexical matches, each tier exhausted before the next is considered, until the
> budget (§6) is spent.

Ranking *within* a tier uses that tier's own native measure — BM25 has its own
score, path distance is a count of hops — and **no cross-tier coefficient is
ever needed, because tiers are never compared to each other.** Brightness (§2)
acts as an ordering within a tier, not as a multiplier across tiers.

This is not a compromise. It is a better design than a tuned blend: it can be
explained to a user in one sentence, it degrades predictably, and every
"why did this surface?" has an exact answer.

### 4.4.4 How we know it works — the only real measure

**Engagement.** Did the agent do anything with what surfaced — read its detail,
follow it, fork to it? That is recorded in the log already (§2.3).

**This must be built in from the first version, not added later.** Without it
there is no way to distinguish resonance that helps from resonance that merely
costs tokens, and the failure mode is silent: a landscape of plausible-looking
memories that nobody ever uses looks exactly like one that works.

It also feeds §2.3's brightness inputs, so the measurement and the mechanism
are the same data. Nothing extra is stored to obtain it.

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
could coherently own, or is it structurally the wrong place for it?*

### 5.2 The engine owner's answer, and what it changes here

**Answered 2026-08-08. Summary: structurally the right place, and not built.**

- **No vector capability exists** — source sweep for ANN/embedding/similarity
  terms returns only unrelated senses of "embedding"; no `hnsw`/`faiss`/
  `usearch`/`annoy`/`simsimd` in any manifest. Independently re-verified by
  this seat. Caution recorded by both seats: haematite's docs use "vector" in
  a different sense entirely (`committed_root_vector() -> Vec<CommittedRoot>`,
  "the coverage vector") — those are `Vec<Hash>` per shard, **not embeddings**,
  and must not be read as roadmap.
- **The design thinking does exist** (`DATABASE-DIRECTIONS.md` §5) and already
  answers §5.1's three consequences: a vector index is itself a derived tree
  carrying the root `R` it was built at; a query is ANN over the index plus
  brute force over `diff(R, head)`. Fork is an O(1) metadata act, point-in-time
  search is querying any historical `(R, root)` pair, and merge is merge-then-
  fold. Paper only — never built, and the owner explicitly does not claim it
  is sound.

**The binding constraint for norn — CONFIRMED at haematite's bytes by this
seat, not taken on report:**

> There is no branch-head-advance event. The root-advance seam
> (`db/root_advance.rs`) is shard-level and its subscriber registry is a
> `Vec` of `Arc<dyn Fn(RootAdvance)>` callbacks, described in its own header
> as *"a DOORBELL, not a log… Nothing here touches disk, ever"* and
> *"never persisted, never replicated"*.

**Therefore: an index living in a different process from the writer is never
notified at all.** This is not a gap that later work closes; it is a different
problem. It constrains the resonance engine's **process topology**, which no
version of this design had considered: either the index is built in the
writer's process, or it polls, or it is rebuilt on demand. That question should
be settled before any daemon-shaped plan assumes notification is available.

### 5.2.1 Topology is a fork in the design, not a constraint — and one arm wins

The engine owner's follow-up correction: the three arms have **different costs
for norn, not different difficulties**, so this is a choice to be priced, not a
limitation to be worked around. Priced:

**What decides it is communality, not notification.** Lanterns are communal —
that is the design's central claim (`DESIGN.md` §Communal Memory) — and norn
runs many sessions concurrently. So every process needs the *whole* landscape,
which appears to force a shared index, which reintroduces exactly the
multi-writer contention that §2.2 had just removed.

It doesn't, because of what the index now is:

> The index is **derived and rebuildable** (§5). Therefore each process can
> hold its **own private in-memory view** over **shared append-only data**.
> There is no shared mutable index, so there is nothing to contend on.

Catch-up is *"read the records after my coverage point"* — which §5.3's field
makes exact, and which is idempotent and cheap because the data is append-only.

**This is why the process-local doorbell (§5.2) does not bind us.** We never
needed notification. A process catches up **at the boundary where resonance is
computed anyway** — between turns. There is no timer, no daemon, and
**no poll interval to invent**, which also settles the `CLAUDE.md`
arbitrary-values problem that a polling design would otherwise have created.

#### BINDING CONSTRAINT — there is no clock, and that is a guarantee

This must be implemented as a **property**, not left as a happy consequence.
Stated so an implementer cannot delete it without noticing it existed:

> **The resonance index SHALL advance only at the boundary where resonance is
> computed. It SHALL NOT be driven by any timer, interval, tick, or background
> task.** Freshness is bounded by *"as of the last turn boundary"* — a
> statement about causality, not about elapsed time.

**Why it must be written down:** a later reader, wanting it to feel faster,
adds a timer. Nothing breaks and nothing complains. What they have deleted is:
(a) the absence of an invented interval, which `CLAUDE.md` forbids and which no
error will ever surface; (b) the guarantee that a view's contents are a
function of *what has happened*, not of *when it was asked* — which is what
makes two agents at the same log position see the same landscape, and makes the
whole thing reproducible; and (c) the property that costs nothing when idle,
because nothing runs when nothing is being asked.

**A timer would not be an optimisation. It would be a silent downgrade from a
deterministic view to an eventually-consistent one**, and it would be
indistinguishable from working correctly right up until two agents disagreed
about the landscape.

**Arm A (index built in the writer's process, private) is the one that loses**,
and it loses on communality: a private index built only from *this* session's
work is not the landscape, it is a diary. Arm C (rebuild fully on demand)
degrades with history for no gain, since incremental catch-up is available at
the same place. What survives is arm B, but "polling" understates it — nothing
is being polled on a clock; a reader advances its own view when it next needs
it.

**Consequence worth stating plainly: the resonance engine does not need a
daemon.** Any plan that assumed one should be re-examined, and the assumption
that motivated it — that an index must be *told* about writes — was never true
for a derived view with a recorded coverage point.

**The one genuinely shared mutable thing left is the lantern log itself**
(appends from many processes). norn has already solved that exact shape and it
should be reused rather than reinvented — the "simplify first" rule applies
directly. `session/persistence/lock.rs` (H18) handles multi-process append plus
read-modify-rewrite over a shared `.jsonl`, and it is hardened in the ways that
matter: a **separate** lock file so the atomic rename-over never replaces the
locked inode, `flock` semantics that exclude other threads as well as other
processes, deadline-bound acquisition with a typed `IndexLockTimeout` instead of
an unbounded stall, and a process-local gate so local waiters hold no
descriptors.

**Honest scope note:** that module is `pub(crate)` and hardcoded to
`INDEX_LOCK_FILE = "index.lock"` (`lock.rs:46`, `lock_index` at `:123`). The
*pattern* is proven here; generalising it to a second log is small real work,
not free.

### 5.2.2 Graphs: the derived-tree contract generalises, but multi-hop does not

The owner asked whether haematite might be well positioned for graphs as well
as vectors. **Answered by the engine owner 2026-08-08, and the answer is more
useful than "yes".**

**The narrow question: the contract is structure-agnostic.** A derived tree is
*"an ordinary haematite prolly tree, owned by a layer, whose content is a
function of the golden database's content, carrying its own root `D` and its
coverage root `R`."* Three requirements, none of which mention rows, keys or
lookup. Their refusal registry explicitly declines to define index shape as
consumer-layer content. **A graph qualifies on exactly the terms a vector index
does** — and their own build list ranks a graph consumer *ahead* of vectors, as
the more natural first consumer rather than a speculative one.

**The constraint that actually matters, and it discriminates against graphs
specifically.** Their framework states normatively that
`R = [(shard_id, root_hash, advance_gen_seen)]` is a **per-shard vector**, and
that **there is no instant at which `R` is a globally consistent cut of the
database, and the framework must never claim one.**

- **For a vector index this is harmless.** Similarity is a per-item property:
  an item is either covered by `R` or it is in the diff. A skewed cut costs
  ranking at the margin.
- **For a graph it is not harmless, and the failure is not staleness.** An
  N-hop traversal crosses shards by construction. Reading edge `A→B` at shard
  1's coverage and `B→C` at shard 3's later coverage assembles a neighbourhood
  **that never existed at any single instant.** Not a stale answer — an
  *incoherent* one, and incoherent in a way that looks entirely normal.

**So the discriminator is not the derived-tree contract. It is the absence of a
cross-shard cut, and it bites multi-hop queries and nothing else.**

#### BINDING CONSTRAINT — shard count is a correctness property here, not a performance knob

norn has no haematite deployment today (§1.2, §5), so "single-shard" is not yet
a fact about us — **it is a choice we have not made.** That is precisely why it
must be recorded before it is made for unrelated reasons:

> **If norn ever stores a derived graph in haematite, the deployment SHALL be
> single-shard, and the reason SHALL be recorded at the configuration site.**
> With one shard, `R` is a single root and therefore a genuinely consistent
> cut, and point-in-time neighbourhood queries become exactly as sound as the
> vector story. With more than one, multi-hop traversal can return neighbourhoods
> that never existed.

**Why this needs writing down:** shard count reads like a throughput dial.
Someone raising it for entirely sensible performance reasons would silently
break multi-hop coherence, and the breakage produces plausible answers rather
than errors. The engine owner's documented alternative for anyone who needs
both is a **quiesced writer** for a cross-shard consistent cut — which is a
much larger commitment than it appears when written as a config change.

#### And the prior question, asked first: do we need a stored graph at all?

Applying §-the-premise rule before designing around any of it. **For v1, no.**
"Lanterns touching this file" is an index lookup. The import neighbourhood is
computable on demand from source we already have, using existing AST and LSP
tooling. **Computed on demand, there is no coverage vector, so there is no
coherence problem to solve** — the constraint above never engages.

A stored derived graph becomes justified only when on-demand traversal cost at
query time stops being acceptable, **and that is a measurement nobody has
needed to take.** This is the third time in one day the answer has been *the
mechanism is available and we may not need it* — after vectors and after
notification. The constraint above exists so that if we ever do need it, we
inherit the requirement rather than rediscover it.

### 5.3 Record the coverage point — adopted, with one translation

The engine owner's recommendation: build the external index recording **the
root it was built at**, even though nothing consumes that field yet. One field.
It converts *"the index may have drifted, rebuild it"* into *"the index is
exact as of `R`, and the gap is exactly `diff(R, head)`"*.

**Adopted — but translated, because v1 has no haematite in it.** The advice
assumes haematite is the source of truth; under §5 the sources of truth are the
session log and git. The faithful translation is:

> **The derived index records its coverage point against every source it
> derives from: the session-log position it has consumed up to, and the git
> commit its churn calculations were computed at.**

Same property, same cheapness, no haematite dependency. Staleness stops being
an unknown and becomes a computable difference. If the index later moves onto
haematite, the field gains a third component and nothing else changes.

This is also the exact form of the "name and bound the violation" this document
asked for in §5.1. The violation was never *"the index is stale"* — it was
**"the index's coverage is implicit and unrecorded."** Recording it is the fix,
and it is available now, at the cost of one field, before any of the rest of
this is built.

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
- **R3. WITHDRAWN — the premise was false** (§1.1). `tool_use_description` is
  already required on every tool call and durably recorded. Nothing to add,
  no cost to weigh. **Replaced by R8.**
- **R8.** Accept §4.4: resonance measured as **intent text against intent
  text, anchored by paths**, retrieved through **categorical tiers rather than
  a weighted score** (so no coefficient is ever invented), with **engagement
  logged from the first version** as the only real evidence it works. And
  accept the lexical arm (§4.4.2 step 3) as part of v1 — it needs no model, no
  vector store, and no engine capability that does not exist.
- **R4.** Accept §5: log-as-truth, no new storage engine for v1, index derived
  and rebuildable. **This is the same ruling already pending on
  `ANNOTATION-UNIFICATION.md` §5 and should be made once, for both.**
- **R5.** The resonance budget as a share of the context window — set the
  share.
- **R6.** Vectors: **answered in part** (§5.2) — haematite has none, the design
  for one exists on paper and is structurally the right home, and it is not
  built. The remaining ruling is whether arm 3 waits for that or proceeds on an
  external index. **Recommendation: wait.** Arms 1 and 2 do not need it, and
  building an external index now means building the thing haematite's own
  design says should be a derived tree.
- **R7.** **Process topology** (§5.2, new — no prior version of this design
  considered it). An out-of-process index cannot be notified of writes by any
  mechanism that exists. **Priced in §5.2.1, and it resolves:** each process
  holds a private derived view over shared append-only data and advances it at
  the turn boundary. No shared mutable index, no notification needed, no daemon,
  and no poll interval to invent. **What needs ruling is the consequence, not
  the choice: any daemon-shaped plan for this feature should be re-examined,
  because the assumption motivating it was never true.** The lantern log's
  multi-process appends reuse the existing H18 lock pattern rather than a new
  mechanism.

## 8. Recommendation

Build arms 1 and 2 — exact path match and structural neighbourhood — with
churn-based brightness computed at read time, notes as separate objects
anchored to lanterns, and no new storage engine. That is a complete, useful
feature with no embedding model, no invented constants, and no consistency
problem. Then measure engagement from the log, which is the only real evidence
that any of this works, and let the observed gap decide whether arm 3 and the
intent field are worth their cost.
