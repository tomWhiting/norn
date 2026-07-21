# Headless non-driven stream exit correction: Gate D handoff

**Date:** 2026-07-22
**Branch:** `codex/p5-d8-role-authority`
**Base:** `c6cc081`
**Source under review:** `1c5a013`
**Source tree:** `b71e0e10149ca63a515a81f1624424e0fc84447e`
**Range:** `c6cc081..1c5a013`

## Verdict requested

Review this narrow correction as an implementation candidate for the
pre-existing non-driven `stream-json` exit-class inversion recorded by D3 Gate
D review `7155196`. This is not D3 or P5 acceptance, and it does not claim to
diagnose every separately observed headless process death.

## Exact scope

The source range changes exactly three files:

- `crates/norn-cli/src/print/error.rs`
- `crates/norn-cli/src/print/orchestrator.rs`
- `crates/norn-cli/src/print/stream_renderer.rs`

The prior path returned a renderer `JoinError` immediately. When a provider or
authentication failure occurred concurrently, that early return replaced the
primary exit class and diagnostic with generic agent exit 1. Simply preserving
the primary `PrintError` would have created a second defect: an authentication
error envelope could then be appended to already incomplete NDJSON.

The correction gives `StreamRendererHandle` one production completion API,
`finish_run`, which consumes both the handle and primary run result. Its raw
task join is private. The resulting contract is:

- clean run + clean renderer preserves success;
- failed run + clean renderer preserves the original failure and envelope
  eligibility;
- clean run + torn renderer returns `StreamTorn`, exit 1, with no terminal
  envelope;
- failed run + torn renderer preserves the primary exit code and both
  diagnostics in `StreamTornWithPrimary`, while remaining ineligible for a
  terminal envelope.

Driven JSON-RPC uses its separate event-emitter shutdown path and is unchanged.

## Evidence

At exact source `1c5a013`, the complete `norn-cli` all-target/all-feature test
gate passed 573/573:

- library unit tests: 521/521;
- integration targets: 52/52, including the seven loopback JSON-RPC tests.

Focused suites passed:

- stream renderer: 7/7;
- print error reconciliation: 3/3.

Strict gates passed without a suppression:

```text
cargo clippy --locked -p norn-cli --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
git diff --check
```

The decisive early-return mutation replaced reconciliation with renderer-first
authority. The integrated auth-plus-renderer regression failed 0/1, observing
`AgentError` instead of `AuthError`; restoring the implementation returned it to
green. An earlier mutation removing torn-stream compound classification also
failed the envelope-suppression assertion. The final handle-level tests exercise
the real task join rather than a synthetic `JoinError` helper.

An independent narrow audit found the original call-site evidence weakness
closed structurally. Its strict-lint, underscore-binding, and stale-comment
findings were corrected in `ac8a95a`, `b1d4f7c`, and `1c5a013`; the final audit
reported no remaining code, API, semantic, test, lint, or documentation finding.

## Review asks

1. Confirm primary exit authority and both diagnostics survive a concurrent
   renderer failure.
2. Confirm a torn NDJSON stream can never receive a terminal error envelope,
   including when the primary failure is authentication-class.
3. Confirm the private raw join plus result-consuming completion API closes the
   old return-before-reconciliation shape without changing successful output.
4. Confirm the non-stream and driven paths retain their previous behavior.
5. Confirm the evidence and tracking text make no broader headless-reliability,
   D3, or P5 acceptance claim.

## Honest boundary

This correction fixes one reproduced reporting defect. It makes a concurrent
primary failure observable with its correct exit class when the non-driven
stream renderer also dies. It does not identify why every headless invocation
reported by the owner has stopped midstream, and it must not be used as evidence
that those separate runtime deaths are resolved. In particular, the existing
broken-stdout path returns normally from the renderer task after a write fails;
this slice covers task panic/cancellation `JoinError`s and does not turn that
separate condition into a typed torn-stream failure.
