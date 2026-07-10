# Remediation review — credential & endpoint security (SEC-01, BACKEND-01, AUTH-01..05)

> **Coordinator intake note:** This report is preserved as provisional review
> input against `7d121c9`. It is not a P0 phase or Gate D verdict. Open
> findings require tracking, a final fix-round review, and complete machine-gate
> evidence.

**Reviewed substrate:** frozen snapshot taken 2026-07-11 **while the implementer was still
working** (snapshot HEAD `7d121c9` "docs: record P0 security remediation and evidence";
base pin `41ea210`). All code references below are to the snapshot, not the live tree.
Build/clippy/test gates were **not** run (prohibited for this review); every verdict is
from code trace only and remains subject to the owner's gate runs.

**Scope:** `config/provider_security.rs`, `provider/endpoint.rs`, `provider/openai/backend.rs`,
`provider/openai/provider.rs`, `provider/auth.rs`, `provider/openai_oauth/*`,
`norn-cli/commands/auth.rs`, `norn-cli/print/provider.rs`, plus read-for-context on the
settings loader, merge, CLI resolve pipeline, `http_client.rs`, and the two docs
(`docs/reviews/2026-07-10-responses-api-implementation-review.md`,
`docs/RESPONSES-API-REMEDIATION-PLAN.md`).

**Note on the reported live diagnostic:** the flagged rustc error ("incorrect unicode
escape sequence" at `provider_security.rs` ~line 120) is **not present in the snapshot**.
The only unicode escapes in that file are the valid brace forms `"hostile\u{7}profile"` /
`'\u{7}'` at lines 424/434, and the full-since-base patch carries the same brace form. Either
it was fixed before the snapshot was cut or the diagnostic referred to unsaved editor
state. I could not run rustc to independently confirm the file compiles.

---

## 1. Verdict table

| Finding | Verdict | Evidence |
|---|---|---|
| SEC-01 (repo `base_url` redirects OAuth bearer + account header) | **Closed** (code-trace; gates pending) | Two independent layers, both traced end-to-end. **(a) Trust boundary at load:** `config/provider_security.rs::validate_working_directory_authority` rejects `provider.base_url`, `api_key_env`, `auth`, `options`, `debug_dump_dir`, `runner_path` (and profile `api_shape`, backend-bearing `model_aliases`, indirect selection via `model` → user backend alias, trusted alias/profile shadowing) in the **raw** project/local layers, *before* `merge_settings`, at all three production entry points: `norn/src/runtime_init/base.rs:122`, `norn-cli/src/runtime/resolve.rs:99`, `norn-cli/src/commands/session.rs:93`. **(b) Endpoint pinning at construction:** `openai/backend.rs::OpenAiBackend::resolve` runs before `build_from_auth_source` (`openai/provider.rs:63-65`); under OAuth any non-canonical `base_url` errors, and even a canonical spelling is discarded in favour of the compiled `CHATGPT_BASE_URL`. Test `hostile_oauth_destination_is_rejected_before_auth_application` asserts `apply_call_count == 0`. Redirect replay is dead: all three clients use `redirect::Policy::none()` (`provider/http_client.rs`), tested for 301/302/303/307/308, blocking, and relative same-origin. `provider_options` cannot move the endpoint (merged into payload JSON only, protected keys enforced in `openai/request.rs:199-267`). Catalog-model precedence is consistent between the validator (`provider_security.rs::is_catalog_model`) and the resolver (`norn-cli/config/model_aliases.rs`), so a user alias named after a catalog model cannot slip past the indirect-selection check. Residuals: R1, R2 below. |
| BACKEND-01 (backend inferred from absence of override) | **Closed** | `OpenAiBackend` is an explicit enum (`CodexSubscription` / `ResponsesApi { base_url }`), resolved once from `(auth_source, configured_base_url)` and owned by the provider (`openai/backend.rs:20-49`). An explicit canonical ChatGPT URL (incl. `:443`, trailing slash, case, surrounding whitespace) resolves to the same variant and the compiled URL. `security_tests::implicit_and_explicit_canonical_oauth_have_identical_semantics` asserts identical base URL, endpoint, catalog backend, and capabilities; `..._serialize_identical_payloads` asserts byte-identical payloads (`store:false`, no threading). `catalog_backend()` tracks the connection, not the default. |
| AUTH-01 (flat vs namespaced JWT claim) | **Not closed** | `openai_oauth/jwt.rs::IdTokenClaims` (lines 31-40) still deserialises only the **flat** shape (`email`, `chatgpt_plan_type`, `chatgpt_user_id`, `chatgpt_account_id`). There is no handling of the namespaced `https://api.openai.com/auth` claim object anywhere in `openai_oauth/` (grep: zero hits). The only jwt.rs change since base is Debug redaction. No realistic-shape fixture exists: tests use placeholder strings ("Id Token") and hand-built structs, never a base64url JWT with the namespaced claim. Consequence trace: on a real current Codex id token the namespaced object is silently ignored (serde drops unknown fields — **no error is even raised**), so `login_server.rs:341-343`'s fallback `token_response.account_id.or_else(|| id_token.chatgpt_account_id.clone())` yields `None` whenever the token endpoint omits top-level `account_id`, and `apply_auth` then omits the `chatgpt-account-id` header. The implementer's own review doc marks this Open (P2) — the claim is honest. |
| AUTH-02 (cross-process refresh race) | **Partially closed** | In-process is genuinely fixed: single-flight `refresh_gate` + `refresh_epoch` (`openai_oauth/manager.rs:226-260`) with a real concurrency test (`concurrent_refreshes_collapse_into_single_exchange`, wiremock, asserts exactly 1 exchange) plus non-poisoning and sequential-refresh tests. **Cross-process is not:** no interprocess lock exists (grep: none); `refresh_token_from_authority` spends the **cached** refresh token without reloading `auth.json` first (`manager.rs:235-241`); `save_auth_dot_json` is atomic for integrity but last-writer-wins for content. Failure scenario (CONFIRMED from code): norn and Codex CLI share `~/.codex/auth.json` under token rotation; both refresh concurrently → the loser gets 401 → `classify_401` default arm → `Permanent("refresh token rejected")` → spurious forced re-login; worse, if norn's exchange resolved first but its **save lands second**, it persists nothing stale — but if norn refreshes from a stale snapshot *after* the CLI rotated and saved, norn's save can overwrite the CLI's fresh rotated token with the result of spending an already-rotated token lineage, killing both processes' credentials. The reload-lock-refresh-save transaction the original review recommended is absent. The doc honestly marks this "Partially fixed". |
| AUTH-03 (swallowed credential-load / proactive-refresh failures) | **Not closed** | All three cited sinks survive: (1) `manager.rs:129-131` — `load_auth_dot_json(..).ok().flatten()` at manager construction still converts a corrupt/unreadable `auth.json` into "logged out"; the eventual user error is `"no OAuth token found; run the login flow"` (`provider/auth.rs:162`) — a false message. `storage.rs::load_file` produces a proper typed error incl. `InvalidData` for bad JSON, so the type system offers the distinction and the manager throws it away without even a `tracing::warn`. (2) `manager.rs:204-211` — `auth()` swallows a **Transient** proactive-refresh failure silently and returns the stale (known-expired) token; no log line. (3) `manager.rs:207` — a **Permanent** proactive-refresh failure collapses to `None`, again surfacing as "no OAuth token found" even though a credential exists and refresh failed for a nameable reason. Additionally `types.rs:56-58` (`IdTokenInfo::from_raw_jwt`) swallows claim-parse errors into `IdTokenClaims::default()`. Marked Open (P2) by the implementer — honest. |
| AUTH-04 (premature browser "login complete") | **Not closed** | `login_server.rs::wait_for_callback` responds `"Login complete. You can close this browser window…"` (lines 219-227) and returns the code; `run_callback_server` (lines 153-172) only **then** performs `exchange_code_blocking` and `save_auth_dot_json`. The browser can display success while the exchange or the disk write subsequently fails. (The CLI-side `"Login successful."` in `commands/auth.rs:50` *is* correctly emitted only after `block_until_done`, i.e. post-save — the defect is specifically the browser page, exactly as originally found.) The exchange is already blocking on the same thread, so responding after save is feasible with `tiny_http`. Marked Open (P2) — honest. |
| AUTH-05 (revoke failure blocks local deletion) | **Not closed** | `revoke.rs::logout_with_revoke` (lines 43-50) still runs `revoke_refresh_token(...).await?` **before** `delete_auth_dot_json`; any network/authority failure propagates and the local credential stays installed. `commands/auth.rs::run_logout` then prints `"norn: logout failed"` and exits `AuthError` with `auth.json` intact — the user believes they logged out of a machine and did not. No separate revoke-status reporting exists. The doc comment ("deletes auth.json on success") documents the defective behaviour rather than fixing it. Marked Open (P2) — honest. |

**Cross-check of the implementer's status claims:** the updated review doc and
`RESPONSES-API-REMEDIATION-PLAN.md` claim only that a **P0 candidate** addresses
SEC-01..SEC-14 + BACKEND-01, and explicitly leave AUTH-01..05 as Open/Partially-fixed P2
work. Every status claim I checked matches the code. No overclaiming found. The
supporting P0 sub-claims I verified in my files also hold: `test-utils` no longer exposes
token-authority redirection (`shared_for_tests` and `from_static_auth_with_token_url` are
now `#[cfg(test)] pub(crate)`, `manager.rs:108-115, 177-183`); injected-auth provider
constructors (`with_auth_provider` on both providers) are `#[cfg(test)] pub(crate)`;
credential Debug is presence-only/redacted with sentinel tests across `types.rs`,
`jwt.rs`, `pkce.rs`, `manager.rs`; authority error bodies are not echoed (`refresh.rs`
security test); OAuth-with-compatible-endpoint is rejected
(`openai_compatible/provider.rs::validate_auth_source`); API keys cannot select the
private Codex authority in any spelling tested (`endpoint.rs`, `backend.rs` tests).

---

## 2. New findings (ranked)

### R1 — `load_settings` is a public bypass of the trust boundary (Medium, CONFIRMED)

`norn::config::load_settings` is `pub` (`config/mod.rs:34`) and returns the three **raw**
layers; the security contract is doc-comment-only ("runtime assemblers must call
`validate_working_directory_authority` before merging", `config/loader.rs:56`). All three
in-repo call sites comply, but any embedder (meridian is an active one) that follows the
obvious `load_settings` → `merge_settings` path silently re-opens SEC-01/SEC-02 for its
own provider construction. Failure scenario: meridian loads settings in a cloned repo,
merges, builds a `ProviderConfig` with the merged `base_url`/`api_key_env` — the entire
P0 boundary is skipped because nothing in the type system forces the call.
Recommendation: make the raw-layer accessor `pub(crate)` (or return a sealed
`ValidatedLayers` witness type from validation and make `merge_settings` require it), so
the boundary is compiler-enforced rather than convention-enforced.

### R2 — cross-process save can clobber a concurrently rotated refresh token (High as a scenario, but this is the AUTH-02 residual, not a regression; CONFIRMED)

Recorded here for completeness of the failure mode (details in the AUTH-02 verdict row):
`refresh_token_from_authority` never reloads `auth.json` before spending the cached
refresh token, and `save_auth_dot_json` is last-writer-wins. The single-flight gate makes
the *in-process* behaviour correct while leaving the *file* a shared mutable credential
with no ownership protocol. Until the reload-lock-refresh-save transaction lands, running
norn and Codex CLI (or two norns) against one `$CODEX_HOME` risks spurious forced
re-login and, in the interleaving described above, mutual credential loss.

### R3 — silent-failure violations in the credential path (Medium, CONFIRMED)

House rule: "No silent failures. Every error handled, logged, or propagated." Three spots
in the reviewed files violate it today (all part of the AUTH-03 cluster, listed so none
gets lost when P2 is scoped):

- `openai_oauth/manager.rs:129-131` — `.ok().flatten()` drops the typed
  `std::io::Error`/`InvalidData` from `load_auth_dot_json` with no log.
- `openai_oauth/manager.rs:204-211` — transient proactive-refresh failure ignored with no
  log; stale token knowingly served.
- `openai_oauth/types.rs:56-58` — `IdTokenInfo::from_raw_jwt` maps any claim-parse error
  to `IdTokenClaims::default()` with no log. Note the interaction with AUTH-01: a
  namespaced-only token is not even an *error* here (serde ignores unknown fields), so
  logging alone will not surface the AUTH-01 miss — a namespaced-shape test fixture is
  required.

### R4 — `should_refresh` 8-day fallback window is an unlabeled constant (Low, CONFIRMED present; provenance unverified)

`openai_oauth/manager.rs:272-274`: when the access-token `exp` cannot be parsed, refresh
is forced only if `last_refresh` is older than `Duration::days(8)`. The value is
pre-existing (it does not appear in the since-base patch) and is neither annotated as
owner-ruled nor as factual (Codex CLI compatibility?), and I find no ruling for it in
`docs/DECISIONS-2026-07.md` (the OAuth rows at lines 173-174 cover only the 10s/5min
timeouts). Under the NO-ARBITRARY-DEFAULTS rule this needs a provenance label or an owner
ruling. Not introduced by this remediation.

### R5 — `provider.auth` remains dead configuration (Low, CONFIRMED — acknowledged as CONFIG-01/P2)

`ProviderSettings.auth` is documented ("oauth"/"api_key"/"env"), merged
(`config/merge/scalar_sections.rs:61`), and now correctly **rejected** from
working-directory layers — but it is still consumed by nothing:
`print/provider.rs:155-162` selects auth purely from `api_key_env` presence. A trusted
user writing `auth: "oauth"` plus a leftover `api_key_env` silently gets API-key auth.
The review doc explicitly defers this to P2 with typed validation; verified the deferral
matches the code. Flagged so the P2 scope does not silently drop it.

### R6 — `auth status` can report "Logged in" for an unusable credential (Low, CONFIRMED)

`commands/auth.rs:125-128` swallows `parse_jwt_expiration` errors (`.ok().flatten()`) and
`print_status` prints `"Logged in"` regardless of whether the token is expired or its JWT
is malformed; expiry, when parseable, is shown but never compared to `now`. A user
diagnosing 401s gets an affirmative status for a dead credential. (Deliberate exit-0
informational semantics per NC13 are fine; the *message* is what misleads.)

### R7 — login thread outcome can vanish; no directory fsync after rename (Low, CONFIRMED)

- `login_server.rs:104` — `let _ignored = tx.send(result);`: if the caller stopped
  awaiting `block_until_done` (e.g. embedder timeout), a token-exchange or storage error
  is discarded with no log, and the detached thread is never joined.
- `storage.rs::save_auth_dot_json` fsyncs the temp file but not the containing directory
  after the rename; on power loss the rename itself may not be durable. For a credential
  file this is a re-login inconvenience, not corruption (rename atomicity still holds) —
  worth a one-line `File::open(dir)?.sync_all()` on Unix when AUTH-04's "durable save"
  work lands.

### R8 — `reject_chatgpt_api_key_destination` covers only the exact `chatgpt.com` host (Info, PLAUSIBLE)

`provider/endpoint.rs:43-58` blocks API-key traffic to `chatgpt.com` (case-insensitive,
trailing-dot tolerant) but not subdomains (`ws.chatgpt.com`, `api.chatgpt.com`). Today no
private Codex endpoint lives on a subdomain and the OAuth side is pinned exactly, so no
credential exposure follows; noting it in case the private surface ever grows a
subdomain.

---

## 3. House-rules compliance of the owned files

- **No `.unwrap()`/`.expect()`/`panic!` in library code:** clean. All occurrences are in
  `#[cfg(test)]` modules or the `#[cfg(any(test, feature = "test-utils"))]`
  `MockAuthProvider` (which uses `unwrap_or_else` fallbacks and typed poison-mapping, not
  unwraps). Mutex poison in `MockAuthProvider` is explicitly mapped to a typed error.
- **`#[allow]` placement:** every `#[allow(...)]` in the owned files sits on a
  `#[cfg(test)]` module — within the test-code exception. (The 24-lint allow blocks on
  `openai/provider.rs` test modules are ugly but legal.)
- **File size:** all owned files are under 500 non-test LOC (largest: `provider_security.rs`
  ≈ 368 before its test module; `manager.rs` ≈ 285; `login_server.rs` ≈ 350).
- **mod.rs discipline:** `openai_oauth/mod.rs` and `openai/mod.rs` are declarations and
  re-exports only.
- **Defaults provenance:** `OAuthHttpOptions` 10s/5min are labeled pre-existing and appear
  in DECISIONS (lines 173-174, "Keep"). The HTTP keepalive/pool constants in
  `http_client.rs` were moved verbatim from the old `openai/provider.rs`, not invented.
  `LOGIN_PORTS [1455, 1457]` are factual Codex-CLI compatibility ports. The one gap is R4
  (8-day fallback). `DEFAULT_PERMITS_PER_INTERVAL`/`DEFAULT_RATE_LIMIT_INTERVAL` carry
  owner-approval annotations dated 2026-06-11 (pre-existing).
- **No backwards-compat shims:** the constructor lockdowns are hard API breaks (correct
  per house rules); no `#[deprecated]`, no wrappers.
- **Secret hygiene:** SecretString, redacted Debug on every credential-bearing type,
  presence-only error messages that never echo configured values (asserted by sentinel
  tests in `provider_security.rs`, `endpoint.rs`, `backend.rs`, `refresh.rs`, `jwt.rs`,
  `types.rs`, `pkce.rs`, `manager.rs`). I found no path in the owned files that logs a
  token, key, URL userinfo, or repo-supplied value.

## 4. Bottom line

The P0 half of this remediation (SEC-01, SEC-02-adjacent boundary, BACKEND-01) is real:
the trust boundary is enforced on raw layers at every production load site, OAuth is
pinned to a compiled destination with the override discarded even when canonical,
redirects are disabled with tests, and the dangerous test seams are compiled out of
production. The AUTH-01..05 cluster is essentially untouched (by design — scheduled P2)
except for the genuinely good in-process single-flight refresh; the implementer's status
reporting is accurate throughout. The items that must not slip: the cross-process
credential transaction (AUTH-02/R2), the load-error swallowing that turns a corrupt
credential file into a false "not logged in" (AUTH-03/R3), logout leaving credentials
installed on revoke failure (AUTH-05), and the compiler-unenforced `load_settings`
boundary (R1).
