# P2 headless device-auth Gate D review

**Date:** 2026-07-23

**Reviewer:** Sable Nightwick (external Gate D coordinator)

**Handoff:** [`2026-07-23-p2-device-auth-handoff.md`](2026-07-23-p2-device-auth-handoff.md)

**Reviewed boundary:** base `6d168830ee3c4edad5893d39a0e1e67950da98ad`, frozen
source `dc908f378185ec5568f54e38209b61c2a9a9f124` (tree
`b763308c866d929fa6d7dbf1bf1907480d350493`), range `6d16883..dc908f3` (28 Rust
files, 2,951/412). Branch head `b894a3e`; `dc908f3..HEAD` is documentation only.
`main` untouched by this review.

**Panel:** two Opus 4.8 seats (endpoint-sealing/disclosure; durability/
cancellation/CLI), one cross-model adversarial seat (norn GPT-5.6 Sol, review/
safety, xhigh; session `claude-review.SxtQbw`), plus my own gates, boundary
reproduction, and a PKCE-guard mutation.

## Verdict

**READY as a narrow implementation candidate**, with a prioritized should-fix
list to close before P2 live acceptance. No reachable BLOCKER or MAJOR survived
adversarial reproduction: every guarantee the handoff actually makes for the
shipped CLI surface holds. The findings below are genuine robustness/hygiene
gaps on the credential path — worth closing — but none is a reachable exploit
that breaks a stated guarantee on the honest contract. This is **not** P2
acceptance: the live A/B/A experiment, authenticated device proof, retained Gate
C, and the D7/P9 authenticated Responses live-wire gate all remain open, as the
handoff states.

## Adjudication of Sol's NOT-READY (MAJOR → downgraded to MINOR-A)

Sol reported that `TokenPollResponse` (`device_login.rs:130-135`) has no
`deny_unknown_fields` and no success/error envelope, so a `200` body carrying
both an OAuth `error` and a full valid code tuple is accepted as success and
reaches token exchange. **The mechanism is real** — I confirmed the type has
three required `String` fields and no `deny_unknown_fields`, so serde silently
ignores an `error` member.

**But the reachability is defused.** All three fields are required and
non-optional, and `validate_authorization_response` requires each to be
nonempty, control-free, and internally PKCE-consistent
(`challenge_for(verifier) == challenge`, byte-exact) before exchange. An honest
authority signaling *pending* has **no `authorization_code` yet** (it is minted
only on user authorization), so a genuine pending-via-200 body omits or empties
that required field and fails the parse or `valid_opaque_value` before exchange.
The only body that reaches exchange while carrying an `error` is one that *also*
supplies a complete PKCE-consistent code tuple — self-contradictory, emitted
only by a hostile authority, which already wins trivially by returning a clean
`200` success with no `error` field. Ignoring the field therefore grants a
hostile authority **zero marginal capability** and cannot harm an
honest-contract user. No stated guarantee is broken (the handoff guarantees
status-based pending and PKCE-consistency-before-exchange, both of which hold;
it never guaranteed body-level error rejection).

This is a real boundary-validation gap by the project's own "validate at
boundaries" rule, on the highest-sensitivity path, and matches the
structure-fragile-parse failure mode that produced P5 CODEX-02. **MINOR-A**:
recommend an explicit success/error envelope that never takes the success path
when an `error` member is present, plus a fixture returning `200` with
`authorization_pending` alongside an otherwise-valid tuple asserting exchange/
save/publish are never reached. Not a candidate blocker.

## Other findings (all MINOR/HARDENING; none blocks the candidate)

- **MINOR-B — orphaned live credential on abort ordering** (pre-existing;
  `account_catalog.rs:127-141`, byte-identical at base). `abort()` durably
  removes the catalog record *before* the slot directory. After a post-exchange
  commit failure (`DuplicateIdentity` is caller-producible by logging the same
  ChatGPT identity under a second alias), a crash — or a `remove_slot` failure,
  which the Drop path only warn-logs — leaves a durably saved `auth.json` with a
  live refresh token in `accounts/<uuid>/` with no catalog record.
  Catalog-driven `logout --all` and `list` cannot see or revoke it. Same-user
  (not a cross-user exposure), pre-existing, but this range newly exposes it via
  the device path. Fix direction: reverse the abort ordering (a pending record
  pointing at a missing slot self-heals via `prepare_named_login`), or sweep
  unreferenced slot directories in `logout --all`. **Prioritize this one** —
  an unrevocable live credential on disk warrants closing even though
  pre-existing.
- **MINOR-C — `block_in_place` panic under `LocalSet`** (`login_commit.rs:70-79`).
  `commit_without_yield` calls `block_in_place` whenever `runtime_flavor() ==
  MultiThread`, which panics inside a `LocalSet` on a multi-thread runtime. A
  library embedder awaiting `provider::auth::login()` under
  `LocalSet::run_until` gets a panic instead of a typed error. Integrity
  survives (unwind drops the transaction and retires the reservation before any
  save); the CLI uses plain `rt.block_on` and is unaffected. Owner call:
  document beside residual #3 or always run inline.
- **MINOR-D — poll status-classification is a live-contract risk** (endpoints
  seat). 403/404 = pending, all else terminal. A transient 429/5xx during
  polling permanently kills a login mid-approval, and operator *denial* (if the
  authority signals it as a persistent 403) is indistinguishable from pending
  (burns the full deadline, reports "expired"). Neither is provably wrong
  without the live contract (handoff residual #1). The live gate should observe
  actual denial and rate-limit statuses before this classification is accepted.
- **HARDENING** — response bodies are read unbounded (`response.json()`) and
  `user_code` length is unbounded before stderr printing (memory-exhaustion
  vector under a compromised authority; a byte cap closes it); the device-code
  presenter call sits outside `DeviceDeadline` (a blocked stderr stalls the
  login — liveness only, integrity unaffected).

## Verified SOUND (with evidence)

- **Endpoint sealing.** All five authority URLs are compiled `pub(super)`
  constants; `DeviceEndpoints::production()` (constants only) is the sole
  production constructor, unconditional in `DeviceLoginOptions::new`;
  `with_test_authority`/`test_authority` are `#[cfg(test)]`. No configuration
  surface (`LoginConfig`, `OAuthHttpOptions`, CLI flags, env beyond `NORN_HOME`)
  reaches the endpoints; the Responses `base_url` override never touches this
  flow; redirects are disabled (`Policy::none()`), so a proxy can deny but not
  redirect. The `exchange_code_async` token-URL parameter (residual #7) is
  `pub(super)`, called only with the sealed constant — a review seam, not a
  production surface.
- **Disclosure.** The browser URL, device verification URL, and one-time code
  reach only `LoginPromptPresenter` (CLI = locked stderr). Every new
  secret-bearing type has a redacting `Debug` (or none); `LoginPrompt` has no
  `Display`. Errors carry `&'static str` stage + `u16` status, never response
  bodies; failure tests assert body secrets absent from `{error}`/`{error:?}`.
  No URL/code/verifier/token reaches tracing, session events, provider events,
  panic messages, or argv (Linux/BSD desktop launch is disabled precisely
  because the launcher would put the URL in argv; macOS delivers via child stdin
  with `env_clear()`). The printed verification URL is the compiled constant,
  not server-echoed — a compromised authority cannot inject a phishing URL; and
  `terminal_safe_code` (ASCII-graphic) blocks escape-sequence injection before
  presentation.
- **Deadline.** Monotonic `tokio::time::Instant`; `saturating_sub` (no
  under/overflow); every authority await wrapped (user-code, each poll, each
  sleep, exchange), `persist_prepared_login` outside — exactly the claimed
  boundary. A straddling poll cannot carry the exchange past the total; zero
  timeout rejected before auth-root access; a `"0"`/non-numeric server interval
  is `DeviceCodeMalformed` before any prompt (no spin-loop).
- **Durability/cancellation.** The no-await claim is literally true: after the
  `spawn_blocking` acquire returns, validate → `save_if_revision` → `commit()`
  run in one synchronous poll under `commit_without_yield`. The blocking task
  returns closures, never acts, so a dropped future cannot publish. Revision is
  re-read under the held flock before the temp-write/rename (the under-lock
  re-check). Concurrent same-default/same-alias logins are serialized
  (`LockTimeout`/`ReservationLost`). Credential writes are
  create-temp→write→fsync→rename→dir-fsync — never truncate-in-place — so a
  failure leaves the canonical file untouched.
- **CLI.** "Login successful." prints only after durable save and (named)
  catalog publication; the browser URL is presented before launch; presentation
  failure is typed and retires the named slot; `--device-auth --name` parses,
  `--name default` is rejected.

## My gates (repository shared target, at `b894a3e` = frozen Rust)

`cargo fmt --all -- --check` clean; `cargo clippy --locked --workspace
--all-targets --all-features -- -D warnings` **exit 0**, no suppression; focused
`device_login` **17/17**, `provider::auth` **31/31**, `provider::openai_oauth`
**240/240**; doctests **8/8**. Added-line token scan: zero production
`unwrap`/`expect`/`panic!`/`allow`; the 25 added `expect(`s are all in the four
new test files. New-file sizes reproduce (max 454 test, 389 production).

**Full-workspace honesty:** I did **not** re-run the entire
`--workspace --all-targets --all-features` suite from a cold build for this
branch (it would recompile the whole workspace against the shared target from
scratch). I rely on the handoff's honestly-disclosed full-library observation
(**4,326 passed / 4 failed**, each of the four reproduced as passing in
isolation and classified as sandbox/contention — mcp-stdio, process-watch,
bash-redirect, descriptor-retention) together with my focused reruns above,
clippy, doctests, and mutation. This is a deliberate rigor/time tradeoff stated
plainly, not a claimed clean full sweep. A clean retained full-suite phase gate
remains required for P2 Gate C, as the handoff says.

**Coordinator mutation:** neutering the PKCE consistency guard
(`challenge_for(verifier) != challenge`) in `validate_authorization_response`
failed exactly `malformed_poll_successes_never_reach_token_exchange` (16/17
pass) — a precise single-guard kill confirming a server-issued mismatched
challenge is rejected before exchange. Restored byte-clean; worktree clean at
`b894a3e`.

## Boundaries

- READY is a bounded implementation-candidate verdict. The should-fix list
  (MINOR-A envelope parse and MINOR-B orphan ordering first, then MINOR-C/D)
  should close before P2 live acceptance; none blocks the candidate.
- Device-path "PKCE validation" is server-issued-pair consistency, **not** RFC
  7636 client proof-of-possession (the browser path retains genuine client-side
  PKCE). Record this so later reviewers do not credit the device path with
  interceptor resistance it does not have.
- Out of scope, unchanged: P2 live A/B/A, authenticated device proof, retained
  Gate C, whole-P2 acceptance, and the D7/P9 authenticated Responses live-wire
  gate.
