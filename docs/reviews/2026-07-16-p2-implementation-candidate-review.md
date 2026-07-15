# P2 OAuth implementation-candidate review

- **Review date (Australia/Melbourne):** 2026-07-16
- **Reviewer:** external review seat (coordinator) + one Fable adversarial seat
  (recovery journal) + three independent read-only Opus seats
- **Reviewed head:** `cf38998`; implementation source `4d51a36`
- **Review range:** `cad2ce8..cf38998`
- **Governing plan:** `docs/RESPONSES-API-REMEDIATION-PLAN.md` § P2 (line 1132)
- **Owner rulings:** `DECISIONS-2026-07.md` § 8 (D9 closed 2026-07-16) and § 8
  D9A (timing)

## Verdict: READY as an implementation candidate

The P2 OAuth lifecycle implementation candidate is acceptance-quality. Across a
Fable adversarial attack on the durable recovery journal and three Opus seats
over named-account isolation, the typed `provider.auth` trust matrix, and the
foreign-`$CODEX_HOME` boundary, **no blocker, major, or minor defect was
found** — every finding is a non-blocking observation. The security-critical
guarantees hold, and the plan honestly records what remains.

**This is not P2 acceptance.** Per the plan, P2 acceptance still requires the P1
dependency, the redacted live A/B/A refresh-validity experiment, and retained
Gate C evidence — all explicitly open. This review covers the implementation
candidate only; the phase-evidence and exit-gate boxes remain correctly
unchecked.

## Battery (coordinator)

Native `1.94.0`, repo warm `target/`, alone on host — reproduces the handoff's
verification claims exactly.

| Leg | Result |
|---|---|
| `cargo fmt --all -- --check` | pass |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | pass |
| `cargo test --locked -p norn --lib openai_oauth` | **216 passed, 0 failed** |
| `cargo test --locked -p norn-cli --lib` | **483 passed, 0 failed** |

Mechanical sweep: every new production file < 500 LOC (largest
`account_catalog.rs` ~412 code lines); zero `unwrap`/`expect`/`panic` in new
production files; `mod.rs` files pure.

## What was proven

**Recovery journal (Fable adversarial seat) — SOUND.** The token-free durable
marker defeats every attack traced:
- **No refresh-token double-spend.** The marker is written and fsync'd (file +
  parent dir, temp+rename, under the credential flock) *before* every authority
  dispatch, keyed on a salted SHA-256 of the exact token being spent. A second
  process/manager re-dispatches only when the on-disk token differs from the one
  already spent. The fault suite includes a **real** `current_exe()` child
  process driven against a TCP authority that reads the full request then
  withholds the response, SIGKILL'd mid-flight; the reconstructed parent replays
  zero times.
- **No unrecoverable brick.** The no-time-escape barrier releases on either a
  proven durable commit (on-disk revision == proposed) or an advanced lineage
  (re-login writes a new token). An ambiguous crash forces re-authentication,
  never a permanent lock.
- **Token-free / metadata-free.** The marker holds only a version byte, random
  nonce, prior/proposed revision hashes, a random salt, a salted lineage hash,
  a phase, and an integrity checksum — no raw token, identity, endpoint, path,
  PID, timestamp, TTL, or retry counter. Debug is `[REDACTED]`, mode 0600.
- Durability, single-flock lock composition, and CLAUDE.md compliance all hold.

**Named-account catalog (Opus) — PROVEN (8/8).** Alias→path safety (an alias is
never a path component; only the opaque UUIDv4 storage id reaches a path join;
the `[A-Za-z0-9][A-Za-z0-9._-]*` regex rejects `.`/`..`/`./`, and
`NornAuthRoot` additionally pops `..` lexically; enforced at every entry point);
case-insensitive uniqueness; per-account isolation; atomic+fsync durable catalog
that fails **closed** on corruption (never a silent reset that would orphan
credentials); `default`-slot coexistence; active-selection semantics
(remove-active clears, never reselects; `auth use` affects new providers only);
`logout --all` per-account independence; no identity disclosure.

**Typed `provider.auth` matrix + selection trust (Opus) — PROVEN (5/5).** Only
`oauth`/`api_key` accepted (`env`/blank/unknown → typed error, no value echoed);
OAuth forbids `api_key_env`, api_key requires it, compatible endpoints cannot
select OAuth, Claude Runner accepts neither; the matrix resolves before any env
lookup, credential I/O, provider construction, or network. Crucially, the **P0
selection-trust boundary holds**: account/`auth`/`api_key_env` are rejected on
project and workspace-local layers pre-merge (`provider_security.rs` via
`runtime_init/base.rs:141`); no settings type carries an `account` field; the
selected account comes only from the CLI arg. No untrusted layer can select or
rotate an account. Identity is pinned to a concrete slot path at provider
construction, so a later `auth use` cannot redirect a built provider; resume
rejects the active-account default and requires explicit selection.

**Foreign-`$CODEX_HOME` boundary + disclosure (Opus) — PROVEN (6/6).** Zero
filesystem read/write/lock/chmod/remove targets the foreign Codex file on any
new named login/list/use/status/logout or recovery path; every write routes
through `PrivateRoot(NornAuthRoot)` under `$NORN_HOME/auth/` and cannot be
pointed at `$CODEX_HOME`. No import/keyring/scrape/shell-out surface. Logout is
local-first-durable with remote revoke reported separately on all three paths;
`REVOKE_URL` is compiled (non-configurable). Credential-bearing `Debug` impls
all redact; the recursive foreign-home sentinels assert byte + metadata +
permission equality and sweep for stray Norn artifacts after every surface
(genuine final-state assertion, not "no output observed"); fixtures are
sanitized `alg:none` synthetic tokens.

## Observations (none blocking)

1. **The typed `provider.auth` matrix is a CLI-layer guarantee, not a library
   invariant.** `resolve_provider_auth` lives in `norn-cli`; the library
   provider constructors take `AuthSource` directly and do not traverse the
   forbid/require matrix. The P0 security boundary (untrusted-settings
   rejection) *is* in the library (`provider_security.rs`), so this is not a
   security hole — but an embedder that builds providers itself (e.g. Meridian)
   does not get the oauth-forbids-`api_key_env` / api_key-requires-`api_key_env`
   / compatible-cannot-select-OAuth checks. Worth an owner decision on whether
   those rules should become a library invariant before P2 acceptance, given
   Meridian is a consumer.
2. **Orphaned Pending slot directory.** A named login dropped without `abort()`
   (no `Drop` impl on the reservation) leaves a `Pending` catalog record and an
   `accounts/<uuid>/` directory. It self-heals on next login of the same alias
   and is swept by `logout --all`, but an alias never retried leaks a hidden
   record + directory. Disk-only; invisible to `list`; no shadow or safety
   impact.
3. **A malformed catalog blocks the `default` slot too.** Corruption fails
   closed with a typed error (correct — never a silent reset), but because
   `auth list`/`resolve` load the catalog first, a corrupt index also hides the
   legacy default slot. Worth an owner note on desired recovery ergonomics.
4. **Recovery-journal integrity is an unkeyed SHA-256 checksum, not a MAC.**
   Only forgeable by a process that already owns the 0600 credential file —
   outside the threat model; it correctly defends torn writes/corruption.
5. **Non-rotating refresh tokens are conservatively blocked** after an ambiguous
   crash (an unnecessary re-login). This is the intended no-TTL trade-off and is
   correct for OpenAI's rotating tokens.

## Plan honesty

All four seats independently confirmed the P2 checklist `[x]` items match the
implementation and the phase-evidence `[ ]` items are genuinely open (retained
Gate C execution, the live validity experiment, and the P1 dependency). No
finish-line overclaim. D9 is recorded closed 2026-07-16 with the storage,
selection, recovery-marker, and provider-auth contracts fixed; D9A timing closed
separately.

## Remaining before P2 acceptance

Not covered here and still open: the P1 dependency, the redacted live A/B/A
refresh-validity experiment (Gate A named-account branch requirement), and
retained Gate C evidence for the phase-specific evidence matrix. Observation 1
(library-vs-CLI matrix enforcement) is an owner decision worth taking before
acceptance.
