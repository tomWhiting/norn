# P2 provider-auth library-boundary correction — verdict

- **Review date (Australia/Melbourne):** 2026-07-16
- **Reviewer:** external review seat (coordinator) + one independent read-only
  Opus seat
- **Corrects:** observation 1 of
  `2026-07-16-p2-implementation-candidate-review.md`
- **Review range:** `c4965e0..a520c5c` (`448353d` fix, `a520c5c` docs)
- **Reviewed head:** `a520c5c`

## Verdict: correction COMPLETE

The typed `provider.auth` policy matrix is relocated from `norn-cli` into a
single pure library module and exposed to embedders through a genuine public
API; the CLI now delegates to it with no divergent duplicate. The P0
untrusted-settings boundary and the resolve-before-side-effect ordering are
intact, and the documentation does not overclaim. No blocker, major, or minor
defect.

## Battery (coordinator)

Native `1.94.0`, repo warm `target/`, alone on host.

| Leg | Result |
|---|---|
| `cargo fmt --all -- --check` | pass |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | pass |
| `cargo test -p norn --lib provider_auth` | 6 passed, 0 failed |
| `cargo test -p norn --test provider_auth_policy_api` (public embedder) | 2 passed, 0 failed |
| `cargo test -p norn-cli --lib provider_auth` | 5 passed, 0 failed |

## What was proven

- **The full matrix is library-owned.** Every rule maps to the new
  `crates/norn/src/config/provider_auth.rs`: omitted→OAuth (`:85`), OpenAI
  oauth-forbids-`api_key_env` (`:87`), OpenAI api_key-requires-env (`:90`),
  compatible-cannot-select-OAuth (`:101`), compatible api_key-requires-env
  (`:102`), compatible default env (`:105`), Claude Runner accepts neither
  (`:114-120`), blank env name → `EmptyApiKeyEnv` (`:124`). The
  only-`oauth`/`api_key`-accepted rule is enforced one layer up at the
  `ProviderAuthMode` custom `Deserialize` (`config/types.rs:354`) — also
  library-owned, and the correct location. The match is exhaustive and
  compiler-checked (no catch-all).
- **Embedders reach it through a real public API.**
  `provider_settings_from_settings` → `ProviderSettingsResolved::resolve_auth`
  (`runtime_init/base.rs:124`) is the canonical embedder-facing path, exercised
  by `tests/provider_auth_policy_api.rs` (2/2) as Meridian would call it —
  validating before any environment read, credential I/O, or provider
  construction.
- **The CLI genuinely delegates.** `norn-cli/.../provider_auth.rs` shrank to a
  thin adapter that calls the library resolver and re-exports its types; zero
  matrix rules remain in the CLI; a test asserts CLI ≡ library across the input
  shapes.
- **The P0 boundary is preserved.** The `base.rs` change only *adds* the
  `resolve_auth` method; the pre-merge rejection of project/workspace-local
  `auth`/`api_key_env`/`base_url` (`provider_security.rs` via
  `validate_working_directory_authority`) is untouched and still runs before
  merge/validate. The relocation neither weakened nor reordered it.
- CLAUDE.md clean: pure resolver (no I/O), typed `Copy` error enum that holds no
  data (cannot disclose values), no unwrap/expect/panic, files 132/18 LOC,
  `mod.rs` pure.

## Residual (pre-existing, owner-dispositioned, not a blocker)

This correction makes the policy the **canonical library-owned embedder path**,
not a type-enforced-unbypassable gate. The low-level constructors
(`OpenAiProvider::new`, `build_from_auth_source`, `provider/mod.rs:12-16`) remain
public and accept a pre-built `AuthSource`, so an embedder that hand-builds an
`AuthSource` and calls a constructor directly still skips the settings matrix.
This is the pre-existing residual the prior review already noted, and the docs
do not overclaim it (DECISIONS §8: the policy is "available to embedders", no
unbypassable-gate claim). The security-critical boundary — untrusted settings
cannot grant authority — is separately and unconditionally enforced in the
library, so this residual is a completeness/ergonomics matter, not a security
hole. Closing it fully would require a single library-owned `build_provider`
orchestrator that no public constructor bypasses; that is an optional further
step, left to owner discretion, and does not gate this correction or P2.

## Standing

This closes observation 1 of the implementation-candidate review. It does not
change P2's status: acceptance still requires the P1 dependency, the redacted
live A/B/A refresh-validity experiment, and retained Gate C evidence.
