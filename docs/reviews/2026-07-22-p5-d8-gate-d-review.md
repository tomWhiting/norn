# P5 D8 prompt-authority Gate D review

**Date:** 2026-07-22

**Reviewer:** Sable Nightwick (external Gate D coordinator)

**Handoff:** [`2026-07-22-p5-d8-gate-d-handoff.md`](2026-07-22-p5-d8-gate-d-handoff.md)

**Reviewed boundary:** base `05be10c40981460c00fed9acc306938ff93b40b2`
(tree `266a3b5543c50cbe8d32dca9e6397f2fad86a3a4`), frozen source
`4fa6c6756ed497a002b4281f51cbb14f7bd7a3eb` (tree
`c0d9f69bb5283184432862016c1212644f7088c2`), branch head `6b1ae93` with zero
Rust changes after the freeze (verified). `main` untouched by this review.

**Panel:** four Opus 4.8 area seats (authority provenance; provider
projection/seed; transport lowering/MCP; cache/timeout durability), one
cross-model adversarial seat (norn GPT-5.6 Sol, review/safety, xhigh; session
`claude-review.V7J1VR`, envelope `~/.norn/delegations/claude-review.V7J1VR`),
plus my own battery, evidence reproduction, and end-to-end reproduction of the
decisive finding.

## Verdict

**D8 NOT READY — one BLOCKER.** The setup/input-durability contract in the
handoff ("Dropping the setup future at that seam therefore cannot re-inject
the same accepted message", claimed exactly for the agent-ID +
pending/durable-coordination case) is **false for the durable pending-store
wake path**, and I reproduced the duplication end-to-end in two variants at
the exact frozen source. Every other reviewed seam is SOUND; the correction
this verdict requires is narrow and the fix infrastructure already exists
in-tree.

## BLOCKER-1 — pending-store wake delivery violates exact-once at the
## durable-append/notification seam (REPRODUCED)

**Where:** `crates/norn/src/loop/delivery.rs:202-240`
(`flush_pending_agent_messages`), called from
`crates/norn/src/loop/runner/setup.rs:263`.

**Mechanism.** `messages_for_delivery` returns *clones*; the authoritative
durable pending store is untouched while `inject_inbound_messages` appends the
framed `UserMessage` durably (line 107), removes only the clone from the local
vector (line 119), and then awaits the cancellable session-event hook
(line 121). The store-side removal — the `agent_message.dequeued` audits and
in-memory `mark_dequeued` — happens only after the whole batch and all hook
awaits (lines 235-238). So between the first durable content append and
`mark_dequeued`, the accepted message is simultaneously durable in the
timeline **and** live in the authoritative retry store.

**Reproduction (mine, at `4fa6c67`).** Mirroring the retained regression's
blocking-hook harness (`delivery_cancellation_tests.rs`), with a
`PendingAgentMessages` store attached and one queued wake message:

1. *Drop variant:* first `flush_pending_agent_messages` future dropped at the
   hook await after the durable append; a second flush with the same pending
   store then re-injects. Result: **2** durable `UserMessage` rows for the one
   accepted message.
2. *Crash/resume variant:* after the same drop, the pending store is rebuilt
   with `PendingAgentMessages::from_events(&store.events())` — exactly what
   arming does on resume — and a fresh context flushes. Result: **2** durable
   `UserMessage` rows.

Both temporary tests failed their `== 1` assertions with `left: 2`; the test
file was then restored byte-clean (worktree clean at `6b1ae93`).

**Reachability.** (a) `run_agent_step` / `run_agent_step_from_messages` are
`pub` — an embedder that wraps the step future in `tokio::select!` or
`tokio::time::timeout` drops it at that await in completely ordinary usage;
no in-tree caller does, but the public API is the contract surface. (b) The
crash variant needs only process death between the content append and the
dequeued-audit append — the well the pending store exists to survive.
(c) Two same-process, no-drop triggers: a dequeued-audit append failure
propagates `?` at `delivery.rs:236` *before* `mark_dequeued`, and a mid-batch
content-append failure returns before it (documented in the code comment at
lines 223-226 as replay-over-loss) — in both cases already-injected messages
stay pending and re-inject on the next flush.

**Why BLOCKER.** The handoff's §"Setup timeout and input durability" makes the
exact-once claim for precisely the coordinated case reproduced here, and the
rustdoc on `inject_inbound_messages` (lines 109-113, "it cannot cause the
durable content to be re-queued and delivered twice") is true only for the
caller-owned vector path — the seed-path regression tests that path alone. A
duplicated, attacker-influenceable User message in the durable transcript is
model-visible on every subsequent request and survives resume: the same
transcript-integrity class that made P4's MAJOR-1 NOT READY. Wake/pending
delivery is a headline D8 deliverable, so this blocks the slice.

**Fix direction (not prescriptive).** Commit each message's framed
`UserMessage` together with its `agent_message.dequeued` audit as one
`EventStore::append_batch` group (the non-interleaving batch machinery D3
landed) *before* any hook await, with in-memory `mark_dequeued` for that
message also ahead of the hook; on batch failure nothing is durable and the
message stays pending (no loss, no duplication). Reordering `mark_dequeued`
alone is not sufficient — it trades duplication for loss on content-append
failure. A regression must cover the pending-store path exactly as the
seed-path drop test does today, plus reconstruction-from-events.

**Attribution.** The norn Sol cross-model seat found this (fourth
security-sensitive P5 slice running); the Opus durability seat independently
flagged the same seam but rated it HARDENING/MINOR after checking only
in-tree callers. I reproduced it before flipping the verdict.

## Everything else — SOUND

**Seam 1 (authority provenance).** `PromptSource::authority()` is the single
const provenance→authority map; no setter attaches a disagreeing role;
materialization never emits System after a lower role. Fork/spawn publish
typed plans; `ParentSystemInstruction` has zero production constructions and
is input-bridge-only (one embedder-System fragment); fork-of-fork strips
`ForkAgentPolicy`. Rule origin persists and reconstructs operator→Developer,
workspace→User, originless→User (never upward); a forged `origin:"system"`
fails deserialization; the originless row genuinely forces the unbound-anchor
replay. Child results stay neutral-framed verbatim User. No filename /
position / content-label / config / transport authority path found. Variant
prompt authority follows prompt-text origin, not name. Workspace profiles
declaring `prompt_commands` are hard-rejected at discovery.

**Seams 2+3 (projection/seed/anchor).** `PromptSeedFingerprint` is
length-framed, domain-tagged SHA-256 over exact fragment bytes — no
normalization holes. Anchor eligibility is exact fingerprint equality at both
the constructor filter and hot re-sync; every mutated source (operator/project
NORN.md, profile, prompt-command output) cuts; System-only changes preserve
the anchor and ride `instructions`. Replay validation rejects reasoning
without nonempty `encrypted_content` before the new prompt persists and before
dispatch (wiremock-proven single request). No Developer duplication or drop
across seed/instructions slicing.

**Seams 4+5 (lowering/MCP).** Compatible Chat: Developer native by default,
`downgrade_to_user` goes to user, `Reject` fails typed, misplaced role-policy
keys rejected across the options tree, policy key stripped from the wire; no
mapping to `system` exists. Claude: only System fragments reach
`--system-prompt`; Developer/User render positionally; no concatenation path.
MCP server prose reaches only live tool definitions; the prompt's tools
section skips every `runtime_dynamic()` tool; hosted-tool prose is compiled
Norn-owned text. The stateless Developer tail is index-tracked,
detached-per-iteration, never persisted, and cannot be impersonated by
crafted history.

**Seam 6 (cache).** Hit filter binds name + exact command text + TTL value +
working directory; absolute deadline set once at insert, hit path never
renews; `Duration::MAX` overflow warns, uses fresh, caches nothing, no panic
(monotonic `Instant` + `checked_add` throughout); the typed stable plan is
frozen before command execution and a self-rewriting command lands next
request (end-to-end hot-reload regression); command failure is typed/logged,
never silently swallowed.

**Seam 7 (setup timeout).** Structural shield confirmed: setup is never
wrapped in the step timeout; the budget wraps only the initialized machine;
`TimedOut` returns only after durable commits; budget arithmetic is
saturating/checked. The **seed-path** exact-once ordering (durable append →
vector removal → hook) is real and regression-proven — the defect above is
specifically the pending-store path.

**Seam 8 (evidence).** Reproduced independently: base/source trees match;
`4fa6c67..HEAD` contains zero Rust; both evidence-file SHA-256s match the
handoff; the NUL inventory reproduces exactly (205 paths, 9,809 bytes, SHA
`72bc3637…`, 102 A / 103 M); new-file max is 494 lines
(`agent/builder/build.rs`); exactly 26 touched files ≥500 physical lines at
source (honest pre-existing residual); all 356 added
`unwrap`/`expect`/`panic!`/`allow` hits classify to test paths or below
`#[cfg(test)]` — zero production-scope bypasses.

## Nonblocking observations (owner awareness; none gate a correction review)

1. **Session-file `origin:"operator"` forgery reconstructs at Developer**
   (`rules/projection.rs`): ceiling is Developer, System impossible,
   consistent with the store's existing trust model — but persisted rule
   origin has no integrity binding comparable to the response anchor's.
2. **`RuleEngine::new`/`add_rule` default to Operator origin** — safe for all
   in-tree callers; a required-origin constructor would remove the future
   embedder foot-gun.
3. **store:true backends wedge on forced full replay** after a mid-session
   Developer/User source change (reasoning persists without
   `encrypted_content`) — correct fail-closed behavior, disclosed by the
   handoff; an owner ruling on in-band recovery may be wanted later.
4. **Responses provider silently forwards unknown `norn_developer_role_policy`
   keys** as pass-through options — the mirror of the Chat-side rejection;
   config ergonomics only, no authority effect.
5. **Transient prompt-command re-run failure drops the Developer section for
   that request** rather than serving last-known-good — documented contract;
   owner taste.

## My battery (network-capable, repository target, at `6b1ae93` = frozen Rust)

`cargo fmt --all -- --check` clean; `cargo clippy --locked --workspace
--all-targets --all-features -- -D warnings` exit 0, no suppression; `cargo
test --locked --workspace --all-targets --all-features --no-fail-fast`
**5,684/5,684** (norn 4,308, CLI 522, TUI 683); doctests **8/8**;
`git diff --check` clean. Focused D8 filter **15/15**; my broader
`prompt_command` filter **27/27** (superset of the handoff's 9); the named
durable-inbound cancellation regression passes — it covers only the
caller-owned-vector path, which is exactly why BLOCKER-1 survived it.

## Boundaries

- This verdict blocks D8 on BLOCKER-1 alone. No other finding gates the
  correction; a narrow same-reviewer confirmation of the corrected seam (plus
  reconciled handoff/rustdoc claims) is the expected next step.
- Whole-P5 acceptance, D7/P9 real-wire conformance, WebSocket transport, P2
  acceptance, the broad volatile-source/concurrent-agent matrices, and
  universal exact-once for coordination-less embedders remain out of scope,
  as the handoff states.
