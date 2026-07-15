# P2 OAuth interim-correction review — verdict

- **Review date (Australia/Melbourne):** 2026-07-15
- **Reviewer:** external review seat (coordinator) + one independent read-only
  adversarial seat on the two subtle corrections
- **Handoff:** `docs/reviews/2026-07-15-p2-interim-correction-handoff.md`
- **Interim review corrected:** `86d95aa` /
  `2026-07-15-p2-oauth-lifecycle-review.md`
- **Correction range:** `289d841..455990a` (`32aa18b` supervision, `9a5bb33`
  docs, `455990a` lock timing)
- **Reviewed head:** `455990a`

## Verdict: READY — interim correction only

This closes the seven interim findings and **D9A** (credential-transaction
timing policy). It is **not** a P2 verdict: P2 acceptance remains gated on its
own retained Gate C evidence, the P1 dependency, and the remaining D9 decisions
(named accounts, the two-account validity experiment, durable refresh recovery
journal, import/keyring scope, and the typed `provider.auth` matrix). Those
stay open and unclaimed here.

## Battery (coordinator)

Native `1.94.0`, repo warm `target/`, alone on host.

| Leg | Result |
|---|---|
| `cargo fmt --all -- --check` | pass |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | pass |
| `cargo test --locked -p norn --lib openai_oauth` | **175 passed, 0 failed** (+15 vs the reviewed head) |
| Retained timing evidence (`2026-07-15-p2-credential-lock.json`) | runner + result SHA-256 **match** the handoff; head `455990a`; committed source; normal `target/`; process-local deadline **20/0**; two-process convergence **20/0** |
| Changed-file LOC | all production files < 500 (tightest 492); no added `unwrap`/`expect`/`panic`/`todo` (clippy-enforced) |

## Findings resolution

| Finding | Verdict | Evidence |
|---|---|---|
| **P2-1** invented non-configurable poll interval | **FIXED** | Removed; `credential_lock_poll_interval` is a public `OAuthHttpOptions` field, default 25 ms owner-approved as D9A; positive-validated (`credential_lock_timing.rs:31`). |
| **P2-2** invented lock-timeout default | **FIXED** | `credential_lock_timeout` overridable, default 30 s owner-approved as D9A (DECISIONS §8; plan line 441); zero rejected before FS access (`credential_lock_timing.rs:28`). |
| **P2-3** panicked/aborted refresh strands all waiters | **FIXED** | Worker `JoinHandle` supervised (`manager_refresh.rs:110-119`); worker/supervisor/runtime death all wake every waiter with typed `Indeterminate` (`manager_attempt.rs:35-37,96-114`); ambiguous dispatch marked Indeterminate before the network call, never replays a possibly-consumed token (`manager_refresh.rs:239-243,321-351`). |
| **P2-4** symlink-aliased roots split single-flight | **REFUTED (false premise)** | Per-component `O_NOFOLLOW` walk rejects a symlinked ancestor or leaf with `ELOOP` before registration and authority I/O (`private_fs.rs:329-347`; ordering `manager_registry.rs:35,42,53`); macOS `/var`,`/tmp` pre-normalize to one identity. The original symlink premise does not hold. See residual observation below. |
| **P2-5** untyped mutex-poison recovery | **FIXED** | `parking_lot::{Mutex,Condvar}` (non-poisoning); process-local contenders use a Condvar, not polling (`credential_transaction.rs:11`). |
| **P2-6** cancelled callback hangs the browser | **FIXED** | Cancellation now writes a generic HTTP 400 failure page (`login_callback_worker.rs:201-208`). |
| **P2-7** duplicated `.norn` literal | **FIXED** | Single production source `DEFAULT_NORN_DIRECTORY` (`config/paths.rs:22`), imported by `auth_root.rs`; remaining literals are test-only. |

## Reviewer correction on P2-4

The original P2-4 was labeled PLAUSIBLE, not confirmed. Adversarial
re-examination refutes its premise: a no-follow, per-component root open makes
two *symlink-distinct* paths unable to both register a coordinator. The finding
is withdrawn.

A **different, narrower** residual exists and is recorded as an observation, not
a re-raise of P2-4: `ManagerIdentity` keys on the lexical `NornAuthRoot`
(`manager_registry.rs:11-17`), so a **bind mount** or the same filesystem
mounted at two mountpoints — which contain no symlink component and therefore
pass the no-follow walk — would register two in-process coordinators for one
`auth.json`. This is **degradation, not corruption**: the inter-process file
lock on the shared inode plus revision-CAS still serialize every writer and the
loser receives a typed `Conflict`. It is closable with a `dev/ino` storage
identity if and when the D9 durable-refresh-recovery work revisits coordinator
identity; it does not gate this correction.

## D9A

The plan records D9A (line 441) as decided and implemented 2026-07-15: the
credential-transaction acquisition deadline (30 s) and inter-process polling
cadence (25 ms) are explicit owner-approved, programmatically overridable, and
positive-validated before credential filesystem access; the deadline bounds
acquisition only, and the transaction retains exclusive ownership across
reload, refresh, and durable save so a rotating refresh token cannot be spent
concurrently. DECISIONS §8 carries the matching ruling and honestly leaves
process/restart-durable protection after an ambiguous refresh as an open D9
journal decision.

## Next milestone

Not P2 acceptance. The remaining D9 decisions: named-account validity/layout and
selection surfaces, the owner-approved two-account live validity experiment,
the durable refresh-recovery journal, foreign import/keyring scope, and the
typed `provider.auth`/source/account configuration matrix (CONFIG-01/02).
