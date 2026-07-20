# P5 `AFFINITY-01` Gate D handoff

Date: 2026-07-20
Candidate branch: `codex/p5-credential-affinity`
Finding: credential/account affinity for provider-owned state
Requested verdict: `READY` or `NOT READY` as an implementation candidate
Whole-P5 acceptance: explicitly out of scope

## Exact review target

Review source `58df83975c0d6ebac6f8b8602453429212f60d73`, tree
`217349035a71534204cb2baaa499469ef515485f`, over accepted CODEX-02 merge base
`5e04281542b546c8d831907831b7cdab92edb4f4`.

The source range contains three commits:

- `f23be00429f7897820f168e2bbbb5ce69394edbf` implements the slice;
- `b37fb17d18565a380394fc2db95a833853e903f3` moves provider-authority
  validation into the existing builder split and removes one added test
  `unwrap` found by the policy gate;
- `58df83975c0d6ebac6f8b8602453429212f60d73` consolidates one import so the
  largest production prefix is 499 rather than exactly 500 lines.

The complete sorted, NUL-delimited 73-path source inventory for the range has
SHA-256 `dacedf2d5bbabab579e5929a86c4b9ba052616b0ec92df013651d26f8e4c8ea9`.
The retained policy JSON enumerates the same 73 Rust files. This handoff and
the retained evidence are a later documentation commit; assess product behavior
at the exact source above.

## Outcome in practice

A session opened through the canonical provider-aware `AgentBuilder`/manager
path is now sticky to the credential principal and authority that created or
adopted its provider-owned state. A token refresh for the same OAuth account/user
continues normally. A different OAuth account or user, API key, Responses
backend, or normalized endpoint fails before a user message is appended or an
HTTP request is sent. Resumed and forked sessions enforce the same rule, and
persistent children inherit it.

Existing active sessions written before this slice remain readable. Their first
trusted stateful provider use creates a durable provider-epoch boundary and
binds the row once. The old response anchor is never reused across that
adoption. There is no implicit in-session account switch.

## Identity contract

`CredentialIdentity` and `ProviderStateIdentity` are opaque 32-byte equality
identities with domain-separated SHA-256 construction and presence-only Debug.
No raw-byte accessor exists.

The built-in providers derive credential identity as follows:

| Credential source | Stable input |
|---|---|
| Managed OAuth | The manager-pinned `(account_id, optional user_id)` principal; access-token rotation does not change identity. |
| API key | The exact secret key. |
| Dispatch-scoped static Codex auth | Optional account ID plus access token, because this path has no stable user identity. |

Static Codex identity therefore changes when its access token rotates; only the
managed OAuth principal is deliberately stable across token refresh.

The OpenAI provider then scopes that credential identity to the compiled
backend and normalized Responses endpoint. Equivalent trailing-slash endpoint
spellings converge; another credential, backend, or endpoint does not.

This is equality metadata, not a password verifier or public cryptographic
commitment. Inputs with low entropy remain guessable. The durable session index
stores only the resulting provider-state digest, never an alias, storage path,
account ID, user ID, API key, or token.

## Durable transition and append authority

Fresh managed sessions stamp the selected identity in their first index row.
For an active unbound row, adoption runs under the recovered inter-process index
lock and exact id/generation check:

1. Open and validate the registered timeline descriptor-relatively.
2. Append and `fsync` one `ProviderIdentityAdoption` epoch boundary, or reuse an
   already-durable terminal adoption boundary after interruption.
3. Recompute exact counters and atomically publish the bound index row.

A crash before step 3 therefore leaves an unbound row with either no boundary
or one harmless durable boundary. Retry recognizes that boundary instead of
adding another. A bound row can never precede its cut in the timeline.

In-memory and custom-sink stores use the store's append serialization boundary.
The identity becomes visible only after their adoption boundary is published.
If a custom sink reports an ambiguous write, the exact pending `SessionEvent`,
including its event ID and bytes, is retained for retry. This is a caller-owned
idempotence contract, not the managed store's cross-process durability claim.

Every registered JSONL append also compares the sink's expected provider
identity with the current index identity under the same retained index lock as
the write. This closes the cross-handle race where one unbound handle could
otherwise append old-account state after another handle adopted an identity.

Fork validates the source identity before child publication and copies the
binding into the child row/store. Its empty-source preflight happens before
adoption, so a rejected empty fork neither binds its source nor publishes a
boundary-only child. Existing generation and source-tail publication checks
remain in force.

## Loop and transport boundary

`AgentBuilder` obtains one provider state identity before opening a managed
session. A provider advertising response threading without an identity fails
typed before session creation. The canonical run-step path then:

1. validates or binds the store before appending the prompt;
2. creates the fresh `ProviderTurnContext` only after the prompt event ID exists;
3. binds the same identity into that context before request construction; and
4. has the OpenAI provider revalidate it before headers or the producer are built.

Identity absence or mismatch is a payload-free terminal error. Tests observe
zero prompt mutation and zero provider dispatch on that path. Retry and explicit
continuation retain the same identity and turn context; the next user step gets
a new turn context under the same session affinity.

Missing managed OAuth credentials remain `AuthenticationFailed` with the
existing `norn auth login` guidance and CLI auth exit code 3. The affinity guard
does not flatten that condition into a generic provider-state error.

## Observer and disclosure boundary

Manual Debug implementations render credential and provider identities as
`[REDACTED]`. Identity mismatch/required errors contain no digest or source
identity. The CLI uses an exhaustive `PublicSessionIndexEntry` projection for
`session list --format json` and `session export`; the durable digest is omitted,
and an integration fixture searches nested output to prove it is absent.

The full public `SessionIndexEntry` remains a library persistence type. A
trusted embedder can deliberately serialize it and observe the opaque digest;
the candidate does not claim otherwise.

## Source-bound evidence

All Cargo work used `/Users/tom/Developer/ablative/norn/target`. The policy
generator used the repository-local `target/build` and `target/evidence` lanes.
No OS temporary Cargo target was used.

Retained artifacts:

| Artifact | SHA-256 | Result |
|---|---|---|
| [`AFFINITY-01` runner](evidence/p5-affinity/run_affinity_01_distributions.py) | `2333d42ef2fd36dba78a2edc5e143af7fb4e4eb5332b4bb8ead8f9eda62b77a4` | Source/tree/branch bound; rejects dirty Rust; repository target only. |
| [`50-observation record`](evidence/p5-affinity/2026-07-20-affinity-01-distributions.json) | `06c22a886330a0fba0f693dc0fdcce0f6594d0e17a4ace7844ba15a63d78ae5b` | `50/50`, zero failures. |
| [`Policy and LOC report`](evidence/p5-affinity/2026-07-20-affinity-01-policy.json) | `a98cfaa258b1e3499bde732919cd67f91f92044153453d07cfdee25306fe6b6e` | Pass; 73 changed Rust files, 28 test-only, zero violations, maximum production prefix 499. |

The repeated cases are:

- concurrent sink-less first binding: `20/20`;
- concurrent legacy-row adoption: `20/20`.

Ten exact single-run sentinels cover interrupted durable adoption, context-edit
epoch cuts, first sink-less anchor cuts, definite and ambiguous custom-sink
failure, stale managed handles, persistent-child inheritance, empty-source
fork rejection, missing-OAuth CLI classification, and list/export disclosure.

Coordinator-observed broad gates at exact source content (separate from the
retained 50-observation JSON):

- `cargo fmt --all -- --check`: pass;
- `git diff --check`: pass;
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`: pass;
- `cargo test --workspace --all-features --quiet`: pass, including the Norn
  4,093-test, CLI 502-test, and TUI 683-test primary harnesses plus all other
  workspace harnesses and doctests.

The policy report records no added `unwrap`/`expect`/`panic`, lint suppression,
debt marker, empty cfg, over-limit file, thin entrypoint, or module-shape
violation. No Clippy bypass was introduced.

## Internal adversarial work

Read-only implementation and persistence audits ran before packaging. Their
attacks found and drove fixes for five real seams: sink-less first adoption
could initially retain a pre-binding anchor; ambiguous custom-sink retry could
mint another event ID; a stale managed handle could append after cross-handle
adoption; missing OAuth credentials lost their auth-specific CLI behavior; and
an empty-source affinity fork could mutate the source before rejecting it.

The final audit reran the two last regressions and reported no remaining product
finding. The policy gate then independently found the added test `unwrap` and
the production line-budget edge; both were corrected before the exact source
and evidence above. This is internal preparation, not Gate D acceptance.

## Honest boundaries

- This is an `AFFINITY-01` implementation candidate, not P5 acceptance.
- No external reviewer has accepted source `58df839`.
- Existing unbound active rows adopt one identity once. Already-bound rows are
  sticky; an explicit durable in-session account-change transaction does not
  yet exist.
- Named-account selection remains P2-owned. This slice prevents state crossing;
  it does not rotate accounts in response to exhaustion or failure.
- Identity scope covers credential, trusted backend label, and normalized
  Responses endpoint. It does not claim model, tool, or general configuration
  affinity.
- Public storage-only `SessionManager` operations, low-level provider
  constructors, `Provider::stream`, provider-independent `EventStore::append`,
  and caller-defined persistence sinks remain trusted embedder surfaces. This
  is not a type-enforced library-wide gate. A custom sink owns idempotent
  handling of the exact retry it receives.
- Identity mismatch and empty-source fork rejection are non-mutating. A valid
  first adoption may durably bind and cut its source before a later unrelated
  child-publication failure; the candidate does not claim otherwise.
- D3 compaction/anchor alignment and D8 role/provenance authority remain open.
- WebSocket transport and the mandatory D7/P9 authenticated real-wire gate are
  separate open work.
- P2 and whole-P5 acceptance remain open.

## Reviewer questions

1. Can any supported credential, account/user, backend, or endpoint input
   unexpectedly derive the same identity or change without invalidating
   provider-owned state?
2. Can a crash between boundary durability and index publication produce a
   bound row without a cut, duplicate a cut, or strand an unusable row?
3. Can a stale process/store handle, fork, or persistent child append or inherit
   state across another handle's adoption?
4. Can any failure path append the prompt, publish a child, or dispatch HTTP
   before identity validation succeeds?
5. Can the digest or any raw credential/principal value reach Debug, errors,
   events, CLI list/export, or another lower-trust observer?
6. Do the custom-sink exact-retry contract and the trusted low-level embedder
   boundaries remain honestly scoped rather than overclaimed?
