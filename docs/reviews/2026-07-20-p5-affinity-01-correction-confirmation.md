# P5 `AFFINITY-01` correction confirmation

**Date:** 2026-07-20

**Reviewer:** Sable Nightwick (same reviewer as the AFFINITY-01 Gate D verdict)

**Controlling review:**
[`2026-07-20-p5-affinity-01-gate-d-review.md`](2026-07-20-p5-affinity-01-gate-d-review.md)
(`0b25d82`, NOT READY on AFFINITY-1 + AFFINITY-2)

**Correction commit:** `693d5b1` (15 Rust files over the reviewed source `58df839`)

**Confirmed head:** `43064ed` on `codex/p5-credential-affinity` (docs commit over
the `693d5b1` product source)

**Correction tree:** `1b4a7c0` (matches the handoff)

## Verdict

**AFFINITY-1 CLOSED; AFFINITY-2 CLOSED; AFFINITY-01 is now READY as an
implementation candidate.** The implementer took the stricter option for
AFFINITY-1 — retaining user-level isolation by failing closed on a missing user
claim rather than narrowing the guarantee — so no owner ruling on granularity is
needed. Whole-P5 and P2 acceptance remain out of scope; the four non-blocking
observations from `0b25d82` carry unchanged.

## AFFINITY-1 — CLOSED (structural elimination + mutation-verified)

The account-only collapse is now **impossible by construction**, not merely
guarded:

- `CredentialIdentity::from_oauth_principal` now takes a **mandatory
  `user_id: &str`** (`state_identity.rs`); the entire `None`/`user-absent`
  branch is deleted. There is exactly one production caller
  (`manager.rs:242`), inside the `Present` arm reached only after a nonempty
  user check — so no code path can derive an OAuth identity without a user id.
  My original reproduction (`from_oauth_principal(account, None)`) no longer
  compiles.
- `AuthManager::credential_identity` returns a three-way `OAuthCredentialIdentity`
  (`MissingCredentials` / `MissingUserIdentity` / `Present`);
  `MissingUserIdentity` fires for both `None` and empty-string user ids.
- `OAuthAuthProvider::resolve_credential_identity` maps `MissingUserIdentity` to
  a typed, non-disclosing `AuthenticationFailed`, and `OpenAiProvider`
  construction now propagates it via `resolve_credential_identity()?` — so a
  userless credential fails at **provider construction**, before any session or
  wire mutation. `MissingCredentials` retains the existing `norn auth login`
  guidance.
- The object-safe `AuthProvider::credential_identity` signature is unchanged; the
  new `resolve_credential_identity` defaults to it, preserving external
  implementations.

**Mutation kill:** reintroducing the collapse (userless → `Present` with an
empty user) fails `stateful_oauth_provider_rejects_a_userless_credential`;
restored clean. That regression constructs a userless OAuth provider, asserts
construction fails with `AuthenticationFailed` containing "stable user identity"
and **not** the account or access-token sentinels (non-disclosure), while the
sibling test confirms the missing-credential path still yields the
`norn auth login` guidance. Present-`(account, user)` identity is unchanged
(same domain and inputs), so token refresh keeps the same identity and existing
sessions remain compatible.

## AFFINITY-2 — CLOSED (mutation-verified)

The stale-snapshot fall-through is replaced by an explicit transition outcome.
`validate_or_bind_provider_state_identity_inner`'s equal-identity arm
(`index_timeline.rs:157-165`) now always returns: `Validated` when the caller's
snapshot was already bound, `AlreadyBoundByPeer` when a stale unbound snapshot
finds the row already bound to the same identity. `AlreadyBoundByPeer` returns
**before** any timeline append or index publish, so no second
`ProviderIdentityAdoption` cut can be minted. Its single consumer,
`ManagedProviderAffinity::validate_or_bind` (`provider_affinity.rs:47-52`), maps
it to the typed, payload-free `ProviderStateIdentityReopenRequired` — a stale
loaded store must reopen rather than graft the peer's cut into its old in-memory
history. A fresh manager resume/fork reads the current bound row and gets
`Validated`, continuing normally; only a store opened before the peer bound hits
the reopen path. Compilation proves no stale `.adoption_boundary` reader remains.

**Mutation kill:** reverting the equal-arm to the old fall-through fails both
`stale_same_identity_validation_reuses_the_winners_single_adoption_cut` and
`stale_loaded_store_must_reopen_after_peer_adopts_the_same_identity`; restored
clean. The two seam tests bind: a stale transaction observing a winner that
advanced the timeline changes no timeline byte and a canonical reopen sees
exactly one adoption boundary; a stale loaded store receives the reopen-required
error, cannot alter the timeline, and succeeds after reopen.

## Confirmation checklist

1. **Userless credential cannot construct a stateful provider or derive
   account-only affinity — CONFIRMED** (structural elimination + regression +
   mutation).
2. **Complete `(account_id, user_id)` identity preserved across refresh —
   CONFIRMED** (user-branch derivation unchanged; refresh-stability and
   cross-user-inequality tests pass).
3. **Stale same-identity snapshot cannot append a second boundary or republish —
   CONFIRMED** (returns `AlreadyBoundByPeer` before append; mutation-killed).
4. **Loaded pre-adoption store cannot continue without reopening — CONFIRMED**
   (`ProviderStateIdentityReopenRequired`; stale-loaded-store test).
5. **Tests fail if either correction is removed; errors free of credential/digest
   material — CONFIRMED** (both mutation-killed; userless error carries no
   account/token, identity errors are payload-free).

## My battery (network-capable, primary-repo target, at `693d5b1`)

`cargo fmt --all -- --check` clean; `cargo clippy --workspace --all-targets
--all-features -- -D warnings` clean; full workspace green — norn 4,097 / cli 502
/ tui 683, matching the handoff. Correction tree `1b4a7c0` confirmed.

## Boundaries

- AFFINITY-1 and AFFINITY-2 are the only items this confirmation covers. The four
  non-blocking observations from `0b25d82` carry unchanged.
- The real ChatGPT issuer's claim shape (whether userless tokens are issued at
  all) remains a D7/P9 authenticated real-wire check; the correction fails closed
  regardless of the answer.
- The named-account catalog's account-ID-based duplicate fingerprint is a
  separate P2 decision, as the handoff notes.
- D3, D8, WebSocket transport, P2 acceptance, and whole-P5 acceptance remain
  open. This is a candidate confirmation, not P5 acceptance; merge is
  appropriate and acceptance remains the owner's action.
