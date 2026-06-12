# Agent Coordination — Wave 3: Inter-Agent Messaging & Recursive Delegation

**Status:** APPROVED 2026-06-12 — all ten DECISIONs resolved by Tom **per their stated recommendations**: M1 `SiblingsAndParent` documented scope; M2 steer/update kinds with update-does-not-wake; M3 opt-in linger-await; M4 inbound capacity into `ChildPolicy` (32 as documented proposal); M5 delete `Mailbox`, fresh `MessageRouter`; R1 documented envelope `remaining_depth = 1`, `max_concurrent_children = 32` (deeper trees are explicit opt-in); R2 single `child_policy` object arg; R3 capacities into the builder envelope (256/32 as documented proposals); R4 **add `subtree_usage` to `SubagentLifecycle::Completed` — breaking schema change, coordinate via MERIDIAN-HANDOFF §8**; R5 **defer per-child `AgentLoopConfig` override — TRACKED DEFERRAL, see rollout order**. Implementation may begin; no decision below remains open.
**Scope:** design only. Implementation follows after current in-flight work (Wave 1/2) lands.
**Ground rules honored:** every knob is builder-configured (CLAUDE.md: NO ASSUMED DEFAULTS — all "default" values below are flagged proposals); `send_message` **replaces** `signal_agent` (NO BACKWARDS COMPATIBILITY); no invented rate caps.

All file:line citations are against the working tree as of 2026-06-12 (`main` @ `ac1a645` plus in-flight edits). Citations are `crates/norn/src/...` unless noted.

---

## Context

Norn today supports one layer of delegation: a root agent spawns/forks children, steers them via `signal_agent`, observes them via typed `SubagentLifecycle` events, and receives their results on a bounded child-result channel drained at loop boundaries. Two structural limits are designed-in and documented as known gaps:

1. **Messaging is parent→child only.** Delivery requires holding the recipient's `AgentHandle`; siblings cannot reach each other, children cannot reach the parent through the same surface, and no message leaves an audit record at send time.
2. **Delegation is depth-1.** `AgentRegistry::reserve` rejects grandchildren because the grandchild **result channel** does not exist — lifecycle events would broadcast, but results would have no delivery path.

Wave 3 removes both limits with one coherent child-policy surface. Feature 1 (messaging) is independently shippable; Feature 2 (recursion) builds on the same policy type.

---

# Feature 1 — Inter-Agent Messaging

## 1.1 Current state (verified)

### The inbound channel — the one delivery path that works

- `loop/inbound.rs:22-29` — `DeliveryMode::{Steer, FollowUp}`. Steer injects after the current tool batch, before the next provider request; FollowUp buffers until the model would otherwise stop, then injects and the loop continues.
- `loop/inbound.rs:32-43` — `ChannelMessage { author: String, content: String, delivery, timestamp }`. **The `author` field is sender-supplied free text** — attribution is not enforced by the harness (see "escaping precedent" below).
- `loop/inbound.rs:96-99` — bounded `tokio::sync::mpsc`; capacity is caller-supplied. Root agents get one only if the embedder sets `AgentBuilder::inbound_capacity` (`agent/builder_setters.rs:357`); spawned children always get one with **hardcoded** capacity `SPAWN_INBOUND_BUFFER = 32` (`tools/agent/spawn.rs:56,284`), forks likewise `FORK_INBOUND_BUFFER = 32` (`tools/agent/fork_pipeline.rs:55`).
- Drain sites in `loop/runner.rs` — the "step boundaries" the agreed design names already exist:
  - after a tool batch: `runner.rs:731-740` (`ToolsOnly` arm);
  - at every would-stop point: `runner.rs:581-603` (`SchemaValid`), `745-755` (`TextStopNoSchema`), `891-901` (`ToolsAndSchemaValid`) via `flush_inbound_messages` (`loop/helpers.rs:481-499`) — if anything was drained, the loop `continue`s instead of returning.
- Injection: `inject_inbound_messages` (`loop/helpers.rs:203-236`) sorts by timestamp, persists each message as a `SessionEvent::UserMessage` (so it is **already resume-safe**: resumed conversations replay it from the store), and pushes a user-role message formatted as `"[{label} from {author}]: {content}"` — **plain text, no escaping, no structural wrapper**.

### `signal_agent` — what gets replaced

- `tools/agent/coord/signal.rs:94-217`. Resolution is registry-ground-truth (path → terminal-by-path → tombstone → UUID, `tools/agent/infra.rs:85-119`). A finished recipient fails honestly with the recorded completion (`signal.rs:112-146`) — **note: the tombstone machinery this depends on lands with the Wave 1 batch** (`agent/registry.rs:104-125, 321-342`; uncommitted at drafting time), so the "[DEPENDS: Wave1-C tombstones]" item is satisfied once Wave 1 is committed.
- Delivery requires `ctx.get_extension::<AgentHandles>()` holding the recipient's handle (`signal.rs:152-191`); only the spawning parent ever holds it (`tools/agent/handle.rs:69-95`). Anyone else gets a structured delivery failure (`signal.rs:198-217`, the H15 rule: never enqueue where nothing drains). **Sibling messaging is therefore structurally impossible today, and child→parent messaging is too** (the child holds no handle for its parent). Wave 3 is not a permission relaxation — it requires a new routing surface.
- Attribution: `sender_label` (`tools/agent/coord/helpers.rs:13-19`) — registry path, else tombstone path, else bare UUID. This is exactly the attribution rule the agreed design keeps, minus the harness-enforced wrapper.
- No audit event is emitted on send. The only trace of a signal is the `UserMessage` injected into the *recipient's* store at delivery time.

### The orphaned `Mailbox`

`agent/mailbox.rs` implements a full per-recipient queue with monotonic sequence numbers and a race-free `wait_for_any` (`mailbox.rs:57-180`). **No agent loop drains it.** Its only production callers are the rhai integration (`integration/rhai/agent_ops.rs:43,66,267`) and runtime wiring that instantiates it into `AgentToolInfra` (`agent/assembly.rs:684`, `norn-cli/src/runtime/wiring.rs:110`). `signal_agent` explicitly refuses to use it (`signal.rs:193-197`). Under the no-zombie-code rule this type cannot survive Wave 3 unchanged: it is either promoted into the real router or deleted (DECISION M5).

### Escaping precedent — how tool results are protected today

`append_tool_result` (`loop/tool_dispatch.rs:242-292`) injects tool output as a `MessageRole::ToolResult` message whose content is `serde_json::to_string(output)` (`tool_dispatch.rs:279`) — JSON encoding is the structural barrier: tool output cannot escape its frame because it is *data inside an encoding*, not concatenated prose. Inbound messages have **no** equivalent today: `helpers.rs:215` concatenates raw `msg.content` (and raw, sender-supplied `msg.author`) into the user turn. A child can already emit `[Inbound from root]: ...` inside its content and it would render indistinguishably. The XML wrapper + escaping rule below closes this.

### The audit-event pattern to copy

`SubagentLifecycle` (`provider/agent_event.rs:95-137`) is dual-carrier, emitted by `LifecycleEmitter` (`tools/agent/lifecycle.rs:100-163`):

- **Live:** child-tagged `AgentEvent` on the shared broadcast channel (`agent_event.rs:177-202`).
- **Replay/audit:** `SessionEvent::Custom` appended to the **parent's** store with stable `event_type` constants `subagent.started` / `subagent.completed` (`agent_event.rs:57-63`) and the serde-stable payload as `data`. Store appends are best-effort-logged, never abort delivery (`lifecycle.rs:138-162`).

Serde stability discipline: snake_case tags, internal `phase` tag, RFC 3339 timestamps, typed `Usage`/`AgentStopReason` payloads, with shape-pinning tests (`agent_event.rs:383-466`). Message events follow this pattern exactly.

### "Idle" — flagged divergence from the agreed wording

The agreed design says *"Idle, the same enqueue wakes the loop."* **Norn has no idle state.** `InboundChannel::drain` is non-blocking `try_recv` (`inbound.rs:59-65`); at a stop boundary the loop flushes whatever is *already buffered* and, if nothing is, **returns** (`runner.rs:745-779`). After `run_agent_step` returns, the agent is terminal — enqueueing does nothing, and late child results are sent into a dropped receiver (error logged, `spawn.rs:385-391`). The same applies to `drain_child_results` (`loop/helpers.rs:508-554`): `try_recv` only; a parent that finishes before its children orphans their results.

So "idle wake" is not a routing question, it is a **new loop capability**: at the would-stop boundary, an agent configured to linger must `await` (inbound channel ∪ child-result channel ∪ cancel token) instead of returning. This is additive and keeps the agreed one-codepath property — a message that arrives during the lingering await is drained by exactly the same `flush_inbound_messages` call as a mid-run message. It is, however, more work than the agreed wording implies. See DECISION M3.

## 1.2 Design

### One mechanism, one codepath

All messages travel the recipient's existing inbound mpsc channel and drain at the existing step boundaries (`runner.rs` sites above) — after the current provider call + tool batch, never mid-stream. No second queue, no mailbox sidecar. The only new loop behavior is the optional linger-await (DECISION M3); when lingering, the wake is the same `mpsc` recv that mid-run drains poll, so idle delivery is literally the same code as mid-run delivery.

### Routing: `MessageRouter` replaces handle-holder-only delivery

A workspace-shared directory of live inbound senders, keyed by agent id, with per-recipient monotonic sequence numbers:

```rust
/// crates/norn/src/agent/message_router.rs (new file; replaces agent/mailbox.rs — see DECISION M5)
pub struct MessageRouter {
    inner: parking_lot::Mutex<HashMap<Uuid, RouteEntry>>,
}

struct RouteEntry {
    inbound_tx: InboundSender,
    /// Per-recipient monotonic sequence, minted at enqueue under the lock.
    next_seq: u64,
}

impl MessageRouter {
    /// Registered by the spawn/fork wrapper at launch, removed at terminal
    /// transition (same ownership as the registry entry — the wrapper or
    /// close_agent, never two actors).
    pub fn register(&self, agent_id: Uuid, inbound_tx: InboundSender);
    pub fn deregister(&self, agent_id: Uuid);

    /// Enqueue. Errors are typed and honest: NotRouted (no live channel),
    /// ChannelClosed (recipient loop ended between resolve and send).
    pub async fn deliver(&self, to: Uuid, msg: ChannelMessage) -> Result<u64, RouteError>;
}
```

`MessageRouter` lives next to the registry in `AgentToolInfra` (`tools/agent/infra.rs:35-54`, replacing the `mailbox: Arc<Mailbox>` field) and is forwarded to child contexts in `build_child_context` (`tools/agent/spawn_context.rs:70-78`) exactly as the mailbox is today (`spawn_context.rs:72`). The root agent's own inbound sender (when `inbound_capacity` is configured) is registered under the root's id so children can address `"parent"` even at the top level; if the root has no inbound channel, messaging the root fails honestly (`NotRouted`, surfaced with the reason "root agent has no inbound channel configured").

`AgentHandles` keeps its `inbound_tx` accessor (`tools/agent/handle.rs:169-174`) for `close_agent`'s shutdown Steer (`coord/close.rs:129-138`); `send_message` routes through the router only.

### Message kinds [PROPOSAL — confirm]

```rust
/// Extends loop/inbound.rs — replaces the raw DeliveryMode on the wire surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    /// Act on this: drains at the next step boundary; wakes a lingering agent.
    Steer,
    /// FYI context: batches at step boundaries; does NOT wake a lingering agent.
    Update,
}
```

Mapping onto the existing loop: `Steer → DeliveryMode::Steer` (inject immediately after the tool batch). `Update → DeliveryMode::FollowUp`-like batching **plus** exclusion from the linger-wake set: a lingering agent's await fires only on Steer arrivals (and child results); buffered Updates are flushed when something else wakes it or at its next natural boundary. Mid-run, Updates batch exactly as FollowUps do today (`helpers.rs:488-498`). This is the chatter-storm control: a noisy sibling spamming `update` costs the recipient one batched injection per step, never extra wakeups. No rate caps are added (CLAUDE.md rule); messaging volume is visible in usage accounting because every injected batch is provider input the recipient pays for, attributable via the audit events below.

### Wire shape of the injected message

Built **by the harness** at injection time (in `inject_inbound_messages`, which gains the wrapper as the replacement for the `[{label} from ...]` format) — never by the sender. One injected user-role message per drained message (1:1 with the persisted `UserMessage` event, preserving the compaction-mapping invariant noted at `helpers.rs:503-507`):

```xml
<agent_message from="/smoke/child" from_id="018f63a2-..." kind="update" seq="42" ts="2026-06-12T03:04:05Z">
escaped content
</agent_message>
```

- `from`: registry ground truth at injection time via the `sender_label` rule (`coord/helpers.rs:13-19`): live path, else tombstone path, else bare UUID. The literal `root` for the root agent (whose registry path is the workspace root path when registered; if unregistered, `root`). **Never sender-supplied.**
- `from_id`: sender UUID, always present.
- `role="..."`: emitted **only** when a role was actually set at spawn (`AgentEntry.role`, `registry.rs:87-88`); forks emit `role="fork"` since that is genuinely set (`agent_event.rs:81-82`). Never synthesized.
- `kind`, `seq` (router-minted per-recipient sequence), `ts` (send time, RFC 3339).
- The `author` field of `ChannelMessage` is dropped from the injected surface entirely; the struct keeps a `sender_id: Uuid` and the harness resolves the label. (`ChannelMessage` gains `sender_id`, `kind`, `seq`; `author` is removed — replace, don't add alongside.)

**Escaping rule:** sender `content` is XML-entity-escaped before framing: `& → &amp;`, `< → &lt;`, `> → &gt;` (text body), plus `" → &quot;` for any attribute position (attributes are harness-generated, so this is defense-in-depth only). This makes `</agent_message>` unforgeable from content — the same structural-encoding principle as the JSON-encoded tool results (`tool_dispatch.rs:279`). Attribute values come only from the registry/UUIDs/enum labels, so injection via attributes is impossible by construction. The persisted `UserMessage` event stores the **framed, escaped** string (what the model saw is what the audit shows; resume replays the identical bytes).

`close_agent`'s shutdown Steer and any other harness-authored `ChannelMessage` go through the same framing path — there is exactly one injection formatter.

### Audit trail: typed events, dual-carrier

Every accepted send is recorded; delivery is recorded by the recipient's loop at injection. Two phases, one serde-stable payload, following the `SubagentLifecycle` pattern (`lifecycle.rs:100-163`):

```rust
/// crates/norn/src/provider/agent_event.rs (extension)
pub const AGENT_MESSAGE_SENT_EVENT_TYPE: &str = "agent_message.sent";
pub const AGENT_MESSAGE_DELIVERED_EVENT_TYPE: &str = "agent_message.delivered";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum AgentMessageLifecycle {
    /// The router accepted the message (send_message returned success).
    Sent {
        message_id: Uuid,
        from_id: Uuid,
        /// Registry label at send time (path / "root" / bare UUID).
        from: String,
        to_id: Uuid,
        to: String,
        kind: MessageKind,
        /// Router-minted per-recipient sequence.
        seq: u64,
        /// Unescaped sender content, verbatim, for audit.
        content: String,
        sent_at: DateTime<Utc>,
    },
    /// The recipient's loop injected the message into its conversation.
    Delivered {
        message_id: Uuid,
        from_id: Uuid,
        to_id: Uuid,
        seq: u64,
        delivered_at: DateTime<Utc>,
    },
}
```

Carriers, mirroring `LifecycleEmitter::emit` exactly:

- **Live:** `AgentEventKind` gains a third variant `Message(AgentMessageLifecycle)` (`agent_event.rs:177-184`), tagged with the *sender's* identity for `Sent` and the *recipient's* for `Delivered`.
- **Audit:** `SessionEvent::Custom` with the constants above. `Sent` is appended to **(a)** the sender's own store and **(b)** the store of the parent that granted the messaging scope (the spawner of the sender) — that is the "parent-visible audit trail": the parent sees every message its children exchange without being on the data path, queryable through the same surfaces that read `subagent.*` events today (action-log/event queries over `SessionEvent::Custom`; the Wave 2 agents status surface adds per-edge message counts from these events). `Delivered` is appended to the recipient's store immediately before the framed `UserMessage` (adjacent events, same `EventBase` parent-chain).

EventStore events ⇒ resume-safe and action-loggable by construction: `rebuild_action_log` (`agent/resume.rs`) replays `Custom` events untouched, and the conversation already replays the framed `UserMessage`.

### Ordering and resume guarantees

- **Per-recipient total order:** router sequence numbers are minted under the router lock; the recipient's mpsc preserves enqueue order; `inject_inbound_messages` keeps its timestamp sort (`helpers.rs:213`) but the authoritative order is `seq` — change the sort key to `seq` (timestamps from concurrent senders are not monotonic; sequences are). Steer/Update partition does not reorder within a kind.
- **Cross-recipient:** no global order is promised (none is promised today for any event).
- **Send vs. delivery:** `Sent` precedes `Delivered` for the same `message_id` in any merged view, because `Delivered` is only emitted by the loop that drained the channel the router enqueued into.
- **Resume:** delivered messages were persisted as `UserMessage` events → replayed in conversation; their `Delivered` audit events replay alongside. Messages still **in flight** (enqueued, not yet drained) at process death are lost with the channel — but the `Sent` event survives in the sender's/parent's store, so the loss is *detectable* (Sent without Delivered), never silent. Re-delivery on resume is explicitly out of scope: channels are process-lifetime, stores are the durable record. Flagged here so nobody mistakes Sent-events for a durable queue.

### Permissioning: scope declared by the parent at spawn/fork time

```rust
/// Part of ChildPolicy (shared with Feature 2 — §2.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessagingScope {
    /// May message siblings under the same parent, and the parent.
    SiblingsAndParent,
    /// May message only the parent.
    ParentOnly,
    /// send_message is not available (tool absent from the child's surface).
    None,
}
```

Enforcement is in `send_message`'s execute against registry ground truth: resolve target → check `target.parent_id == sender.parent_id` (sibling) or `target.id == sender.parent_id` (parent) per scope. `None` removes the tool from the child's allow-list at spawn (`SubAgentExecutor` allow-list, `tools/agent/infra.rs:137-158`) *and* is checked at execute as defense-in-depth. The root agent (no parent) is unrestricted toward its own children. Scope travels on the child's `AgentToolInfra` (new field) so grandchildren get their own scope from *their* parent (Feature 2).

### Tool surface: `send_message` replaces `signal_agent`

`SignalAgentTool` (`coord/signal.rs`), its registration (`tools/registry_builder.rs:91,118`), guidance files (`tools/guidance/signal_agent.*`), and re-exports (`tools/agent/mod.rs:19`, `tools/mod.rs:43`) are **deleted and replaced** — no shim, no alias.

```rust
pub struct SendMessageTool;
// name: "send_message"
// schema:
{
  "type": "object",
  "required": ["to", "kind", "content"],
  "additionalProperties": false,
  "properties": {
    "to":      { "type": "string", "description": "Recipient: hierarchical path, UUID, or the literal \"parent\"." },
    "kind":    { "type": "string", "enum": ["steer", "update"] },
    "content": { "type": "string" }
  }
}
```

Behavioral contract (all paths verified against current honest-failure precedents):

- `"parent"` resolves via the sender's `AgentToolInfra.parent_id` (`infra.rs:46-47`); a root sender gets a typed failure ("this agent has no parent").
- Out-of-scope target → structured failure naming the granted scope (mirrors the H15 wording discipline, `signal.rs:198-217`).
- Tombstoned/terminal recipient → the existing honest already-finished failure, verbatim mechanism from `signal.rs:112-146` / `infra.rs:85-119`. [DEPENDS: Wave1-C — the machinery ships in the Wave 1 batch.]
- Routed but channel closed (recipient finished between resolve and enqueue) → `delivered: false` typed failure; the `Sent` event is **not** emitted (nothing was accepted).
- `content` is a string, not arbitrary JSON (signal_agent took any JSON, `signal.rs:80-83`; structured payloads go in the string — the wrapper escapes it either way). Flagged as a deliberate narrowing; revisit only if a consumer needs machine-parse guarantees beyond what escaped text gives.

### Loop changes (mid-run + linger)

1. `drain_and_partition` (`helpers.rs:179-195`) partitions on `MessageKind` instead of `DeliveryMode` (same shape).
2. `inject_inbound_messages` (`helpers.rs:203-236`) emits the XML frame, appends `Delivered` audit events, sorts by `seq`.
3. Linger-await (if M3 accepted): at the three would-stop sites (`runner.rs:605-610, 757-767, 903-908`), when `loop_context.linger` is configured and no buffered work exists, `select!` on inbound-Steer arrival, child-result arrival, cancel token, and the linger deadline; on wake, fall through to the existing flush/drain calls. Cancellation and `max_iterations`/`step_timeout` semantics are unchanged — the await counts toward `step_timeout` (it is wall-clock inside the step).

## 1.3 DECISIONs (Feature 1)

- **DECISION M1 — default `MessagingScope` for spawned/forked children.** Options: (a) `SiblingsAndParent`, (b) `ParentOnly`, (c) `None`. Per the no-assumed-defaults rule this "default" is itself builder-set (`AgentBuilder::child_messaging_scope`, required when agent tools are enabled — build error if spawn tools are registered and no scope is configured); the question is what norn *documents and ships in examples*. **Recommendation: (a) `SiblingsAndParent`** — matches the agreed direction; the audit trail and update/steer split are the safety mechanism, not isolation.
- **DECISION M2 — message kinds `steer`/`update` with update-does-not-wake semantics.** [PROPOSAL as agreed — confirm.] Recommendation: confirm as specified; it is the entire chatter-storm story.
- **DECISION M3 — linger ("idle wake") mechanism.** The agreed wording assumes an idle agent exists to wake; it does not (see §1.1). Options: (a) **opt-in linger-await at stop boundaries**, builder/spawn-configured (`linger: Option<LingerPolicy { deadline: Duration }>` — no default duration; unset = current return-immediately behavior), making "wake the idle agent" real for agents configured to wait (typically parents awaiting children/peers); (b) messaging is mid-run-only in Wave 3, linger deferred. **Recommendation: (a)** — without it, child→parent `steer` only lands if the parent happens to still be mid-run, which guts the feature; and it fixes the existing orphaned-late-child-results gap (`spawn.rs:385-391`) with the same primitive.
- **DECISION M4 — child inbound capacity into `ChildPolicy`.** `SPAWN_INBOUND_BUFFER`/`FORK_INBOUND_BUFFER` = 32 are hardcoded today (`spawn.rs:56`, `fork_pipeline.rs:55`) — that violates the configurability rule the moment siblings can fill the channel. Move into `ChildPolicy.inbound_capacity` (builder-required envelope, per-spawn narrowable). Current value 32 becomes the *documented proposal*, not a hardcoded default. **Recommendation: accept; 32 as documented proposal.**
- **DECISION M5 — fate of `agent/mailbox.rs`.** Options: (a) delete `Mailbox`, build `MessageRouter` fresh (rhai `agent_ops` migrates to the router — its send/recv/wait surface maps 1:1); (b) refactor `Mailbox` into the router in place. **Recommendation: (a)** — the router's value type and error contract differ enough that in-place refactor preserves nothing but the name; no-zombie-code either way.

## 1.4 Test strategy (Feature 1)

- **Framing/escaping:** content containing `</agent_message>`, attribute-quote, and entity edge cases round-trips inert (model-visible string contains no unescaped frame tokens); property test over arbitrary content strings asserting the frame parses back to the original content.
- **Forgery:** a child whose output *is* a fake `<agent_message from="root">` frame arrives fully escaped; assert the injected turn contains exactly one real frame.
- **Routing/scope matrix:** sibling/parent/none × live/terminal/tombstoned/unknown recipient → exact typed outcomes (extend the `signal.rs` test patterns at `signal.rs:243-448`, which already cover the honest-failure paths being inherited).
- **Kinds:** update batches and does not wake a lingering recipient; steer wakes; mixed batch injects in `seq` order; per-recipient seq total order under concurrent senders (loom-style or stress test on the router lock).
- **Audit:** every accepted send → exactly one `Sent` in sender + scope-granting parent stores; every injection → `Delivered` adjacent to the framed `UserMessage`; serde shape-pinning tests for both phases (mirror `agent_event.rs:383-466`).
- **Resume:** kill after `Sent`/before drain → resumed store shows Sent-without-Delivered and the conversation lacks the frame; kill after delivery → frame replays byte-identical.
- **Replacement:** no `signal_agent` symbol, guidance file, or registry entry remains (grep gate in CI test).

## 1.5 Dependencies (Feature 1)

- [DEPENDS: Wave1-C fork lifecycle] only for tombstone shape stability — honest-finished-recipient behavior ships in the Wave 1 batch.
- [DEPENDS: Wave 2 agents status surface] for *displaying* message edges; not required to ship messaging.
- Independent of Feature 2, but `MessagingScope` must land inside `ChildPolicy` (§2.2) from day one so the spawn-time API is one parameter, not two bolted-on args.

---

# Feature 2 — Recursive Delegation

## 2.1 Current state (verified)

- **Depth gate:** `AgentRegistry::reserve` rejects a reservation whose parent itself has a `parent_id` — "children cannot spawn grandchildren" (`agent/registry.rs:417-426`); concurrent cap `MAX_CONCURRENT_CHILDREN = 32` counts non-terminal children of one parent (`registry.rs:15, 429-439`). *(Note: the brief cited `registry.rs ~103, ~328`; current lines are as above — the file grew.)*
- **Why depth-1: the result channel is root-only.** `install_agent_infra` creates one `ChildResultSender`/receiver pair with hardcoded `CHILD_RESULT_CHANNEL_CAPACITY = 256` (`agent/result_channel.rs:49-53`, `agent/assembly.rs:677-698`) and the receiver is wired onto the **root** loop's `LoopContext.child_result_rx` (`loop/loop_context.rs:222`, default `None` at `:267`). The spawn wrapper fetches the sender from the *spawning agent's* context (`tools/agent/spawn.rs:614`) — but `build_child_context` (`tools/agent/spawn_context.rs:63-117`) does **not** install a `ChildResultSender` on the child's context, and the child's `LoopContext` keeps `child_result_rx: None`. A grandchild's results would have nowhere to go: lifecycle events broadcast (the channel *is* forwarded, `spawn_context.rs:108-112`), result delivery doesn't exist. Exactly the recorded gap.
- **The context machinery is already grandchild-ready:** each child gets a fresh empty `AgentHandles` "so it can spawn grandchildren" (`spawn_context.rs:26-28, 86`), its own `AgentToolInfra` with correct `parent_id` (`spawn_context.rs:70-78`), shared registry/permissions/hooks/session-tree (grandchildren even branch session stores correctly — `tools/agent/handle.rs:59-62`). Only the registry gate and the result channel block recursion.
- **Result drain:** `drain_child_results` (`loop/helpers.rs:508-554`) — `try_recv` batch, one formatted `UserMessage` per batch; called at the same boundaries as inbound flush (`runner.rs:269-271, 605-610, 757-767, 903-908`).
- **Cancellation:** the root run takes a cooperative token (`agent/instance.rs:43,145`; checked per-iteration and mid-stream, `runner.rs:292-294, 448`). As of the Wave 1 batch, **children get their own independent `CancellationToken`** (created at launch, passed as `cancel: Some(...)`, trigger stored on `AgentHandle.cancel`) — but these are not child tokens of the parent's, so root `handle.cancel()` still does **not** reach children. The only cascading stop is `close_agent`: DFS post-order over the registry subtree (`coord/close.rs`), per-agent best-effort Steer → cancel the child's run token → **join** the wrapper (never abort — the wrapper records the run's real outcome, `Cancelled` for a mid-run stop). Note `collect_subtree` is already written depth-unbounded ("CO1: no hardcoded limits").
- **Usage:** a child's accumulated usage reaches the parent on `ChildAgentResult.usage` (`result_channel.rs:34-41`) and on `SubagentLifecycle::Completed.usage` (`agent_event.rs:125-127`; fork also `SessionEvent::ForkComplete.usage`, `session/events.rs:208-220`). The parent's own `total_usage` counts only its own provider calls (`runner.rs:401,456`) — child usage is event/result-carried, never folded into the parent's run usage. Honest-zeros limitation on hard-error/panic paths is documented (`spawn_outcome.rs:73-80,185-199`, `lifecycle.rs:42-49`).
- **Child loop config:** children run `AgentLoopConfig::default()` (`spawn.rs:305`, `fork_pipeline.rs:535`) — they inherit nothing from the parent's loop config. Relevant when per-child budgets arrive: there is already a per-child config seam, it is just not exposed.

## 2.2 Design

### One coherent spawn-time policy parameter

The agreed requirement: messaging scope and delegation budget are **one** parameter shape, set by the parent per child at spawn/fork time.

```rust
/// crates/norn/src/agent/child_policy.rs (new file)
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChildPolicy {
    /// Who this child may message (Feature 1).
    pub messaging: MessagingScope,
    /// Delegation budget for this child's own spawning.
    pub delegation: DelegationBudget,
    /// Bounded capacity of this child's inbound channel (replaces
    /// SPAWN_INBOUND_BUFFER / FORK_INBOUND_BUFFER — DECISION M4).
    pub inbound_capacity: usize,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct DelegationBudget {
    /// How many levels of descendants this child may create below itself.
    /// 0 = leaf (may not spawn). Decrements per level: a child spawned with
    /// remaining_depth = n grants its own children at most n - 1.
    pub remaining_depth: u32,
    /// Max non-terminal direct children this child may have at once
    /// (replaces the hardcoded MAX_CONCURRENT_CHILDREN for this subtree node).
    pub max_concurrent_children: usize,
}
```

Plumbing:

- **Builder (root envelope, required):** `AgentBuilder::child_policy(ChildPolicy)` — required whenever the agent-coordination tools are registered; building with spawn/fork tools and no policy is a build-time error (no assumed default). This is the policy the *root* stamps on its children when the spawn call doesn't narrow it.
- **Spawn/fork tool args (narrowing only):** `spawn_agent`/`fork` gain an optional `child_policy` argument; the effective policy is `min`-clamped against the caller's own remaining budget (`remaining_depth ≤ caller.remaining_depth - 1`, `max_concurrent_children` ≤ caller's grant, scope ⊆ caller's scope). A parent can tighten, never widen. Attempting to widen is a typed tool failure naming the caller's own budget.
- **Carriage:** the effective `ChildPolicy` is stored on the child's `AgentToolInfra` (new field) and on its registry entry (`AgentEntry` gains `policy: ChildPolicy` — serialized with the entry, visible to status surfaces), so enforcement reads ground truth, not tool-context folklore.

### Registry: budgeted depth replaces the flat gate

`AgentRegistry::reserve` (`registry.rs:401-460`) changes:

- Delete the `parent_entry.parent_id.is_some()` rejection (`registry.rs:417-426`).
- Reserve checks the **parent's** `policy.delegation`: `remaining_depth >= 1` else typed `SpawnFailed("delegation depth exhausted: this agent's budget is N levels, all used")`; non-terminal-children count `< parent.policy.delegation.max_concurrent_children` (generalizing `registry.rs:429-439` — the count query is already per-parent and needs no change).
- `MAX_CONCURRENT_CHILDREN` const (`registry.rs:15`) is deleted.
- Paths are already arbitrary multi-level strings with no depth assumption (`registry.rs:85`, `path_index` is a flat map) — `/a/b/c` works today; spawn's auto-path generation must namespace under the *spawning agent's* path rather than the root's `/spawn/...` (today's auto-path; `spawn.rs` schema doc at `:487`). Tombstones are per-id with a path index (`registry.rs:141-147`) and apply per level unchanged. [DEPENDS: Wave1-C if it reshapes tombstones.] The Wave 2 agents status surface renders the tree from `parent_id` links (`registry.rs:228-236` `children()` already exists; `norn-tui/src/agents/tree.rs` consumes it).

### Recursive result delivery: one hop at a time

Principle (agreed): each agent owns delivery for its **direct** children only; results bubble one hop, never skipping levels. Mapping onto existing machinery — what generalizes is almost everything; what's missing is one channel per spawning agent plus wiring:

1. **Channel-per-agent:** `build_child_context` (`spawn_context.rs:63-117`) creates a fresh `(ChildResultSender, Receiver)` pair for the child **iff** the child's effective `delegation.remaining_depth >= 1`, installs the sender as a context extension (exactly what `install_agent_infra` does for the root, `assembly.rs:695-697`), and returns the receiver to the launch path.
2. **Wiring:** the spawn/fork launch (`spawn.rs:261-435`, `fork_pipeline.rs:~500-664`) sets the child's `LoopContext.child_result_rx = Some(rx)`. The drain sites already exist in every loop (`runner.rs:269-271, 605-610, 757-767, 903-908`) — zero loop changes for delivery.
3. **Bubbling is emergent, not built:** a grandchild's wrapper fetches `ChildResultSender` from *its parent's* (the child's) context (`spawn.rs:614` already reads the spawning agent's context — unchanged), the child's loop drains it and injects the formatted result into the child's own conversation; the child's final result then flows to *its* parent on the channel that already exists. One hop per level, no level skipping, no new delivery code path.
4. **Channel capacity:** `CHILD_RESULT_CHANNEL_CAPACITY = 256` (`result_channel.rs:53`) becomes part of the builder envelope (`AgentBuilder::child_result_capacity`, applied per spawning agent) — DECISION R3.
5. **Linger interaction:** with M3 accepted, a mid-tree agent whose own task is done but whose children still run lingers at its stop boundary and is woken by child results — closing the orphaned-result gap at every level, not just the root.

### Cancellation cascade

Two paths, both specified:

- **Cooperative (`handle.cancel()` on the root, or any agent's token):** spawn/fork wrappers create the child's run token as `parent_cancel.child_token()` and pass it as `cancel: Some(...)` (upgrading the Wave 1 batch's independent per-child tokens to hierarchical ones — the plumbing through `AgentStepRequest.cancel` and `AgentHandle.cancel` already exists; only the token's parentage changes). The parent's token must be reachable at the spawn site: `AgentToolInfra` gains `cancel: CancellationToken` (the root's comes from the builder, `instance.rs:43`; each child's is its own child token, stored when its context is built). `tokio_util` child tokens cascade automatically: root cancel → every descendant's loop sees `is_cancelled` at its next iteration/stream boundary (`runner.rs:292-294, 448`) → each returns `Cancelled { usage }` → each wrapper runs its normal terminal path: lifecycle `Completed` (with usage), `ChildAgentResult` (with usage), registry mark, reclamation. **Cancellation therefore yields a fully-accounted tree, not aborted tasks.**
- **Forced (`close_agent`):** unchanged in shape — `collect_subtree` is already depth-unbounded. As of the Wave 1 batch the close already cancels the target's run token and joins the wrapper (no abort anywhere); with hierarchical tokens that cancellation cascades, so descendants get the cooperative path even when the closer only holds the top handle (today the closer can only cancel runs it holds handles for, and reports deeper live agents `"unreachable"`, close.rs `shutdown_one`; with token cascade, "unreachable" survives only for agents whose token lineage is broken — which the design makes impossible for spawned descendants).

### Usage aggregation up the tree

Requirement: grandchild usage must not vanish. Mechanism, one hop at a time like results:

- `ChildAgentResult` gains `subtree_usage: Usage` — the child's own `total_usage` **plus** the sum of `subtree_usage` over every `ChildAgentResult` the child received. The accumulation point is the child's wrapper: the loop already owns `total_usage` per run (`runner.rs:273,456`); the wrapper additionally folds in the drained children's subtree usage, which requires the drain path to record what it drained — `LoopContext` gains a `children_usage: Usage` accumulator that `drain_child_results` adds into (one line at `helpers.rs:515-517`), surfaced on every `AgentStepResult` arm alongside `usage` (the arms already all carry usage, `runner.rs:619-919`).
- `SubagentLifecycle::Completed` gains the same `subtree_usage` field (serde-additive; shape-pinning tests updated — this is a **breaking schema change** to a documented-stable event, justified under no-backwards-compat but called out for meridian consumers in MERIDIAN-HANDOFF terms).
- Parent's own `total_usage` stays own-calls-only (unchanged semantics, `runner.rs:456`) — aggregation is explicit in `subtree_usage`, never double-counted.
- Honest-zeros rule propagates: a panicked mid-tree agent reports its own usage as unknown-zeros (`spawn_outcome.rs:214-224`) but its *children's* delivered `subtree_usage` is still folded in — partial truth beats silent loss; the zeros-mean-unknown caveat is documented on the field exactly as today (`result_channel.rs:34-41`).

### Messaging × recursion

`MessagingScope` is evaluated against registry ground truth at send time, so it composes with depth for free: siblings = same `parent_id` at any level; `"parent"` = one hop up at any level. A grandchild with `SiblingsAndParent` may message its own siblings and its parent (the child), **not** the root — escalation crosses one audited hop at a time, mirroring result bubbling. The `Sent` audit event lands in the sender's store and the *granting parent's* store at every level, so each subtree owner sees its own children's traffic; the root sees the whole tree through the Wave 2 surface walking child stores (`AgentHandles::event_store`, `tools/agent/handle.rs:184-189`, plus the session tree for grandchildren).

## 2.3 DECISIONs (Feature 2)

- **DECISION R1 — documented-proposal values for `DelegationBudget`.** The builder envelope is mandatory (no default in code). For docs/examples: current behavior maps to `remaining_depth = 1`, `max_concurrent_children = 32` (`registry.rs:15,417-439`). Options: (a) document exactly those as the recommended starting envelope; (b) recommend a deeper default (e.g. 2–3). **Recommendation: (a)** — current values are production-proven; deeper trees are an explicit opt-in per deployment.
- **DECISION R2 — where the spawn tool's `child_policy` arg sits in the schema.** Options: (a) one `child_policy` object arg on `spawn_agent`/`fork` (mirrors the Rust type 1:1); (b) flattened scalar args. **Recommendation: (a)** — one coherent shape was the agreed requirement; flattening recreates two-bolted-on-args at the JSON layer.
- **DECISION R3 — `child_result_capacity` and `inbound_capacity` envelope placement.** Both become builder-required alongside `child_policy` (envelope), per-spawn narrowable. Documented proposals: 256 (`result_channel.rs:53`) and 32 (`spawn.rs:56`). **Recommendation: accept; current values as documented proposals.**
- **DECISION R4 — `subtree_usage` on `SubagentLifecycle::Completed` (breaking schema change to a stable contract).** Options: (a) add the field (consumers: meridian matches on this event); (b) carry subtree usage only on `ChildAgentResult` and the registry/status surface. **Recommendation: (a)** — the lifecycle event is the audit record; usage that isn't on it vanishes from any store-only consumer, which is precisely the failure mode the requirement forbids. Coordinate the meridian-side match update in the handoff.
- **DECISION R5 — child loop config inheritance.** Children currently run `AgentLoopConfig::default()` (`spawn.rs:305`). With recursion, per-child `max_iterations`/`step_timeout` become real cost controls. Options: (a) fold an optional `loop_config` override into `ChildPolicy` now; (b) defer to a later wave. **Recommendation: (b) defer** — it is severable, and Wave 3 is already large; but record it, because "children ignore the parent's timeouts" surprises people.

## 2.4 Test strategy (Feature 2)

- **Registry budgets:** depth-0 child cannot reserve; depth-n chain reserves exactly n levels and the n+1th fails with the typed message; widening attempts at spawn fail typed; concurrent-cap per node honored independently at each level; path auto-generation namespaces under the spawner; tombstone + path-reuse behavior at depth ≥ 2 (extend `registry.rs` tests at `:939-996, 1020-1093`).
- **Result bubbling:** 3-level tree with `MockProvider` (pattern: `spawn.rs` tests `:1308-1660`): grandchild result text reaches the child's conversation, child's final result reaches root; kill the middle agent (panic injection) → grandchild's `Sent`/lifecycle events survive, root sees the child's honest failure, nothing hangs.
- **Cancellation cascade:** root `handle.cancel()` → every descendant returns `Cancelled` with usage; every wrapper emits `Completed` lifecycle + result; registry fully reclaimed; assert no dangling `Started` (the invariant `spawn.rs:287-295` already states). `close_agent` on a mid-tree node: leaves-first order observed, grandchildren reachable via token even without held handles.
- **Usage rollup:** synthetic usages at each level sum exactly once at the root (`subtree_usage` = Σ descendants + self); panic at mid-level → zeros for that node, grandchild usage still present; property test: rollup is associative over arbitrary trees.
- **Messaging × depth:** grandchild → root direct send fails typed under `SiblingsAndParent`; one-hop relay works and leaves `Sent` audit at each granting parent.

## 2.5 Dependencies / sequencing (Feature 2)

- [DEPENDS: Wave1-C fork lifecycle] — tombstone/reclaim shape at depth ≥ 2; the reclaim ownership rule (`tools/agent/reclaim.rs` doctrine cited at `registry.rs:313-320`) must be re-verified per level (it is per-parent already, so expected clean).
- [DEPENDS: Wave 2 agents status surface] — tree rendering of multi-level paths; not a code dependency, a display one.
- Depends on Feature 1 only for the shared `ChildPolicy` type (land the type with Feature 1; recursion fills in `delegation` enforcement).

---

# Combined rollout order

1. **W3.0 — `ChildPolicy` type + builder envelope** (`child_policy`, `inbound_capacity`, `child_result_capacity` setters; build-time error when coordination tools are registered without them). No behavior change yet. *(Unblocks both features; smallest reviewable unit.)*
2. **W3.1 — `MessageRouter` + framed injection + audit events** (delete `Mailbox`, migrate rhai ops; XML wrapper + escaping in `inject_inbound_messages`; `agent_message.sent/delivered`; `AgentEventKind::Message`). `signal_agent` still present and routing through the router — one commit of coexistence inside the wave, deleted in W3.2, never released.
3. **W3.2 — `send_message` replaces `signal_agent`** (tool, scope enforcement from `ChildPolicy.messaging`, guidance, registry-builder swap, delete signal). Feature 1 shippable here (mid-run delivery), modulo M3.
4. **W3.3 — linger-await** (DECISION M3) at stop boundaries; closes orphaned-child-result gap; makes idle steer-wake real.
5. **W3.4 — registry budgets + per-agent result channels + wiring** (delete depth gate and `MAX_CONCURRENT_CHILDREN`; `build_child_context` channel pair; `child_result_rx` on child loops; path namespacing). Recursion functional.
6. **W3.5 — cancellation cascade** (`AgentToolInfra.cancel`, child tokens, `cancel: Some`, close_agent token-first).
7. **W3.6 — usage rollup** (`children_usage` accumulator, `subtree_usage` on `ChildAgentResult` + `SubagentLifecycle::Completed`, shape-pin updates, meridian handoff note).
8. **W3.7 — surfaces** (Wave 2 agents tree shows depth + message edges; action-log queries over the new event types).

Each step passes the full gate (`cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check`, tests) and gets a Fable-model review before landing, per CLAUDE.md.

**ROLLOUT COMPLETE (2026-06-13).** All eight steps landed, each Fable-reviewed: W3.0/W3.1/W3.3 at `fd1c587`, W3.2 at `a01cd43`, W3.4 at `2a2a7a2`, naming reversal at `ae6c02c`, W3.5 at `74763c9`, W3.7 at `7283e88`, W3.6 at `fcea51d` (final — the DECISION R4 breaking change, with the per-step `children_usage` reset from its review). 3358 tests at completion. One deliberate type deviation recorded at W3.6: `LoopContext.children_usage` is a shared `ChildrenUsage` handle, not the spec's plain `Usage` field — a plain field dies with a panicked inner task, losing the delivered grandchild usage the honest-zeros sentence requires to survive (appendix-grade refinement, documented on the type). Remaining obligations out of this wave: **R5 closure (per-child `ChildPolicy.loop_config` including `linger`) — due next wave**; named follow-ups: embedder-root cascade opt-in, rhai per-spawn narrowing.

**TRACKED DEFERRAL (R5, approved 2026-06-12): per-child `AgentLoopConfig` override.** Children run `AgentLoopConfig::default()` — they do NOT inherit the parent's `max_iterations`/`step_timeout`, and with recursion these become real cost controls. Deferred out of Wave 3 as severable; the fix is an optional `loop_config` field on `ChildPolicy` (W3.0 establishes the type, so the slot is cheap to add later). This deferral must be re-raised at Wave 3 review and closed in the wave that follows at the latest; until closed, spawn/fork guidance must state that children ignore the parent's loop limits.

**R5 re-raised at the W3.0/W3.1/W3.3 review (2026-06-12) — the gap has widened.** `LingerPolicy` rides on `AgentLoopConfig`, so until R5 closes only the root can linger: §"Messaging × recursion" item 5 (a mid-tree agent lingering for its children's results) is unachievable, and late grandchild results will still orphan at the child level when W3.4 makes recursion functional. When R5 closes, the `loop_config` field on `ChildPolicy` must include `linger`. The interim guidance statements landed with the batch (spawn_agent.usage.md, fork.usage.md).

**Review carry-forwards into W3.2 (from the W3.0/W3.1/W3.3 Fable review, 2026-06-12) — ALL FIVE CLOSED by W3.2 (verified at its Fable review):** dual-store Sent audit (sender + ParentGrant.parent_store); route register/deregister owned by spawn/fork wrappers + root registration in assembly; rhai kind parameter + sender_attribution reuse + terminal-state checks; CLI CoordinationEnvelope published by install_agent_tool_infra with all transitional buffer consts deleted (inbound channels sized from ChildPolicy.inbound_capacity); full-channel router contention test. The original items, for the record:
1. **Dual-store `Sent` audit:** the §"Audit trail" requirement that `Sent` lands in the sender's store *and* the scope-granting parent's store is satisfied only for (a) today (the only senders are handle-holding parents and root rhai hosts). `send_message` must write both stores.
2. **Router deregistration ownership:** `MessageRouter::deregister` has no production caller yet; routes are cleaned only lazily on `ChannelClosed`. The spawn/fork completion wrappers take ownership of register-at-launch / deregister-at-terminal in W3.2 as planned.
3. **Rhai `send_message` parity:** add a script-side message-kind parameter (currently hard-coded `Update`), reuse `sender_attribution` (tombstone fallback), and add the registry terminal-state check so scripts get the same honest already-finished failure `signal_agent` gives.
4. **CLI envelope boundary:** the CLI assembly path (`install_agent_tool_infra`) publishes no `CoordinationEnvelope` extension; nothing reads it yet, but W3.2's spawn-time policy reads will. Publish the CLI's deliberate envelope there before any reader lands, and wire `ChildPolicy.inbound_capacity` to replace the transitional `SPAWN_INBOUND_BUFFER`/`FORK_INBOUND_BUFFER` consts (32) in W3.2/W3.4.
5. **Router contention test:** the seq-order stress test never makes `reserve()` actually await (capacity 1024 vs 256 sends); the replaced-route/closed-route mid-await races are now covered by dedicated tests, but a full-channel contention variant of the stress test should land with W3.2's higher-traffic surface.

**NAMING SUPERSEDED (owner decision, 2026-06-12, post-W3.4):** the messaging tool's final name is **`signal_agent`**, not `send_message` — meridian's own workspace member-messaging tool collides with the `send_message` name in the combined registry norn agents see when embedded. Same args/semantics/audit, no length cap (explicitly declined; if ever wanted it is a `ChildPolicy` field, never a hardcoded constant). Internal mechanism names stay message-named (`MessageRouter`, `agent_message.*` events, frames, `MessageKind`). Where this document says `send_message` for the tool, read `signal_agent`.

**NAMED FOLLOW-UP (from W3.4, 2026-06-12): rhai per-spawn narrowing.** Script spawns derive inherit-with-decrement grants only — `spawn_agent` from rhai has no `child_policy` narrowing parameter yet (the tools do). Add it when the rhai surface next gets attention; the narrowing core (`ChildPolicy::grant_for_child`) is shared, so it is wiring only. Also recorded: rhai script children run against the host's shared tool context with no tool surface — if they ever gain tools they need their own child context (documented at the executor construction site).

**NAMED FOLLOW-UP (from the W3.2 Fable review, 2026-06-12): CLI root inbound wiring.** The CLI/TUI drivers build their root agent without an inbound channel, so a child granted `siblings_and_parent` or `parent_only` cannot actually reach its parent on those surfaces — `send_message(to: "parent")` fails deterministically with the precise typed reason ("the root agent has no inbound channel configured"). The design explicitly permits this state (§Routing), and the guidance tells children it exists, but it makes the granted parent-scope half-dead on the CLI surfaces. Wire a root inbound channel (and its drain surface) for the CLI drivers as a deliberate feature — candidate for W3.7 (surfaces) alongside the TUI message-edge work. **CLOSED by W3.7 (2026-06-13):** `install_agent_tool_infra` creates and routes the root inbound channel (sized from the envelope's `child_policy.inbound_capacity`), and both drivers drain it via `AgentStepRequest.inbound`. The library-surface boundary is unchanged — an embedder root still opts in via `AgentBuilder::inbound_capacity`.

**NAMED FOLLOW-UP (from the W3.5 Fable review, 2026-06-12): embedder-root cascade opt-in.** The CLI/TUI assembly paths (`install_agent_tool_infra`, norn-tui `rotation.rs`) publish no `AgentCancellation`, so those roots' direct children get free-standing tokens — honest today only because those roots run with `cancel: None` (no root cancellation exists for the cascade to sever from). The moment either surface gains a real root run token (e.g. Esc-interrupt wired as a token), publishing `Arc::new(AgentCancellation(root_token))` on the shared context becomes mandatory, or root cancellation will silently strand depth-1 subtrees. Zero norn-core changes needed; natural companion to the CLI root inbound wiring above.

---

# Appendix — points where the code contradicts or refines the agreed design (flagged, not silently adapted)

1. **"Idle, the same enqueue wakes the loop"** — no idle state exists; drains are non-blocking and the loop returns at stop (`inbound.rs:59-65`, `runner.rs:745-779`). Honoring the agreed semantics requires the new linger-await (DECISION M3); without it, idle wake is impossible, not merely unwired.
2. **Sibling messaging needs new routing, not new permissions** — delivery is handle-holder-only today (`signal.rs:152-217`); there is no channel a sibling could be permitted onto. Hence `MessageRouter`.
3. **Wave1-C tombstone dependency is already (at least partly) on main** — honest already-finished failures exist end-to-end (`registry.rs:104-125`, `signal.rs:112-146`, `close.rs:259-275`). The dependency reduces to "Wave1-C must not reshape `AgentTombstone`".
4. **Brief's registry line numbers are stale** — depth gate is at `registry.rs:417-426`, cap at `:429-439`, const at `:15` (brief said ~103/~328).
5. **Today's injected-message format is forgeable** — `[Inbound from {author}]` with sender-supplied `author` and unescaped content (`helpers.rs:215`, `inbound.rs:36-38`); the XML wrapper is a security fix, not just a format change. `ChannelMessage.author` is deleted in the process (replace, don't add alongside).
6. **Root cancel does not cascade today** — as of the Wave 1 batch each child has its own independent run token (cancelled by `close_agent`, which then joins the wrapper so the true `Cancelled` outcome is recorded), but the tokens have no parent/child relationship, so root `handle.cancel()` does not reach children. The cascade in §2.2 is a parentage upgrade of Wave 1's plumbing, not new plumbing.
7. **An orphaned `Mailbox` already exists** (`agent/mailbox.rs`) — undrained by any loop, used only by rhai integration. It must be replaced or deleted (DECISION M5); leaving it beside `MessageRouter` would violate the no-zombie-code rule.
8. **Children ignore parent loop config** — `AgentLoopConfig::default()` per child (`spawn.rs:305`, `fork_pipeline.rs:535`); recorded as DECISION R5 (defer), so nobody assumes per-child timeouts exist when budgeting deep trees.
9. **W3.5 deviation (reviewed, approved at the W3.5 Fable review 2026-06-12): the caller's cancel token travels as its own `AgentCancellation` ToolContext extension, not as a field on `AgentToolInfra`** as §"Cancellation cascade" wrote it. `AgentToolInfra` is also constructed by embedder surfaces that own no run token (norn-cli `wiring.rs`, norn-tui `rotation.rs`); a required field there would force token-less embedders to invent a token they don't control (violating NO ASSUMED DEFAULTS). The cascade contract is unchanged: spawn/fork mint child tokens via `child_token()` of the published extension; `build_child_context`/`build_fork_context` take the child token as a **required** parameter and publish it on the child context at construction, so lineage from depth 1 downward cannot silently go missing — the only optional point is the embedder root, which is typed, documented, and pinned by test.
