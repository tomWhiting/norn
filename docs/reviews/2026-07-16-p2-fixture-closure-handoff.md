# P2 fixture-closure handoff

- **Date:** 2026-07-16 (Australia/Melbourne)
- **Source commit:** `fcd1b30`
- **Review range:** `fcd1b30^..fcd1b30`
- **Purpose:** close the bounded P2 fixture gaps identified after the
  implementation-candidate review
- **Disposition requested:** review the fixture closure as a P2 candidate
  supplement, not as whole-phase acceptance

## Scope

The source commit changes six files. Runtime behavior is unchanged outside
`cfg(test)`: the credential transaction gains test-only fault injection at the
real publication and deletion boundaries, and the remaining changes are test
fixtures.

The candidate adds:

- flat-claim JWT login and refresh chains through durable reload and exact
  bearer/account request headers;
- publication faults at temporary create, write, credential file sync, final
  rename, and parent-directory sync;
- deletion faults at quarantine rename, quarantine removal, and post-delete
  directory sync;
- exact state, cleanup, typed-error, non-disclosure, convergence, and
  no-refresh-replay assertions for those faults; and
- a joined resume test entering production `build_provider` after the active
  account changes, with the active account deliberately made unusable so an
  implicit fallback cannot pass unnoticed.

The old coarse `AuthPublication` fault point was removed rather than retained
as an alias.

## Internal adversarial correction

The conversation-local review task was `/root/p2_fixture_review`. It is recorded
for implementation traceability only; it is not a retained independent P2
review artifact and does not satisfy Gate D.

A read-only reviewer found the first resume test was helper-only: it called
account validation and root resolution separately, so it could not prove the
production builder retained the alias. That finding was fixed before commit.

The replacement proves three production-path outcomes:

1. resumed OAuth construction rejects an omitted account;
2. implicit construction fails after the newly active account is corrupted;
3. explicit resumed construction still succeeds for the requested account.

The same reviewer re-ran the focused case and marked the finding closed with no
remaining finding in the six-file diff.

## Verification

All commands used the repository's normal `target/` directory.

| Command | Result |
|---|---|
| `cargo test -p norn provider::openai_oauth::chain_tests` | 6 passed, 0 failed |
| `cargo test -p norn provider::openai_oauth::credential_recovery::fault_tests -- --nocapture` | 3 passed, 0 failed |
| `cargo test -p norn provider::openai_oauth::revoke::tests -- --nocapture` | 9 passed, 0 failed |
| focused joined production resume test | 1 passed, 0 failed |
| `cargo test -p norn provider::openai_oauth` | 219 passed, 0 failed |
| `cargo test -p norn-cli --lib` | 482 passed, 0 failed |
| `cargo clippy --workspace --all-targets -- -D warnings` | pass |
| `cargo fmt --all -- --check` | pass |
| `git diff --check` | pass |
| scoped `allow`/Clippy-suppression/panic/unwrap/expect scan | no match |

Physical line counts at source commit:

| File | Lines |
|---|---:|
| `credential_recovery_io.rs` | 211 |
| `credential_transaction.rs` | 499 |
| `credential_recovery_fault_tests.rs` | 261 |
| `revoke_tests.rs` | 400 |
| `oauth_chain_tests.rs` | 347 |
| `provider_tests.rs` | 447 |

No lint bypass or file-size exception was added.

The table records the focused command outcomes. Raw outputs and hashes were not
retained, so this summary is not a complete retained Gate C evidence bundle.

## Honest limits

This is not P2 acceptance and does not populate the phase evidence ledger.
Still open:

- the P1 dependency and its D0 exit disposition;
- the owner-approved live two-account A/B/A refresh-validity experiment;
- the missing historical P2 phase-base disposition;
- a complete retained P2 Gate C bundle and independent phase acceptance; and
- the mode-000 unreadable-file fixture is platform-conditional when the
  executing identity can bypass permission bits. It is not represented as
  unconditional permission-denied evidence.

Unrelated pre-existing changes in `.claude/skills/norn/SKILL.md`,
`CONVENTIONS.toml`, `crates/norn/src/tools/diagnostics_check/tests.rs`, and
untracked P1 bytecode directories were excluded from the source commit.
