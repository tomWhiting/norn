# P2 OAuth lifecycle correctness — interim review

- **Review date (Australia/Melbourne):** 2026-07-15
- **Reviewer:** external review seat (coordinator) + four independent read-only
  seats over disjoint module areas
- **Reviewed head:** `289d841`
- **P2 change range:** `6669b9d..289d841` (feat `6a76d9f`, docs `289d841`)
- **Governing plan:** `docs/RESPONSES-API-REMEDIATION-PLAN.md` § "P2. OAuth
  lifecycle correctness" (line 1129)
- **Owner rulings:** `DECISIONS-2026-07.md` § 8 (revised in this range)

## Scope and standing

This is a **correctness review of in-progress work**, not an acceptance gate.
P2 is explicitly not up for acceptance: its phase-evidence and exit-gate boxes
are deliberately unchecked, and it depends on P1 and D9 (neither closed). The
review verifies (a) the implementation of the checklist items marked done, (b)
adversarial behavior on failure/cancellation/concurrency paths, and (c) that
line 1129 honestly records what remains. No READY verdict is issued or implied.

## Outcome

The credential lifecycle is **fundamentally sound and its security-critical
guarantees are proven**. The plan records what remains **honestly** — every
area seat independently confirmed the `[x]`/`[ ]` split matches the code, with
no half-built version of an open item and no finish-line overclaim. Two
P2-introduced invented constants violate the CLAUDE.md NO-ARBITRARY-LIMITS rule
and must be resolved (owner ruling or sourced/configurable) before the P2
candidate gate; two further robustness gaps are the owner's call. None of the
findings corrupts a credential or touches the foreign Codex file.

## Battery (coordinator)

Native toolchain `1.94.0`, repo warm `target/`, alone on host.

| Leg | Result |
|---|---|
| `cargo fmt --check` | pass |
| `cargo clippy -p norn --all-targets -- -D warnings` | pass |
| `cargo test -p norn --lib openai_oauth` | **160 passed, 0 failed** |

## What is proven

- **Foreign-file boundary (the headline P2 guarantee) — structurally proven.**
  Zero production `$CODEX_HOME` reads exist; every credential write, lock,
  chmod, and remove routes through `PrivateRoot(NornAuthRoot)`, which cannot be
  constructed from `$CODEX_HOME`. Status uses a read-only observational open
  (no create/chmod). The foreign `$CODEX_HOME/auth.json` cannot be mutated,
  locked, permission-hardened, or deleted by any login/refresh/status/logout
  path. No silent import/copy of the Codex file exists.
- **Durability backbone.** `create_new = O_EXCL|O_NOFOLLOW`, regular-file
  enforced post-open, write → `file.sync_all` → atomic `renameat` →
  parent-dir fsync; durable delete with a typed `DeletedButUndurable` outcome;
  ancestors fsynced at create. The sole production persistence path is the
  durable `CredentialTransaction`; no non-durable production writer exists.
- **Durable-before-success ordering.** Both the browser login and the refresh
  path return success only after the durable save is confirmed. The browser
  success page is gated on a `Committed` acknowledgement that is sent only when
  the durable store result is `Ok`; the sole credential writer is the owning
  future, so a cancelled flow cannot orphan a "surprise" write.
- **Lock-ignoring writer detection.** A pre-write revision check returns a typed
  `Conflict` with no write; the residual check→rename TOCTOU window is
  unavoidable with portable rename and is honestly documented.
- **No silent stale-token fallback**, and ownerless/static credentials are
  non-refreshable (rejected, not stranded).
- **JWT/claims.** The namespaced Codex auth claim is parsed and preferred;
  conflicting account IDs are rejected without disclosure; the old invented
  eight-day unknown-expiry fallback is fully removed (`last_refresh` never
  synthesizes expiry); the `IdTokenClaims` serialize authority is removed; the
  six-state credential evaluator is total, local, side-effect-free, and
  discloses no token/identity material.
- **Logout** always deletes the local credential first (durably) and reports
  remote revocation as a separate typed, non-fatal result — the remote revoke
  is no longer a prerequisite for local logout.
- **Callback security.** Loopback-only bind (`127.0.0.1`), a 256-bit random
  `state` required-match (CSRF), no code/token logged or rendered.
- **Refactor integrity.** `login_server.rs` genuinely split 976 → 424 LOC into
  `login_callback.rs`/`login_callback_worker.rs`; no god file, no zombie/dup
  logic; `mod.rs` pure; zero production `unwrap`/`expect`/`panic`.

## Findings

**P2-1 — MAJOR (CONFIRMED) — invented non-configurable poll interval.**
`credential_transaction.rs:21` `LOCK_POLL_INTERVAL = Duration::from_millis(5)`
is a hardcoded constant introduced by this change with no factual or owner-ruled
source and **no configuration override**. CLAUDE.md names "poll intervals"
explicitly among values that may not be invented. Fix: source the value
(owner ruling) or make it configurable with a sourced/derived default; do not
guess.

**P2-2 — MAJOR (CONFIRMED) — invented lock-timeout default.**
`options.rs:43` `DEFAULT_CREDENTIAL_LOCK_TIMEOUT = Duration::from_secs(10)`,
introduced by this change, is justified only by analogy to the unrelated
network round-trip timeout. Its siblings are documented as owner-approved
pre-existing values; this one is new and unsourced. It is overridable via
`OAuthHttpOptions` (explicit config wins), so the defect is the invented
*default*, not the knob. Fix: owner ruling on the value, or a factual source.

**P2-3 — MAJOR (PLAUSIBLE) — a panicked/aborted refresh task strands all
waiters.** `manager_refresh.rs:112` spawns the single-flight refresh with the
`JoinHandle` dropped; `record()`/`notify_waiters()` run only after the refresh
future returns, and waiters await a `Notify` with **no timeout**
(`manager.rs:167`). If that future ever panics or is aborted mid-flight, every
`auth()`/refresh waiter blocks forever. Production is panic-free by construction
today, so this is defense-in-depth — but thin for a credential path held to this
standard. Fix: a bounded wait, or a `JoinHandle`-observing supervisor that
converts a dead task into a typed error.

**P2-4 — MAJOR (PLAUSIBLE) — symlink-aliased roots split the single-flight
coordinator.** The coordinator identity (`manager_registry.rs:11`) and the
process gate key normalize roots **lexically** only (no `canonicalize`), so two
symlink-distinct paths to the same on-disk `auth.json` produce two in-process
coordinators for one storage identity. This is not corrupting — the cross-process
file lock plus revision-CAS still serialize writes and the loser gets a typed
`Conflict` — but it silently degrades in-process single-flight to the slower
inter-process path and opens a redundant authority-exchange window. Fix:
canonicalize (or dev/ino-key) the storage identity, consistent with the
`O_NOFOLLOW` open guarantees already in place.

**P2-5 — MINOR (CONFIRMED) — std-Mutex poison recovered, not typed.**
`credential_transaction.rs:111,447,464` recover `PROCESS_GATES` poison via
`into_inner()` rather than mapping to a typed error. Defensible (the guarded
data is a trivial `HashSet<PathBuf>` and the critical sections run no user
code), but it deviates from the CLAUDE.md letter that poison be mapped to a
typed error.

**P2-6 — MINOR (CONFIRMED) — cancelled callback drops the stream with no
response.** `login_callback_worker.rs:159-168`: on `Ours(Ok(code))` with the
owner dropped mid-flight, `claim_callback` returns `Canceled` and the function
returns while dropping the stream with no HTTP response written, so the browser
hangs to its own timeout instead of getting a page. UX-only; no correctness or
security impact.

**P2-7 — OBSERVATION — duplicated `.norn` product-dir literal.**
`auth_root.rs:10` and `config/paths.rs:22` independently spell `.norn`. Both are
owner-ruled/factual, so not a defect, but a shared source would prevent drift.

## Plan honesty (line 1129)

**Accurate.** All four seats independently confirmed that each `[x]` item in
their area holds in code and each `[ ]` item is genuinely open — named-account
index/selector/CLI surfaces, foreign import/migration, the CONFIG-01/02 typed
matrix, the recovery journal, and an approved persistence-sink interface are all
absent, not half-built. The phase-evidence notes correctly describe fixtures as
present-but-not-a-pass-claim, and every evidence/exit-gate box is correctly
unchecked. The one honesty gap is internal: the two invented constants (P2-1,
P2-2) are exactly what the plan's own NO-ARBITRARY-LIMITS contract exists to
catch.

## Recommendation

Before P2 reaches its candidate gate:

1. Resolve P2-1 and P2-2 — **owner ruling required** (a factual/derived value,
   or make them configurable with a sourced default). These are standard
   violations, not style preferences.
2. Owner's call on P2-3 (bounded wait / supervisor) and P2-4 (canonicalized
   storage identity) — both are real robustness gaps for a credential path,
   neither is corrupting today.
3. P2-5/P2-6/P2-7 at implementer discretion.

P2 acceptance remains gated on its own retained candidate run, the closure of
P1 and the D9 items, and the plan's exit-gate reviews — none of which this
interim review substitutes for.
