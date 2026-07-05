# Fleet Playbook — getting Fable-grade output from Opus-grade fleets

Written 2026-07-04 at Tom's request, from the methods used in the norn hardening,
monitor-stack, and meridian norn-integration campaigns. The premise: the orchestrator's
diligence can substitute for a lot of per-agent diligence, **if** the structure forces
errors to surface. None of this requires a smarter model — it requires refusing to let
"compiles and tests pass" stand in for "correct."

## 1. The failure taxonomy (what actually goes wrong)

Every serious defect in these campaigns fell into a small number of shapes. Name them,
hunt them by name.

1. **Symbol-level migration.** Code ported across an API change by renaming things until
   the compiler stops complaining, without re-reading the contract underneath. *Case:
   meridian's LSP monitor was ported across the chiron migration — compiles fine,
   restarts crashed servers without `record_crash()`, silently resyncs an empty document
   set, crash-loops every 2s forever.* The compiler proves shape, never meaning.
2. **Gate on a forgeable signal.** A security check keyed on data an attacker (or a
   permissive auth mode) controls. *Case: a provision gate keyed on `AuthUser.is_admin()`
   — but loopback auth maps ANY `X-Member-Id` to an elevated Admin, so the gate was
   theater in exactly the deployment it defended. The fix: gate on the storage-resolved
   role, the same unforgeable source everywhere.*
3. **Enforcement in a dead branch.** The check exists, is unit-tested, and never runs —
   because it sits after a routing fork that bypasses it. *Case: a join gate placed
   inside the in-process match arm when every real deployment took the HTTP-routed fork
   above it. Its tests passed because they called the helper directly.*
4. **Tests that lock in the wrong model.** A test asserting the implementer's
   misunderstanding, so the suite defends the bug. *Case: a translate test asserting
   per-provider-call turn-finalization — norn emits Done per call, not per turn; the test
   enshrined the wrong ordering.*
5. **The fix round's own bugs.** Fixes are new code written under pressure with narrower
   review. *Case: the turn-boundary fix corrected duplicate usage events and thereby
   broke the context gauge (~N× overstatement) and created an aborted-spend black hole.
   Case: an ambient `Utc::now()` introduced BY a fix round time-bombed four fixed-date
   tests to start failing at 12:02 UTC the same day.*
6. **Documented invariants that are false on the default configuration.** Rustdoc says
   "no-op on clean runs"; default-on auto-compaction makes every clean run non-clean.
   The doc is part of the diff — review it against the defaults, not the happy path.
7. **Silent sibling paths.** The gated command has an ungated twin. *Case: membership
   insertion gated on `workspace_add_member` while `member_register{workspace_id}` and
   the provision endpoint did the same thing ungated. Always sweep for every caller of
   the sensitive primitive, not just the one in the finding.*
8. **Designed, documented, never wired.** A feature with types, docs, and config threading
   that no call site activates. *Cases: an LSP pool module never declared in any mod.rs;
   a documented 3-layer permission model stubbed `|_| true`; confinement machinery no
   agent-build path turns on; a `data_dir` threaded everywhere and read nowhere.* Grep
   for consumers of everything that claims to be load-bearing.

## 2. The loop that catches them

**Implement (Opus) → adversarial review (best available) → fix (Opus) → RE-VERIFY → commit.**

Non-negotiables learned the hard way:

- **Every fix round gets re-verified.** Three of the worst bugs in these campaigns were
  introduced by fix rounds. A fix round that isn't re-reviewed is unreviewed code on the
  most sensitive paths you have.
- **Reviews are adversarial, not confirmatory.** The prompt is "try to break it / try to
  refute it," never "check this is okay." Give the reviewer the exploit *shapes* to
  attempt (see §4). A reviewer told to hunt bypass-via-loopback found it twice.
- **Empirical proof beats plausible reasoning.** When a reviewer claims a deadlock or a
  hang, require a repro (the >64KB pipe deadlock was proven with 1MB through `cat`, then
  the fix re-proven at 2MB with an amplifying filter — both are now permanent regression
  tests). When an implementer claims "already delivered live," make the reviewer verify
  the delivery mechanism, not the claim.
- **Expect NOT READY — as the prior, not a quota.** In the monitor-stack campaign every
  single unit failed review at least once; that is the process working, and a campaign
  where everything sails through first review should make you suspect the reviewer
  before the implementer. But never make findings *obligatory*: a reviewer who must
  find something to look credible manufactures marginal findings, and that noise
  trains orchestrators to discount reviews — the opposite of the goal. A clean READY
  on a small, well-specified diff is valid WHEN the verified-clean list proves the
  attack surface was actually walked (name what was checked and how, not just "looks
  good"). Calibrate reviewers in both directions: track misses AND false positives.
  (Amended 2026-07-04 after Dr. Spaceman's pushback — accepted.)

## 3. Briefs for implementers (how to prompt Opus)

The single biggest lever. Opus executes well-specified work excellently and fills
underspecified gaps with plausible-looking guesses. So:

- **Exact scope, stated twice.** "Your scope is findings A1 and A2 ONLY; do not fix
  anything else you notice — the rest is owned by other waves." Strict ownership prevents
  both collisions and scope-creep guesswork.
- **File:line anchors for every claim.** Not "the gate in the workspace tool" but
  `workspace/mod.rs:426-459`. If you don't know the line, say what to grep for.
- **State the semantic contract, cite the source, and require re-verification.** "Norn
  emits Done per provider call BEFORE that call's tool results — verify against
  classify.rs:198-215 and tool_dispatch.rs:307 before coding to it." Making the agent
  re-derive the contract from source catches your own brief's errors (twice, an
  implementer correctly corrected my brief: a nonexistent accessor name, a wrong
  chokepoint count).
- **Enumerate the standards inline.** Don't rely on CLAUDE.md ambiently: repeat the five
  that matter for this diff (no unwrap/expect in prod, no #[allow] outside cfg(test), no
  swallowed Results, no invented constants, failure-path tests).
- **Name the verification gates and make the agent run them.** Exact commands, "all must
  pass before you finish," and require the tails pasted in the report. Agents that must
  paste gate output don't skip gates.
- **Forbid the known traps explicitly.** "Never run bare `cargo update`" (it re-breaks a
  pinned dep). "You are the ONLY builder" (concurrent cargo corrupts the shared target
  dir). "Do not commit — leave the tree for review."
- **Demand a structured report:** what changed per finding, test names, evidence
  citations, gate tails, "anything you found that changes the brief's understanding."
  That last field is where implementers surface your mistakes — reward it.
- **Decisions you haven't made, name as decisions.** "Decide X per the existing model's
  semantics and WRITE DOWN the rule you chose" beats letting the choice happen silently
  inside the diff. And where only the owner can rule (an invented default, a security
  posture), instruct: record it, don't invent it.

## 4. Briefs for reviewers (how to prompt the adversary)

- **Give attack vectors, not checklists.** "Hunt for any remaining ungated path to
  workspace membership mutation — other tool commands, other server routes, service-layer
  callers that skip the gate" produces findings; "check the auth is correct" produces
  LGTM.
- **Ask the identity question always:** *is the signal this check keys on forgeable by
  the caller, in every auth/deployment mode?* This one question found the two worst
  security bugs of the campaign.
- **Ask the reachability question always:** *does this code actually run in the
  deployments that matter?* (Dead-branch gates, inert machinery, `|_| true` stubs.)
- **Make the reviewer run the gates independently.** Implementers' pasted output is
  honest but stale; trees drift.
- **Require the verdict format:** READY / NOT READY + findings with file:line, defect,
  concrete failure scenario, severity. "Concrete failure scenario" forces the reviewer to
  prove exploitability instead of pattern-matching style nits — and it's what makes the
  fix brief writable.
- **Ask for "verified clean" too.** A list of what was checked and held prevents the next
  round from re-litigating settled ground, and tells you the review actually covered the
  surface.
- **Fresh reviewer per unit; same reviewer across rounds of a unit.** Continuity within a
  unit (they remember what they found); fresh eyes between units (no accumulated trust).

## 5. Orchestrator disciplines (your job, not the agents')

- **You are the memory.** Agents die, sessions compact, laptops close mid-flight. Keep
  ground truth in committed artifacts: a findings doc in-repo, decision items with
  explicit owner-confirm markers, memory files updated at every landing. After any
  interruption: re-establish disk ground truth (git status/log) before believing anything
  from a transcript.
- **One builder per target dir, ever.** Parallelize *reviews* of disjoint scopes freely;
  serialize anything that runs cargo on the same tree. Stuck-cargo deadlocks and corrupt
  target dirs cost more than parallelism buys. *Sanctioned escape hatch (added
  2026-07-04, Dr. Spaceman's pushback — accepted): git worktrees with separate
  `CARGO_TARGET_DIR` give genuinely parallel builders with zero corruption risk — the
  rule is one builder per target dir, not one builder per repo. Cost it first: each
  target dir in this workspace runs 30-60G+, and disk crises are not hypothetical
  (2026-07-04: 3.5G free mid-campaign). Measure free disk, budget one full target per
  concurrent builder, and tear worktrees down after landing. The systemic fix for
  build contention remains the shared diagnostics job server (roadmap).*
- **Exit codes are the only truth.** `cargo … | tail` masks failure ($? is tail's).
  rust-analyzer diagnostics mid-fleet are stale noise. If a suite "passed" through a
  pipe, re-run it capturing the real exit code.
- **Never let a fix be described as bigger than it is.** "A2 fixed" when the machinery is
  inert until a config is wired is a lie in a commit message; write "machinery landed;
  enforcement pending decision." The audit trail must survive being read by someone
  who wasn't there.
- **Route every invented number to the owner.** Timeouts, limits, backoffs, channel
  capacities: factual (from a catalog/spec) or owner-ruled, else it's a flagged decision
  item — with the recommendation attached so the owner can rubber-stamp instead of
  research. Corollary, owner-supplied: **a limit an agent doesn't know about is an
  assassination, not a limit** — any cap (turns, TTL, timeout) must be visible to the
  agent it governs, ideally with an approaching-limit signal.
- **Log lessons where the next campaign will find them** (this file, DECISIONS docs,
  memory). A lesson that lives only in a transcript is already lost.

## 6. When to spend the expensive model

If Fable-tier attention is scarce, spend it where errors are *quiet*: review of
security-sensitive diffs, event-ordering/concurrency semantics, anything whose failure
mode is silent corruption rather than a loud crash. Opus with a prescriptive brief is
fully adequate for: well-specified implementation against a written contract, mechanical
migrations with verification gates, test authoring against a stated model, doc
regeneration. The structure above — contracts cited, gates mandatory, adversarial
review, re-verify fix rounds — is precisely the scaffolding that lets the cheaper tier
carry the load without the quiet failures getting through.
