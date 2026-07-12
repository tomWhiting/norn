# P0 Gate D external review — 2026-07-11

**Reviewer:** Claude (Fable) coordinator + 8-area multi-agent fleet (Opus 4.8 ×5, Opus 4.6 ×3), every BLOCKER/MAJOR finding adversarially verified by an independent 3-vote refutation panel, plus a completeness critic and an independent coordinator root-cause investigation of the failing gate test.

**Range reviewed:** `82a2708..c0d32e5` (code head `ebb82c8`, then docs to HEAD), per the handoff `docs/reviews/2026-07-11-p0-gate-c-handoff.md`. The earlier range `41ea210..82a2708` was reviewed in the prior cycle (archived under `2026-07-11-remediation-review/`); regression against it was in scope, re-review of it was not.

**Verdict: NOT READY** — but narrowly, and for evidence-integrity reasons more than code reasons. The code itself is the strongest changeset this campaign has produced. The items below are what stands between this and a whole-phase `READY`.

---

## 1. Independent gate reruns (coordinator, clean local runs at HEAD)

| Command | Handoff claims | Independent result |
|---|---|---|
| `cargo fmt --all --check` | Pass | **Pass** (exit 0) |
| `cargo clippy --workspace --all-targets -- -D warnings` | Pass | **Pass** (exit 0) |
| `cargo test --workspace --all-targets` | Pass | **FAIL, then pass on rerun** — see §2 |
| `cargo test --workspace --doc` | Pass | **Pass** (exit 0) |

The one failure: `session::manager::tests::open_or_resume_concurrent_same_id_converges_on_one_session` panicked `Io(Os { code: 2, NotFound })` in two of its four racing threads. Isolated rerun ×10: **6 failures out of 10**. This is not a flake; it is a coin-flip. The Gate C table records an unqualified "Pass" for a suite containing a test that fails the majority of the time under direct repetition — the recorded pass was a lucky sample, and the handoff's evidence claim is thereby broken regardless of the root cause.

## 2. Root cause of the failing convergence test

*(Coordinator investigation — fleet split 3–1 on this, with all three "confirmed production race" mechanisms individually refuted by the verification panel; the static traces below and the instrumented-probe result are the authoritative account.)*

Static tracing of `open_or_resume` → `insert_index_entry_if_absent` (index-locked insert) → `open_fresh`/`resume_entry` → `open_session_append_under` (reopen → stamp → linkat no-replace publish → reopen) finds every interleaving convergent: each NotFound-capable step either tolerates it (`io.rs:221`, `io.rs:389/396`, `io.rs:292`, `index.rs:49/58`, `mutation.rs:106`) or is protected by the linkat/EEXIST protocol. Four independent tracers (coordinator, completeness critic, session-persistence reviewer, two refutation votes) reached the same conclusion.

**ROOT CAUSE FOUND (coordinator, instrumented probe + OS-level repro — CONFIRMED):**

The static traces were all correct — and all looking at the wrong layer. An instrumented worktree run localised the NotFound to `insert_index_entry_if_absent` → `lock_index` → `PrivateRoot::open_lock` — i.e. the **index-lock file creation**, before any of the seams the reviewers were auditing. One level deeper: the failing syscall is the `openat(dir_fd, "index.lock", O_WRONLY|O_CREAT|O_NOFOLLOW|O_NONBLOCK)` in `open_file_at` (`crates/norn/src/util/private_fs.rs:421`).

A norn-free Python reproduction then isolated the ingredient matrix (100–300 trials per cell, 4 racing threads, Darwin 25.3.0 / macOS 26.3.1, APFS):

| Variant | Spurious ENOENT |
|---|---|
| `openat(dir_fd, same name, O_CREAT)` — norn's exact flags | **257/100 trials' thread-attempts** |
| same, without the `fchmod` mode-healing | 278 (fchmod exonerated) |
| same, **absolute-path `open()` instead of `dir_fd`** | **0** |
| `openat(dir_fd, O_CREAT)`, **different names per thread** | **0** |
| norn's exact flags **+ one retry on ENOENT** | **0** (232 retries fired) |

**Conclusion: this macOS release has a kernel/VFS race in which concurrent same-name `openat(dir_fd, …, O_CREAT)` calls spuriously fail ENOENT** — semantically impossible for O_CREAT with an intact parent. The pre-P0 code used absolute-path `std::fs` opens and was immune; the P0 descriptor-relative migration exposed the platform bug. It is **not a norn logic error** — every reviewer who traced the protocol as convergent was right — but it is a **P0-introduced, production-reachable regression on macOS**: any two norn processes or threads concurrently opening/creating the same lock, session, or spool file can spuriously fail with NotFound (fail-loud, no corruption; but it breaks `open_or_resume`'s convergence contract, which is the primitive's whole point).

**Fix (proven):** bounded retry on ENOENT in `open_file_at` when `O_CREAT` is in the flags — a genuinely missing parent keeps failing and still propagates. Applied in a probe worktree: the convergence test went from **6/10 failing to 0/15 failing**. Patch:

```rust
// in open_file_at (crates/norn/src/util/private_fs.rs:421), replacing the single openat call
let mut attempts = 0;
let descriptor = loop {
    match openat(parent, name, flags, file_mode) {
        Ok(fd) => break fd,
        // macOS (observed Darwin 25.3/APFS): concurrent same-name
        // openat(dirfd, O_CREAT) races spuriously fail ENOENT even
        // though O_CREAT makes a missing entry creatable. Retry;
        // a genuinely missing parent keeps failing and propagates.
        Err(rustix::io::Errno::NOENT)
            if flags.contains(OFlags::CREATE) && attempts < 16 =>
        {
            attempts += 1;
        }
        Err(error) => return Err(io::Error::from(error)),
    }
};
```

(The retry bound and its comment should get an owner-visible note; 16 is a safety bound on a loop that empirically converges in one retry, not a tuning knob. `create_new`/O_EXCL paths share `open_file_at` and are covered by the same guard.)

## 3. Findings

### F1 — BLOCKER: macOS spurious-ENOENT race in `open_file_at` breaks concurrent same-file open/create; Gate C "all-targets Pass" recorded over a 60%-failing test
`crates/norn/src/util/private_fs.rs:421` (code), `docs/reviews/2026-07-11-p0-gate-c-handoff.md:97` (evidence)
Root-caused and fix-proven in §2. Two required actions: (a) land the retry guard (or an owner-chosen equivalent) — this is a production defect on macOS, not just a flaky test; (b) correct the Gate C evidence: rerun the suite with the convergence test looped (e.g. 50×) and record the distribution, not a single lucky pass.

### F2 — MAJOR: "complete artifact-family coverage" is false — the fetch cache writes web content to the workspace with default permissions
`crates/norn/src/tools/web/fetch.rs:510-535` — `save_fetched_content` writes fetched page content (source URL embedded in frontmatter) to workspace-relative `.norn/fetched/<sha256>.md` via plain `tokio::fs::write`: no `PrivateRoot`, no 0600, default umask. CONFIRMED by coordinator and completeness critic (the area finder's refutation panel mis-scored this one; the underlying facts were never in dispute).
**Nuance for the owner:** the file's own comments cite "Track B finding 6" requiring this artifact to land in the working directory — so this may be a deliberate earlier ruling colliding with P0's claim rather than a forgotten family. Either way the handoff claim as written is false. Ruling needed: either the fetch cache is workspace-intended (then the handoff/plan text must scope the claim and the risk of fetching authenticated content into a shared repo should be documented) or it migrates to the private root like its siblings.

### F3 — MAJOR: no dormant-MCP regression guard
`docs/RESPONSES-API-REMEDIATION-PLAN.md:591` — honestly disclosed as missing, but the consequence stands: `mcp_servers` remains a merged, unvalidated, workspace-writable surface with **no test asserting it has no runtime consumer**. Any future wiring silently inherits repo-controlled server definitions. The fixture is cheap (assert the merged value is never read by runtime assembly); it should exist before P0 closes.

### F4 — MINOR (cluster): half-finished seams, all confirmed
- **Zombie validation-bypass helpers:** `session_file_path` / `resolved_session_file_path` (`io.rs:32,43`) are `pub`, re-exported from `session/mod.rs`, do a raw `data_dir.join()` with none of `session_file_relative`'s shape validation, and have **zero production callers** (verified: every caller workspace-wide is inside `#[cfg(test)]`). Under NO-ZOMBIE-CODE they should be deleted or demoted to `pub(crate)`/test-only.
- **Doc/code drift in the new primitive:** `PrivateRoot` docs claim it "creates and hardens only the root" and the parameter is named `create_final`, but `open_absolute` (`private_fs.rs:300`) creates **every** missing ancestor (0700) when the flag is set. Not a confinement break (every component is O_NOFOLLOW-opened and hardened), but the doc and the identifier are both wrong, and a missing mount point gets silently manufactured instead of failing loudly.
- **LOC-audit imprecision:** the handoff's table mixes whole-file counts (including tests) with production-prefix counts. Disclosed, but it produced figures like `secure_file.rs` 409 vs a true production count of 237, and `process/manager.rs` 403 vs 465 recount. All files verified under 500 — the *conclusion* holds — but the *numbers* don't survive a recount, which is corrosive for an audit document.
- **429/401 non-disclosure is safe by construction but unguarded:** both branches never read the response body (`exec.rs:220-236, 238-274` — verified), but no fixture pins that invariant; a future "parse the JSON retry-after" refactor would regress silently. One sentinel test closes it.
- **Missing stalled/timeout fixtures:** the `send_binary_confirmed`-style timeout arm of the drain path and the try_send→send lossless hand-off have no test at the loop level.

### F5 — Owner confirmations needed (not defects until you say so)
1. **DECISIONS §8 owner attributions** (`docs/DECISIONS-2026-07.md:795,808`): "The owner wants to sign in to multiple ChatGPT/Codex accounts…" and "the owner reports that logging into another account can invalidate prior tokens." The section is otherwise properly hedged (D9/D10 explicitly open, no ruling claimed), and its external citation checked out as real (coordinator fetched `learn.chatgpt.com/docs/auth` — it exists and says what the doc says). If you did say those two things, this finding evaporates; if not, §8's framing of P2 needs rework.
2. **Gate A timing exception:** the handoff is honest that the two Gate A ordering claims cannot be repaired retroactively and require an explicit owner-approved P0-only exception. That decision is yours; Gate D can only note that everything else in the tracker was verified accurate (21-claim sample by the plan-tracker reviewer, all held; archived provisional reports confirmed textually unaltered apart from intake annotations).

## 4. What held up (verified, not assumed)

- **The private-fs primitive is excellent.** Every P0-ARTIFACT-R2 claim traced true: descriptor-pinned roots (dup + openat, never re-resolving), O_NOFOLLOW on every component, fchmod-on-fd healing that doubles as an ownership gate, dev/ino-verified no-replace publication, O_EXCL creation, and a genuinely adversarial test suite (symlink-swap, replaced-ancestor, publication races). The deleted `permissions.rs` left no dangling references and the migration *fixed* a pre-existing truncate-overwrite hazard in the spool path.
- **Authority sealing is real.** `LoadedSettings`/`load_settings`/`merge_settings` are crate-internal with compile-fail doctests; `validate_working_directory_authority` still runs on raw layers before merge at all three production sites; the static-Codex constructor is pinned to the compiled endpoint with URL-canonicality tests; child spawn/fork/variant paths inherit the provider by `Arc` — no workspace-driven reconstruction path found by two independent reviewers.
- **Terminal diagnostics are sound.** HMAC-keyed per-process discriminators, fail-closed on entropy failure, non-panicking, no raw authority bytes on any traced error path; redirect refusal precedes any body read; header values fully redacted; disclosure sentinels use CR/LF/ESC injection consistently.
- **No regressions against the prior cycle.** Credential Debug redaction, bounded no-redirect clients, absolute-CODEX_HOME, and all provider_security validators survived the five closure commits intact; the removed VM-token constructor was replaced (not shimmed) by the validated `StaticCodexCredential`.
- **Policy audit numbers independently re-derived:** 91 changed Rust files exact; zero added unwrap/expect/panic/allow/ignore in added production lines (one `#[allow]` was actually *removed*); boundary-huggers `spawn.rs` (497) and `disk.rs` (496/498) under the limit via legitimate structure; all seven changed `mod.rs` files clean; the only high-entropy literal is the hmac crate checksum; new deps are exactly `hmac` + `sha2` (+ dev-only `temp-env`), all used, AGPL-compatible.
- **Cross-boundary threat chains examined and closed:** workspace→config→credential (single sanctioned merge path, validation-before-merge), child-spawn→provider-authority (Arc inheritance + sentinel test), artifact-write→workspace-exfiltration (closed for every family except the fetch cache, F2). Gate D residuals #2 (unbounded workspace reads — pre-existing, owner-deferred, honest) and #4 (public scan APIs — trusted-input-only, no workspace-rooted production caller) were independently assessed and hold as stated.

## 5. Implementer-agent assessment (requested)

The context-reset fingerprints are present but almost all *cosmetic*: a misnamed parameter and stale doc comment in `private_fs`, a `test:`-labeled commit that also edits production doc comments, two counting methods blended in one audit table, helpers migrated-away-from but not deleted, and a split test-code idiom (elaborate `?`-based Result tests in new code beside legacy unwrap tests). Two are *substantive*: the unqualified Gate C "Pass" recorded over a majority-failing concurrency test (the single worst artifact of the phase — an evidence claim that dissolves on first independent rerun), and the "complete artifact-family coverage" claim written without sweeping for non-obvious writers (the fetch cache lives in `tools/web`, outside the families the agent was staring at).

Against that: zero invented rulings found (the one suspicious citation was fetched and verified real), an honestly-maintained open-items ledger, self-disclosed limitations that all check out, disciplined test hygiene beyond the house minimum, and a threat model held coherent across resets. The failure mode to engineer against is **evidence overclaim at the finish line, not code decay**: the agent's last-mile packaging asserts stronger facts (unqualified gate passes, "complete" coverage) than its own working state supports. Mechanical countermeasures: (a) gate claims must be produced by a script that runs the suite N× and records the distribution, never a single run; (b) coverage claims of the form "every X is migrated" must ship with the grep/AST inventory that enumerates X, so the claim is checkable and the sweep is forced.

## 6. Required actions before P0 `READY`

1. Root-cause and fix the convergence failure (§2), then record a 50× stability run.
2. Resolve the fetch-cache contradiction (owner ruling: workspace-intended + scoped claim, or migrate).
3. Add the dormant-MCP fixture.
4. Delete or demote the zombie path helpers; fix the `private_fs` doc/identifier drift.
5. Regenerate the LOC table with a single deterministic method.
6. Owner: confirm/deny the §8 attributions; decide the Gate A P0-only exception.
7. Optional but cheap: the 429/401 body-drop sentinel and the two missing loop-level fixtures.
8. **OWNER-RULED (2026-07-11, post-review): `RLIMIT_NOFILE` mitigation is mandatory.** The descriptor-relative design plus held session sinks against macOS's default soft limit of 256 makes EMFILE a certainty, not a risk, and users must never be asked to run `ulimit`. Required: (a) at process init raise the soft limit to the hard limit (macOS: clamp to `kern.maxfilesperproc`; children inherit through fork/exec, covering all spawned/forked agents) — no invented numbers, OS-granted maximum only; (b) `norn doctor` reports soft/hard/currently-open descriptors; (c) EMFILE/ENFILE map to a typed, self-diagnosing error naming usage vs limit, not generic `Io`; (d) punch-list follow-up: LRU pooling for long-lived session sink descriptors (append-only sinks reopen safely via the existing heal path). Reference point: Codex does not raise NOFILE, but its `process-hardening` crate sets `RLIMIT_CORE=0` to prevent credential-bearing core dumps — worth adopting alongside.
