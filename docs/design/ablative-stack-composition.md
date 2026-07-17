# Ablative stack composition architecture

**Status:** Draft for owner and cross-repository review

**Date:** 2026-07-16

**Scope:** The shared contracts that let Norn, Aion, Frame, Beamr, Haematite,
Liminal, Lys, Yggdrasil, Tharsis, Urd, Chiron, Prospekt, and adjacent services
compose without erasing their independent authorities.

This document records the architecture discussed with the owner. It is a
direction and compatibility contract, not a claim that every described runtime
surface exists today. The incremental Norn execution plan lives in
[`../NORN-STACK-INTEGRATION-PLAN.md`](../NORN-STACK-INTEGRATION-PLAN.md).

The current Norn Responses candidate at `07bf9c1` now inventories and validates
all 28 public output-item discriminators, preserves the shipped non-audio nested
content/tool shapes and caller lineage canonically, and exercises representative
real spawn/fork replay. This is an implementation candidate, not phase
acceptance: D2 is decided but its strict store and offline migrator are not
implemented; response-scoped audio, the exhaustive all-lifecycle media matrix,
retained evidence, and independent P3/P4 review remain open. The candidate
source range remains frozen for that review.

## 1. Executive position

The stack already has a valuable execution vertical: Aion can provision work,
run Norn activities against Yggdrasil-managed workspaces, and resume after a
machine failure. The owner confirms that this path is operating today. The next
step is therefore not to replace it with a new orchestration foundation. It is
to make the existing systems compose through small, typed contracts.

The architecture has three connective data contracts:

1. A stack-wide semantic event record for correlation and audit.
2. A delivery wrapper, owned by the transport, for replay and stream position.
3. A Frame component bundle and contribution contract for runtime composition.

When a mutation crosses a process or node boundary, it uses a fourth,
request/outcome control contract. A command that has not executed is not an
event and must never be encoded as `EventRecordV1`.

These contracts connect existing domain records. They do not replace Aion
workflow history, Norn transcripts, Yggdrasil source history, or any other
domain authority.

The first visible proof should be deliberately small: one local Frame surface
in which independently started Aion and Norn processes can contribute views,
status, events, and existing actions. Distribution, cross-machine resume,
attestation, and database-native session storage follow only after the local
contract is demonstrated.

## 2. Current reality and proposed direction

Statements in this document use three evidence classes:

- **Operating baseline:** confirmed by the owner as working in the present
  stack. This document does not reopen it without contrary runtime evidence.
- **Repository-observed:** present in source or design material inspected during
  this review.
- **Proposed:** the target contract or sequencing decision still requiring
  implementation and review.

### 2.1 Operating baseline

- Aion, Yggdrasil, and Norn already form a crash-resumable execution path.
- Aion can provision a workspace, execute workflow steps through Norn, survive
  a machine interruption, and resume the workflow.
- Norn driven mode is the workflow-facing invocation contract.
- Norn sessions and sub-agent work are intended to retain their fork history,
  convergence, action history, and compaction provenance rather than flattening
  the result into one linear transcript.

### 2.2 Repository-observed foundations

- Beamr already supports namespaced module loading, multiple code generations,
  safe purge, and rollback when module loading fails.
- Frame already has stable component identity, lifecycle, dependencies,
  capabilities, child trees, state, and incarnation fencing.
- Aion already translates Norn driven events into neutral Aion integration
  events and preserves unknown events rather than silently dropping them.
- Aion's operations console has feature boundaries and lazy build chunks, but
  its routes, navigation, and providers are still build-time registries.
- Frame's view layer, external registration protocol, browser bundle loader,
  and runtime contribution assembly are not implemented.
- Frame currently loads native components into Beamr's default namespace even
  though Beamr has namespaced loading. Independent components can therefore
  collide unless Frame adopts one stable namespace per component.
- Liminal contains useful participant and delivery ideas, but its proposed
  participant contract is not yet a single implemented stack-wide protocol and
  its current envelope families are not interchangeable.
- Lys provides detached cryptographic evidence but does not define application
  identity, causality, or ordering.
- Chiron already supplies important diagnostics, syntax, and LSP capabilities
  to Norn. The ownership boundary between diagnostic execution and agent policy
  is already useful and should be retained.
- Norn's current persisted session edge is single-parent. `ForkComplete`
  records child completion for visualization, but it is not a persisted
  multi-parent convergence node. Multi-parent native history remains a target,
  not an input the first read model may invent.

### 2.3 Proposed direction

- Frame becomes the local composition host and later the distributed component
  host. It does not become the source of every domain event.
- Norn gains a detachable local supervisor surface so sessions can continue
  without a terminal UI and clients can attach, detach, and observe them.
- Aion and Norn register leased Frame contributions over a local control
  channel. The assembled Frame snapshot is authoritative; lifecycle events are
  invalidation hints.
- Norn projects canonical session events into the shared event contract without
  replacing its provider transcript or session storage model.
- Haematite later becomes a Norn session-store implementation behind a stable
  manager seam. Before that later engine change, Responses D2 moves new strict
  JSONL sessions to the versionless `~/.norn/session-store/` namespace and keeps
  legacy `~/.norn/sessions/` untouched. Existing JSONL is imported explicitly,
  not silently dual-read forever.
- Liminal later carries semantic records between nodes. Beamr supervises local
  processes. Aion continues to own durable workflow execution.
- Lys attestations can be added to canonical records and artifacts when trust
  across nodes becomes necessary.

## 3. Target authority map

No component may infer that another component's record is merely a differently
encoded copy of its own. Each authority answers a different question. This
table states the intended ownership boundary; it does not claim that every
surface is implemented. Section 2.2 records the observed maturity, and rows
labelled future remain proposals until their own repository accepts them.

| System | Authoritative for | Not authoritative for |
|---|---|---|
| Norn | Model/tool transcript, agent session causality, forks, joins, intervention, and agent-facing projections | Durable workflow scheduling, source-control history, or message delivery order |
| Aion | Deterministic durable workflow execution, activity state, retries, signals, and workflow recovery | Provider transcript truth or source snapshots |
| Yggdrasil | Current Git-compatible stacked changes, landing, and source history in the operating toolchain | Agent transcript or workflow history |
| Tharsis | Future live branchable workspace state, leases, and workspace provenance | Source-control record or agent intent |
| Urd | Future database-native source history and change operations | Current Git interoperability until it exists and is adopted |
| Chiron | Code intelligence, diagnostic execution, normalized findings, caches, and node-local diagnostic scheduling | Deciding when an agent must run a check or what the result means to a workflow |
| Haematite | Durable content-addressed and distributed storage primitives | Domain meaning, workflow policy, or UI composition |
| Liminal | Participant messaging, delivery, replay position, and transport continuity | Global actor identity or semantic event meaning |
| Lys | Signatures, attestations, seals, and verifiable artifact evidence | Logical actor identity, causality, freshness, or event order |
| Beamr | Lightweight processes, supervision, module generations, and local distribution primitives | Durable workflow history or browser composition policy |
| Frame | Component lifecycle, dependency/capability composition, contribution assembly, and host surfaces | Replacing the authoritative histories of loaded components |
| Prospekt | Document models, declared evidence, proof requirements, and lifecycle gates | Executing workflows or persisting agent transcripts |

This division is intentional. A workflow activity can link to a Norn session,
which links to a workspace snapshot and a diagnostic run, without any one of
those records swallowing the others.

## 4. Shared semantic event contract

### 4.1 Purpose

The shared event record enables correlation, navigation, replay, audit, and
optional attestation across the stack. It is a small envelope around a
domain-owned payload. It is not a universal payload schema.

Conceptually:

```text
EventRecordV1
  event_id
  event_kind
  schema_ref
  producer_ref
  producer_epoch
  subjects[]
  actor_ref?
  direct_causes[]
  correlation_id?
  links[]
  occurred_at?
  payload
```

Field intent:

- `event_id` is the stable, domain-qualified identity of this immutable
  projection record. A one-to-one projection may retain the native identity;
  otherwise it derives identity from the native domain, native record ID,
  projection kind, and projection version under the accepted construction.
  One native record may map to several projection records only when each
  projection kind has a stable declared cardinality.
- `event_kind` supports routing and coarse inspection without decoding the
  domain payload.
- `schema_ref` identifies the exact canonical payload schema.
- `producer_ref` identifies the logical producing component or service.
- `producer_epoch` identifies the semantic producer generation and is distinct
  from component, supervisor, session-run, and delivery epochs.
- `subjects` contains zero or more typed, role-labelled subject references.
  Lifecycle events need not invent a subject, and multi-entity events need not
  pretend that one entity is uniquely primary.
- `actor_ref` is optional because a system event need not pretend to be a
  human or agent action.
- `direct_causes` contains immediate causal parents. The contract can represent
  multiple parents without storing an ever-growing ancestor list, but a Norn
  projection initially carries only the source-observed single parent plus
  typed child-completion links. It gains multiple parents only after Norn has a
  reviewed native convergence record.
- `correlation_id` groups a larger operation without claiming causality.
- `links` are typed references to related workflow, session, agent, workspace,
  change, diagnostic, artifact, document, or message records.
- `occurred_at` is informational. Wall-clock time never establishes global
  order.
- `payload` is canonical inline bytes or an artifact reference owned by the
  domain schema.

An `EventRecordV1` is immutable once identified or attested. A cross-domain
relationship discovered later is a separate immutable relation record; it does
not mutate the earlier record's links and digest. Read-model state may project
that relation but is never its sole durable authority.

### 4.2 Durable relation records

Every durable cross-domain link is first recorded by one named domain authority
and may then be projected as:

```text
RelationRecordV1
  relation_id
  relation_kind
  schema_ref
  asserting_producer_ref
  asserting_producer_epoch
  endpoints[]
  supporting_native_refs[]
  direct_causes[]
  supersedes_relation_id?
  retracts_relation_id?
  payload?
```

`relation_id` is stable and domain-qualified. `endpoints` are typed and
role-labelled; their order is canonical only where the owning relation schema
gives it meaning. The asserting authority owns the relation kind and must cite
the native record or evidence that justifies it. A correction, supersession, or
retraction is another immutable record rather than an edit. Two domains may
assert different relations between the same endpoints, but they may not mint
competing records under the same authority and identity.

`EventRecordV1.links` contains only relationships already present in the
owning native event. A relationship discovered later enters the read model from
`RelationRecordV1`; a cache or UI join cannot become the only source of an
auditable link.

### 4.3 Native records remain native

An Aion workflow event remains an Aion workflow event. A Norn Responses item
remains part of Norn's ordered provider transcript. Projection into
`EventRecordV1` adds stable correlation metadata; it does not normalize away
the native record or become the persistence source of truth by accident.

Text, image, audio, file, structured MCP content, and future content variants
remain typed native content or content-addressed artifacts. A display renderer
may derive text, thumbnails, or summaries, but the shared projection must not
flatten multimodal or structured content into an irreversible string.

Unknown native variants remain opaque and non-executable unless their owning
domain explicitly classifies them. The shared envelope must not create a second
path that silently drops or guesses unknown event semantics.

### 4.4 Identity and digest discipline

The stack must use distinct types for values that happen to have the same byte
length:

- component identity;
- producer identity;
- schema digest;
- artifact digest;
- semantic-record digest;
- signer key identity;
- participant identity;
- stream identity.

Replacement epochs are also distinct types with distinct allocators:

- `ComponentGenerationId` is allocated by Frame for one loaded component
  generation and gates contribution publication.
- `SupervisorInstanceId` identifies one local supervisor process.
- `SessionRunEpoch` is allocated by the one durable session-execution authority
  recorded for that session and gates provider dispatch, session mutation, and
  operator actions. The target local model uses Norn's durable session owner for
  standalone sessions and Aion's execution lease for Aion-owned workflow
  sessions; authority cannot change without the reviewed transfer protocol.
- `ProducerEpoch` identifies the semantic producer recorded on events.
- `DeliveryStreamEpoch` is allocated by the delivery authority and gates stream
  sequence.

These values may link to one another, but one must never be compared as another.
Replacing a browser component, supervisor process, session writer, semantic
producer, and delivery stream are different lifecycle events.

Schema and artifact references use a cryptographic content digest over a
canonical representation with an algorithm and domain tag. A generic
`[u8; 32]` is not a safe cross-stack identity type. Conversation-local Liminal
participant IDs are not promoted into global actor IDs.

### 4.5 Causality and order

There is no stack-wide total order.

- Domain stores define their own authoritative order.
- `direct_causes` expresses causal dependency.
- A delivery stream supplies sequence only within that stream and epoch.
- A wall-clock timestamp aids display and diagnostics but cannot break ties or
  prove causality.
- A future Norn convergence record may have several role-labelled direct
  parents without pretending their histories were textually merged. When the
  owning domain gives parent order semantic meaning, canonical bytes preserve
  that order. Otherwise the canonical encoding sorts the set by edge role and
  domain-qualified event identity. NS0 must freeze this rule before the first
  multi-parent native record.

## 5. Delivery contract

Transport state is separate from semantic state:

```text
DeliveryV1
  stream_ref
  stream_epoch
  sequence
  record_digest
  record_bytes?
```

- `stream_ref` scopes delivery position.
- `stream_epoch` is minted by the delivery authority when its stream sequencing
  authority changes. It is independent of `ProducerEpoch`: a producer may
  change while a durable stream continues, and a delivery binding may change
  while the producer does not.
- `sequence` is monotonic only within `(stream_ref, stream_epoch)`.
- `record_digest` binds the wrapper to the semantic record.
- `record_bytes` may be omitted when the receiver already has the referenced
  record.

Liminal is the natural owner of this wrapper when cross-node transport is
implemented. A local Unix-domain control channel may use the same semantic
record while having a simpler local delivery mechanism. The transport may retry
delivery; it may not rewrite semantic identity or domain causality.

## 6. Component and contribution contract

### 6.1 Bundle

The installable unit is a versioned, content-addressed bundle:

```text
ComponentBundleV1
  contract_version
  component_id
  component_version
  content_digest
  runtime_artifacts[]
  requires[]
  provides[]
  requested_capabilities[]
  storage_compatibility
  supervision_policy
  upgrade_policy
  contributions[]
```

Capability declarations are requests, not grants. The host resolves them
against operator policy before activation.

### 6.2 Contribution

```text
ContributionV1
  local_id
  target_slot
  kind
  order
  artifact_ref?
  entrypoint?
  input_schema_ref?
  actions[]
  subscriptions[]
```

The stable contribution key is `(component_id, local_id)`. An active instance
also carries its `ComponentGenerationId` so delayed registrations from an old
generation can be rejected.

Candidate validation rejects duplicate route names, command names, action IDs,
or contribution keys before publication. Slot order uses the declared `order`
and then the stable `(component_id, local_id)` key as its deterministic
tie-breaker. When withdrawal removes the active route, the host applies its
declared navigation fallback, normally the containing workspace page or shell
home; a component cannot choose a fallback into another component's private
route.

`target_slot` is a host-advertised semantic name, not a closed global enum.
Initial examples are:

- `shell/nav`;
- `workspace/page`;
- `entity/inspector`;
- `timeline/overlay`;
- `status/indicator`;
- `commands`.

The host declares which contribution kinds and schemas each slot accepts.
Unknown slots fail registration explicitly rather than disappearing.

### 6.3 Authoritative assembly

Frame owns one authoritative snapshot of currently active contributions.
Lifecycle notifications tell clients that the snapshot may have changed; they
are not durable history and need not contain the full state.

- A component publishes or withdraws its contribution set atomically with its
  running `ComponentGenerationId`.
- A dropped notification forces a snapshot refresh.
- Disconnect or lease loss withdraws only the affected component generation.
- Historical Norn and Aion records remain available after a live view is
  withdrawn.
- Reconnection publishes a new component generation and a fresh authoritative
  set.

This avoids ghost navigation entries, half-installed routes, and stale actions.

## 7. Browser composition

Browser components must not exchange React component objects. Doing so binds
independently released bundles to one React runtime, hook dispatcher, context
graph, and bundler contract.

The browser ABI is framework-neutral:

```typescript
activate(host: FrameHostV1): Promise<Disposable>
```

The imported module is side-effect-free until `activate` is called. The host
provides a staging scope that owns every tentative route, handler,
subscription, style root, and DOM mount. Failed activation revokes the whole
scope even when no `Disposable` was returned. Successful disposal is idempotent
and means withdrawing those host resources; imported JavaScript is not claimed
to be unloadable.

The host supplies:

- route and slot registration;
- schema-checked action invocation;
- event and data subscriptions;
- component identity, current backend `ComponentGenerationId`, and a distinct
  browser-local `BrowserActivationId`;
- navigation and deep-link services;
- theme tokens and stable host primitives;
- disposal and upgrade fencing.

Activation negotiates the exact `FrameHostV1` contract version before any
registration. Custom-element names are digest or generation qualified, or the
module mounts only beneath a host-owned root, so a later generation never tries
to redefine a page-global element name. DOM and styles are isolated through
that mount root and host theme contract.

Aion and Norn may use React internally. Standard views should prefer
declarative fragments rendered by the host. Specialized surfaces, such as an
interactive Norn transcript tree, may use a content-addressed custom element or
DOM-module escape hatch behind the same activation and disposal ABI.

A same-origin custom element or DOM module is not capability-confined by that
ABI. It can directly use browser DOM, storage, and network APIs. The initial
policy is therefore explicit:

- untrusted or unapproved contributions are host-rendered declarative data only;
- executable browser modules are trusted first-party code installed from an
  operator-controlled publisher/version allowlist;
- content addressing proves which bytes were loaded, not that those bytes are
  authorized or safe;
- server-side Norn and Frame authorization remains mandatory even for a trusted
  module;
- admitting third-party executable modules later requires a separately reviewed
  isolation design such as a sandboxed frame or worker with a narrow message
  capability protocol.

Live application data travels over subscriptions. A contribution descriptor is
static for one component generation; it is not repeatedly replaced to deliver changing
session state.

## 8. Local Frame node and Norn supervisor

The first composition host is local and intentionally narrow:

1. A Frame node owns component registration and the assembled contribution
   snapshot.
2. It listens on a user-private Unix-domain socket.
3. Independently launched Aion and Norn processes register leased contribution
   sets.
4. A browser client subscribes to the authoritative snapshot and domain event
   streams.
5. Stopping Norn removes its live contributions without stopping Aion or
   deleting historical session records.

Norn's supervisor is a separate concern from the provider connection. Its
target surface owns session processes, attach/detach, observation, and operator
actions. The first NS3 slice is local and read-only; cross-process mutations are
added only through the common NS5A action path. Neither slice implies public TCP
listening or a new remote authentication system.

The first merged-console result should make these capabilities visible:

- Aion contributes workflows, namespaces, incidents, workflow detail, and
  existing pause/resume/signal actions.
- Norn contributes sessions, agents, live status, transcript/action timeline,
  and existing intervene/fork/stop operations.
- Norn can target an Aion workflow-detail extension slot using typed links
  rather than Aion importing Norn UI code.
- A combined timeline follows shared event links while preserving which domain
  owns every record.

## 9. One action declaration, several surfaces

Frame UI actions, MCP tools, and local programmatic calls should be projections
of one typed action declaration and one authorization/dispatch path. This avoids
an MCP implementation acquiring capabilities that the UI path does not audit,
or a UI button bypassing the tool diagnostics and policy path.

An action declaration identifies:

- stable action name and version;
- input and output schemas;
- target component and subject types;
- requested capabilities;
- idempotency and cancellation semantics;
- emitted event kinds;
- operator-facing label and confirmation policy.

The declaration is metadata. A mutation is dispatched only through a
non-forgeable, library-owned invocation context:

```text
AuthorizedActionInvocation
  authenticated_principal
  invocation_surface
  delegation_chain
  target_ref
  target_generation_or_run_epoch
  granted_capabilities
  authorization_evidence
  idempotency_key?
  input
```

Frame controls admission to host surfaces. Norn remains the final authority for
Norn domain mutations. Operator controls, agent tools, calls to external MCP
servers, and capabilities exported by Norn's MCP server are separate action
classes even when they share schema and dispatch infrastructure. The resulting
event's `actor_ref` is audit data; it is not a substitute for the invocation
authority.

Cross-process and cross-node mutation uses a distinct request/outcome pair. The
wire request is untrusted input and never carries a serialized local
authorization decision:

```text
ActionRequestV1
  request_id
  action_name_and_version
  target_ref
  target_generation_or_run_epoch
  presented_delegation_credential?
  input
  idempotency_class
  attempt_id
  correlation_id?

ActionOutcomeV1
  request_id
  attempt_id
  terminal_class
  result_or_error
  committed_event_refs[]
```

The serialized request does not contain its authenticated channel context. The
receiving Norn authority binds it to a non-serialized `AuthenticatedPeerContext`
supplied by the transport, verifies any delegation credential, performs
authorization, and only then constructs the non-public
`AuthorizedActionInvocation` locally. A delegation credential, where supported,
is verifiably bound to issuer, audience, action and version, target and epoch,
capability set, validity/revocation policy, and request or idempotency identity.
Caller-supplied principal or capability fields are never trusted. The outcome
is bound to the authenticated responder and the original request and attempt.

Delivery of an action request is not evidence that the action executed. Only a
domain-committed outcome and its referenced semantic events establish the
result. A transport must provide targeted request/reply semantics or the remote
control plane must use a separate protocol that does.

This contract describes existing operations first. It must not invent new Norn
authority merely to populate a console.

MCP has two independent directions:

- **Norn as an MCP client:** Norn connects to operator-configured servers,
  negotiates their capabilities, follows dynamic catalog changes, and makes
  server-qualified capabilities available to selected agents. The existing
  layered user/project/local/CLI/session configuration and live-control surface
  remain the base. Protocol completeness later adds the currently unclaimed
  lifecycle and capability families, including reconnect/resumption, HTTP
  session shutdown, OAuth, sampling, resources, and prompts, after a fresh
  official-spec inventory.
- **Norn as an MCP server:** Norn exposes selected typed actions and read-only
  resources to MCP consumers through the same authorization, diagnostics,
  event, and result path used by local callers. It does not blindly export
  every internal function or private session artifact.

Discovered server tools and resources remain dynamic. They are not compiled
into the Frame contract or treated as permanent Norn-native capabilities.

## 10. Hot install and upgrade

Frame allocates one stable Beamr module namespace per `ComponentId`. A
`ComponentGenerationId` is a Frame lifecycle token, not another Beamr
namespace. Every module load and every supervisor/child spawn for the component
must use its stable namespace; the current default-namespace paths are not
accepted for composed components.

The backend upgrade transaction is:

```text
verify bundle
  -> resolve stable component namespace and new ComponentGenerationId
  -> stage every module generation in that namespace
  -> start and probe readiness
  -> atomically publish contribution snapshot
  -> drain old component generation
  -> safe-purge old code
```

Required properties:

- Stable `ComponentId`, durable state, and conversations survive an upgrade.
- Process-local actor state does not silently cross generations.
- Existing requests either complete once on the old generation or wait behind
  the activation barrier; they are not replayed ambiguously.
- The previous generation remains last-known-good until the candidate proves
  ready.
- A failed candidate leaves the old contribution snapshot authoritative.
- Multi-module candidates require bundle-wide rollback of every staged module
  on partial failure. A single module's `on_load` rollback is not represented as
  proof of bundle atomicity.
- Three consecutive upgrades and an old actor that prevents safe purge are
  explicit conformance cases; Frame reports the blocked retirement rather than
  discarding code still in use.
- The first hot-upgrade proof is limited to stateless components or components
  whose durable state is external and schema-unchanged. A later state migration
  requires an atomic snapshot/migrate/probe/promote/rollback protocol; an
  irreversible migration never runs before candidate promotion.

Frame atomically publishes a versioned authoritative backend/contribution
snapshot. Browsers are independent clients and do not join that global atomic
transaction. Each browser downloads and stages a compatible artifact under a
fresh `BrowserActivationId`, binds that scope to the current backend
`ComponentGenerationId`, and swaps its local view atomically. A browser that is
offline or fails activation does not roll back the globally accepted backend.
It may reuse last-known-good artifact bytes only after the host validates their
declared compatibility and rebinds them in a fresh scope to the current backend
generation. Otherwise the old scope becomes read-only or update-required; it
cannot dispatch under a stale generation token. Bundle metadata states host and
backend contract compatibility and declares which routes, subscriptions, and
actions may survive a rebind.

## 11. Norn session history and storage

Norn's target logical history is a causal tree that can converge, not a
flattened chat log. The current native event model is single-parent and records
child completion as a related event; a later tree-session phase must add and
review exact multi-parent convergence before any projection claims it. The
target persistence model retains:

- provider transcript items in exact order;
- typed image and other multimodal input/output content;
- structured MCP result content and referenced resources;
- local tool calls and results;
- forks and spawn provenance;
- child transcripts;
- convergence records with multiple parents;
- intervention and action history;
- compaction summaries and the records they summarize;
- full-output and fetched-document artifact references;
- links to workflow, workspace, diagnostic, and source-change records.

The Responses remediation plan P3 and P4 define the canonical provider-item
substrate and streaming fidelity. The tree-session model builds on that
substrate; it must not introduce a second lossy transcript representation.

JSONL remains the current storage engine. Under the owner-decided Responses D2
contract, all new strict runtime sessions belong under the versionless
`~/.norn/session-store/` path. The legacy `~/.norn/sessions/` tree remains
untouched as source and backup; there is no `sessions-v2` path, in-place
upgrade, runtime dual-read, or dual-write period. A separate explicit offline
migration is atomic, idempotent, interruption-recoverable, and classifies each
session before publication. Canonically complete sessions may resume from a
fresh provider-state epoch; flattened but coherent sessions require an explicit
degraded/fresh-epoch resume with recorded fidelity loss; corrupt or ambiguous
sessions are inspect/export-only. This contract is decided but not yet
implemented or accepted.

Haematite integration follows a later session-store manager seam and its own
explicit offline import path. The target Haematite model uses append-only
branches, stable event addresses, arbitrary committed fork anchors, and multi-
parent convergence records. It does not require text-merge semantics.

Cross-machine resume becomes possible only when the session store, referenced
artifacts, workflow links, and workspace provenance can all be resolved on the
destination node and the old writer is durably fenced. Haematite alone does not
make a running process migratable. Aion, as durable execution authority, must
own a monotonic session-execution lease or equivalent ownership transfer that
every provider dispatch, session append, and mutating action validates.

Live transfer is a drain protocol, not only an epoch increment. The old owner
stops admission, cancels and joins provider, tool, child-agent, and child-process
work, durably classifies completed versus ambiguous attempts, checkpoints
session and workspace state, and releases the lease with compare-and-swap before
Aion grants the next epoch. Lease loss or renewal failure actively stops old
work. If an in-flight provider or external side effect cannot be cancelled,
joined, fenced, or authoritatively classified, live transfer is prohibited and
the supported operation is cold disaster recovery after the prior writer is
definitively stopped.

## 12. Aion, AWL, and scripted Norn execution

Aion remains the durable workflow authority. AWL can provide ahead-of-time
checked orchestration of Norn tools and agents without adding an independent
workflow engine inside Norn.

The useful convergence is:

- Aion compiles and executes the workflow;
- Norn supplies agent and tool workers;
- Yggdrasil or a future workspace provider supplies isolated source state;
- Chiron supplies diagnostics and code intelligence;
- Frame exposes the live operational surface;
- shared event links make the entire execution auditable.

Dynamic agent planning may create or select workflows, but durable step state,
retry, crash recovery, and workflow signals remain Aion responsibilities.

## 13. Workspace and diagnostics seams

Norn should eventually depend on a backend-neutral workspace contract rather
than on one source-control implementation. Before the public Norn read model
freezes cross-links, the current Yggdrasil/Aion owners must define the existing
`WorkspaceRef`, `SnapshotRef`, and `ChangeRef`; Norn must not mint substitute
identities. The later behavioral seam needs operations for lease, root,
snapshot, fork, diff, checkpoint, and landing candidate. Its first adapter wraps
the working Yggdrasil path without changing behavior. Tharsis can implement the
same contract after its workspace and failover spikes are proven.

Chiron owns diagnostic execution and normalized code-intelligence results. Norn
owns the policy that decides when write/edit/apply-patch operations require
checks and how results enter an agent turn. Diagnostic records need typed links
to the workspace snapshot, source change, Norn session, tool call, and Aion
activity that caused them.

Node-local build deduplication belongs with Chiron's jobserver/resource layer.
Cross-node scheduling and recovery belong with Aion and the supervisor rather
than a globally shared compiler semaphore.

## 14. Transport boundaries

Three socket uses must remain distinct:

1. **OpenAI Responses WebSocket:** provider transport inside Norn. It carries
   Responses requests and provider events and must share canonical event mapping
   with HTTP/SSE.
2. **Norn/Frame control channel:** local supervisor, attach/detach, contribution
   registration, action dispatch, and live observation.
3. **Liminal transport:** eventual participant messaging and cross-node event
   delivery.

Using WebSocket framing in more than one layer does not make the protocols
compatible. Provider transport is sequenced under the Responses P3-P8 work.
The local supervisor begins with a private Unix-domain socket. Liminal adoption
waits for its participant and delivery contracts to be implemented and reviewed.

## 15. Prospekt and Lys

Prospekt should describe deliverables, evidence, and lifecycle gates that Aion
executes and Norn satisfies. It should not become another scheduler. Because it
is newer than the operating execution vertical, it is integrated after the
shared action/event contracts are stable rather than made a prerequisite for
the first local proof.

Lys attestations remain Lys canonical COSE artifacts; the stack does not invent
a second serialized attestation shape. The signing input is the
construction-specific domain tag concatenated with the canonical semantic
record or artifact bytes. Lys performs its defined payload hashing and retains
the payload hash, signer public key, signature, and signing timestamp. Passing a
precomputed record digest into that API would create a second hash and is not
the default construction.

The signature proves that a key signed those construction-specific bytes. A
separate identity binding says what that key represents. Signing time does not
establish event freshness or order.

## 16. Architectural invariants

The following are non-negotiable:

1. No universal DAG replaces domain histories.
2. No wall clock or delivery sequence becomes global semantic order.
3. No transport-specific envelope becomes the semantic event contract.
4. No unknown event or item becomes executable through a generic decoder.
5. No component capability declaration grants its own authority.
6. No stale `ComponentGenerationId` can publish actions, views, or state.
7. No browser bundle shares framework internals as its public ABI, and no
   executable browser module is described as sandboxed when it is trusted
   same-origin code.
8. No UI, MCP, or programmatic action bypasses the common typed dispatch and
   domain-authorization path.
9. No storage migration becomes a permanent dual-read compatibility layer.
10. No provider WebSocket state leaks into supervisor or Liminal protocol design.
11. No proposed cross-stack integration reopens a proven execution path without
    concrete contrary evidence.
12. No implementation bypasses repository linting, diagnostic, test, or
    production-file-size policy.
13. No read model invents a causal parent or stable cross-domain identifier that
    its owning domain has not recorded.
14. No cross-machine resume starts a new writer before the prior writer is
    durably fenced.

## 17. Deliberate non-goals for the first proof

- Cross-machine agent migration.
- Public remote supervisor access.
- A universal identity provider for every repository.
- Arbitrary third-party browser code loading.
- Replacing JSONL with Haematite.
- Replacing Yggdrasil with Tharsis or Urd.
- Making Prospekt mandatory for all work.
- Rewriting Aion workflow history into shared events.
- Implementing a second workflow engine in Norn.
- Conflating provider, control-plane, and participant sockets.

## 18. Evidence basis

The repository-observed statements above were initially grounded against these
adjacent working copies on 2026-07-16. The Norn evidence was refreshed through
`07bf9c1` on 2026-07-17. The paths are evidence pointers, not a claim that the
sibling repositories are vendored dependencies of Norn.
Two read-only specialist agents performed the initial Frame/Beamr/Aion and
Liminal/Lys inspections; the primary implementer reconciled their findings into
this draft. Source-owner acceptance remains an open NS0 gate.

| Repository snapshot | Inspected evidence |
|---|---|
| Norn `07bf9c1` (`7429490` authoritative output contracts; `ad9fffe` caller-aware replay; `07bf9c1` fork/spawn lifecycle) | `crates/norn/src/provider/openai/response_contract.rs`, `crates/norn/src/provider/openai/response_reconciler/item_channels/authority/`, and `crates/norn/src/provider/openai/output_item_test_fixtures.rs` for the exact 28-item inventory and schema/actionability contract; `crates/norn/src/provider/request/tool_call_caller.rs`, `crates/norn/src/session/events.rs`, and the fork/spawn canonical lifecycle fixtures for caller-aware persistence and representative child replay; the Responses plan for candidate status and P3-P8 authority |
| Beamr `d60f826` | `module_management.rs:17`, `spawning.rs:99`, `module.rs:280`, `tests/hot_code_loading.rs:171` under `crates/beamr` |
| Frame `dadd430` | `frame-core/src/component.rs:10`, `frame-core/src/registry.rs:132,182`, `frame-state/src/handle.rs:39`, `frame-view/src/lib.rs:1`, `docs/briefs/F-5a-fragment-registration-assembly.md:1,103`, `docs/briefs/F-7b-frame-dev.md` |
| Aion `833f271e` | `apps/aion-ops-console/src/app/routes.tsx:57,89`, `AppShell.tsx:22`, `providers.tsx:31`, `crates/aion-integration-norn/src/translate.rs:277` |
| Liminal participant worktree `55856ae` | `PARTICIPANT-CONTRACT.md:474,509,1797,6005,6013,6147`; core/protocol/durability envelopes at lines `42`, `33`, and `15`; `services_schema.rs:43` |
| Lys `28e01a4` | `attestation/artifact.rs:30,59`, `attestation/mod.rs:33`, `seal/authenticated.rs:41` under `crates/lys-core/src` |
| Chiron dependency `25161bc8` (`399ec98` working copy) | Norn pins Chiron `diagnostics`, `lsp`, and `syntax` at root `Cargo.toml:19-21`; broader Chiron jobserver ownership remains proposed until its source-owner review |
| Haematite `02b8592` | Norn requirements reconciled through `tree-sessions/haematite-branch-requirements.md`; runtime adoption remains proposed |

The Aion/Yggdrasil/Norn crash-resumable execution chain is labelled
owner-confirmed because this document did not rerun the complete live workflow.
Its implementation remains the behavioral baseline for later conformance tests.

## 19. Open decisions

These require owner and repository-specific review before implementation:

Responses D2 is no longer an open decision. On 2026-07-17 the owner selected the
versionless strict `~/.norn/session-store/` namespace, untouched legacy
`~/.norn/sessions/`, and explicit offline migration with canonical,
degraded/fresh-epoch, and inspect/export-only outcomes. Implementation and
acceptance remain open in the Responses plan.

1. The canonical encoding and digest construction for `EventRecordV1`.
2. Which repository owns the shared contract crate and TypeScript bindings.
3. The stable global `producer_ref` and `actor_ref` identity binding model.
4. The initial Frame host slots and declarative fragment vocabulary.
5. The exact action authorization and human-confirmation contract.
6. The local Frame node's process ownership and discovery mechanism.
7. The boundary between a Norn supervisor process and sessions that remain
   in-process for TUI, print, driven, or library use.
8. Whether and when to add isolated third-party browser modules beyond the
   approved first-party same-origin policy.
9. The Liminal participant contract version that is sufficiently implemented
    to carry cross-node records.
10. The typed action principal, delegation, grant, and authorization-evidence
    contract.
11. The stable workspace, snapshot, change, and diagnostic reference owners.
12. The Aion-owned session-execution lease and workspace ownership-transfer
    contract for live cross-machine resume.

## 20. Repository references

- [`../RESPONSES-API-REMEDIATION-PLAN.md`](../RESPONSES-API-REMEDIATION-PLAN.md)
  owns Responses transcript, event, state, transport, request, and cache
  correctness.
- [`../NORN-STACK-INTEGRATION-PLAN.md`](../NORN-STACK-INTEGRATION-PLAN.md)
  sequences the Norn-side implementation.
- [`../norn-domain-ledger.md`](../norn-domain-ledger.md) retains the broader
  Norn idea and debt inventory.
- [`../frame-v1-norn-map.md`](../frame-v1-norn-map.md) records the earlier
  Frame-facing Norn snapshot.
- [`tree-sessions/haematite-branch-requirements.md`](tree-sessions/haematite-branch-requirements.md)
  records Norn's Haematite branch requirements.
- [`norn/reference-pi-sessions.md`](norn/reference-pi-sessions.md) records a
  reference tree-session implementation.
- [`norn-provider-ws/DESIGN.md`](norn-provider-ws/DESIGN.md) is retained as
  blocked historical design input. It must be rewritten on the P3-P8
  authorities before implementation and remains separate from the
  supervisor/control protocol described here.
