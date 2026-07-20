# P5 `AFFINITY-01` Gate D review

**Date:** 2026-07-20

**Reviewer:** Sable Nightwick (coordinator) + four Opus area seats (identity/disclosure;
durability/crash-safety; concurrency/fork/child; loop/transport/scoping) + norn
cross-model pass (GPT-5.6 Sol, safety preset, read-only, session
`fb7673c5-8ce8-4ee0-b1e1-fca932a1edcd`)

**Handoff:**
[`2026-07-20-p5-affinity-01-gate-d-handoff.md`](2026-07-20-p5-affinity-01-gate-d-handoff.md)

**Reviewed source:** `58df839`, tree `2173490`, product range `5e04281..58df839`
(over accepted CODEX-02 merge base `5e04281`)

## Verdict

**AFFINITY-01: NOT READY as an implementation candidate.** One BLOCKER-class
finding (reproduced) and one MINOR, both found by the norn cross-model pass; the
BLOCKER additionally contradicts the identity seat's SOUND verdict, and the MINOR
I independently co-found while reading the adoption transaction. The large
correctness/durability/concurrency surface is otherwise strong: every other
reviewer question across four Opus seats verified SOUND, with the load-bearing
claims re-verified by me.

## AFFINITY-1 (BLOCKER-class — reproduced) — OAuth affinity collapses to account-only when the user claim is absent, defeating the stated user-level boundary

The handoff's "outcome in practice" promises that "a different OAuth account **or
user** … fails before a user message is appended or an HTTP request is sent."
The code does not deliver user-level isolation when the id_token lacks a user
claim.

- `CredentialIdentity::from_oauth_principal(account_id, None)`
  (`crates/norn/src/provider/state_identity.rs:46-49`) derives identity from the
  account id plus a fixed `user-absent` marker only — **no token or user
  lineage**. The bearer/refresh token is not an input to the OAuth principal.
- `user_id = None` is a **reachable, accepted** credential form:
  `chatgpt_user_id` is `Option` in the parsed id_token
  (`openai_oauth/jwt.rs:70`); `AccountIdentity::from_auth` copies it including
  `None` while requiring only `account_id` (`openai_oauth/manager.rs:58-64`); and
  `ChatGptTokens::validated` requires access/refresh/account but **not** a user
  claim (`openai_oauth/types.rs:53-73`). `account_id` here is the ChatGPT account
  (org/workspace accounts are shared across members).
- **Reproduced end-to-end** (temporary test, since removed; worktree restored
  byte-clean): two `from_oauth_principal(shared_account, None)` principals scoped
  to the same backend/endpoint derive the **identical** `ProviderStateIdentity`,
  while `Some("user-A")` vs `Some("user-B")` correctly differ. When the user
  claim is present, users are distinguished; when absent, they collapse to the
  account.

**Consequence:** in a deployment where two distinct users share one ChatGPT
account and their tokens carry no user claim, one user's session can be resumed
or forked under the other's credential — the durable identity compare succeeds,
so the second bearer receives the first user's persisted Responses anchor
(`previous_response_id`) instead of failing closed. The affinity boundary's
entire purpose is to prevent exactly this cross-principal state crossing.

**Honest reachability caveat.** The *concrete* cross-user exploit depends on the
ChatGPT issuer actually emitting user-less id_tokens for multiple distinct users
of one shared account — which repository evidence neither proves nor refutes. The
certain part is that the code **accepts** the user-less form and **cannot prove**
the user-level isolation it advertises; the identity is injective in
`account_id` but not in "user," contradicting the stated guarantee.

**Why this is blocking under the patient-records standard.** A credential-isolation
boundary that rests on an unstated external assumption and claims more isolation
than it delivers is not trustworthy for the stated purpose. Resolution is likely
an **owner ruling** on the acceptable granularity:
- If norn's model is single-user machines / account-level is the intended
  boundary → correct the "a different user fails" overclaim in the handoff/docs,
  and add a guard/test making the account-level contract explicit; **or**
- If user-level isolation is required → require a stable per-user claim for
  stateful OAuth (reject stateful construction/resume when absent), or fold a
  stable credential discriminator (e.g. token lineage) into the missing-user
  identity, validated across refresh. Add a regression: two valid bearer bundles
  sharing an account id and lacking a user claim must not resume each other's
  state.

The identity seat rated this SOUND on the reasoning that "an actor able to
rewrite auth.json already owns the credential." That threat model is too narrow —
this is not attacker-forges-account; it is **two legitimate users of one shared
account**, whom the boundary silently fails to separate. The norn cross-model
pass caught the case the Opus panel missed.

## AFFINITY-2 (MINOR — confirmed by inspection + independent corroboration) — stale unbound snapshot can append a redundant same-identity adoption cut

In `validate_or_bind_provider_state_identity_inner`
(`crates/norn/src/session/persistence/index_timeline.rs:152-160`), the
equal-identity arm returns a no-op **only** when the caller's captured
`registered.provider_state_identity` was already `Some`. A caller that resolved
the row while unbound (`registered = None`) falls through even when the on-disk
row is now bound to the **same** identity. Recovery reuses an existing adoption
boundary only when it is the timeline **tail** (`:178-179`); if the winning
handle appended any normal event meanwhile, `:181-196` mint and fsync a **second**
`ProviderIdentityAdoption` boundary and re-publish the already-equal binding.

Reachable sequence: two provider-aware opens resolve the same legacy unbound row;
open A binds it and appends a prompt/turn; delayed open B enters with its stale
unbound snapshot and the same identity, sees A's normal event at the tail, and
appends a second adoption cut.

**Severity MINOR:** same-credential only — no cross-account crossing and no
phantom binding, and it fails safe. But it contradicts the handoff's explicit
claim that adoption "appends one boundary" and that interruption retry "reuses
it" (only a *terminal* boundary is reused), and the redundant cut discards a
valid response anchor just produced by the same credential, splitting concurrent
same-principal work into an unnecessary fresh epoch. Fix: when the current row is
already bound to the requested identity, return the no-op validate (reload the
authoritative timeline) rather than re-entering adoption; add a deterministic
interleaving test asserting exactly one adoption boundary. I independently
identified this while reading the transaction; the norn pass confirmed it; the
durability and concurrency seats rated the path SOUND (they verified the security
boundary and happy-path convergence — the 20/20 concurrent-adoption evidence
proves convergence, not "exactly one boundary").

## Verified SOUND (all load-bearing claims re-verified by me)

- **Identity construction:** domain-separated SHA-256 with each part
  independently hashed (fixed 32-byte framing → no concatenation-ambiguity
  collision), distinct null-terminated domains per credential type, arity length
  differences prevent public-derive vs built-in-scoped collision. OAuth stable
  across token refresh (pinned `(account_id, user_id)`, re-derived not
  re-pinned); static-codex changes on rotation (includes access token);
  endpoint normalization converges only equivalent trailing-slash spellings.
  (The user-absent collapse above is the one exception — the digest is not
  injective in "user" when the claim is absent.)
- **Disclosure:** `CredentialIdentity` has no Serialize (ephemeral);
  `ProviderStateIdentity` Debug is `[REDACTED]`, no raw-byte accessor; error
  variants are payload-free unit types; adoption events carry no digest; the CLI
  `PublicSessionIndexEntry` **exhaustively destructures** with
  `provider_state_identity: _` (compile-enforced omission), proven by the
  list/export integration fixture. The trusted-embedder serialize path is
  honestly disclosed.
- **Crash safety:** the adoption boundary is `write_all` + `sync_all`'d before
  the `boundary_durable()` crash hook, the identity is stamped only after, and
  the atomic index publish (temp-then-rename + fsync) is last — all under one
  continuously-held index lock. A crash between fsync and publish leaves an
  unbound row with at most one harmless durable boundary; retry reuses a terminal
  adoption tail. A bound row never precedes its cut. (I read the full transaction
  at `index_timeline.rs:135-211`.)
- **Cross-handle race:** `open_registered_timeline_bound` compares the sink's
  expected identity against the on-disk row **under the retained index lock**,
  atomically with the write+fsync (no TOCTOU), failing closed with a typed
  mismatch. A stale unbound handle cannot append after another handle bound a
  different identity. (I read `index_timeline.rs:64-92`.)
- **Fork/child:** empty-source preflight precedes adoption; source identity
  validated before child publication; child inherits the copied binding; gated at
  both loop-setup and `publication_parent`. Fail-closed on mismatch.
- **Fail-before-mutation:** `validate_or_bind_store_identity` at `setup.rs:61`
  runs before the `require_state_identity` guard (`:123`) and the prompt append
  (`:129`); provider revalidates before headers/producer. Mismatch → payload-free
  terminal error, zero prompt mutation, zero dispatch. (I read `setup.rs:55-142,
  251-270`.)
- **Auth preservation:** missing OAuth credentials stay `AuthenticationFailed`
  (exit 3, `norn auth login` guidance), not flattened into the affinity error.
- **CODEX-02 interaction:** turn-state redaction/first-wins/backend-gating
  unchanged; turn state now additionally bound to credential identity.
- **My battery** (network-capable, primary-repo target): fmt clean, clippy
  `-D warnings` clean, full workspace green — norn 4,093 / cli 502 / tui 683,
  matching the handoff — with 30 affinity-focused tests passing.

## Observations (non-blocking, carried)

1. Framing asymmetry: `single_part_digest` (api-key) vs `domain_separated_digest`
   (all others) — safe today (single-part, distinct domain), latent trap if a
   future multi-part credential reuses `single_part_digest`.
2. `bind_state_identity` runs for all OpenAI backends while the turn-state header
   is codex-only — positive defense-in-depth.
3. Pre-existing torn-tail-without-newline window on the shared timeline append —
   fails loud/typed (`TornTail`), leaves the row unbound, not introduced by this
   slice.
4. The public `SessionIndexEntry` remains serializable by a trusted embedder
   (digest observable) — honestly disclosed.

## Boundaries

- This is an AFFINITY-01 candidate review, not P5 acceptance.
- AFFINITY-1's cross-user reachability should also be confirmed against the real
  ChatGPT issuer at the mandatory D7/P9 authenticated real-wire gate.
- D3, D8, resume/concurrency-broader, WebSocket transport, P2, and whole-P5
  acceptance remain open.
- Expected path: resolve AFFINITY-1 (owner ruling on granularity + code/doc fix)
  and AFFINITY-2 (no-op on stale same-identity), then narrow same-reviewer
  confirmation.
