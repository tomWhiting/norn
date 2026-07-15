# P2 OAuth interim-correction handoff

Date: 2026-07-15

## Review boundary

This is an interim correction gate, not a P2 acceptance gate.

- Previously reviewed source head: `289d841`
- Interim review record: `86d95aa`
- Refresh-supervision correction: `32aa18b`
- Credential-lock timing correction: `455990a`
- Source review range: `289d841..455990a`
- Documentation reconciliation before this handoff: `9a5bb33`

The requested verdict is `READY` or `NOT READY` for the complete interim-review
correction only. It must not be represented as a P2 verdict.

## Correction outcome

The source range addresses all seven interim findings:

- `P2-1` and `P2-2`: owner-approved 30-second lock-acquisition and
  25-millisecond inter-process polling defaults, both overridable, with zero
  rejected before credential filesystem access.
- `P2-3`: structurally supervised refresh workers wake all waiters on abnormal
  termination and classify ambiguous dispatch as indeterminate.
- `P2-4`: the alleged symlink-alias coordinator split was a false premise;
  no-follow root opening rejects that root before registration or authority I/O.
- `P2-5`: process-local coordination uses non-poisoning synchronization.
- `P2-6`: callback cancellation returns a generic HTTP 400 failure page.
- `P2-7`: the default Norn product-directory literal has one source.

The 30-second value is only a wait-to-acquire deadline. A holder retains the
exclusive transaction across credential reload, authority refresh, and durable
save. Releasing it during the network exchange would allow cooperating
processes to spend the same rotating refresh token concurrently.

## Retained evidence

The timing runner refuses `CARGO_TARGET_DIR`, verifies the OAuth source is
committed, and verifies Cargo resolves the repository's normal `target/`:

- Runner: `docs/reviews/evidence/run_p2_credential_lock_evidence.sh`
- Result: `docs/reviews/evidence/2026-07-15-p2-credential-lock.json`
- Source head: `455990a2186ae42ed96b8c236ed96726cced79c1`
- Resolved target: repository-relative `target/`
- Process-local deadline: 20 passed, 0 failed
- Two-process refresh convergence: 20 passed, 0 failed
- Runner SHA-256: `4cdd27391de5295702d5ab4fa6a059ada61c7cc98b2e119e4cbda33d7ef6556c`
- Result SHA-256: `b7565111ec4a01af74844ac5b2754834da72f30cfb699150f2d7c2c176e79f09`

Focused working-state checks at the same source boundary:

- `cargo fmt --all -- --check`: pass
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: pass
- `cargo test --locked -p norn --lib openai_oauth`: 175 passed, 0 failed
- Added-line forbidden-pattern scan: no lint suppressions, panic, unwrap,
  expect, todo, unimplemented, or unreachable constructs
- Changed Rust files: maximum 492 physical lines

An independent read-only audit found no production defect. Its three low
findings were corrected before `455990a`: login ordering now uses a
pre-existing invalid root, a distinct timing override is pinned, and the public
timing documentation covers both network and credential coordination.

## Compatibility and non-claims

Adding `credential_lock_poll_interval` to public `OAuthHttpOptions` is a source
compatibility change for downstream exhaustive struct literals. Such callers
must provide the field or use struct update syntax with
`OAuthHttpOptions::default()`.

This handoff does not claim:

- process- or restart-durable protection after an ambiguous refresh outcome;
- named-account validity, layout, selection, import, or keyring support;
- closure of the typed `provider.auth` configuration matrix;
- complete P2 Gate C evidence or P2 acceptance;
- satisfaction of P2's P1 dependency.

Those remain in D9 and the unchecked P2 evidence and exit gates.
