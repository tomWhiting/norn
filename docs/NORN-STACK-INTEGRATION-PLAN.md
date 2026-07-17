# Norn stack integration plan

**Status:** Draft execution plan; no integration phase is accepted until its
review gate is complete.

**Date:** 2026-07-16

**Architecture:** [`design/ablative-stack-composition.md`](design/ablative-stack-composition.md)

**Protocol dependency:** [`RESPONSES-API-REMEDIATION-PLAN.md`](RESPONSES-API-REMEDIATION-PLAN.md)

## Purpose

This plan turns the broader Ablative stack architecture into small Norn-side
increments. It is intentionally less ambitious than the end state. The first
milestones make Norn's existing history observable and composable locally;
distributed storage, remote attachment, and cryptographic trust come later.

This is a companion to the Responses remediation plan, not a replacement for
it. Responses phases P3-P9 continue to own provider transcript, streaming,
conversation state, transport, request construction, and prompt-cache
correctness. This plan consumes those results rather than creating a parallel
event model.

## 1. Outcome

When the plan is complete:

- Norn retains an auditable causal history of root agents, forks, children,
  joins, tools, interventions, compactions, and referenced artifacts.
- A Norn session can continue under a local supervisor while terminal, browser,
  and programmatic clients attach and detach.
- Norn publishes typed events, views, status, and existing actions into a local
  Frame host without Frame importing Norn internals.
- Aion and Norn appear in one operational surface and cross-link workflow,
  activity, session, agent, workspace, change, and diagnostic records.
- UI actions, MCP tools, and library calls use one typed dispatch path.
- The existing Aion/Yggdrasil/Norn execution path remains intact while a
  backend-neutral workspace seam allows later Tharsis adoption.
- Norn can store session trees in Haematite through a replaceable session-store
  interface and an explicit import operation.
- Later, Liminal can carry records and supervisor operations between nodes and
  Lys can attest them, enabling trustworthy cross-machine observation and
  resume.

## 2. What this plan does not reopen

The owner confirms the following operating baseline as of 2026-07-16:

- Aion workflows already provision Yggdrasil workspaces and run Norn steps.
- The execution survives machine interruption and resumes correctly.
- Norn driven mode is the workflow-facing process contract.

No phase may replace that chain merely to make the architecture look cleaner.
A change to it needs a demonstrated defect, an explicit migration plan, and
independent recovery evidence.

## 3. Relationship to the Responses plan

The dependency is direct:

```text
Responses P1 accepted
  -> Responses P2 accepted
  -> Responses D2 implemented and accepted
  -> Responses P3 canonical transcript
  -> Responses P4 complete event/item reconciliation
  -> NS2 stable Norn event projection and read model
  -> NS3 local read supervisor
  -> NS4/NS5A Frame views and actions
```

Other dependencies:

- Responses P5 supplies correct turn, account-affinity, cancellation, and
  provider-state boundaries before the read supervisor may add mutations or
  claim active-turn recovery.
- Responses P6 supplies transport/retry/usage truth before remote operational
  status can claim attempt or cost accuracy.
- Responses P7 supplies truthful tool/schema/action behavior before generated
  MCP and UI action surfaces are accepted.
- Responses P8 and P9 are not prerequisites for a local read-only Frame proof,
  and do not block independent acceptance or use of the local milestones.
  "Integrated release" means only the final whole-campaign Responses and stack
  claim, which remains blocked until the Responses campaign is complete.

Responses D2 concerns legacy and pre-canonical persisted sessions whose flat
transcripts cannot recover P3 item order, phase, or other omitted data. The
owner decided D2 on 2026-07-17, and corrected source `e9755fe` now uses the
versionless `~/.norn/session-store/` namespace for strict format-2
sessions. The existing `~/.norn/sessions/` tree remains untouched; migration
publishes a separate immutable digest-addressed private backup and never creates
a `sessions-v2` path. `norn session migrate` is the explicit offline, staged,
no-replace operation. Normal startup does not decode legacy history: the shared
standard resolver checks legacy-path metadata and, only when needed, a bounded
cutover receipt and ownership proof. `norn session legacy verify` performs the
separate history-proportional verification of the strict store, backup,
manifest, and live legacy tree. Canonically complete legacy sessions begin a
fresh provider-state epoch; flattened but coherent sessions require explicit
degraded/fresh-epoch approval with the fidelity loss recorded; corrupt or
ambiguous sessions remain inspect/export-only. `SessionManager::standard()`
applies the same cutover boundary to library embedders. Corrected range
`2c0350d..e9755fe` is frozen with retained D2 Gate C at 10/10 gates, 280/280
process-isolated distributions, and a zero-violation policy report. Gate D at
`59dc244` returned `READY` contingent on F1; the populated backup-stage fsync is
fixed and re-evidenced, while the same reviewer's narrow confirmation and D2
acceptance remain open.
D2 does not select Haematite or perform the later storage-engine migration. P3
remains on Norn's JSONL session-storage authority; NS9 separately introduces the
store seam and Haematite import after the logical event model is stable.

NS0 authority and identity inventories may proceed in parallel with P3 and P4.
The shared encoding and Norn payload-fixture freeze follows P4 because it must
consume the accepted canonical item model. Neither NS0 arm may create a second
canonical transcript implementation or block native P3/P4 work.

### Provider WebSocket track

The existing `design/norn-provider-ws/` material concerns OpenAI Responses
transport, not the local Norn supervisor or Liminal. Its implementation remains
inside the Responses arm:

- P4 must establish one canonical event/item mapper shared by SSE and
  WebSocket frames.
- P5 must establish turn ownership, cancellation, and provider-state lifetime.
- P6 must establish retry, terminal, fallback, and attempt-accounting semantics.
- P7 must establish the immutable request profile and capability truth.
- Cache prewarm or any other cache-shaping behavior is outside the base
  transport and remains blocked until P8's measured cache policy.
- Before implementation, the design is reconciled against current official
  OpenAI documentation and the current open Codex implementation. Historical
  endpoint, handshake, connection-age, compression, acknowledgment, or fallback
  assumptions are not treated as provider facts without that check.
- No provider WebSocket type, connection state, or retry rule is reused as the
  supervisor or Liminal protocol merely because each may use socket framing.
- The current `design/norn-provider-ws/` briefs and checklists are blocked design
  input, not dispatch authority. They must be replaced after these dependencies
  are accepted, and no WebSocket arm may edit the P3/P4 provider modules in
  parallel.

## 4. Delivery rules

These rules apply to every phase.

### 4.1 Scope and replacement

- One phase owns each new behavior.
- Replace an old path and update every caller; do not leave a compatibility
  wrapper, dual write, shadow store, or zombie public helper.
- A design-only type must not be shipped as though its runtime exists.
- Every public claim identifies whether it is owner-confirmed, source-proven,
  test-proven, live-proven, or proposed.

### 4.2 Quality policy

- `cargo fmt --all -- --check` passes.
- `cargo clippy --workspace --all-targets -- -D warnings` passes.
- No Clippy allowance, lint suppression, ignored test, dead-code hiding, or
  command-line bypass is introduced.
- The diff adds no `unwrap`, `expect`, or `panic` path prohibited by repository
  policy, including tests covered by broad legacy allowances.
- No changed production Rust file exceeds 500 production lines. A touched
  legacy over-limit file is decomposed as part of the phase.
- `mod.rs` contains declarations and re-exports only; `lib.rs` and `main.rs`
  remain thin.
- No timeout, retry count, lease, buffer, queue, retention period, or other
  operational value is invented. Values are factual, configurable and
  owner-ruled, or the phase stops for a decision.
- Errors remain typed at library boundaries. No result is silently dropped or
  defaulted into apparent success.

### 4.3 Evidence discipline

- A test claim records the command, revision, denominator, and distribution.
- Concurrency claims use repeated and adversarial process-level evidence, not
  one lucky run.
- Every `all`, `every`, or `complete` coverage claim includes the inventory used
  to establish it.
- Persistence claims enumerate and test each durable-write crash window.
- Protocol claims include raw fixtures and compare canonical bytes or typed
  values, not only rendered UI.
- Live tests that cannot run remain open; they are not converted into a pass.
- Retained evidence contains no credentials, private prompts, or identifying
  token/account data.

### 4.4 Review boundary

Each milestone ends before the next begins:

1. Gate A: contract, dependencies, inventories, and owner decisions recorded.
2. Gate B: implementation and focused tests complete.
3. Gate C: deterministic local battery and retained evidence complete.
4. Gate D: an independent capable reviewer returns `READY` after inspecting the
   actual range and reproducing the relevant evidence.

The implementer does not accept their own phase. Review findings are fixed or
explicitly ruled; they are not silently deferred.

## 5. Roadmap

| Phase | Status | Visible outcome |
|---|---|---|
| NS0. Architecture and contract boundary | [ ] Drafted; review open | The stack has one documented authority map and compatible contract vocabulary. |
| NS1. Responses transcript and event substrate | [ ] Frozen P3/P4 transcript/streaming candidate through `07bf9c1`, plus corrected/evidenced D2 persistence source `e9755fe`: the exact 28-item output union, shipped non-audio nested/tool schemas, canonical caller-aware persistence/replay, the complete pinned public manifest and scoped Codex overlay, identity-keyed reconciliation, refusal and hosted-search matrices, representative real spawn/fork paths, authoritative UI suffix repair, strict format-2 store, and offline migration policy are implemented; D2 Gate D is `READY` contingent and awaits narrow F1 confirmation; response-scoped audio, the exhaustive lifecycle matrix, and independent P3/P4 phase gates remain open | Norn has a lossless canonical provider transcript and complete event reconciliation. |
| NS2. Norn semantic projection and read model | [ ] Not started | Existing session history is queryable through stable typed records and cursors. |
| NS3. Local detachable read supervisor | [ ] Not started | A session can continue while local read clients observe, detach, and reconnect without acquiring mutation authority. |
| NS4. Read-only Frame contribution | [ ] Not started | Norn sessions, agents, status, and timelines appear in a Frame host. |
| NS5A. Unified Norn actions and MCP-server projection | [ ] Not started | Existing Norn operations share one typed authorization and dispatch path across local, Frame, and server surfaces. |
| NS5B. Complete MCP client | [ ] Not started | Norn consumes the selected pinned MCP protocol completely and honestly. |
| NS6. Aion/Norn correlation and merged console | [ ] Not started | Workflow and agent activity cross-link in one local operational view. |
| NS7. Hot install and upgrade | [ ] Not started | Trusted component generations upgrade without rebuilding the host or creating a second registration authority. |
| NS8. Workspace-provider seam | [ ] Not started | Current Yggdrasil behavior sits behind a backend-neutral Norn contract. |
| NS8A. Native tree-session convergence | [ ] Not started | Norn records exact native multi-parent convergence rather than inventing it in a projection. |
| NS9. Session-store seam and Haematite | [ ] Not started | Norn session trees can use Haematite after explicit import and recovery proof. |
| NS10. Cross-node delivery and remote attach | [ ] Not started | A client can securely observe and control a session through Liminal transport. |
| NS11. Attestation and fenced cross-machine resume | [ ] Not started | Records and artifacts are verifiable and a new writer starts only after exclusive ownership transfer. |

## 6. NS0: architecture and contract boundary

**Scope:** NS0A is documentation and cross-repository review only. NS0B is a
post-P4 contract-fixture phase with minimal test-only Rust and TypeScript
decoders; it creates no Norn production integration or shared runtime crate.

### NS0A: non-blocking authority and identity inventory

- [x] Record the cross-stack authority map.
- [x] Separate semantic record, delivery wrapper, and component contribution
  contracts.
- [x] Record the local Frame node, browser ABI, hot-upgrade, action, storage,
  and transport boundaries.
- [x] Record the operating Aion/Yggdrasil/Norn baseline without reopening it.
- [ ] Inventory the existing Rust and TypeScript identifiers that could be
  confused with component, producer, participant, schema, artifact, stream,
  signing, session, agent, workspace, snapshot, change, diagnostic, action
  principal, or replacement-epoch identities.
- [ ] Ask the current Yggdrasil, Aion, Chiron, and Norn owners to name the
  authoritative `WorkspaceRef`, `SnapshotRef`, `ChangeRef`, `DiagnosticRef`,
  `SessionRef`, and `AgentRef`; Norn may not mint substitute cross-domain IDs.
- [ ] Define distinct ownership for `ComponentGenerationId`,
  `BrowserActivationId`, `SupervisorInstanceId`, `SessionRunEpoch`,
  `ProducerEpoch`, and `DeliveryStreamEpoch`. Exactly one durable execution
  authority is recorded for each session: Norn for a standalone session or
  Aion for an Aion-owned workflow session.
- [ ] Define the mandatory authenticated principal, invocation surface,
  delegation chain, granted capabilities, target epoch, and authorization
  evidence constructed locally for a Norn mutation. A wire request carries
  untrusted request data and presented credentials, never a caller-asserted
  authorization decision.
- [ ] Define the domain-qualified native-to-projection identity mapping,
  projection cardinality, and immutability rule.
- [ ] Define `RelationRecordV1` identity, asserting authority, role-labelled
  endpoints, supporting native evidence, and immutable
  supersession/retraction. Read models project durable relations; they never
  become the only authority for late cross-domain links.
- [ ] Pin every repository-observed claim to repository revision and source
  location, or relabel it owner-confirmed, proposed, or unverified.

This arm may run beside P3/P4 and must not freeze Norn payload bytes.

### NS0B: post-P4 shared contract freeze

- [ ] Produce canonical fixture candidates for one accepted Norn event, one
  Aion event, one Frame contribution event, one durable relation, one opaque
  unknown payload, and a future multi-parent example labelled unsupported by
  current Norn persistence.
- [ ] Decide the owner repository for shared contract types and generated
  bindings.
- [ ] Resolve the canonical encoding, digest construction, and versioning rule.
- [ ] Add minimal test-only Rust and TypeScript fixture decoders after the owner
  repository and canonical bytes are accepted. Do not add a second Norn event
  implementation or production dependency.
- [ ] Review the contract against Aion, Frame, Beamr, Liminal, Lys, Haematite,
  Yggdrasil, Tharsis, Chiron, and Prospekt source owners.

### Difference after the phase

Teams can implement against one small vocabulary without assuming that one
system owns all history. No runtime behavior changes.

### Evidence and gate

- [ ] Each current-state claim links to source or is labelled owner-confirmed.
- [ ] Every proposed type has one owning authority and no duplicate semantic
  field in another wrapper.
- [ ] Rust and TypeScript fixture decoders agree on canonical bytes and reject
  unknown incompatible contract versions.
- [ ] Cross-repository reviewers return `READY` on the frozen contract.

**Review stop M0:** Do not create a shared production runtime crate or begin NS2
until NS0A and the post-P4 NS0B freeze are accepted. Neither arm blocks native
P3/P4 work.

## 7. NS1: Responses transcript and event substrate

**Scope:** This phase is satisfied by the owning work in Responses P3 and P4;
it does not duplicate that implementation here.

### Work

At `07bf9c1`, the transcript and streaming implementation-candidate work is
marked complete below. The separate corrected D2 source candidate is frozen at
`e9755fe`, and exact-head verification is retained at 10/10 gates and 280/280
distributions. These checks record candidate behavior only; they do not accept
D2, P3, P4, or NS1. Gate D is `READY` contingent; the narrow F1 confirmation,
response audio, exhaustive lifecycle fixtures, and independent P3/P4 review
remain open where noted.

- [x] Complete the ordered canonical 28-discriminator Responses output-item
  union under P3, including one authoritative validator and an explicit
  inert/executable/conditional classification for every discriminator.
- [x] Preserve provider item order, identity, phase, annotations, refusals,
  hosted calls, compaction, reasoning, and opaque unknown data.
- [x] Preserve the shipped non-audio image, file, binary, and structured content
  shapes without flattening them into display text; keep large content in
  referenced private artifacts where the canonical provider schema permits a
  reference.
- [ ] Add the D2-compatible response-scoped audio artifact sidecar without
  fabricating an output-item identity or terminal audio item.
- [x] Complete the P4 public/Codex event and item manifests.
- [x] Reconcile deltas with authoritative completed items by stable identity.
- [x] Persist and replay representative canonical non-audio vectors through
  uninterrupted, resumed, spawned, and forked sessions.
- [ ] Complete the exhaustive all-discriminator/optional-shape lifecycle matrix;
  the representative real spawn/fork fixtures do not imply that broader claim.
- [x] Implement the decided Responses D2 contract: strict new sessions under
  `~/.norn/session-store/`, untouched legacy `~/.norn/sessions/`, and a separate
  offline atomic migration with canonical, degraded/fresh-epoch, and
  inspect/export-only classifications. Normal runtime uses a bounded cutover
  proof; complete migration verification is explicit and offline; the shared
  checked resolver and `SessionManager::standard()` cover standard CLI and
  library construction. Retained D2 Gate C is complete; Gate D returned `READY`
  contingent, F1 is corrected and re-evidenced, and the same reviewer's narrow
  confirmation remains open.

### Difference after the phase

Norn's event substrate no longer loses the information that later transcript,
Frame, audit, or Haematite integrations need. UI text and tool views are
projections rather than the canonical record.

### Evidence and gate

The P3 and P4 Gate C/D evidence is authoritative. NS1 adds no second acceptance
path. Both Responses phases must be `READY` before NS2 production code begins.

**Review stop M1:** Review P3 and P4 as a cohesive transcript/event milestone
before supervisor or Frame production work.

## 8. NS2: Norn semantic projection and read model

**Dependencies:** NS0 and NS1.

### Work

- [ ] Inventory the canonical Norn session event families, child records,
  action-log records, artifact references, and provider transcript projections.
- [ ] Define a read-only `NornEventRecord` projection compatible with the shared
  semantic record.
- [ ] Apply the NS0 native-to-projection identity and cardinality contract; a
  replay of unchanged native history produces the same projection IDs and
  bytes.
- [ ] Preserve native event identity, the source-observed single parent edge,
  branch anchors, and explicit child-completion links. Do not reinterpret a
  child completion as a second causal parent.
- [ ] Expose only owner-approved typed related links, `ProducerEpoch`, schema
  reference, and opaque payload.
- [ ] Define stable query cursors scoped to one session/event stream; do not
  invent a global sequence.
- [ ] Expose session summary, session detail, agent tree, transcript item,
  action, artifact, and diagnostic-link queries through a library-owned read
  interface.
- [ ] Keep unknown native events opaque and non-executable.
- [ ] Prove that projection performs no session mutation and does not require a
  second persistence representation.

### Difference after the phase

Norn history can be consumed by TUI, browser, Frame, Aion, and forensic tools
through one stable read model without giving those clients access to internal
mutable state.

### Evidence and gate

- [ ] The checked inventory accounts for every persisted Norn event family.
- [ ] Golden fixtures preserve event identity, parent links, item order,
  artifact links, and unknown payload bytes.
- [ ] Reprojection is byte-stable, and adding a later relation does not mutate
  or re-identify the source event record.
- [ ] Single-parent, branch-anchor, and child-completion fixtures remain exact
  after projection; no projection fixture claims native multi-parent history.
- [ ] Cursor resume produces no gap or duplicate under the documented stream
  contract.
- [ ] Existing TUI, print, driven, and library session behavior is unchanged.

## 9. NS3: local detachable read supervisor

**Dependencies:** NS2. This phase is observation-only; active-turn recovery and
all cross-process mutations wait for the accepted Responses state/action
substrate.

### Work

- [ ] Define process ownership for supervised versus in-process TUI, print,
  driven, and library sessions.
- [ ] Add a local-only supervisor service over a user-private Unix-domain
  socket using the repository private-filesystem primitives.
- [ ] Expose typed list, inspect, subscribe, read-only attach, detach, and
  reconnect operations over the NS2 read model.
- [ ] Make observer disconnect independent from session lifetime. The existing
  in-process session owner remains the only mutation authority in NS3.
- [ ] Reject cross-process cancel, stop, intervention, prompt, fork, and other
  mutations until NS5A installs the common action path.
- [ ] Fence the read service with `SupervisorInstanceId`. NS3 neither allocates
  nor accepts caller-supplied `SessionRunEpoch` values.
- [ ] Reconstruct subscriptions from authoritative snapshots and scoped cursors
  after reconnect.
- [ ] Add `doctor` visibility for supervisor ownership, socket, session count,
  and typed degraded states without exposing private content.

### Difference after the phase

An operator can close a read client and later reconnect to a locally supervised
Norn session. The session lifetime is explicit rather than accidentally owned
by one observer connection. This phase does not claim that killing the
supervisor resumes an in-flight provider turn.

### Evidence and gate

- [ ] Process-level tests prove observer disconnect does not cancel a session.
- [ ] Every attempted mutation is rejected before dispatch; no public mutation
  method exists on the NS3 protocol.
- [ ] Kill/restart tests prove completed snapshot plus cursor reconstruction is
  gap- and duplicate-safe and report an active in-flight turn as unsupported,
  not resumed.
- [ ] Socket replacement, stale incarnation, malformed message, and unauthorized
  local-user cases fail closed.
- [ ] No public TCP listener or implicit remote surface exists.

**Review stop M2:** Stop after NS2-NS3 for independent session, process,
filesystem, and protocol review before adding Frame or browser code.

## 10. NS4: read-only Frame contribution

**Dependencies:** NS0 and NS3. Frame host support is a cross-repository
dependency and is not silently implemented inside Norn.

### Work

- [ ] Define a Norn component descriptor using the accepted contract.
- [ ] Add the minimal local Frame registration authority: one user-private
  control socket, leased component registration, and one authoritative
  contribution snapshot. Do not create a compiled-adapter registration path.
- [ ] Contribute session list, session detail, agent tree, live status,
  transcript timeline, action timeline, and diagnostics links as read-only
  views.
- [ ] Publish one atomic contribution set for each `ComponentGenerationId`.
- [ ] Withdraw live contributions on lease loss while retaining historical
  records.
- [ ] Use host-rendered declarative fragments for the first proof. Executable
  custom browser modules remain NS7 work.

### Difference after the phase

Starting Norn makes its read-only operational surface appear in Frame. Stopping
it removes the live surface without rebuilding or restarting Aion or deleting
session history.

### Evidence and gate

- [ ] The authoritative Frame snapshot never contains a half-installed Norn
  contribution set.
- [ ] Dropped lifecycle notifications recover through snapshot refresh.
- [ ] A stale `ComponentGenerationId` cannot publish or withdraw the current
  generation.
- [ ] Read-only views cannot dispatch a Norn mutation.
- [ ] Declarative view mount/withdrawal leaves no route, slot, or subscription
  leak. Executable browser activation/disposal remains an NS7 gate.

## 11. NS5A and NS5B: actions and MCP

The two subphases are both required for the end state but have different
authorities. NS5A depends on NS4 and accepted Responses P7 capability/schema
truth, which transitively supplies the P5/P6 state, cancellation, and attempt
semantics. NS5B inventory, official-spec pinning, transport-only lifecycle work,
and isolated fixtures may run in parallel. Its provider, canonical-content,
persistence, replay, and UI integration depends on accepted NS1 and P7 and may
not edit modules owned by the active Responses arm. NS5B does not block NS6.

### NS5A: unified Norn actions and MCP-server projection

- [ ] Inventory existing Norn operator actions and Norn-as-MCP-server
  operations.
- [ ] Define one versioned action declaration with input/output schemas,
  subject types, capability request, idempotency, cancellation, confirmation,
  and emitted event kinds.
- [ ] Define a library-owned `AuthorizedActionInvocation` containing the
  authenticated principal, invocation surface, delegation chain, target
  `SessionRunEpoch` or `ComponentGenerationId`, granted capabilities,
  authorization evidence, optional idempotency key, and input.
- [ ] Route library, local control, Frame UI, and generated MCP projections
  through one authorization and dispatch implementation.
- [ ] Define `ActionRequestV1`/`ActionOutcomeV1` for local cross-process control.
  Bind the untrusted request to the authenticated channel at the receiver,
  validate any presented delegation credential, and construct the non-public
  `AuthorizedActionInvocation` only inside Norn.
- [ ] Distinguish operator controls, Norn agent tools, external MCP calls, and
  Norn MCP-server exports. Frame admits a host action; Norn makes the final
  authorization decision for a Norn mutation.
- [ ] Initially expose existing operations only, including intervene, wake,
  signal, fork, and stop where already supported and semantically valid.
- [ ] Complete Norn's MCP server as an intentional projection of selected
  actions and read-only resources. Its declared capabilities must match its
  implementation; private session data is not exported by default.
- [ ] Feed write/edit/apply-patch diagnostics through the existing Norn/Chiron
  post-mutation policy rather than creating a UI-only shortcut.

### NS5B: complete MCP client

- [ ] Inventory every implemented and currently unclaimed Norn-as-MCP-client
  capability.
- [ ] Preserve the accepted user, shared-project, private-local, CLI,
  live-session, root-agent, variant, and spawned-agent MCP configuration and
  selection behavior without letting model input grant new authority.
- [ ] Keep MCP client discovery dynamic; do not compile discovered server tools
  into the Frame contract.
- [ ] Prove any early transport-only MCP work has a disjoint write set from the
  Responses transcript, provider-item, persistence, and tool-schema modules.
- [ ] Complete the Norn MCP client against a freshly pinned official protocol
  inventory. Close or explicitly classify the currently unclaimed HTTP GET
  listener, reconnect/resumption, HTTP session shutdown, OAuth, sampling,
  resources, prompts, and applicable capability-change notifications.
- [ ] Preserve typed MCP content, including structured data and referenced
  binary/image content, through tool result, provider input, persistence,
  replay, and UI projection rather than reducing it to text.

### Difference after the phase

After NS5A, the same Norn-owned action has the same validation, authorization,
diagnostics, event, and result semantics whether invoked from a model tool,
Norn's MCP server, browser, terminal, or library embedder. After NS5B, Norn's
separate role as an MCP client has a complete, version-pinned lifecycle and
capability matrix.

### NS5A evidence and gate

- [ ] The action inventory accounts for every exported mutation path.
- [ ] Direct public mutation APIs are removed, made internal, or proven to
  construct the same authorized invocation; no alternate authority remains.
- [ ] Generated UI and Norn-server MCP schemas round-trip against the library
  action type.
- [ ] A capability denied through one surface is denied through all surfaces.
- [ ] Duplicate, cancelled, stale-incarnation, and ambiguous-dispatch cases
  follow the declared action semantics.
- [ ] Mutation diagnostics cannot be skipped by changing the invocation surface.

### NS5B evidence and gate

- [ ] A checked client capability matrix accounts for every method and
  notification in the pinned MCP protocol version as implemented,
  intentionally unsupported, or inapplicable.
- [ ] Reconnect, session shutdown, capability refresh, OAuth, sampling,
  resources, prompts, structured content, and cancellation fixtures prove each
  advertised path end to end.

## 12. NS6: Aion/Norn correlation and merged console

**Dependencies:** NS4, NS5A, and the existing Aion/Norn driven integration.
NS5B remains required program work but does not block the local merged console.

### Work

- [ ] Add typed links from Aion workflow/activity/run records to the
  owner-approved Norn session, agent, workspace, change, and diagnostic
  references frozen in NS0A.
- [ ] Preserve unknown Norn events in the Aion integration adapter.
- [ ] Contribute Norn status and activity to Aion workflow-detail slots without
  hard-wiring Norn components into Aion routes.
- [ ] Compose a combined timeline that identifies the authoritative domain for
  every row.
- [ ] Expose existing Aion and Norn actions through their own typed declarations
  in one shell.
- [ ] Prove existing crash-resume behavior still reconstructs all cross-links.
- [ ] Represent relationships discovered after an immutable semantic record as
  native authority-owned `RelationRecordV1` records and project them into the
  read model. Do not mutate an existing `EventRecordV1`, its ID, or its attested
  bytes, and do not make a UI/cache join the sole durable authority.

### Difference after the phase

An operator can move from a workflow to the exact Norn session, agent, tool
action, workspace change, or diagnostic that explains it, then return without
losing causal context.

### Evidence and gate

- [ ] One end-to-end workflow fixture survives process and machine-style restart
  with stable cross-links.
- [ ] A Norn fork and child-completion link remain visible without flattening
  agent history into Aion workflow history. Native multi-parent convergence
  remains NS8A work.
- [ ] Unknown events remain visible as typed raw records.
- [ ] No Aion retry creates an accidental second Norn session when the existing
  idempotent session identity should resume.

**Review stop M3:** Stop after NS4-NS6 for an external end-to-end review of the
first useful product slice: a local merged Aion/Norn console.

## 13. NS7: hot install and upgrade

**Dependencies:** The local merged-console proof and accepted Frame/Beamr
bundle contracts.

### Work

- [ ] Retain NS4's local Frame node and leased registration as the only
  contribution authority; do not introduce a bundle-specific registry.
- [ ] Allocate one stable Beamr namespace per `ComponentId`; allocate a distinct
  Frame `ComponentGenerationId` for each upgrade. Route every module load and
  supervisor/child spawn through that stable namespace.
- [ ] Implement bundle-wide module staging and rollback, readiness probe,
  atomic authoritative-snapshot publication, old-generation drain, safe purge,
  and typed blocked-retirement reporting.
- [ ] Restrict the first proof to stateless or externally durable components
  whose storage schema is unchanged. Design any state migration as a separate
  atomic snapshot/migrate/probe/promote/rollback transaction before admitting
  schema-changing bundles.
- [ ] Add content-addressed browser artifact loading behind the framework-neutral
  activation ABI.
- [ ] Allocate a browser-local `BrowserActivationId` for each staged scope and
  bind it to the current backend `ComponentGenerationId`; neither identifier may
  substitute for the other.
- [ ] Require side-effect-free import, exact host-contract negotiation, a
  host-owned staging scope, generation-qualified custom elements or a mount-root
  ABI, idempotent disposal, and explicit DOM/style isolation.
- [ ] Treat executable browser artifacts as trusted first-party same-origin code
  admitted only by an operator-controlled publisher/version allowlist. Untrusted
  contributions remain host-rendered declarative data.
- [ ] Retain last-known-good code and contribution snapshot until the candidate
  proves ready.
- [ ] Validate duplicate routes, commands, action IDs, contribution keys, stable
  order tie-breaks, and active-route withdrawal fallback before publication.

### Difference after the phase

Aion and Norn can be installed, started, stopped, and upgraded independently.
Their operational surfaces appear and disappear without recompiling the host or
restarting unrelated components.

### Evidence and gate

- [ ] Upgrade fault injection covers every boundary in the activation
  transaction.
- [ ] Requests complete exactly once across the barrier or remain held; none are
  ambiguously replayed.
- [ ] A failed backend candidate leaves the old authoritative snapshot active.
  A failed or offline browser may reuse last-known-good artifact bytes only
  after compatibility validation and rebinding in a fresh activation scope to
  the current backend generation; otherwise it becomes read-only or
  update-required without rolling back the backend.
- [ ] Module-name collisions are impossible across stable component namespaces.
- [ ] Old browser activation scopes and old backend component generations cannot
  publish after replacement; a rebound artifact uses the new activation ID and
  current backend generation.
- [ ] Tests prove content digest alone grants no trust and server-side action
  authorization still applies to approved browser modules.
- [ ] Tests acknowledge that approved same-origin modules can directly use DOM,
  storage, and network APIs while proving direct network calls cannot bypass
  server-side Norn/Frame authorization.
- [ ] Multi-artifact partial failure, three consecutive upgrades, an old actor
  preventing safe purge, import-time side effects, partial browser activation,
  duplicate registrations, and active-route withdrawal are covered.

## 14. NS8: workspace-provider seam

**Dependencies:** Existing Yggdrasil integration remains the behavioral
reference.

### Work

- [ ] Inventory the exact workspace operations Norn and Aion currently use.
- [ ] Define a library workspace provider for lease, root, snapshot, fork,
  diff, checkpoint, provenance, and landing candidate.
- [ ] Wrap the existing Yggdrasil path without changing behavior or data layout.
- [ ] Bind session and diagnostic events to stable workspace/snapshot/change
  references.
- [ ] Define conformance fixtures that a future Tharsis adapter must satisfy.
- [ ] Do not implement a Tharsis adapter until its workspace semantics and
  failure model pass their own review.

### Difference after the phase

Norn no longer assumes that one source-control implementation is the workspace
itself, while the working Yggdrasil path remains unchanged.

### Evidence and gate

- [ ] Existing Aion/Yggdrasil/Norn crash-resume and landing tests are unchanged
  or strengthened.
- [ ] The provider seam introduces no alternate identifier, write path, or
  fallback implementation.
- [ ] Snapshot and change references survive session persistence and resume.

## 15. NS8A: native tree-session convergence

**Dependencies:** NS1-NS2 and an owner-approved tree-session semantic design.
This phase does not block NS3-NS7.

### Work

- [ ] Define a native persisted convergence record with exact direct parent
  event identities and role-labelled edges.
- [ ] Decide whether parent order is domain-semantic; otherwise canonicalize the
  edge set by role and domain-qualified event identity.
- [ ] Preserve the current single-parent event and child-completion semantics
  until the replacement format is accepted; do not synthesize parents from
  visualization links.
- [ ] Version and replace the affected native session format under the same
  migration/rejection discipline as Responses D2.
- [ ] Extend the NS2 projection only after the native record exists.
- [ ] Prove fork, child completion, convergence, compaction, replay, and resume
  retain exact causal identities.

### Difference after the phase

Norn can record a synthesis that directly consumes several child endpoints
without flattening histories or asking a read model to guess causality.

### Evidence and gate

- [ ] Baseline fixtures prove the old record cannot express exact multi-parent
  causality and that no existing link is misclassified as a parent.
- [ ] Cross-language fixtures preserve the accepted parent-edge ordering and
  canonical bytes.
- [ ] Interrupted migration/rejection tests fail before mutation or recover
  exactly under the selected policy.
- [ ] The prior NS2 single-parent and child-completion fixtures remain valid.

## 16. NS9: session-store seam and Haematite

**Dependencies:** NS8A and Haematite's branch-commit requirements.

### Work

- [ ] Define the session-store manager interface around append, branch,
  converge, replay, random lookup, child enumeration, artifact reference, and
  durable checkpoint.
- [ ] Keep JSONL as the current implementation while callers move to the one
  manager interface.
- [ ] Implement Haematite append-only branch advancement and multi-parent
  convergence without content-merge semantics.
- [ ] Provide one explicit offline, atomic, idempotent, interruption-recoverable
  import from the supported JSONL version.
- [ ] Remove runtime dual-read and dual-write paths after import is accepted.
- [ ] Retain stable event addresses and referenced artifact integrity.

### Difference after the phase

Norn can use content-addressed branch storage and later replication without
changing the logical transcript, fork, convergence, or query contracts.

### Evidence and gate

- [ ] A crash matrix covers every reservation, append, branch, converge,
  checkpoint, import, and publication write boundary.
- [ ] JSONL and Haematite implementations pass one store-conformance suite.
- [ ] A session with roots, forks, children, convergence, compaction, and
  artifacts imports without identity or ordering change.
- [ ] Repeated or interrupted import is recoverable and never starts a runtime
  compatibility fallback.
- [ ] Cross-store canonical projections are byte-equivalent where the contract
  requires them to be.

**Review stop M4:** Stop after NS8, NS8A, and NS9 for storage, source-history,
crash, and recovery review before adding cross-node behavior.

## 17. NS10: cross-node delivery and remote attach

**Dependencies:** Local supervisor proven; Liminal participant and delivery
contracts implemented and reviewed; Haematite replication semantics available
where session data must follow observation.

### Work

- [ ] Map the accepted semantic record into Liminal delivery without replacing
  domain identity or causality.
- [ ] Scope delivery sequence to stream and epoch.
- [ ] Define stable participant binding separately from human, agent, component,
  and signer identity.
- [ ] Keep read-only semantic-event delivery separate from mutation requests.
- [ ] Require targeted request/reply semantics for `ActionRequestV1` and
  `ActionOutcomeV1`, including action version, target and epoch, untrusted input,
  any presented delegation credential, attempt identity, idempotency class,
  correlation, and committed event references. The transport supplies a
  non-serialized authenticated caller/session context; the target binds it to
  the request, performs authorization, and constructs its local invocation
  context. If the accepted Liminal version cannot provide that contract, use a
  separate reviewed remote-control protocol rather than encoding commands as
  events.
- [ ] Extend attach, observe, and action dispatch across nodes with explicit
  authorization and typed epoch fencing.
- [ ] Preserve local operation when Liminal is absent or partitioned.
- [ ] Make delivery retry idempotent without replaying ambiguous mutations.

### Difference after the phase

An authorized client can attach to and operate a Norn session from another node
while retaining the same typed history and action contracts used locally.

### Evidence and gate

- [ ] Partition, reconnect, duplicate, reorder, epoch change, stale participant,
  and node-restart tests preserve semantic identity.
- [ ] Observation resumes by scoped cursor without claiming a global sequence.
- [ ] Ambiguous mutation delivery never causes an automatic replay unless the
  action contract proves it safe.
- [ ] Delivery acknowledgment is never reported as action execution; only a
  committed outcome closes a request.
- [ ] A transport partition does not corrupt local session or workflow state.

## 18. NS11: attestation and fenced cross-machine resume

**Dependencies:** NS8-NS10 plus agreed identity binding, Aion execution-lease,
workspace ownership-transfer, and node-eligibility policies.

### Work

- [ ] Pass `domain_tag || canonical_record_or_artifact_bytes` to Lys and retain
  the exact canonical COSE attestation. Do not pre-hash a digest into an
  accidental double-hash construction or define a second attestation format.
- [ ] Bind signer keys to stable producer identities through an explicit trust
  record.
- [ ] Make Aion own a durable session-execution lease with a monotonic
  `SessionRunEpoch` checked before provider dispatch, session append, and every
  mutating action.
- [ ] Implement live transfer as an ordered drain: stop old admission; cancel
  and join provider, tool, child-agent, and child-process work; durably classify
  completed versus ambiguous attempts; checkpoint session and workspace state;
  release the old lease with compare-and-swap; then grant and start the next
  epoch. Lease loss or renewal failure actively stops old work.
- [ ] Transfer or reacquire workspace ownership under the accepted NS8 provider
  contract before the destination may mutate it.
- [ ] Define which records and artifacts must be available before another node
  may claim resumability.
- [ ] Restore the session, provider/account affinity, workspace snapshot,
  workflow link, artifacts, and stable component/session identity on an
  eligible node. Restore lineage and fence state, then mint new runtime,
  component-generation, producer, and delivery epochs as applicable.
- [ ] Record migration/resume as new semantic events; never rewrite the prior
  node's history.
- [ ] Keep signing time distinct from event order and freshness decisions.
- [ ] When exclusive prior-writer fencing cannot be proven, support only cold
  disaster recovery after the prior writer is definitively stopped. The same
  restriction applies when an in-flight provider or external side effect cannot
  be cancelled, joined, fenced, or authoritatively classified.

### Difference after the phase

A session can be resumed on another machine with verifiable provenance for the
history and artifacts it consumes. The new runtime continues the causal history
rather than impersonating the old incarnation.

### Evidence and gate

- [ ] Tampered record, artifact, schema, identity binding, or signer metadata is
  rejected before resume.
- [ ] Missing data reports a typed non-resumable state rather than partial
  success.
- [ ] Cross-machine resume preserves stable identities and creates new typed
  runtime epochs without impersonating the old writer.
- [ ] A partitioned old node cannot dispatch a provider request, append an
  event, mutate the workspace, or execute an operator action after ownership
  transfer.
- [ ] Fault injection at every drain/lease transition proves that a late provider
  response, tool call, child completion, or process exit cannot cross the new
  epoch or be replayed ambiguously.
- [ ] A complete disaster-recovery exercise reproduces the intended workflow,
  session, workspace, and audit state on another node.

## 19. Reliable parallel work arms

Parallelism is safe only where ownership and write sets are disjoint.

### Arm A: Responses correctness

Owns P3-P9 in the existing remediation plan. P3 and P4 are the immediate
critical path. This arm changes Norn's canonical provider and persistence
substrate and therefore must not run concurrently with another arm editing the
same session/event modules.

### Arm B: shared contract fixtures

Owns NS0 identity inventory, canonical fixtures, encoding experiments, and
cross-language decoding. The documentation inventory can run in parallel with
P3/P4; canonical Norn payload fixtures and decoders wait for accepted P3/P4
types.

### Arm C: Frame host and browser composition

Owns Frame contribution assembly, host slots, browser ABI, Beamr namespaces,
and later bundle activation in the Frame repository. It can prototype against
contract fixtures without editing Norn.

### Arm D: Aion integration and console

Owns Aion-side links, slots, and console composition. It consumes Norn fixtures
until NS2/NS4 are ready and must preserve Aion's current crash-resumable workflow
contract.

### Arm E: later storage and transport

Owns workspace-provider, Haematite, Liminal, and Lys work only after their entry
gates. Splitting these prematurely would create unreviewable interfaces and is
not useful parallelism.

At any one time, one agent owns a file. Cross-arm changes are exchanged as
commits or frozen fixtures, not by editing the same worktree concurrently.

## 20. Structured Norn delegation

The repository's [`.claude/skills/norn`](../.claude/skills/norn/SKILL.md) skill
is the default way to add read-only scouting, research, and independent review
beyond the immediate sub-agent slots.

Delegated work follows these rules:

- Use persistent Norn sessions; never discard the audit trail with
  `--no-session`.
- Use the mode-specific structured output schema and `--output-format json`.
- Save the complete envelope beneath the trusted Norn delegation store.
- Record the Norn session ID and result-envelope path in the handoff.
- Give each run a bounded question, repository root, allowed-tool set, and
  explicit no-mutation instruction where applicable.
- Treat delegated output as a review input, not proof. The primary implementer
  verifies every material claim against source or executable evidence.
- Do not use more agents to compensate for an unfrozen contract. Parallelize
  inventories, fixtures, and adversarial reviews, not competing designs of the
  same authority.

Recommended use by phase:

| Phase | Structured Norn work |
|---|---|
| NS0 | Cross-repository authority and identity audits; contract refutation |
| NS1 | Responses taxonomy inventory and raw-fixture review under the owning plan |
| NS2 | Persisted-event inventory and projection completeness review |
| NS3 | Read-only process/socket state-machine and reconnect adversarial review |
| NS4-NS7 | Contribution, browser isolation, and upgrade-transaction review |
| NS8-NS9 | Workspace/tree/store conformance and crash-window enumeration |
| NS10-NS11 | Delivery, identity, trust, partition, and disaster-recovery review |

## 21. Immediate next milestone

The next implementation milestone is not the supervisor or distributed stack.
The critical and parallel paths are:

1. Complete P1 acceptance.
2. Complete P2 retained evidence and acceptance.
3. Obtain the same reviewer's narrow F1 confirmation and record D2 acceptance.
4. In parallel, complete NS0A's authority and identity inventory without
   editing the P3/P4 transcript implementation.
5. Close P3's remaining response-audio, exhaustive lifecycle-fixture, final
   evidence, and independent-review gates, then obtain P3 acceptance.
6. Close P4's remaining audio, fixture-matrix, and independent-review
   gates, then obtain P4 acceptance.
7. Complete the post-P4 NS0B contract freeze.
8. Stop at M1 and confirm that the canonical event substrate is sufficient for
   Norn's tree/session projection before beginning NS2.

Under the current owner ruling, P1 work and its deterministic local gate proceed
without GitHub Actions, but D0 still needs an explicit exit disposition before
P1 can be called `READY`. No remote-enforcement claim may be inferred from the
local gate. This decision does not block implementing or reviewing the local P1
foundation while the exit mechanism is unresolved.

In practice, this is the work that delivers the missing Responses items,
events, refusals, phases, annotations, hosted-search records, compaction data,
typed multimodal content, identity-safe tool completion, and exact
persistence/replay behavior the owner originally asked to fix. The stack work
then consumes that result instead of delaying it. NS5A's action/server work and
NS5B's full MCP-client work remain explicit, independently reviewable milestones
rather than implied side effects of Frame integration.

## 22. Completion checklist

- [ ] NS0 contract reviewed and frozen.
- [ ] Responses P3-P4 accepted and NS1 closed.
- [ ] NS2 read model accepted.
- [ ] NS3 local detachable read supervisor accepted.
- [ ] NS4 read-only Frame contribution accepted.
- [ ] NS5A unified action and MCP-server projection accepted.
- [ ] NS5B complete MCP client accepted.
- [ ] NS6 merged Aion/Norn console accepted.
- [ ] NS7 hot install/upgrade accepted.
- [ ] NS8 workspace-provider seam accepted.
- [ ] NS8A native tree-session convergence accepted.
- [ ] NS9 Haematite session store accepted.
- [ ] NS10 cross-node delivery/remote attach accepted.
- [ ] NS11 attestation/fenced cross-machine resume accepted.
- [ ] Architecture document reconciled with the implemented system.
- [ ] Every retained limitation has an owner, evidence, and explicit status.
