# P2 browser presentation and device-auth implementation handoff

- **Date:** 2026-07-23 (Australia/Melbourne)
- **Branch:** `codex/p2-device-auth`
- **Base:** `6d168830ee3c4edad5893d39a0e1e67950da98ad`
- **Source commit:** `dc908f378185ec5568f54e38209b61c2a9a9f124`
- **Source tree:** `b763308c866d929fa6d7dbf1bf1907480d350493`
- **Review range:** `6d16883..dc908f3`
- **Disposition requested:** review as a bounded P2 implementation candidate,
  not as P2 acceptance or authenticated production-authority evidence

## Purpose

The CLI previously attempted browser PKCE without first giving the operator a
usable URL. That is insufficient when desktop launch fails, and a localhost
callback URL cannot be completed from a different machine. This candidate:

1. presents the exact browser authorization URL before a local desktop launch;
2. adds `norn auth login --device-auth` for remote/headless authorization in any
   browser; and
3. commits either flow through the existing Norn-owned durable default or named
   credential transaction.

There is intentionally no bearer-token, refresh-token, or callback-code paste
path. Device authorization exposes only the fixed verification URL and a
one-time user code through the trusted terminal presenter.

## Source scope

The source commit changes 28 Rust files: 2,951 insertions and 412 deletions. It
adds the device protocol, the public login configuration/presenter boundary, a
shared browser/device durable-login commit boundary, CLI routing, and focused
tests. No dependency or durable credential format changes are included.

### User-visible behavior

- `norn auth login` remains browser PKCE. Before launch, stderr prints the exact
  authorization URL, explains that it is for the current machine, and directs a
  remote operator to `--device-auth`.
- `norn auth login --device-auth` prints
  `https://auth.openai.com/codex/device` and a one-time code, then waits for the
  operator to authorize in any browser.
- `--device-auth` composes with `--name <ALIAS>` and uses the same opaque named
  storage slot/catalog semantics as browser login.
- Success is printed only after credential storage and, for a named login,
  catalog publication complete.
- Linux/BSD automatic desktop launch is disabled because the available launcher
  interface puts the state-bearing URL in observable process arguments. The
  printed browser URL and device flow remain available. macOS keeps its no-argv
  desktop integration.

### Protocol and authority

Production code has one compiled authority set:

| Stage | Endpoint |
|---|---|
| Create user code | `https://auth.openai.com/api/accounts/deviceauth/usercode` |
| Poll authorization | `https://auth.openai.com/api/accounts/deviceauth/token` |
| User verification | `https://auth.openai.com/codex/device` |
| Exchange code | `https://auth.openai.com/oauth/token` |
| Redirect identity | `https://auth.openai.com/deviceauth/callback` |

Only test-compiled code can substitute a loopback authority. Poll responses
`403` and `404` mean pending; every other non-success is terminal. A successful
poll must include nonempty, control-free authorization material and a PKCE
challenge matching the returned verifier before token exchange.

One total authority deadline covers user-code creation, operator wait, polling
sleeps, and exchange. The default is 15 minutes, matching the current
provider/Codex device-code lifetime; `LoginConfig::with_device_code_timeout`
can override it. Zero is rejected before auth-root access. Local credential
revision inspection occurs before the authority clock starts; durable local
publication is intentionally outside the authority deadline.

### Disclosure boundary

The browser URL, device verification URL, and one-time user code are presented
only through `LoginPromptPresenter`. The CLI binds that presenter to locked
stderr. Prompt `Debug` output is redacted, authority errors do not include
response bodies, and the flow does not emit those values to tracing, debug
dumps, session history, provider events, or error payloads. Tokens and account
identity retain the existing redaction and typed-error boundaries.

### Durability and cancellation

Browser and device paths inspect the expected credential revision before
authority dispatch. After successful exchange, only
`CredentialTransaction::acquire` runs in `spawn_blocking`. Its result returns
the transaction plus validation and catalog-commit closures to the login
future. Validation, revision-checked durable credential save, and named-catalog
publication then execute synchronously in one future poll with no await point.

This prevents cancellation from leaving a detached worker that later publishes
a credential or catalog entry. A failed or canceled named login drops its
reservation and attempts to abort it. Existing credentials remain unchanged
when authority, validation, coordination, or pre-publication storage fails.

## Verification

Every Cargo command used the repository's normal shared target directory:

```text
CARGO_TARGET_DIR=/Users/tom/Developer/ablative/norn/target
```

No build or evidence output was redirected to a temporary target directory.

| Command | Observed result |
|---|---|
| `cargo check -p norn --all-targets` | pass |
| `cargo test -p norn 'provider::openai_oauth::device_login::' -- --nocapture` | 17 passed, 0 failed |
| `cargo test -p norn 'provider::auth::' -- --nocapture` | 31 passed, 0 failed |
| `cargo test -p norn 'provider::openai_oauth::' -- --nocapture` | 240 passed, 0 failed |
| `cargo test -p norn-cli --lib` | 523 passed, 0 failed |
| `cargo test -p norn 'provider::openai_oauth::browser::tests::'` | 9 passed, 0 failed |
| `cargo test -p norn 'provider::openai_oauth::login_server::browser_prompt::tests::exact_url_is_presented_before_a_failed_desktop_launch' -- --exact` | 1 passed, 0 failed |
| `cargo test -p norn-cli --lib 'commands::auth::tests::terminal_prompts_show_only_intended_interactive_values' -- --exact` | 1 passed, 0 failed |
| `cargo clippy -p norn -p norn-cli --all-targets --all-features -- -D warnings` | pass |
| `cargo clippy -p norn --lib -- -D warnings` | pass after the final source edit |
| `cargo fmt --all -- --check` | pass |
| `git diff --cached --check` | pass before the source commit |

### Full-library observation, not a pass

`cargo test -p norn --lib` observed **4,326 passed and 4 failed**. It must not be
reported as an unqualified full-suite pass. The failures were:

1. `integration::mcp_stdio::protocol_tests::pump_answers_roots_observes_tools_change_and_emits_root_change`
2. `process::manager::tests::a_late_attached_watch_catches_up_over_a_large_region_without_wedging`
3. `tests::descriptor_retention::active_process_permits_release_on_terminal_paths`
4. `tools::bash::tests::output_over_threshold_redirects_to_file_with_shape_and_content`

Each exact selector was rerun:

| Exact rerun | Observation |
|---|---|
| MCP stdio root/tool-change case | passed 1/1 in isolation |
| descriptor-retention terminal-path case | passed 1/1 in isolation |
| late-attached process-watch case | failed once inside the execution sandbox, then passed 1/1 outside it |
| bash redirected-output case | failed inside the execution sandbox with `Operation not permitted`, then passed 1/1 outside it |

These reruns support contention/sandbox classification for the four observed
failures, but they do not rewrite the original 4,326/4,330 suite result into a
pass. A clean retained phase gate is still required.

## Source policy and size

- No lint allowance, Clippy suppression, production `panic!`, `unwrap`, or
  `expect` bypass was added.
- Every new file is at or below 454 physical lines.
- The largest touched existing modules are `login_server_tests.rs` (493),
  `login_server.rs` (489), and `commands/auth.rs` (486).
- `args.rs` is a pre-existing 1,014-line mixed production/test file. Its
  production prefix ends at line 434; this source commit changes 27 lines (24
  additions and 3 deletions) and does not introduce another oversized module.

## Honest boundaries and residuals

1. No authenticated production device-code login was run. Deterministic
   loopback fixtures do not prove the current live OpenAI authority contract.
2. This package contains no retained hashed machine-evidence artifact and is not
   a complete P2 Gate C run.
3. Current-thread runtime embedders synchronously block during the bounded
   no-yield credential/catalog commit. This is an owner-accepted ergonomics
   residual, not a detached-publication path.
4. Canceling credential-lock acquisition can still leave/heal the Norn-owned
   auth root and `.norn-auth.lock`; it cannot later publish credentials or a
   named catalog entry.
5. `PendingNamedLogin::Drop` may synchronously attempt to retire a reservation
   on an async executor before publication. Abort is best-effort: a
   filesystem/catalog cleanup failure is logged and can leave a recoverable
   pending reservation. It cannot publish a credential, but it is an
   integrity-safe liveness residual.
6. Linux/BSD automatic browser launch is unavailable. The CLI has the printed
   URL and device flow; a presenter-less library caller receives a typed error.
7. The private async exchange helper accepts a test token URL, while the
   production device call graph is sealed to compiled endpoints. This is a
   defense-in-depth review seam, not a production configuration surface.
8. A library browser caller can omit the optional presenter. The guarantee that
   the URL is always printed applies to the high-level CLI, not every embedder.
9. Existing P2 live A/B/A account-validity evidence, final retained Gate C,
   independent phase acceptance, and the separate D7/P9 authenticated Responses
   live-wire gate remain open.

## Review request

Review the frozen source range, not the later documentation commit. In
particular, attack:

- production endpoint sealing and status classification;
- URL/code/token disclosure through every terminal, debug, trace, event, error,
  process-argument, and session sink;
- total-deadline behavior across request, sleep, poll, and exchange;
- PKCE proof validation and malformed-authority responses;
- default and named-account revision conflicts, reservation cleanup, and
  durable publication ordering;
- cancellation before, during, and after transaction acquisition; and
- CLI behavior when presentation or desktop launch fails.

A `READY` verdict on this range would accept it only as an implementation
candidate. P2 remains open until its live A/B/A experiment, authenticated device
proof, retained gates, and independent whole-phase acceptance are complete.
