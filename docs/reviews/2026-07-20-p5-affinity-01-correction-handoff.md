# P5 `AFFINITY-01` correction handoff

Date: 2026-07-20
Candidate branch: `codex/p5-credential-affinity`
Original verdict: `NOT READY` at `0b25d82`
Requested verdict: close `AFFINITY-1` and `AFFINITY-2`, or identify a remaining defect
Whole-P5 acceptance: explicitly out of scope

## Exact review target

Review corrected source `693d5b1e159166385dbed3d51a203b5c9d5dbf75`, tree
`1b4a7c0c8ab49b556033a112556bacd10aab070a`, against original reviewed source
`58df83975c0d6ebac6f8b8602453429212f60d73`.
The branch correction range is `0b25d82..693d5b1`; the intervening review and
handoff commits are documentation-only, while the Rust source delta is
`58df839..693d5b1`.

The narrow correction is one source commit:

- `693d5b1e159166385dbed3d51a203b5c9d5dbf75` closes the two findings in
  [`0b25d82`](2026-07-20-p5-affinity-01-gate-d-review.md).

The correction commit changes 15 Rust files. Its sorted NUL-delimited Rust path
inventory has SHA-256
`652313a8056c3455e1c017d4c6fd7b40b5dd55e5540fa8966dc2c71e18f0df55`.
The complete candidate range remains
`5e04281542b546c8d831907831b7cdab92edb4f4..693d5b1e159166385dbed3d51a203b5c9d5dbf75`.

## `AFFINITY-1`: user-level OAuth isolation

The correction retains the stronger user-level contract. Managed OAuth state
identity requires both a stable account ID and a nonempty stable user ID. A
credential document may still be loaded and inspected without that user claim,
but it cannot construct the stateful OpenAI provider, open a managed session, or
dispatch a request.

`AuthManager::credential_identity` now distinguishes three outcomes:

- no usable credential document;
- a credential that has an account ID but no nonempty stable user ID; and
- a complete `(account_id, user_id)` principal.

`OAuthAuthProvider::resolve_credential_identity` maps the second outcome to a
typed, non-disclosing `AuthenticationFailed` error. The first outcome retains
the existing `norn auth login` guidance. `OpenAiProvider` resolves that identity
before provider construction, so userless credentials fail before session or
wire mutation.

The existing object-safe `AuthProvider::credential_identity` signature remains
unchanged. A new provided `resolve_credential_identity` method defaults to the
old behavior, preserving external implementations while allowing OAuth to
represent the indeterminate-user failure explicitly.

For credentials with a stable user claim, the digest input and domain are
unchanged: `(account_id, user_id)` scoped to backend and normalized endpoint.
Token refresh therefore keeps the same identity, and sessions created by the
original candidate remain compatible. No token, account ID, or user ID is
placed in the error or durable session event.

The regression constructs a valid persisted OAuth bundle with a real account
ID and no user claim, then proves stateful provider construction returns the
typed authentication failure without disclosing account, access-token, or
refresh-token sentinels. Existing tests separately prove same-user refresh
stability, cross-user inequality, and missing-credential CLI guidance.

## `AFFINITY-2`: one adoption cut under a stale same-identity race

The index transaction now returns an explicit transition outcome:

- `Validated` when the caller and current row were already bound;
- `Adopted` when this transaction durably creates or reuses the adoption cut;
  or
- `AlreadyBoundByPeer` when a stale unbound snapshot finds the current row
  already bound to the requested identity.

`AlreadyBoundByPeer` returns before timeline append or index publication. The
manager resume/fork paths consume the returned current entry and reload the
authoritative timeline, so they see the peer's adoption cut and any later
events. A previously loaded `EventStore` cannot safely graft that cut into its
old in-memory history; it instead returns the typed, payload-free
`ProviderStateIdentityReopenRequired` error. Reopening obtains the authoritative
timeline and continues normally.

Two new deterministic seam tests each run 20 times in retained evidence:

- a stale transaction observes a winner that adopted and advanced the timeline,
  changes no timeline byte, and a canonical reopen sees exactly one adoption
  boundary plus the winner's follow-up; and
- a stale loaded store receives the reopen-required error, remains unbound,
  cannot alter the timeline, and succeeds after a canonical reopen with exactly
  one adoption boundary and complete follow-up history.

This is intentionally stronger than treating every same-identity case as a
generic no-op. Only callers that reload the authoritative history may continue;
a loaded pre-adoption store must reopen rather than reuse its old response
anchor.

## Source-bound evidence

All Cargo commands used the repository `target/` directory. No OS temporary
Cargo target was used.

| Artifact | SHA-256 | Result |
|---|---|---|
| [`AFFINITY-01` runner](evidence/p5-affinity/run_affinity_01_distributions.py) | `8707a4f9bc1ba9d6f5ccfbd19ab4db69cd86a25e6a1bbc9734ba911447f144b4` | Exact source/tree/branch bound; rejects dirty Rust; repository target only. |
| [`91-observation record`](evidence/p5-affinity/2026-07-20-affinity-01-distributions.json) | `d904e198fa67876f6877fb2b39cdb7be22d249094715bc0c3f1cba884302b9da` | `91/91`, zero failures. |
| [`Policy and LOC report`](evidence/p5-affinity/2026-07-20-affinity-01-policy.json) | `64b75c883bf3c3a080883e57d4942671ed3453dfcd1d976a2fb93a8ff723f150` | Pass; 76 changed Rust files, 30 test-only, zero violations, maximum production prefix 499. |

The four repeated cases pass `20/20` each:

- concurrent sink-less first binding;
- concurrent legacy-row adoption;
- stale same-identity transaction after the winner advances; and
- stale loaded store after peer adoption.

Eleven exact single-run sentinels cover interrupted adoption, userless OAuth
rejection, context-edit and first-binding epoch cuts, custom-sink failure and
retry, stale cross-identity append, persistent-child inheritance, empty-source
fork rejection, missing-OAuth CLI classification, and list/export disclosure.

Coordinator-observed broad gates at the exact corrected source content:

- `cargo fmt --all -- --check`: pass;
- `git diff --check`: pass;
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`: pass;
- `cargo test --workspace --all-features --quiet`: pass, including Norn
  `4,097/4,097`, CLI `502/502`, and TUI `683/683`, plus all other workspace
  harnesses and doctests.

Two independent static precommit reviews found no blocker, major, or minor
finding in the corrected auth or stale-affinity paths. They are preparation,
not a substitute for the requested same-reviewer confirmation.

## Honest boundaries

- This is a narrow correction request, not P5 or P2 acceptance.
- The original `NOT READY` review remains the historical record. The candidate
  is not `READY` until the reviewer confirms these corrections.
- Userless managed OAuth credentials now fail every `OpenAiProvider`
  construction, including a caller that intended only an ephemeral request,
  because provider identity is a construction invariant rather than a mode
  selected after construction.
- The current issuer's real claim shape remains a D7/P9 authenticated live-wire
  check. The correction does not assume that userless tokens are or are not
  currently issued.
- The named-account catalog's existing duplicate fingerprint remains based on
  account ID. This correction protects provider-state affinity; it does not add
  two separately named Norn slots for two users sharing one account ID. That
  would require an explicit P2 catalog-format and migration decision.
- A deterministic public manager resume/fork interleaving does not force the
  `AlreadyBoundByPeer` branch. Direct transaction, loaded-store, and canonical
  reopen tests bind the load-bearing behavior; broader lifecycle concurrency
  remains a whole-P5/P9 concern.
- D3 compaction/anchor alignment, D8 role/provenance authority, WebSocket
  transport, P2 acceptance, and whole-P5 acceptance remain open.

## Reviewer confirmation questions

1. Can any accepted managed OAuth credential without a nonempty stable user ID
   still construct a stateful provider or derive account-only affinity?
2. Does the correction preserve the exact identity for an existing complete
   `(account_id, user_id)` credential across token refresh and upgrade?
3. Can a stale same-identity snapshot append a second adoption boundary or
   republish the already-bound row after a peer advances the timeline?
4. Can a loaded pre-adoption store continue without reopening and therefore
   reuse an anchor that predates the peer's durable adoption cut?
5. Do the new tests and retained distributions fail if either correction is
   removed, and do errors remain free of credential or digest material?
