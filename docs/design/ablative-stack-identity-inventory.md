# Ablative stack identity and authority inventory

**Status:** NS0A source-inventory candidate; owner and source-owner decisions
remain open.

**Date:** 2026-07-19

**Architecture:**
[`ablative-stack-composition.md`](ablative-stack-composition.md)

**Execution plan:**
[`../NORN-STACK-INTEGRATION-PLAN.md`](../NORN-STACK-INTEGRATION-PLAN.md)

## 1. Purpose and boundary

This document records the identifiers and correlation values that exist in the
current Ablative repositories. It prevents later integration work from treating
equal-looking strings, integers, UUIDs, hashes, or generations as one global
identity.

This is an inventory, not a shared identity schema. It does not allocate a new
identifier, declare a cross-domain owner, freeze projection bytes, or authorize
runtime integration. Native authorities remain native until the open NS0A
owner decisions are made.

The governing rules are:

1. A value is meaningful only with its native domain and scope.
2. A serialization alias is not a second authority.
3. A path, hash, display name, sequence, or connection-local number is not
   promoted into a global reference merely because it is convenient.
4. A replacement token is valid only for the lifecycle whose allocator minted
   it.
5. Absence is recorded explicitly. Norn must not mint substitute Yggdrasil,
   Aion, Chiron, Frame, or other domain identifiers.

## 2. Method and pinned snapshots

This candidate was produced from pinned committed objects with `git show` and
`git grep`. Dirty working-copy changes in adjacent repositories were excluded.
The exact Rust/TypeScript source selection, exclusions, four discovery queries,
34 source-bound semantic records, and seven negative assertions are retained in
[`../reviews/evidence/ns0/inventory-manifest.json`](../reviews/evidence/ns0/inventory-manifest.json).
[`../reviews/evidence/ns0/verify_inventory.py`](../reviews/evidence/ns0/verify_inventory.py)
replays them without reading uncommitted source. Its retained report records 11
verified commit pins and zero failed assertions in
[`../reviews/evidence/ns0/inventory-report.json`](../reviews/evidence/ns0/inventory-report.json).

The semantic review covers the identifier classes named by NS0A: component,
producer, participant, schema, artifact, stream, signing, session, agent,
workspace, snapshot, change, diagnostic, action principal, and replacement
epoch. It also retains adjacent message, credential, connection, capability,
and storage-lifecycle tokens where omitting them would create a substitution
risk. The discovery query hashes establish a reproducible lexical baseline;
the 34 semantic records bind the load-bearing authority seams. They do not yet
give every lexical match an individual semantic disposition, so the plan's
exhaustive-sweep acceptance item remains open rather than being inferred from a
green verifier.

The inclusion boundary is mechanical but not lexical: a value is retained when
it crosses a durable, wire, tool, process, storage, ownership, or adapter seam,
or when it fences replacement at such a seam. Parser/lexer tokens, compiler-IR
indices, test-only values, and private collection keys that add no scope or
lifetime beyond already inventoried components are excluded. This prevents a
type-name grep from being mistaken for either completeness or relevance.

The selected committed source includes Rust plus TypeScript/TSX from Aion,
Yggdrasil, Liminal, and Frame. Frame's current TypeScript includes applications,
packages, and examples; the manifest binds `FeedEnvelope` specifically as an
example protocol rather than a shared authority. The other selected
repositories expose the inventoried authorities through Rust at these pins.

| Repository | Committed snapshot | Inventory role |
|---|---|---|
| Norn | `2ee67c5b708c6eb4f57bb9ffb494960a49869de0` | Accepted Responses P3/P4 base plus session, agent, event, provider, credential, MCP, artifact, workspace, and local lifecycle values |
| Aion | `eef93212ed0c8ef2406bcd387903495c8462835b` | Workflow/run/activity/timer/schedule, attempt and stream order, agent/worker/caller, supervisor/routing, authoring, write capability, and SDK aliases |
| Yggdrasil | `6ccaeb89f457ef57880e683249608182b7681e8f` | Repository, Git snapshot, logical branch, dependency module, workspace, issue, change, and operation values |
| Chiron | `399ec98c006a7b16b03b6d9416d8bdec7f332fa3` | Diagnostic kind, commit, custom/LSP request, logical server, and daemon-instance correlation |
| Frame | `fbc03b55c53913f7f62d900152aae68552157bba` | Component/entity/reference, capability/grant, schema absence, storage incarnation, lifecycle news, and TypeScript example/application values |
| Beamr | `c716992fdbe72a8b0949009d941c8bafcb784b66` | Runtime namespace/actor/handle, module/readiness/connection/peer lifecycle, event subscription, service instance, replay, and capability-audit values |
| Liminal | `2bf71c43ad693963a96a15b99f3b2b0b989c2e23` | Participant process/credential, publisher/consumer/subscriber, conversation, binding, schema, message, delivery, connection, and stream values |
| Haematite | `dc907a78e028a0027362775af4279b830aae3ca5` | Storage hash/stream/branch, carrier connection, distributed node/write, ownership ballot/stamp, and local expiry values |
| Lys | `28e01a4fb9f0a3626b8b471fe8b55c3109596472` | Signing/note/certificate identities, attestation/payload digest, transparency root/checkpoint/proof, and sealed-envelope values |
| Tharsis, including its Urd documents | `acac7baec611d7020366f07ca042dfba288c5dd7` | Provisional layer/snapshot/branch/actor/build-lock/NFS-file/bracket/tree-root types and absences |
| Prospekt | `f00d919b2679bcfc45f694eab8fbabed112644ef` | Model-local document, relation, schema-file, and workspace-path values |

The full revisions above are evidence pins, not dependency declarations.
Unless a row explicitly labels historical input, source locations below resolve
against the corresponding pinned committed object. Checkout HEAD is
informational freshness metadata: later commits do not change the retained
object evidence, and only an explicit freshness sweep may re-pin it.

## 3. Native identity inventory

### 3.1 Norn

| Value | Native authority and scope | Evidence |
|---|---|---|
| Durable session | There is no canonical `SessionRef` or typed store namespace. Current mutation authority is the implicit `data_dir` root plus an exact index row `(id, generation, rel_path)`, revalidated under the index lock. `id` is an opaque validated string; `generation: Uuid` identifies one incarnation; `rel_path` is its storage locator but currently participates in the mutation comparison. Store qualification and relocation semantics remain design decisions. | `crates/norn/src/session/persistence/types.rs:421-470`; `session/manager/open.rs:234`; `session/persistence/index.rs:212-276` |
| Tool-context session | `tool::context::SessionId(String)` is a tool-context wrapper, not the durable index authority. A bare session string cannot distinguish store or incarnation. | `crates/norn/src/tool/context.rs:15-18` |
| Resolved run session | Serializable `ResolvedAgentInfo.session_id` is either a durable index ID or a run-scoped ID minted during a non-persistent build. It is exposed to embedders and the model for correlation, but the string alone does not reveal which lifetime applies. | `crates/norn/src/agent/handle.rs:76-103` |
| Agent | An agent UUID may be caller-supplied or freshly minted without registry participation. When coordination is enabled, `AgentEntry.id`, hierarchical path, and tombstones are scoped to that in-memory `AgentRegistry`; those registry guarantees do not apply to every root agent. There is no canonical cross-session `AgentRef`. | `crates/norn/src/agent/build_support.rs:228-270`; `agent/handle.rs:76-103`; `agent/registry.rs:83-114,452`; `tools/agent/infra.rs:146` |
| Persistent child | A persistent child deliberately reuses its registry UUID text as its child session ID, but the child session receives an independent session generation. That local equality is not a universal agent/session equivalence. | `crates/norn/src/tools/agent/spawn.rs:429-445`; `session/branch_materialize.rs:18-41`; `session/action_log_tree.rs:1-13` |
| Inter-agent and harness message | `ChannelMessage.id: Uuid` correlates queued/dequeued and sent/delivered audit phases. Router traffic carries sender and recipient agent UUIDs and an optional router-minted per-recipient `seq`. Cron, process-manager, watch, and other harness injections are unsequenced; their delivered event may use nil `from_id`, with the string label providing attribution. None is session-wide event order or a durable cross-session message reference by itself. | `crates/norn/src/loop/inbound.rs:63-101`; `agent/message_router.rs:149-250`; `provider/agent_event.rs:216-285`; `agent/pending_messages.rs:29-79` |
| Active operator input | `ActiveInput.id: Uuid` correlates one active-turn steer with the acknowledgement emitted after it is persisted. It exists only for that live input channel and is not the resulting `EventId`, a message ID, or durable user-input identity. | `crates/norn/src/loop/active_input.rs:15-61,83-121` |
| Event | `EventId(String)` is normally UUIDv7, but parsing accepts arbitrary text. Timeline append order, not UUID ordering, is authoritative. A cross-session event reference therefore needs its owning session incarnation. | `crates/norn/src/session/events.rs:10-73`; `session/store.rs:176`; `session/persistence/strict/reader.rs:90` |
| Provider output item, tool call, response, resource, and stream coordinates | Provider-owned `item_id`, `call_id`, and `response_id` have different meanings. MCP items additionally retain `approval_request_id`, while advanced tool environments may name a provider `container_id`; neither is a Norn approval record or artifact. `ResponseStreamProvenance` separately carries optional `item_id`, `output_index`, `content_index`, and provider `sequence_number`; these are response-stream reconciliation and diagnostic coordinates, not replayable item identity or session event order. | `crates/norn/src/provider/events.rs:95`; `provider/response_item.rs:149-174`; `provider/openai/response_reconciler.rs:128-154`; `provider/openai/response_reconciler/model.rs:10`; `provider/openai/response_reconciler/item_channels/authority/hosted_schema.rs:216-220,274-279`; `provider/openai/response_reconciler/item_channels/authority/advanced_tool_schema.rs:180-190`; `session/events.rs:77-104` |
| Named authentication account | A user-facing alias resolves through a private catalog to an opaque UUID storage ID, so the alias never becomes a path component. `AccountIdentityFingerprint` privately hashes the remote account ID; `CredentialRevision` hashes the exact serialized credential bytes for compare-and-swap. `ProviderProfileId` names a deployment/auth target. These are respectively selection, storage, remote-identity, byte-revision, and provider-profile domains, not action principals or interchangeable hashes. | `crates/norn/src/provider/openai_oauth/account_catalog.rs:34-59,315-417`; `provider/openai_oauth/account_identity.rs:1-32`; `provider/openai_oauth/credential_revision.rs:1-30`; `provider/api_shape.rs:85-118` |
| Credential coordination | The in-process manager key combines trusted auth root, storage mode, token URL, and private remote account tuple. `RefreshLineage` hashes the refresh token, while the durable recovery marker carries an operation nonce, prior/proposed credential revisions, and salted lineage/integrity proofs. These are redacted single-flight and no-replay mechanisms, not user-visible account identity or interchangeable credential hashes. | `crates/norn/src/provider/openai_oauth/manager.rs:52-104`; `provider/openai_oauth/manager_registry.rs:11-55`; `provider/openai_oauth/credential_recovery.rs:72-168` |
| MCP server, definition, tool, request, and client instance | A configured logical server name and `McpDefinitionFingerprint` identify the normalized definition consumed by approval policy; approval scope additionally uses the canonical project path and server name. A qualified tool name is derived for registry exposure. Numeric JSON-RPC request IDs are client/connection-local; `mcp_client.instance_id` is a client-lifecycle token; `tool_list_revision` is a separate client-local replacement counter. None is a remote MCP server identity or general execution epoch. | `crates/norn/src/config/mcp.rs:35-80,329-379`; `config/mcp_approval.rs:188-229`; `integration/mcp_proxy.rs:132-140`; `integration/mcp_stdio.rs:229-245,332-390`; `integration/mcp_client.rs:61`; `integration/mcp_protocol.rs:80-116` |
| Response-audio artifact | `ResponseAudioArtifactRef` is a canonical UUIDv4. Files physically live below `root_session_id`, while the artifact header and every operation bind the originating timeline's `owner_session_id` and exact `owner_generation`. Provider `response_id` is metadata, never artifact identity. | `crates/norn/src/session/response_audio.rs:64-105,136-210` |
| Spool artifact | Spool files have no separate artifact ID; their immutable reference is derived from the owning `EventId` beneath the root-session namespace. | `crates/norn/src/session/spool.rs:59`; `session/events.rs:247` |
| Session-storage transaction | Deletion journals and timeline/audio publication recovery allocate independent UUIDv4 transaction strings. They correlate one recoverable filesystem mutation and its owned stage/journal paths; they are not session, generation, artifact, run, or producer identity. | `crates/norn/src/session/persistence/index_deletion.rs:22-30,72-84`; `session/persistence/publication_recovery.rs:93-103` |
| Action principal | `ActionLogEntry` has no self-contained authenticated principal. Runtime attribution is structural through the owning in-memory per-agent log/tree; `ActionOrigin` records causality, not authorization. Durable `ToolResult` rows carry no agent principal, and resume does not reconstruct a complete durable action-log tree. | `crates/norn/src/session/action_log.rs:50-77`; `session/action_log_scope.rs:1`; `session/action_log_tree.rs:1-37`; `session/events.rs:247-276` |
| Workspace | No `WorkspaceRef` exists. Current authority is path-based: canonical launch directory, mutable per-agent working directory, optional confinement root, and a persisted working-directory string. | `crates/norn/src/agent/assembly.rs:59`; `tool/context.rs:49,185-195`; `runtime_init/extensions.rs:23`; `session/persistence/types.rs:438-439` |
| Norn task | `TaskEntry.id` is caller-supplied or UUIDv4 and is interpreted within a `TaskStore`; durable disk scope also includes a validated task-group slug. Dependencies and parents use bare task strings, while `assigned_agent` is an unauthenticated registry-path string. These are task coordination and attribution data, not Aion activity identity or an action principal. | `crates/norn/src/tools/task/types.rs:30-52,127-149`; `tools/task/disk.rs:36-60` |
| Norn rule | `rules::RuleId(String)` names a configured trigger rule. It identifies the rule definition, not one firing, diagnostic finding, authorization grant, or Chiron `RuleId`. | `crates/norn/src/rules/types.rs:8-41` |
| Durable schedule | `ScheduleRecord.id: Uuid` remains stable across `schedule.*` lifecycle events and resume reconstruction; `owning_agent_id` names the runtime agent to wake. The schedule ID is durable session content, not an event, agent, action, or producer identity. | `crates/norn/src/schedule/entry.rs:397-425`; `schedule/store.rs:52-76` |
| Background process and watch | Tool-visible `pN` process and `wN` watch labels are monotonic only within one `ProcessManager` and its `WatchRegistry`, then reset. The manager's separate UUID `run_id` scopes spool storage. Labels in completion/watch injections are local handles, not OS process IDs, producer identities, or durable stream references. | `crates/norn/src/process/manager.rs:79-101`; `process/watch.rs:42-74,171-211` |
| Follow-up turn | `CurrentTurnId(String)` is a runtime value used to expire turn-scoped follow-ups. The inspected Norn surface exposes no durable allocator or cross-session meaning for it. It is not an `EventId`, provider response, session run, or execution epoch. | `crates/norn/src/tools/follow_up/dispatch.rs:46-48`; `tools/follow_up/expiry.rs:63-150` |
| Private file | `PrivateFileIdentity { device, inode }` detects replacement when a private file is lazily reopened. It is an OS-local ABA guard, not a portable artifact, workspace, or storage reference. | `crates/norn/src/util/private_file_identity.rs:1-29` |
| Shell-hook aliases | `HookInput` serializes session, agent, subagent, and provider tool-call IDs as plain strings for a shell boundary. These fields preserve their originating domains and do not become a new hook-owned authority. | `crates/norn/src/integration/hooks/input.rs:67-95,126-129` |
| Other generations | Session-generation UUIDs, tool-store replacement generations (`u64`), audio attempt numbers, and provider-assigned thread anchors belong to distinct authorities and state machines. None is a general producer or execution epoch. | `crates/norn/src/tool/generation.rs:24,333`; `session/response_audio.rs:136-159`; `loop/conversation_state.rs:9` |

### 3.2 Aion

| Value | Native authority and scope | Evidence |
|---|---|---|
| Workflow | `WorkflowId(Uuid)` identifies a logical workflow. | `crates/aion-core/src/ids.rs:8-35` |
| Workflow run | `RunId(Uuid)` identifies one workflow run and is carried by workflow events. It is distinct from the logical workflow. | `crates/aion-core/src/ids.rs:162-190`; `aion-core/src/event.rs:105-130` |
| Workflow, activity, and cluster sequence | `EventEnvelope.seq` is monotonic within one workflow history. `ActivityRecord.store_seq` is server-allocated within one `(workflow, activity, attempt)` observability stream. `ClusterEventMeta.cluster_seq` is process-local to one deployment publisher, supports reconnect gap detection, and has no epoch. None defines stack-wide event or delivery order. | `crates/aion-core/src/event.rs:13-25`; `aion-store/src/observability.rs:36-96`; `aion-core/src/cluster_event.rs:55-67`; `aion-server/src/cluster_publisher.rs:44-74` |
| Activity | `ActivityId(u64)` is derived from the activity's schedule-sequence position within workflow history. | `crates/aion-core/src/ids.rs:67-89` |
| Timer and schedule | `TimerId` is either an author-assigned name or an engine-assigned workflow-history sequence position. `ScheduleId(Uuid)` identifies a persisted schedule resource. Neither is an activity, run, Norn schedule, or generic event identity. | `crates/aion-core/src/ids.rs:99-160`; `aion-core/src/schedule.rs:12-45` |
| Attempt and scoped owner keys | Attempt is a one-based integer. `ActivityStreamKey`, server `AttemptKey`, and worker `SessionKey` all contain `(WorkflowId, ActivityId, attempt)` but respectively own a durable observability stream, server intervention index, and live worker intervention session. `ActivityExecutionKey` omits attempt for heartbeat bookkeeping, while some task paths additionally carry `RunId`. Equal tuples do not erase these owner scopes. | `crates/aion-store/src/observability.rs:36-70`; `aion-server/src/worker/intervention.rs:42-73`; `aion-worker/src/runtime/intervention.rs:39-64`; `aion-worker/src/protocol/heartbeat.rs:102-119`; `aion-worker/src/protocol/task.rs:10-31` |
| Agent | `ActivityEvent` carries `agent_id: Uuid`. No cross-repository `AgentRef` contract is established by that field. | `crates/aion-core/src/activity_event.rs:181-221` |
| Connected worker | `WorkerId(u64)` is assigned by one Aion server process to one connected worker stream and returned on the wire for log correlation. It is not the worker's process, node, durable agent, or cross-restart identity. | `crates/aion-server/src/worker/registry.rs:131-175` |
| Authenticated caller | `CallerIdentity` is adapter-supplied authenticated subject and namespace/deploy grant state. It is an Aion request-authority principal, not a Norn action principal, workflow actor, or signing key. | `crates/aion-server/src/namespace/resolver.rs:67-105` |
| Event-store write capability | `WriteToken` is a private-field capability required by the recorder append path to uphold Aion's single-writer invariant. It has no serializable identifier or stable lifecycle and must not be logged or projected as a producer ID. | `crates/aion-store/src/store.rs:11-36` |
| Package artifact | `PackageVersion(String)` is intended to carry the lowercase 64-character content hash of a loaded workflow package, but its public constructor performs no validation. Aion also records the reserved non-hash value `aion:schedule-coordinator` for its virtual coordinator history. Consumers must therefore treat it as an Aion package-version label and validate the loaded-package case at the resolving boundary; it is not a generic artifact or schema digest. | `crates/aion-core/src/ids.rs:38-65`; `crates/aion/src/engine/api_schedule.rs:340-351` |
| Outbox dispatch | The outbox uses a `workflow_id:ordinal` dispatch key plus `claimed_at`; it has no general claim, lease, or producer token. | `crates/aion-store/src/outbox.rs:151-208,289-299` |
| Routing node and epoch | `NodeRef` combines a configured distribution node string, optional gRPC address, and believed ownership epoch. The static directory currently reports epoch `0` because it has no epoch source. The name/address are routing data and zero cannot be promoted into fencing authority. | `crates/aion-core/src/cluster_event.rs:350-361`; `aion-server/src/routing/directory.rs:20-64,134-161` |
| Logical supervisors | `EngineSupervisorId` is the unit type for the single engine root; `TypeSupervisorId` wraps workflow type. They identify logical supervision-tree nodes, not process incarnations or the proposed cross-stack `SupervisorInstanceId`. | `crates/aion/src/supervision/tree.rs:8-40` |
| Client start idempotency | Private `StartFingerprint` binds namespace, workflow type, exact input content, and caller idempotency key to one client-side start operation. It is duplicate-request comparison state, not workflow/run identity or a signing fingerprint. | `crates/aion-client/src/ops.rs:538-560` |
| AWL revision and deployment | `Revision.content_hash` is lowercase SHA-256 of exact authoring source. `DeploymentRecord.deployment_id` is a separate string alongside content/package/workflow/run fields. An ordinary loaded-package `PackageVersion` may have the same 64-hex shape, but the unconstrained package-version wrapper and its virtual sentinel make shape-based substitution even less valid. | `crates/aion-server/src/awl/revisions.rs:13-29,43-63` |
| AWL diagnostic | `awl::Diagnostic` carries class, message, line, and column but no diagnostic or run ID. It cannot supply Chiron's missing `DiagnosticRef`. | `crates/aion-server/src/awl/handlers.rs:21-27` |
| Norn run correlation | `AgentRunSpec` carries workflow, activity, and attempt, but omits `RunId`, workspace identity, and Norn session identity. The Norn adapter preserves that triple; JSON-RPC IDs are connection-local request correlation. | `crates/aion-integrations/src/spec.rs:5-27`; `aion-integration-norn/src/translate.rs:34-47`; `aion-integration-norn/src/session.rs:33-95` |

### 3.3 Yggdrasil, Chiron, Tharsis, and Prospekt

| Domain value | Native authority and scope | Evidence |
|---|---|---|
| Yggdrasil repository | `RepoId(PathBuf)` derives from the canonical Git common directory. It is a local path identity and is ambiguous across relocation, copy, mount alias, and machines. | `crates/libyggd/src/events/repo_id.rs:13-35` |
| Yggdrasil Git snapshot | `ObjectId([u8; 20])` uses canonical 40-character SHA-1 text. It identifies a Git object, not a workspace, logical change, or operation. | `crates/libyggd/src/git/repo.rs:17-109` |
| Yggdrasil logical branch | `BranchNode.id: Uuid` remains stable across rename and restack. It is a logical branch-node identity, not the commit or workspace identity. | `crates/libyggd/src/tree/node.rs:12-57` |
| Yggdrasil module | `ModuleId(String)` is a workspace dependency-graph identifier derived from a crate name, package name, or explicit ID. It is scoped to that graph/workspace and is not a Frame component, source snapshot, or runtime module generation. | `crates/libyggd/src/graph/mod.rs:24-55` |
| Yggdrasil workspace | A workspace is represented by path, branch name, and isolation strategy. No stable `WorkspaceId` is implemented. | `crates/libyggd/src/isolation/mod.rs:31-72` |
| Yggdrasil change and operation | `StructuralChange` has no `ChangeId`. File-operation metadata is stored against a commit hash and has no durable operation ID. Its optional `actor` is free-form attribution that may contain an agent session ID or username, not authenticated authority. Separately, the web server mints UUIDv4 strings for bounded in-memory operation history and background handles; those values die with the server. The web UI's `OperationId` union names command kinds, not operation instances. | `crates/libyggd/src/ast/diff.rs:18-41`; `tracking/operation.rs:37-61`; `tracking/log.rs:67-84`; `crates/ygg-web/src/history.rs:1-14,28-74,105-122`; `background_ops.rs:54-75`; `apps/ygg-web/src/components/OperationsSidebar.tsx:23-40` |
| Yggdrasil issue link | `IssueRef` caches a GitHub issue number, title, and state; the remote remains authoritative. The number is not globally meaningful without its remote/repository and is not a source change or branch identity. | `crates/libyggd/src/issues/types.rs:1-29` |
| Chiron diagnostic | `DiagnosticEvent` has no `DiagnosticId`; `source_tool`, optional tool `code`, and `doctoroxide::RuleId` identify producer/rule kinds rather than one finding. Persisted batches are keyed by `commit_id: String`, and one request/response is carried per connection without a correlation ID. | `crates/diagnostics/src/event.rs:68-104`; `crates/doctoroxide/src/model/finding.rs:21-52`; `crates/diagnostics/src/persistence/types.rs:15-45,64-95`; `server/protocol.rs:47-99` |
| Chiron daemon | The socket name is a deterministic hash of the canonical workspace root, while an OS file lock owns the live daemon instance. The socket hash is discovery data, not a portable workspace or supervisor ID. | `crates/diagnostics/src/server/socket.rs:3-21,86-145` |
| Chiron LSP request and logical server | LSP JSON-RPC `RequestId` is integer, string, or null and is connection/request correlation only. The private `ServerKey = (config_name, root_path)` selects a logical server registry entry; it is neither a diagnostic ID nor a live server/supervisor incarnation. | `crates/lsp/src/transport/message.rs:69-116`; `lsp/src/server/registry.rs:27-40` |
| Tharsis model | `LayerId`, `SnapshotId`, `BranchId`, `ActorId`, and `MountId` are string wrappers; `BracketId` is `u64`; `RootHash` is 32 bytes. Core layer/snapshot operations remain `todo!`, so these are provisional model types, not an accepted replacement for current Yggdrasil authorities. | `crates/tharsis-core/src/layer.rs:9-63`; `snapshot.rs:9-28`; `store.rs:17-30`; `attribution.rs:24-39`; `mount.rs:9-27` |
| Tharsis activity link | `ActivityRef` stores workflow and activity as strings and omits run and attempt. It is a proposed integration correlator, not the typed Aion authority. | `crates/tharsis-core/src/snapshot.rs:12-28` |
| Tharsis build-lock correlation | `TicketId(u64)` and `BuilderId(String)` identify the holder and FIFO waiters of one in-memory global build-lock state machine. They are provisional lock correlation, not an authenticated actor, durable lease, Aion worker, or general producer identity. | `crates/tharsis-server/src/build_lock.rs:18-81` |
| Tharsis NFS file ID | `FileAttrs.fileid: u64` exists, but `FileIdAllocator` still has two candidate strategies and its allocator is `todo!` pending S2. No stable path/content identity policy has been selected. | `crates/tharsis-nfs/src/attrs.rs:1-40` |
| Urd | The pinned Urd subtree contains design documents but no Rust or TypeScript runtime identity allocator. | `urd/docs/ARCHITECTURE.md` in Tharsis `acac7baec611` |
| Prospekt document | Document IDs are model-kind-local strings minted from a configured prefix and the highest existing numeric suffix. Relations use those strings within the model layout. They are document references, not stack actor, event, or artifact IDs. | `crates/prospekt-core/src/model.rs:12-49`; `document.rs:414-450` |
| Prospekt workspace and schema | The workspace is a configurable filesystem root and display label. Schemas are JSON values and paths under model/kind directories; no content-addressed `SchemaId` or portable workspace identity exists. | `crates/prospekt-core/src/workspace.rs:1-57`; `model.rs:27-30` |

### 3.4 Frame and Beamr

| Value | Native authority and scope | Evidence |
|---|---|---|
| Frame component | `ComponentId([u8; 32])` is derived with BLAKE3 domain `frame/component-id/v1` over publisher namespace and component name. It is stable independently of version and bytecode. | `crates/frame-capability/src/lib.rs:14-28`; `frame-core/src/component.rs:10-20` |
| Frame entity and cross-component link | `EntityId([u8; 32])` is the BLAKE3 digest of one entity's exact bytes. `CrossComponentRef { target, entity }` combines it with a `ComponentId` in a versioned fixed-width encoding. Entity content identity and component namespace identity remain distinct. | `crates/frame-state/src/types.rs:6-73` |
| Frame service capability | `ServiceCapability.id: String` names a service-wiring contract. It is explicitly separate from host authority and must not substitute for component identity. | `crates/frame-core/src/component.rs:21-43` |
| Frame capability grant | A typed `Capability` names exact path, host, or component-read scope. An active `Grant` keys authority to the grantee `ComponentId` and request; it has no independent grant ID or generation. `GrantProvenance.granted_by: String` is host-supplied audit attribution, not an authenticated principal. | `crates/frame-capability/src/lib.rs:108-175,219-265` |
| Frame component schema | `ComponentSchema` currently contains merge policy only; it has no `SchemaId`, digest, or version. It cannot substitute for Liminal schema identities or the future shared schema reference. | `crates/frame-state/src/types.rs:131-146` |
| Frame storage incarnation and archive generation | Durable `MetaRecord.incarnation: u64` starts at zero, increments when an archived component namespace is reinstalled, fences every `ComponentStoreHandle` operation against stale storage authority, and names archived namespace generations. This is real per-component storage lifecycle authority, not a general loaded-runtime `ComponentGenerationId`. | `crates/frame-state/src/types.rs:159-170,224-244`; `frame-state/src/handle.rs:39-70`; `frame-state/src/store.rs:55-106,139-203`; `frame-state/src/archive.rs:38-133` |
| Frame loaded runtime generation | No separate accepted `ComponentGenerationId` for loaded component processes was found. Component metadata's version string, registry membership, and the storage incarnation above do not collectively define that runtime generation. | `crates/frame-core/src/component.rs:10-31`; `frame-core/src/registry.rs:41-48,132-163` |
| Beamr namespace | `NamespaceId(u64)` scopes a module registry within one scheduler. Default namespace zero is not a component or tenant identity. | `crates/beamr/src/namespace.rs:1-22` |
| Beamr module generation | Each module-name entry receives a registry-local monotonically increasing `u64` generation. Code pointers use it to distinguish retained module versions. It is not Frame's proposed component generation. | `crates/beamr/src/module.rs:106-143,221-300` |
| Beamr readiness token | `ReadinessToken { slot, generation }` protects kernel-readiness registrations against stale slot reuse. Its private per-slot `Generation` is neither a module generation nor a cross-domain epoch. | `crates/beamr/src/scheduler/readiness/types.rs:40-61` |
| Beamr connection and peer incarnation | `ConnectionGeneration(u64)` increases per peer for the lifetime of one local connection manager and distinguishes link sessions. `NodeUp.peer_creation` instead comes from the authenticated handshake and changes when the remote VM restarts. Link generation resets with the manager; peer creation is not a local link counter or general producer epoch. | `crates/beamr/src/distribution/connection_events.rs:205-252,398-411` |
| Beamr event subscriber | `SubscriberId(u64)` is an opaque handle into one process-local connection-event hub. It identifies neither the subscriber principal nor a delivery stream outside that hub. | `crates/beamr/src/distribution/connection_events.rs:323-389` |
| Beamr service instance | `ServiceInstanceId(u64)` is process-unique for one constructed ancillary service; clones of a shared service retain it and zero is the disabled sentinel. It is useful for process-local deduplication, not a durable supervisor or component-generation identity. | `crates/beamr/src/scheduler/service.rs:14-54` |
| Beamr actor | `ActorRef` and cooperative `CoopActorRef` carry scheduler-local `u64` process IDs plus typed sender handles. The PID is meaningful only with its scheduler and actor API; it is not a Norn agent, OS process, or cross-node participant reference. | `crates/beamr/src/native/actor.rs:331-360`; `native/actor_cooperative.rs:79-106` |
| Beamr readiness consumer | `ServiceConsumerId(u64)` is minted process-wide to route shared readiness registrations back to one scheduler state. It is a private routing token, not an application consumer, participant, or producer principal. | `crates/beamr/src/scheduler/readiness/core.rs:18-34` |
| Beamr runtime handles | `TimerRef`, monitor `Reference`, and `EtsTableId` are monotonically allocated handles within their owning timer wheel, monitor set, or ETS registry. Bare integers do not identify an event, agent, durable resource, or producer outside those owners. | `crates/beamr/src/timer.rs:15-33`; `supervision/monitor.rs:19-28`; `ets/table.rs:7-8` |
| Beamr term references | A boxed local `Reference` carries one runtime `u64`; an `ExternalReference` carries node atom plus `u64`. They are BEAM-term reference values, not Frame entity/component references or a stack action/resource ID. | `crates/beamr/src/term/boxed/accessors.rs:364-384,422-450` |
| Beamr replay delivery | `ReplayRecorder` assigns `RecordedMessageDelivery.order` from a counter local to one recorder/log; records also carry process-local PIDs and logical clocks. No replay-log ID or epoch qualifies that order outside its owning log. | `crates/beamr/src/replay/recorder.rs:11-49`; `replay/driver.rs:162-178` |
| Beamr capability audit | `CapabilityAuditEvent.pid` attributes a check to one process-local actor PID and records capability/verdict state. It is runtime attribution, not an authenticated application action principal. | `crates/beamr/src/capability/audit.rs:22-40` |

### 3.5 Liminal

| Value | Native authority and scope | Evidence |
|---|---|---|
| Participant conversation | The participant protocol defines `ConversationId`, `ParticipantId`, and `ParticipantIndex` as `u64`, with event order local to the conversation aggregate. They are protocol-scoped, not global actors. | `crates/liminal-protocol/src/wire/primitives.rs:1-8`; `lifecycle/conversation.rs:15-77` |
| Runtime participant process | `conversation::ParticipantPid(u64)` wraps one Beamr process participating in a conversation. It is scheduler/runtime identity, distinct from the participant protocol's same-shaped `ParticipantId(u64)`. | `crates/liminal/src/conversation/types.rs:99-126` |
| Participant generation and binding | `Generation(NonZeroU64)` belongs to participant capability lifecycle. `BindingEpoch` combines a server/connection incarnation with a capability generation. It is not a session-run, producer, or delivery-stream epoch. | `crates/liminal-protocol/src/wire/primitives.rs:10-32,63-104` |
| Participant credential lifecycle | Fixed-width enrollment, attach, detach, and leave attempt tokens have single protocol purposes; `AttachSecret` is participant credential material. Live identity binds participant/conversation, generation, secret, cursor, enrollment fingerprint, and latest terminal. Retired identity removes the attach secret but retains generation, enrollment/leave fingerprints, attempt token, verifier, and committed result. These credentials and fingerprints are not signing identities or generic participant IDs. | `crates/liminal-protocol/src/wire/primitives.rs:106-158`; `lifecycle/membership.rs:12-76,353-477` |
| Delivery order | `DeliverySeq`, `TransactionOrder`, and `ObserverEpoch` are raw `u64` aliases with their named protocol scopes. None defines a stack-wide total order. | `crates/liminal-protocol/src/wire/primitives.rs:34-41` |
| Participant cursor fact | `CursorProgressKey { participant_index, boundary }` keys durable acknowledgement progress for one participant. It is cumulative cursor state, not a stream, event, or delivery epoch. | `crates/liminal-protocol/src/lifecycle/cursor_facts.rs:11-43` |
| Protocol stream | `protocol::StreamId(u32)` is unique only within one connection's stream table. Stream zero is connection control. | `crates/liminal/src/protocol/multiplex.rs:9-48` |
| Core publisher and routing parties | `PublisherId(String)` is envelope attribution and defaults to `anonymous`; routing `ConsumerId(String)` and `SubscriberId(String)` identify table/function participants. The types do not themselves authenticate a principal or define a stack-wide producer/participant reference. | `crates/liminal/src/envelope.rs:6-54`; `routing/function/execute.rs:18-48`; `routing/table.rs:6-31` |
| SDK connection and subscription | `PoolConnectionId(usize)` identifies a managed connection slot, while `SubscriptionId(u64)` is application-visible and survives reconnect through recovery bookkeeping. Both are scoped to the SDK/pool and cannot substitute for protocol `StreamId`, participant generation, or a delivery-stream epoch. | `crates/liminal-sdk/src/connection/pool.rs:53-85`; `connection/recovery.rs:9-57` |
| Participant transport session | `ParticipantSession` stores only participant capability state negotiated on one authenticated connection. Despite its name, it is neither durable participant identity nor a Norn-style `SessionRef`. | `crates/liminal-server/src/server/participant/transport.rs:9-20` |
| Protocol schema | `protocol::SchemaId([u8; 32])` is a content-schema hash carried with opaque payload bytes. | `crates/liminal/src/protocol/envelope.rs:6-41` |
| Channel schema collision | `channel::SchemaId(Uuid)` is a generated JSON-schema version identifier. It has the same Rust name but different bytes and semantics from `protocol::SchemaId`. | `crates/liminal/src/channel/schema.rs:8-45` |
| Conversation collision | `liminal-sdk::ConversationId(String)` is application-visible correlation, while participant wire `ConversationId` is `u64`. Neither may be decoded as the other without an explicit adapter. | `crates/liminal-sdk/src/conversation.rs:10-30`; `liminal-protocol/src/wire/primitives.rs:1-2` |
| Message collision | Core/protocol message IDs and envelopes exist in more than one module. The UUID `causal::MessageId` is local causal correlation, not an `EventRecordV1` identity. | `crates/liminal/src/causal.rs:7-43`; `protocol/causal.rs:14`; `durability/channel.rs:17`; `protocol/envelope.rs:33-41` |
| Durable-channel partition | A caller-provided `PartitionKey` function maps envelopes to numeric partitions inside one durable channel; the channel itself is a string. Neither the closure nor its numeric output is a portable stream or participant identity. | `crates/liminal/src/durability/channel/storage.rs:7-43` |

### 3.6 Haematite and Lys

| Value | Native authority and scope | Evidence |
|---|---|---|
| Haematite hash | `Hash([u8; 32])` represents structural tree roots and is also used for a value-content hash, with source documentation distinguishing those uses. It is a storage address, not a semantic event or artifact reference. | `crates/haematite/src/tree/node.rs:19-44` |
| Haematite event stream | `Event.seq` is zero-based within one caller-supplied byte-string stream key. The key is not a typed domain identity and the sequence has no meaning outside that stream. | `crates/haematite/src/api/types.rs:12-22,36-62` |
| Haematite branch | `BranchRefRecord` has no `BranchId` newtype. Durable branch identity currently uses name plus immutable creation timestamp; commit sequence is branch-local and shard heads are hashes. | `crates/haematite/src/branch/refrecord.rs:6-41` |
| Haematite carrier connection | `EpisodeId`, `AttemptGeneration`, `ConnectionId`, `Incarnation`, and `ConnectionKey` distinguish carrier episodes, attempts, accepted connections, and successive peer connections. Their constructors consume owner-supplied material; their authority remains carrier-local rather than a universal transport or delivery epoch. | `crates/haematite/src/carrier/types.rs:3-74` |
| Haematite replication node and write | `SyncNodeId(String)` identifies a distributed sync node, distinct from shard IDs. `WriteId { origin, origin_creation, counter }` adds the originating node's restart creation to prevent a stale acknowledgement satisfying a reused counter. It is write correlation, not a signing principal or generic event ID. | `crates/haematite/src/sync_codec/ids.rs:1-54`; `sync_codec/message/write.rs:11-33` |
| Haematite ownership epoch and commit order | `Ballot { counter, node }` is the globally unique, durable, monotonic per-shard ownership epoch. `Stamp { epoch, seq }` adds per-owner write order. These are real storage fencing/order authorities and must not be replaced with Aion routing epoch zero, a Norn run, or a generic producer counter. | `crates/haematite/src/sync_codec/ballot.rs:19-80` |
| Haematite expiry arm | A private shard-actor `Generation(u64)` rejects stale expiry-timer arms. It is an internal storage-lifecycle fence, not a shard, connection, or session generation. | `crates/haematite/src/shard/actor/expiry_index.rs:32-57` |
| Lys signer | `Ed25519Identity` exposes a 32-byte verifying key. The key proves possession for signatures but does not by itself establish an application principal, role, or grant. | `crates/lys-core/src/keys/identity.rs:43-79` |
| Lys attestation | `Attestation` covers payload SHA-256, signer public key, signature, and signing timestamp. The consuming domain supplies semantic meaning, authority, freshness, and revocation policy. | `crates/lys-core/src/attestation/artifact.rs:30-68` |
| Lys signed-note verifier | `NoteVerifierKey` binds a validated name, a four-byte key ID derived from name plus key, and the 32-byte public key. The short key ID is signed-note lookup material, not a globally unique signer or principal ID. | `crates/lys-core/src/checkpoint/verifier_key.rs:48-99` |
| Lys certificate subject | `IssuedCertificate` carries a fresh subject keypair, SHA-256 fingerprint of exact DER, expiry, and issuer public key. The certificate authority key, subject key, DER fingerprint, and subject string have distinct credential roles; none alone grants application authority. | `crates/lys-core/src/ca/authority.rs:34-71`; `ca/certificate.rs:19-54` |
| Lys transparency root, checkpoint, and proof artifacts | `merkle::RootHash` binds a 32-byte SHA-256 tree hash to its leaf count. `CheckpointBody` binds origin, tree size, and root bytes, and the origin doubles as the signed-note key name. Raw inclusion/consistency proofs are ordered hash paths; self-contained v1 artifacts additionally bind format, tree size(s), leaf index where applicable, and verbatim signed checkpoint(s). A bare hash, path, or origin string is not the complete proof/checkpoint authority. | `crates/lys-core/src/merkle/proof.rs:30-79,94-179`; `checkpoint/body.rs:27-67`; `tlog/artifact.rs:19-67` |
| Lys sealed envelope | The X25519 public key in `SealedEnvelope` is freshly generated per envelope. It is encryption material, not actor identity. | `crates/lys-core/src/seal/sealed_envelope.rs:1-18,68-79` |

## 4. TypeScript boundary inventory

TypeScript surfaces frequently erase Rust newtype distinctions. They remain
wire or presentation aliases and never become a second native authority.

| Surface | TypeScript representation | Consequence | Evidence |
|---|---|---|---|
| Aion generated console types | `WorkflowId`, `RunId`, `PackageVersion`, and `ScheduleId` are strings; `ActivityId` and `cluster_seq` are numbers; `TimerId` is a named/anonymous union | Generated aliases mirror serialized Rust values but provide no runtime domain separation. JavaScript number representation is also a transport constraint, not sequence authority. | `apps/aion-ops-console/src/types/generated/index.ts:1-16,669` |
| Aion client protobuf facade | `ProtoWorkflowId` and `ProtoRunId` are objects containing `uuid: string` | These preserve field shape but not Rust newtype authority; optional run IDs must not be silently dropped during correlation. | `sdks/typescript/aion-client/src/client.ts:117-147` |
| Aion worker SDK | `WorkerIdentity`, `WorkflowId`, and `ActivityIdKey` are strings; activity IDs are decimalized from wire sequence positions | `ActivityIdKey` is a transport key, not a new activity allocator. The SDK task still omits `RunId`, workspace, and Norn session identity. | `sdks/typescript/aion-worker/src/session.ts:14,80-95,519-536` |
| Yggdrasil web branch and operations | Branch-node UUIDs, process-local operation UUIDs, and pipeline IDs are plain strings. `OperationId` is separately a string-literal union of UI command kinds. | These aliases mirror distinct Rust/server concepts. Neither the in-memory operation UUID nor a command-kind label is the missing durable file-operation reference. | `apps/ygg-web/src/types/branch.ts:1-17`; `lib/api.ts:17-22,267-290`; `components/OperationsSidebar.tsx:23-40` |
| Liminal WASM SDK | `connectionId` and `streamId` are numbers; `schemaId` is `Uint8Array` | The TS surface does not brand connection, stream, or schema domains. Callers must retain the protocol scope and exact 32-byte schema rule. | `sdks/liminal-ts/src/wasm.ts:3-28,59-64` |
| Frame example envelope and Chiron | Frame's `FeedEnvelope` is an example-local encoded envelope; no authoritative Chiron TypeScript identity type is selected | A demonstration carrier cannot become the shared event contract, and future diagnostic clients must consume explicit bindings rather than infer authority from strings or paths. | Frame `examples/frame-demo/src/protocol.ts`; Chiron source inventory |

## 5. Collision and substitution register

| Collision | Required discipline |
|---|---|
| Norn `data_dir`, index ID/generation/`rel_path`, tool-context `SessionId`, resolved run `session_id`, agent UUID, and persistent-child session text | Preserve the current rooted index authority and distinguish durable from run-scoped strings. No current typed store namespace exists, and reuse in the persistent-child path does not establish universal equality. |
| Norn `EventId`, active-input UUID, inter-agent message UUID, schedule UUID, storage-transaction UUID, router sequence, provider `item_id`/`call_id`/`response_id`/`approval_request_id`/`container_id`, and stream indices/sequence | Qualify by native domain and owning session, live input, schedule, filesystem operation, recipient, response, or provider resource. Acknowledgement, lifecycle/recovery correlation, timeline order, output position, and provider-frame order are different relations; never use one as a fallback for another. |
| Norn task ID/group, dependency/parent strings, assigned-agent label, schedule owner, and Aion task/activity values | Preserve store/group/session scope and treat assignment as unauthenticated attribution. Do not import Norn task or schedule values into Aion's typed orchestration spine or use them as action authority. |
| Norn account alias, opaque storage UUID, remote-account fingerprint, credential revision, refresh lineage/recovery marker, provider profile, MCP project/server/fingerprint, and qualified tool name | Preserve selection, storage, remote identity, byte revision, no-replay operation, deployment, project approval, and registry-name domains. Equal-looking strings, UUIDs, or hashes are not interchangeable authority. |
| Norn session/tool generations, MCP client/request/tool-list revisions, private file device/inode, process run, `pN`/`wN` handles, audio attempt, and current turn | Treat each allocator, counter, or OS guard as a distinct local lifecycle. None is a general producer, artifact, or execution epoch. |
| Aion workflow/run/activity/attempt keys, three sequence domains, timer/schedule, agent/worker/caller, routing node/epoch, logical supervisors, start fingerprint, AWL content/package/deployment, and write capability | Preserve the orchestration spine and each owner scope. Equal sequence integers or 64-hex hashes do not merge histories/artifacts; workers, callers, logical supervisors, idempotency comparisons, and append capabilities do not fill missing durable authority fields. |
| Yggdrasil repository/workspace path, module, branch UUID, commit OID, issue number, process-local operation UUID, UI operation kind, and free-form operation actor | Keep repository, workspace, dependency module, logical branch, source snapshot, remote issue, operation instance, and attribution separate. Missing durable IDs and authenticated principals remain missing. |
| Frame `ComponentId`, `EntityId`, cross-component link, schema-without-ID, service/capability scope, grant provenance, component version, storage incarnation, and archive generation | Preserve namespace, content, schema absence, capability, attribution, release, and storage-lifecycle domains. `granted_by` is not a principal, and the storage incarnation is not automatically the proposed loaded-runtime `ComponentGenerationId`. |
| Frame identities, Beamr actor PID, readiness token/consumer, namespace/module/link generation/peer creation, subscriber/service IDs, timer/monitor/ETS handles, term references, and replay order | Beamr values are scheduler, actor API, registry, slot, timer/monitor/table, connection-manager, remote-VM, event-hub, term, process, or recorder-log scoped. None substitutes for Frame identity, a Norn `AgentRef`, or a durable stack supervisor/producer/delivery epoch. |
| Liminal protocol `ParticipantId`, Beamr `ParticipantPid`, credentials/fingerprints, misleading transport `ParticipantSession`, publisher/consumer/subscriber strings, SDK conversation/connection/subscription, message UUID, cursor/partition keys, stream, sequence, and binding epoch | Qualify by protocol layer and connection/conversation/pool/channel/scheduler owner. Same-shaped integers and session-like names do not merge identity; secrets and attribution are not authenticated application principals. |
| Liminal protocol and channel `SchemaId` | Preserve module/domain qualification. One is a 32-byte content hash; the other is a UUID schema-version token. |
| Haematite tree/value hashes, stream key/sequence, branch name/creation timestamp, carrier episode/attempt/connection/incarnation, sync node/write ID, ownership ballot/stamp, and expiry generation | Storage and transport adapters must preserve domain tags, owner scope, restart creation, and the real per-shard fence. Raw addresses and local counters do not become semantic events, participants, or delivery epochs. |
| Chiron `RuleId`, diagnostic code/source tool, commit-keyed batch, and future `DiagnosticRef` | Rule and producer labels classify findings but do not identify one finding or run. A future diagnostic identity must not be synthesized from them without owner-approved canonicalization. |
| Norn `RuleId`, Chiron `RuleId`, and rule/diagnostic firing | The two string types name definitions in different engines. Neither names one evaluation or diagnostic occurrence, and their equal text never establishes cross-engine identity. |
| Chiron LSP request ID, logical `(config, root)` server key, socket hash, and live server process | Request correlation, registry selection, discovery, and process incarnation are distinct. None is a portable `DiagnosticRef` or `SupervisorInstanceId`. |
| Lys identity/CA/subject/note public keys, note key ID, certificate/payload/tree hashes, leaf/index/count, proof path/format, checkpoint origin/text, and ephemeral encryption key | Bind verified keys, hashes, proofs, and checkpoints to a domain principal, credential/checkpoint chain, exact tree coordinates, and purpose. Byte width, origin text, or a short lookup ID does not establish complete identity or authority. |
| Tharsis provisional IDs, build ticket/builder correlation, unresolved NFS file ID, and established Yggdrasil/Aion authorities | Do not replace the operating execution chain with skeletal model, in-memory lock, or unselected allocator types. Adopt only after the owning seam, lifetime, authentication, and migration are accepted. |
| Prospekt document IDs and runtime record IDs | Prospekt IDs are model-kind-local authoring references, not runtime event, action, or actor identities. |

## 6. Implemented versus proposed target vocabulary

| Target concept | Current source status | NS0A consequence |
|---|---|---|
| Stable component identity | Implemented as Frame `ComponentId` | Retain Frame ownership. |
| `ComponentGenerationId` | No accepted loaded-runtime type is implemented. Frame state already has a durable per-component storage incarnation/archive generation. | Frame owner must decide whether the runtime allocator is distinct and define its publication fence. The existing storage incarnation and Beamr module generation may not be silently promoted. |
| `BrowserActivationId` | Not implemented | Browser/Frame owner decision remains open. |
| `SupervisorInstanceId` | Not implemented | Aion's logical supervisor IDs, Chiron logical server key/socket hash/OS lock, and Beamr's process-local `ServiceInstanceId` demonstrate narrower topology, registry selection, discovery, ownership, and deduplication mechanics but are not the Norn supervisor identity. |
| Durable `SessionRef` | Not implemented; current authority is an implicit `data_dir` plus exact index row. | Norn must define store qualification, generation binding, and relocation semantics before projection or remote resume. |
| Cross-session `AgentRef` | Not implemented | Norn registry UUID/path remain local. Source owner must define cardinality and lifetime. |
| `SessionRunEpoch` | Not implemented | The single durable execution authority and transfer protocol remain owner decisions. Norn session generation and Aion epoch zero are not substitutes. |
| `ProducerEpoch` | Not implemented | Provider attempts, process run IDs, Aion worker/routing values, Beamr connection generations/service instances, and Liminal publisher/binding values do not satisfy it. Haematite `Ballot` is a real per-shard ownership epoch, but not a generic Norn producer epoch. |
| `DeliveryStreamEpoch` | Not implemented | Aion workflow/activity/cluster sequences, Beamr recorder order, Liminal stream/subscription IDs, observer/binding epochs and delivery sequence, plus Haematite carrier connection identities, all have narrower owners and lifetimes. |
| Stable `WorkspaceRef` | Not implemented in current Yggdrasil, Norn, Chiron, or Prospekt surfaces | Yggdrasil source owner must define canonicalization, portability, and versioning. Tharsis types remain provisional. |
| Stable `SnapshotRef` | Git OIDs and provisional Tharsis `SnapshotId` exist | The source/workspace owner must decide whether a projection references Git object, Tharsis snapshot, or another native snapshot type. |
| Stable `ChangeRef` and operation-instance ref | Not implemented in Yggdrasil | Do not promote AST enum values, UI command kinds, or commit hashes into substitutes. |
| Stable `DiagnosticRef` and diagnostic-run ref | Not implemented in Chiron or Aion AWL diagnostics | Future fingerprints need canonicalization and versioning; rule IDs, tool codes, commit IDs, source positions, and message text are not finding identity. |
| Authenticated action principal | Aion has an authenticated request caller, but Norn action records have no durable authenticated principal; Norn task assignment, Yggdrasil operation actor, and Frame `granted_by` are attribution. Lys supplies cryptographic identities, not application grants by itself. | Define principal, invocation surface, delegation, grants, target epoch, and evidence before remote mutation. Do not import attribution, a caller object, or a raw key without an explicit authority adapter. |
| Stack schema and artifact references | Domain-specific values, including Frame `EntityId`/`CrossComponentRef`, exist; Frame `ComponentSchema` has no identity, and no shared contract is frozen. | NS0B must select domain tags, algorithms, canonical bytes, and owners after P4. Content hashes do not become generic artifacts by byte-width coincidence. |

## 7. Observed integration gaps

The current Aion-to-Norn execution adapter carries workflow, activity, and
attempt but loses `RunId` and carries neither workspace identity nor a durable
Norn session reference. There is therefore no typed durable correlation spine
across workflow run, workspace, Norn session incarnation, and agent execution.
That is an observed gap, not permission for Norn to invent the missing owners.

Yggdrasil design material proposes UUID-shaped Norn session and workspace
fields, but the current runtime does not implement those contracts and Norn's
durable session ID is not necessarily a UUID. Documentation assumptions do not
override native source types. The proposal is recorded at Yggdrasil
`411b3d47364e` in
`docs/design/aion-integration/briefs/AI-003.json:9-60`.

The eventual orchestration correlation candidate must at least preserve Aion's
`workflow_id`, `run_id`, `activity_id`, `attempt`, and `agent_id`, then link to
owner-approved Norn session/agent and workspace/source references. The exact
record, cardinality, and authority remain open in NS0A.

## 8. Decisions still required

This inventory establishes a revision-pinned source candidate; it does not
close exhaustive enumeration or select the shared contract. It leaves these
plan gates open:

1. A reproducible mechanical sweep must retain exact source selection, queries,
   exclusions, and a zero-unresolved disposition ledger.
2. Source owners must name the authoritative workspace, snapshot, change,
   diagnostic, session, and agent references.
3. Owners must assign allocators and lifetimes for component, browser,
   supervisor, session-run, producer, and delivery-stream generations.
4. Norn's authenticated mutation principal and authorization evidence must be
   defined.
5. Native-to-projection identity, cardinality, and immutability rules must be
   accepted.
6. `RelationRecordV1` identity and asserting-authority rules must be accepted.
7. NS0B must freeze canonical encoding and cross-language fixtures after P4.

Until those decisions are reviewed, integrations carry domain-qualified native
values and reject ambiguous substitutions rather than normalizing them into a
generic string, UUID, integer, or 32-byte hash.
