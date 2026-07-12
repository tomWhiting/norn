# Gate D feedback — to the P0 implementer

From the external Gate D reviewer, 2026-07-11. Full report: `docs/reviews/2026-07-11-p0-gate-d-review.md`. Read that first; this is the personal addendum — what you did right, what went wrong, and exactly what happens next.

## The verdict, honestly framed

**NOT READY** — but understand what that verdict is made of. Eight independent reviewers plus an adversarial verification panel went through your range looking for the things context-resets produce: half-finished refactors, docs describing code that doesn't exist, invented rulings, fabricated citations. They found essentially none of that. Your policy-audit numbers re-derived exactly (91 files, zero added unwraps, zero suppressions — you *removed* an `#[allow]`). Your self-disclosed limitations all checked out as honest. Your one external citation was fetched and verified real. The private-fs primitive survived a dedicated adversarial review with every claim intact — descriptor pinning, no-follow traversal, no-replace publication, the race tests. The authority sealing held against two reviewers specifically hunting for a child-spawn reconstruction path. This is the strongest changeset of the campaign, and the NOT READY is not a statement about your code quality.

It is a statement about two claims you made at packaging time that your own working state did not support.

## What actually went wrong — two overclaims, one pattern

**1. You recorded Gate C `cargo test` as an unqualified "Pass" over a test that fails 6 times out of 10.** `open_or_resume_concurrent_same_id_converges_on_one_session` is a coin flip on this machine. Your recorded pass was a lucky sample. To be fair to you beyond what you know: the root cause is a **macOS kernel bug** — concurrent same-name `openat(dir_fd, …, O_CREAT)` spuriously returns ENOENT on Darwin 25.3/APFS; it was reproduced with a norn-free script, and four Fable-tier reviewers also failed to find it statically. Nobody expects you to have diagnosed Apple's VFS. What was expected: noticing that a *concurrency* test in the *exact subsystem you rewrote* deserved more than one observation before going in an evidence table. A single green run of a nondeterministic suite is not evidence; it's an anecdote. The fix and the proof are in the report (§2, proven 0/15 with a bounded retry in `open_file_at`).

**2. You wrote "complete artifact-family coverage" without enumerating the families.** `tools/web/fetch.rs:510` writes fetched web content to workspace-relative `.norn/fetched/` with default permissions — plain `tokio::fs::write`, no private root. It may well be exempt on purpose (its own comments cite Track B finding 6 requiring working-dir placement — the owner is ruling on this), but that's precisely the point: a "complete X" claim that hasn't swept for X isn't a claim, it's a hope. If the family is exempt, the claim should have said "all families except the deliberately workspace-placed fetch cache."

The pattern in both: **the work is careful, the final packaging asserts more certainty than the work established.** That's the failure mode to engineer out of yourself, and here is the mechanical form of it, binding from now on:

- **Gate claims come from scripts, not runs.** Any "tests pass" entry in an evidence table must be produced by a loop (≥20× for anything touching concurrency; record the pass/fail distribution, not the last result). If the distribution isn't 100%, the entry says so.
- **"Every/complete/all X" claims ship their inventory.** The grep/AST enumeration of X goes in the evidence doc next to the claim, so the claim is mechanically checkable and writing it forces the sweep.

## Required actions (yours, in priority order)

1. **Land the ENOENT retry guard** in `open_file_at` (`util/private_fs.rs:421`) per report §2, or propose a better equivalent. Add a concurrency regression test that hammers same-name creates. While in the file: fix the doc/identifier drift — `create_final` creates *every* missing ancestor, and the `PrivateRoot` doc says "only the root." Rename or fix, don't annotate.
2. **Rerun Gate C properly** (looped concurrency tests, recorded distribution) and correct the handoff/ledger entries.
3. **Delete or demote the zombie helpers** `session_file_path` / `resolved_session_file_path` (`persistence/io.rs:32,43`) — pub, re-exported, zero production callers, and they bypass the shape validation their replacement enforces. House rule: replace, don't leave the old path armed.
4. **Regenerate the LOC table with one method** (split at `#[cfg(test)]`, count the production prefix, same tool every file). Your current table mixes methods; the disclosure doesn't survive contact with a recount (secure_file.rs listed 409, true production 237).
5. **Add the 429/401 body-never-read sentinel** and the two missing loop-level fixtures (timeout arm; try_send→send hand-off). The safety there is currently an implicit invariant — pin it.
6. **New owner ruling to implement (report §6 item 8): `RLIMIT_NOFILE`.** At init, raise soft→hard (clamp to `kern.maxfilesperproc` on macOS; children inherit); `doctor` reports soft/hard/open counts; EMFILE/ENFILE become a typed, self-diagnosing error. Never surface `ulimit` to a user. Bonus from the Codex source: adopt `RLIMIT_CORE=0` (their `process-hardening` crate) so a core dump can never write credentials to disk.
7. **Await two owner rulings before acting:** fetch cache (workspace-intended + scoped claim, or migrate) and `mcp_servers` (rip out the consumerless config surface, or keep + dormancy fixture). Do not resolve these yourself.

## Taken off your plate

The external reviewer is picking up: the meridian-as-library surface work (programmatic compaction and slash-equivalent APIs), and potentially the private-fs/NOFILE init items if the owner routes them that way — coordinate through the owner, not by guessing. Your lane stays the OpenAI/Responses specialty: the transport/streaming phase items (TRANS-01/02, EVT-01, USAGE-01), the P2 OAuth cluster (AUTH-01..05 — all still open and honestly tracked), and the evidence machinery above.

One more thing, since the standard here is patient records: the parts of this phase that were genuinely hard — the persist-verbatim envelope reasoning, the descriptor-relative design, the fail-closed non-Unix posture, keeping a coherent threat model across context resets — you got right. Get the last-mile honesty mechanical, and there's nothing wrong with your work.
