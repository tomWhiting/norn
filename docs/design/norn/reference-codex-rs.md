# Codex-rs Scout Report for Norn

Working tree: `/Users/tom/Developer/tools/codex`

Scope: Responses API wire/model types, built-in tool implementations, multi-agent control, and OpenAI Responses SSE streaming. Line numbers are from the current checkout inspected on 2026-05-10.

## 1. OpenAI Responses API Types

### Provider-facing request/stream types

- `codex-rs/codex-api/src/common.rs` (311 LoC)
  - `ResponseEvent` at lines 71-111: normalized internal event enum emitted by provider streams. Covers created/completed, output items, text deltas, custom tool input deltas, reasoning deltas, rate limits, model metadata, and model verification recommendations.
  - `Reasoning` at lines 113-119: serializes Responses `reasoning` controls.
  - `TextFormatType`, `TextFormat`, `TextControls`, `OpenAiVerbosity` at lines 121-167: structured output and verbosity request controls. `TextFormat` is the JSON schema output format wrapper.
  - `ResponsesApiRequest` at lines 169-190: canonical HTTP `/responses` request payload. Key fields: `model`, `instructions`, `input: Vec<ResponseItem>`, `tools: Vec<Value>`, `tool_choice`, `parallel_tool_calls`, `reasoning`, `stream`, `include`, `text`, and `client_metadata`.
  - `ResponseCreateWsRequest`, `ResponseProcessedWsRequest`, `ResponsesWsRequest` at lines 215-277: Responses-over-WebSocket request envelopes.
  - `create_text_param_for_request` at lines 279-297: converts optional verbosity and `output_schema` into `TextControls { format: Some(TextFormat { type: json_schema, strict, schema, name }) }`.
  - `ResponseStream` at lines 299-311: stream wrapper around `mpsc::Receiver<Result<ResponseEvent, ApiError>>` plus optional upstream request id.

- `codex-rs/core/src/client.rs` (2176 LoC)
  - `build_responses_request` at lines 679-734: assembles the `ResponsesApiRequest` from a `Prompt`, including tool JSON via `create_tools_json_for_responses_api`, reasoning include flags, verbosity, output schema, prompt cache key, and installation metadata.
  - `stream_responses_api` at lines 1154-1280: turn-scoped HTTP Responses streaming. Handles SSE fixtures, auth recovery on 401, request telemetry, compression, and maps `codex_api::ResponseStream` into core stream events.
  - `stream` at lines 1509-1565: dispatches to Responses WebSocket if enabled, otherwise HTTP SSE.
  - `map_response_stream` / `map_response_events` at lines 1669-1835: converts API stream events to core stream events, records completed/failed/cancelled inference traces, tracks output items, and surfaces terminal errors.

### Response input/output item model

- `codex-rs/protocol/src/models.rs`
  - `ResponseInputItem` at lines 659-695: request-side input items sent back to Responses. Variants include `Message`, `FunctionCallOutput`, `McpToolCallOutput`, `CustomToolCallOutput`, and `ToolSearchOutput`.
  - `ContentItem` / `ImageDetail` / `MessagePhase` at lines 697-739: message content and assistant message phase model.
  - `ResponseItem` at lines 741-835 and continuing in the same enum: provider output items. Key variants visible here:
    - `Message` at lines 743-756
    - `Reasoning` at lines 757-767
    - `LocalShellCall` at lines 768-777
    - `FunctionCall` at lines 778-791. Arguments are intentionally retained as a raw JSON string.
    - `ToolSearchCall` at lines 792-803
    - `FunctionCallOutput` at lines 809-814
    - `CustomToolCall` at lines 815-826
    - `CustomToolCallOutput` begins at lines 830-835
  - `SearchToolCallParams` at lines 1248-1254.
  - `ShellToolCallParams` at lines 1256-1278 and `ShellCommandToolCallParams` at lines 1280-1304.
  - `FunctionCallOutputContentItem` at lines 1306-1322: structured tool output content items (`input_text`, `input_image`).
  - `function_call_output_content_items_to_text` at lines 1324-1354: lossy conversion for logging/legacy surfaces.
  - `FunctionCallOutputPayload` and `FunctionCallOutputBody` at lines 1374-1389: structured output payload for function/custom tool outputs. `FunctionCallOutputBody` is untagged text or content items.
  - `FunctionCallOutputBody::to_text` at lines 1391-1403.

### Tool specification types for Responses

- `codex-rs/tools/src/tool_spec.rs` (155 LoC)
  - `ToolSpec` at lines 13-53: serializes directly as Responses API tools. Variants: `function`, `namespace`, `tool_search`, `local_shell`, `image_generation`, `web_search`, and `custom` freeform.
  - `ToolSpec::name` at lines 55-67.
  - `create_tools_json_for_responses_api` at lines 97-111: serializes `ToolSpec` values into request JSON.
  - `ResponsesApiWebSearchFilters` / `ResponsesApiWebSearchUserLocation` at lines 113-151.

- `codex-rs/tools/src/responses_api.rs` (155 LoC)
  - `FreeformTool` and `FreeformToolFormat` at lines 11-23: freeform/custom tool format.
  - `ResponsesApiTool` at lines 25-38: function tool schema. Includes `name`, `description`, `strict`, optional `defer_loading`, `parameters`, and skipped `output_schema`.
  - `LoadableToolSpec`, `ResponsesApiNamespace`, `ResponsesApiNamespaceTool` at lines 40-67.
  - `dynamic_tool_to_responses_api_tool` / `dynamic_tool_to_loadable_tool_spec` at lines 69-90.
  - `coalesce_loadable_tool_specs` at lines 92-120.
  - `mcp_tool_to_responses_api_tool` / deferred variant at lines 122-140.
  - `tool_definition_to_responses_api_tool` at lines 142-151.

### SSE event wire parsing types

- `codex-rs/codex-api/src/sse/responses.rs` (1375 LoC)
  - `Error` at lines 122-130: partial Responses error payload.
  - `ResponseCompleted` / `ResponseCompletedUsage` at lines 132-149 and token usage conversion at lines 151-167.
  - `ResponsesStreamEvent` at lines 179-192: generic deserialization shape for SSE events. Fields include `type`, headers/metadata/response/item, item/call ids, delta, summary/content indices.
  - `ResponsesStreamEvent::response_model` at lines 199-218 and `model_verifications` at lines 220-229.
  - `ResponsesEventError` at lines 284-295.
  - `process_responses_event` at lines 297-431: maps raw Responses event kinds into `ResponseEvent`.

## 2. Tool Implementations

Tool dispatch is centered around:

- `codex-rs/core/src/tools/registry.rs`
  - `ToolHandler` trait at lines 45-104. Each handler declares name, optional model-visible spec, parallel support, kind, mutating heuristic, pre/post hook payloads, streamed argument diff consumer, and `handle`.
  - `ToolArgumentDiffConsumer` at lines 106-117.
  - `AnyToolResult::into_response` at lines 126-135.
- `codex-rs/core/src/tools/router.rs`
  - `ToolCall` at lines 31-36 and `ToolRouter` at lines 38-43.
  - `ToolRouter::from_config` at lines 54-100 builds specs and registry from config, MCP, dynamic, discoverable, and unavailable tools.
  - `build_tool_call` at lines 174-265 converts provider `ResponseItem` into executable `ToolCall`. It handles function/MCP calls, client `tool_search`, custom/freeform calls, and `local_shell`.
  - `dispatch_tool_call_with_code_mode_result` at lines 268-296 builds `ToolInvocation` and dispatches through the registry.
- `codex-rs/core/src/tools/context.rs`
  - `ToolInvocation` and `ToolPayload` at lines 47-78.
  - `ToolOutput` at lines 92-113.
  - Response shaping:
    - `McpToolOutput` at lines 140-211.
    - `ToolSearchOutput` at lines 214-253.
    - `FunctionToolOutput` at lines 255-304.
    - `ApplyPatchToolOutput` at lines 306-343.
    - `AbortedToolOutput` at lines 345-380.
    - `ExecCommandToolOutput` at lines 382-488.
    - `function_tool_response` at lines 548-573 chooses function vs custom tool output.
- `codex-rs/core/src/tools/handlers/mod.rs`
  - Module/export list at lines 1-31 and 47-76.
  - Shared `parse_arguments` at lines 78-85: `serde_json::from_str` into the handler args type, returning `FunctionCallError::RespondToModel`.
  - `parse_arguments_with_base_path` at lines 87-96, `resolve_workdir_base_path` at lines 98-108, and `resolve_tool_environment` at lines 110-129.

### Built-in handlers and adaptation notes

| Tool area | Runtime file and LoC | Key names | Validation | Output |
|---|---:|---|---|---|
| Unified exec / Bash-like command | `codex-rs/core/src/tools/handlers/unified_exec/exec_command.rs` (353) | `ExecCommandHandler`, `ExecCommandHandlerOptions`, `handle` lines 144-344 | Parses `ExecCommandEnvironmentArgs` at lines 165-180, then `ExecCommandArgs` with cwd base at lines 183-184. Resolves shell command at lines 193-200. Validates permission escalation and additional permissions at lines 215-270. Intercepts `apply_patch` at lines 272-296. | Returns `ExecCommandToolOutput` with chunk id, wall time, process id or exit code, raw output, token count, then `FunctionCallOutput` text via `context.rs` lines 405-414 and response text lines 461-487. |
| Unified stdin | `codex-rs/core/src/tools/handlers/unified_exec/write_stdin.rs` (110) | `WriteStdinHandler`, `WriteStdinArgs` | Parses `session_id`, chars, yield/max tokens at lines 20-30 and line 82. | Calls `unified_exec_manager.write_stdin` lines 85-97, emits `TerminalInteraction` lines 99-106, returns `ExecCommandToolOutput`. |
| Legacy shell / Bash | `codex-rs/core/src/tools/handlers/shell.rs` (312) plus `shell/local_shell.rs` (121), `shell/shell_handler.rs` (150), `shell/shell_command.rs` (249), `shell/container_exec.rs` (101) | `run_exec_like` lines 109-308, `LocalShellHandler` lines 23-121 | Shared path validates available environment, dependency/env policy, permission escalation, additional permissions, approval policy, and apply_patch interception at lines 124-209. `LocalShellHandler` accepts `ToolPayload::LocalShell`, converts to `ExecParams` at lines 98-118. | Uses `ToolOrchestrator` + `ShellRuntime`, emits begin/end events, returns `FunctionToolOutput` with formatted exec output and post-tool-use response at lines 293-307. |
| Apply patch / Edit/Write equivalent | `codex-rs/core/src/tools/handlers/apply_patch.rs` (611) | `ApplyPatchHandler`, `ApplyPatchArgumentDiffConsumer`, `intercept_apply_patch` | Supports function and custom/freeform payloads at lines 383-394. Verifies patch with `codex_apply_patch::maybe_parse_apply_patch_verified` lines 410-417. Computes file permissions at lines 419-420. Streaming diff parser validates partial deltas at lines 79-143. | Returns `ApplyPatchToolOutput` text. Direct or delegated runtime path emits patch events and applies via `ApplyPatchRuntime` lines 421-480. Shell interception path returns `FunctionToolOutput` lines 504-607. |
| Tool search | `codex-rs/core/src/tools/handlers/tool_search.rs` (442) | `ToolSearchHandler`, `search` | Requires `ToolPayload::ToolSearch`, trims non-empty query, requires positive limit at lines 81-103. Uses BM25 index built in `new` lines 31-49. | Returns `ToolSearchOutput { tools }` at lines 105-111; serialized as `ResponseInputItem::ToolSearchOutput` in `context.rs` lines 237-251. |
| MCP tool call | `codex-rs/core/src/tools/handlers/mcp.rs` (244) | `McpHandler`, `mcp_hook_tool_input` | Requires `ToolPayload::Mcp` at lines 79-90. Hook input best-effort parses raw JSON at lines 117-123. Actual validation delegates to `handle_mcp_tool_call`. | Wraps `CallToolResult` as `McpToolOutput` lines 107-113. Output is converted to function-call output with wall time and truncation in `context.rs` lines 181-211. |
| MCP resources | `codex-rs/core/src/tools/handlers/mcp_resource.rs` (325) plus `mcp_resource/list_mcp_resources.rs` (168), `list_mcp_resource_templates.rs` (170), `read_mcp_resource.rs` (151) | `ListMcpResourcesHandler`, `ListMcpResourceTemplatesHandler`, `ReadMcpResourceHandler` | Shared parse helpers: raw JSON optional parse at lines 284-297, typed `serde_json::from_value` at lines 299-320. | `serialize_function_output` JSON-serializes payloads into `FunctionToolOutput` at lines 271-282. |
| Dynamic app/client tools | `codex-rs/core/src/tools/handlers/dynamic.rs` (164) | `DynamicToolHandler`, `request_dynamic_tool` | Requires function payload lines 56-63, parses arbitrary JSON `Value` at line 65, waits for external dynamic response. | Maps `DynamicToolResponse.content_items` to Responses `FunctionCallOutputContentItem` lines 80-88. |
| Multi-agent v1 tools | `codex-rs/core/src/tools/handlers/multi_agents/*.rs` | `spawn_agent`, `send_input`, `resume_agent`, `wait_agent`, `close_agent` | Each parses typed args through shared `parse_arguments`. `spawn_agent` validates depth and incompatible fork overrides. `wait_agent` validates timeout > 0 and clamps at lines 83-91. | Each implements custom `ToolOutput` and returns JSON text through `multi_agents_common.rs`. See multi-agent section below. |
| Multi-agent v2 tools | `codex-rs/core/src/tools/handlers/multi_agents_v2/*.rs` | `spawn`, `send_message`, `message_tool`, `wait`, `close_agent`, `list_agents`, `followup_task` | Same registry pattern, but path/addressing is AgentPath oriented. | JSON tool outputs for list/wait/close/spawn, text for message/followup. |
| View image / Read image | `codex-rs/core/src/tools/handlers/view_image.rs` (277) | `ViewImageHandler`, `ViewImageArgs`, `ViewImageOutput` | Rejects if model lacks image modality lines 84-94. Parses path/environment/detail at lines 113-117. Valid detail values are constrained by comments and logic beginning line 118. | Returns `ResponseInputItem` containing image/text content; custom `to_response_item` starts at line 227. |
| Plan/update checklist | `codex-rs/core/src/tools/handlers/plan.rs` (99) | `PlanHandler`, `parse_update_plan_arguments` | Rejects in Plan mode lines 80-84. Parses `UpdatePlanArgs` via `serde_json::from_str` lines 95-98. | Emits `EventMsg::PlanUpdate` lines 86-89 and returns `PlanToolOutput`. |
| Request permissions | `codex-rs/core/src/tools/handlers/request_permissions.rs` (82) | `RequestPermissionsHandler` | Typed args via shared parser; validates request against session policy. | Returns text `FunctionToolOutput`. |
| Request plugin install | `codex-rs/core/src/tools/handlers/request_plugin_install.rs` (362) | `RequestPluginInstallHandler` | Typed args and allowlist-style install workflow validation. | Returns text `FunctionToolOutput`. |
| Request user input | `codex-rs/core/src/tools/handlers/request_user_input.rs` (92) | `RequestUserInputHandler` | Typed args, mode-gated. | Returns text `FunctionToolOutput`. |
| Goals | `codex-rs/core/src/tools/handlers/goal/*.rs`, `goal.rs` (165) | `CreateGoalHandler`, `GetGoalHandler`, `UpdateGoalHandler` | Typed JSON args. | Returns text `FunctionToolOutput`. |
| Code-mode nested tools | `codex-rs/core/src/tools/code_mode/execute_handler.rs` (124), `wait_handler.rs` (112) | `CodeModeExecuteHandler`, `CodeModeWaitHandler` | Parses code-mode runtime request args and coordinates runtime cell execution/waiting. | Returns `FunctionToolOutput`, often structured content items. |
| Agent jobs / CSV fanout | `codex-rs/core/src/tools/handlers/agent_jobs.rs` (764), submodules | `SpawnAgentsOnCsvHandler`, `ReportAgentJobResultHandler` | CSV and job-result validation in submodules. | Returns text `FunctionToolOutput`. |
| Unavailable/test sync | `unavailable_tool.rs` (71), `test_sync.rs` (167) | `UnavailableToolHandler`, `TestSyncHandler` | Simple function payload checks. | Error text or ok text. |

There are no first-class `Read`, `Write`, or `Edit` tools with those names in the current core registry. File reading generally happens through shell/unified exec or MCP resource tools; file writing/editing is intentionally represented by `apply_patch` or shell commands. Hook aliases map `Write` and `Edit` to `apply_patch` compatibility in `codex-rs/core/src/tools/hook_names.rs` lines 31-37.

## 3. Multi-Agent System

### Core types

- `codex-rs/core/src/agent/control.rs` (1258 LoC)
  - `SpawnAgentForkMode`, `SpawnAgentOptions`, `LiveAgent`, `ListedAgent` at lines 47-72.
  - `AgentControl` at lines 129-145: control-plane handle shared across the root/session agent tree. Holds a shared `SessionId`, weak `ThreadManagerState`, and an `Arc<AgentRegistry>`.
  - Constructor and session id helpers at lines 147-163.
  - `spawn_agent_with_metadata` at lines 183-193 and `spawn_agent_internal` at lines 195-341.
  - Forked spawn path at lines 343-447, including rollout filtering for forked history.
  - Resume path at lines 449-630.
  - Messaging and lifecycle:
    - `send_input` lines 633-653.
    - `send_inter_agent_communication` lines 671-692.
    - `interrupt_agent` lines 694-698.
    - `shutdown_live_agent` lines 713-733.
    - `close_agent` and descendant shutdown lines 735-760.
    - `get_status` lines 763-773.
    - `subscribe_status` lines 832-840.
    - `list_agents` lines 864-937.
  - Completion watcher at lines 939-1016: waits for final child status, then either sends `InterAgentCommunication` to parent for v2 agent paths or injects a user message into the parent thread.
  - `prepare_thread_spawn` at lines 1018-1056: reserves path/nickname, constructs `SessionSource::SubAgent(ThreadSpawn { ... })`, and records metadata.
  - Tree traversal and persistence helpers at lines 1104-1205.

- `codex-rs/core/src/agent/registry.rs` (344 LoC)
  - `AgentRegistry` at lines 16-26: shared per-root-session registry for active agents and total spawned count.
  - `ActiveAgents` at lines 28-33.
  - `AgentMetadata` at lines 35-42: `agent_id`, `agent_path`, nickname, role, and last task message.
  - Spawn slot reservation and max thread enforcement at lines 79-97 and atomic increment lines 275-291.
  - Root registration and lookup at lines 121-143.
  - Metadata/live agent listing/update at lines 145-181.
  - Spawn registration and nickname/path reservation at lines 183-259.
  - `SpawnReservation` RAII commit/drop at lines 294-340.

- `codex-rs/core/src/agent/mailbox.rs` (161 LoC)
  - `Mailbox` and `MailboxReceiver` at lines 11-20.
  - `Mailbox::new` lines 22-37 creates an unbounded channel plus watch sequence counter.
  - `subscribe` lines 39-41.
  - `send` lines 43-48: assigns monotonic sequence, sends `InterAgentCommunication`, updates watchers.
  - Receiver pending/drain helpers lines 51-71.

- `codex-rs/protocol/src/agent_path.rs` (240 LoC)
  - `AgentPath` at lines 9-15; root constants at lines 17-20.
  - `root`, `morpheus`, `from_string`, `as_str`, `is_root`, `name`, `join`, `resolve` at lines 22-72.
  - Validation:
    - `validate_agent_name` lines 125-147: lowercase ASCII letters, digits, underscore; no `/`, `.`, `..`, or `root`.
    - `validate_absolute_path` lines 149-171: must start `/root` or be `/morpheus`, no trailing slash.
    - `validate_relative_reference` lines 173-180.

- `codex-rs/protocol/src/protocol.rs`
  - `InterAgentCommunication` at lines 786-811: author path, recipient path, other recipients, content, and `trigger_turn`.
  - `AgentStatus` at lines 1668-1688: `PendingInit`, `Running`, `Interrupted`, `Completed(Option<String>)`, `Errored(String)`, `Shutdown`, `NotFound`.
  - `SessionSource` at lines 2500-2514 and `SubAgentSource::ThreadSpawn` at lines 2561-2579. Thread spawn stores parent thread id, depth, optional `AgentPath`, nickname, and role.
  - `SessionSource` accessors for nickname, role, path at lines 2625-2650.

### Multi-agent tool surface

- v1 tool files:
  - `codex-rs/core/src/tools/handlers/multi_agents/spawn.rs` (232 LoC)
    - `Handler` implements `spawn_agent` lines 25-197.
    - Args: `SpawnAgentArgs` lines 199-208 with `message`, `items`, `agent_type`, `model`, `reasoning_effort`, `fork_context`.
    - Builds child config, applies model/role/runtime overrides, constructs thread spawn source, calls `AgentControl::spawn_agent_with_metadata` lines 83-124, emits begin/end events lines 69-82 and 166-183.
    - Output `SpawnAgentResult` lines 210-232: `agent_id`, optional nickname.
  - `send_input.rs` (129 LoC)
    - Parses target and input lines 36-40, optional interrupt lines 46-52, calls `AgentControl::send_input` lines 67-71, emits interaction events lines 54-92.
  - `wait.rs` (262 LoC)
    - Parses target list and timeout lines 57-91.
    - Subscribes to each status receiver lines 107-143.
    - Waits until first final status or timeout lines 145-175.
    - Output `WaitAgentResult` lines 218-240 maps target string to `AgentStatus` plus `timed_out`.
  - `close_agent.rs` (135 LoC)
    - Parses target lines 35-37, gets status subscription lines 55-81, calls `AgentControl::close_agent` lines 82-85, returns previous status lines 103-111.
  - `resume_agent.rs` (186 LoC) resumes previous agent threads through `AgentControl`.

### Coordination model for Norn adaptation

- Spawning creates a new `CodexThread` through `ThreadManagerState`, not just a task. The child receives the same `AgentControl`, so registry and status coordination are shared inside one root session tree (`control.rs` lines 233-261).
- Spawn metadata is tracked independently of threads in `AgentRegistry`, with RAII reservation to enforce max thread count and prevent path collisions (`registry.rs` lines 79-97, 242-259, 294-340).
- Parent/child relationship is carried in `SessionSource::SubAgent(SubAgentSource::ThreadSpawn { parent_thread_id, depth, agent_path, ... })` (`protocol.rs` lines 2561-2579).
- Message delivery uses `ThreadManagerState::send_op` with either `Op::UserInput` or `Op::InterAgentCommunication` (`control.rs` lines 633-692).
- Status is watch-channel based per thread (`subscribe_status` lines 832-840), which makes `wait_agent` cheap and race-resistant.
- Agent paths are strict, absolute path-like identifiers (`/root/worker`) with relative resolution from the current agent (`agent_path.rs` lines 54-72).

## 4. Streaming / SSE for OpenAI Provider

### HTTP SSE endpoint

- `codex-rs/codex-api/src/endpoint/responses.rs` (153 LoC)
  - `ResponsesClient` at lines 26-29 and `ResponsesOptions` at lines 31-39.
  - `stream_request` at lines 70-100:
    - Serializes `ResponsesApiRequest` to JSON lines 84-85.
    - Azure item id attachment lines 86-88.
    - Adds client/session/subagent headers lines 90-97.
    - Calls lower-level `stream`.
  - `stream` at lines 117-152:
    - Sets `Accept: text/event-stream` lines 129-143.
    - Uses `EndpointSession::stream_with`.
    - Wraps response with `spawn_response_stream` lines 146-151.

- `codex-rs/codex-api/src/requests/responses.rs` (37 LoC)
  - `attach_item_ids` lines 11-37: injects original item ids for Azure Responses endpoint on stored items.

### SSE parser and error handling

- `codex-rs/codex-api/src/sse/responses.rs`
  - `spawn_response_stream` at lines 63-120:
    - Extracts rate limits, model etag, `openai-model`, `x-reasoning-included`, and `x-request-id` headers lines 69-88.
    - Emits metadata events before processing body lines 98-113.
  - `process_responses_event` at lines 297-431:
    - `response.output_item.done` -> deserialize `ResponseItem` lines 300-307.
    - `response.output_text.delta` lines 309-313.
    - `response.custom_tool_call_input.delta` -> `ToolCallInputDelta` lines 314-324.
    - reasoning summary/content deltas lines 325-340.
    - `response.created` lines 341-345.
    - `response.failed` error mapping lines 346-380.
    - `response.incomplete` lines 381-390.
    - `response.completed` parses id/usage/end_turn lines 392-408.
    - `response.output_item.added` lines 410-417.
    - `response.reasoning_summary_part.added` lines 418-423.
  - Error classification helpers:
    - `try_parse_retry_after` lines 521-545.
    - context/quota/usage/invalid/cyber/overload checks lines 547-569.
  - `process_sse` at lines 433-519:
    - Wraps byte stream with `eventsource()` line 439.
    - Enforces idle timeout with `tokio::time::timeout` lines 443-468.
    - Converts eventsource errors to `ApiError::Stream` lines 449-455.
    - Emits stream-closed-before-completed errors lines 456-461.
    - Ignores malformed individual SSE JSON events after logging lines 473-478.
    - Emits changing server model / model verification metadata lines 480-501.
    - Stores terminal `response.failed` error in `response_error` and returns it if stream later closes lines 503-516.
    - Returns on `ResponseEvent::Completed` lines 503-511.

### Retry/fallback/reconnection behavior

- HTTP SSE itself does not reconnect mid-stream. If the byte stream ends before `response.completed`, `process_sse` sends an error (`sse/responses.rs` lines 456-461), and `core/src/client.rs` records stream failure if mapped stream ends without terminal completion (`client.rs` lines 1821-1825).
- Auth retry exists for initial HTTP `/responses` request failures only:
  - `stream_responses_api` loops and handles HTTP 401 by invoking `handle_unauthorized`, setting `pending_retry`, and continuing lines 1198-1266.
  - Other request errors are mapped and returned lines 1267-1277.
- Responses WebSocket is preferred when enabled, then falls back to HTTP:
  - `responses_websocket_enabled` lines 736-748.
  - `stream` chooses WebSocket and falls back to HTTP on `FallbackToHttp` lines 1531-1565.
  - Warmup handles WebSocket fallback lines 1467-1506.
- API stream mapping tracks consumer cancellation and stream drop:
  - `map_response_events` uses a cancellation token at lines 1702-1728.
  - Completed events record token usage and inference trace lines 1748-1786.
  - Error events record request id and failed telemetry lines 1797-1818.
  - End-of-stream without completed records failed trace lines 1821-1825.

## Extraction Candidates for Norn

1. Best whole-cloth extraction candidates:
   - `codex-rs/protocol/src/models.rs` Responses item/output model subset.
   - `codex-rs/codex-api/src/common.rs` request/event/stream structs.
   - `codex-rs/codex-api/src/sse/responses.rs` SSE parser, with dependencies on `ApiError`, telemetry, and `TokenUsage` trimmed.
   - `codex-rs/tools/src/tool_spec.rs` and `responses_api.rs` for tool schema generation.
   - `codex-rs/core/src/tools/registry.rs`, `router.rs`, and `context.rs` as a reusable tool runtime skeleton.
   - `codex-rs/protocol/src/agent_path.rs`, `core/src/agent/registry.rs`, `mailbox.rs` for multi-agent addressing/registry/mailbox.

2. Best adaptation candidates:
   - `ExecCommandHandler` and `ShellRuntime` stack: powerful but tightly coupled to Codex approvals, sandboxing, hooks, telemetry, and unified exec process manager.
   - `ApplyPatchHandler`: good patch verification and streaming progress model, but depends on `codex_apply_patch`, sandbox policy, event emitters, and orchestrator.
   - `AgentControl`: strong architecture for thread-backed agents, but tied to Codex `ThreadManagerState`, rollout persistence, config snapshots, and app-server analytics.

3. Important design patterns to preserve:
   - Keep provider output item parsing separate from tool dispatch (`codex-api` -> `core/src/client.rs` -> `ToolRouter`).
   - Keep tool outputs responsible for their own Responses `ResponseInputItem` conversion (`ToolOutput::to_response_item`).
   - Use typed args with `serde_json` at handler boundaries and return model-visible validation errors via `FunctionCallError::RespondToModel`.
   - Use watch channels for agent status and monotonic sequence watches for mailbox notifications.
