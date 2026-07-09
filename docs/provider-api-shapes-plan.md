# Provider API Shapes Plan

## Purpose

Norn needs to support more than one way of talking to models without making the
CLI, TUI, model catalog, and provider runtime pretend that every backend has
OpenAI Responses semantics.

The current `--provider` flag is doing too much. It can mean:

- the organization or product being used, such as OpenAI, Anthropic, LM Studio,
  Ollama, vLLM, or an internal server;
- the authentication/deployment path, such as Codex subscription, OpenAI API
  key, Claude Code subscription, or a local unauthenticated server;
- the wire/API shape, such as OpenAI Responses, OpenAI-compatible Chat
  Completions, Anthropic Messages, or an agent RPC protocol;
- the model capability set, such as reasoning controls, context window, hosted
  tools, image input, service tiers, and streaming event types.

Those are separate axes and should be modeled separately.

## Current Observations

Norn already has the beginning of this split in `assets/models.json`:

- top-level provider: `openai`;
- backend/deployment: `codex_subscription`, `responses_api`,
  `openai_compatible_chat`;
- API surface: `codex_responses`, `responses`, `chat_completions`;
- backend-specific model capability metadata.

That direction is correct. The next iteration should make the naming and runtime
shape explicit enough that the CLI, TUI, library, and catalog all resolve the
same concept.

The local LM Studio test exposed a practical version of the problem:

- the OpenAI-compatible Chat Completions request path worked with a reduced tool
  surface;
- the full Norn runtime prompt and tool schema set exceeded the local model's
  effective context budget;
- LM Studio also has its own native REST API shape, separate from its
  OpenAI-compatible endpoints.

This is not just a one-off local-model problem. Provider-specific assumptions
already leak into UX surfaces such as help text and reasoning-effort controls.
Those controls need to be capability driven, not hardcoded around the Codex GPT
model set.

## Source Terminology

Primary docs checked while writing this plan:

- OpenAI exposes both `/v1/responses` and `/v1/chat/completions` as separate
  API endpoints in its API spec.
- OpenAI Harmony is a prompt/response format for gpt-oss style open-weight
  inference. OpenAI describes it as a format that inference providers may need
  to map to underneath a Chat Completions, Responses, or other API surface.
- LM Studio documents both native `/api/v1/*` REST endpoints and
  OpenAI-compatible endpoints, including `/v1/responses`,
  `/v1/chat/completions`, and `/v1/completions`.
- Anthropic Messages is a distinct API shape with its own controls for tools,
  extended thinking, effort, context management, and streaming.
- Agent Client Protocol is not a model-provider API. It is a protocol between
  editors/surfaces and coding agents. It belongs in the "agent surface /
  transport" layer, not the model invocation layer.

## Recommended Mental Model

Use four explicit concepts.

### API Shape

The protocol or wire contract Norn serializes to and streams back from.

Initial first-class shapes:

| ID | Meaning | Status |
| --- | --- | --- |
| `openai_responses` | OpenAI Responses-style request/response and SSE events | Existing for OpenAI/Codex path |
| `openai_chat_completions` | OpenAI-compatible Chat Completions request/stream shape | Existing as local/third-party backend |
| `anthropic_messages` | Anthropic Messages request/stream shape | Planned |

Deferred shapes:

| ID | Meaning | Recommendation |
| --- | --- | --- |
| `openai_harmony` | Harmony prompt/response formatting for direct gpt-oss inference | Defer; important if Norn ever talks directly to an inference engine or tokenizer-level runtime |
| `lmstudio_native` | LM Studio native `/api/v1/chat` and model-management endpoints | Useful after the generic OpenAI-compatible path is stable |
| `agent_rpc` | Process/RPC adapter for CLI/SDK-backed agents such as Claude Code or other agent runtimes | Treat as adapter process boundary, not raw HTTP templating |
| `agent_client_protocol` | ACP integration between Norn and editor/agent surfaces | Separate surface layer; do not model as an LLM provider API |

### Provider Profile

The configured deployment/auth target.

Examples:

| Provider profile | API shape | Auth/deployment |
| --- | --- | --- |
| `openai_api` | `openai_responses` | OpenAI API key |
| `codex_subscription` | `openai_responses` or `codex_responses` | ChatGPT/Codex OAuth |
| `openai_compatible_local` | `openai_chat_completions` | Local base URL plus dummy/non-empty env key |
| `lmstudio_openai_compat` | `openai_chat_completions` or `openai_responses` | LM Studio OpenAI-compatible server |
| `lmstudio_native` | `lmstudio_native` | LM Studio native REST API |
| `anthropic_api` | `anthropic_messages` | Anthropic API key |
| `claude_code_subscription` | `agent_rpc` or dedicated adapter | Claude Code binary/session |
| `internal_gateway` | selected by config | Company/provider gateway |

A provider profile owns base URL, auth source, headers, retry policy, timeout
policy, and provider-specific defaults. It should not imply a single model
capability set.

### Model Capability Profile

The capabilities for a specific model through a specific provider profile and
API shape.

This key matters:

```text
(provider_profile, api_shape, model_id)
```

The same model name can have different limits and controls when reached through
different backends. For example, a hosted Responses backend can support
threading or hosted tools that an OpenAI-compatible local server cannot.

Capability metadata should include:

- context window and max output tokens;
- input modalities;
- output modalities;
- tool support and tool-call dialect;
- parallel tool-call support;
- hosted tool support;
- reasoning controls;
- reasoning summary support;
- service tiers;
- image detail controls;
- response threading support;
- server-side context management support;
- prompt caching support;
- stream event dialect;
- token usage reporting;
- recommended Norn tool profile or max tool-schema budget.

Reasoning controls should be typed, not hardcoded:

```text
none
openai_effort(values: none|minimal|low|medium|high|xhigh|max)
anthropic_effort(values: provider-defined)
anthropic_thinking_budget(tokens or auto)
custom(name, allowed_values)
```

This lets `/effort`, help text, autocomplete, and CLI validation show only what
the selected model/backend actually supports.

### Model Aliases

Users should be able to define short local aliases for long or frequently used
model IDs.

Examples:

```json
{
  "model_aliases": {
    "55": {
      "provider_profile": "codex_subscription",
      "api_shape": "openai_responses",
      "model": "gpt-5.5"
    },
    "spark": {
      "provider_profile": "codex_subscription",
      "api_shape": "openai_responses",
      "model": "gpt-5.3-codex-spark"
    },
    "local": {
      "provider_profile": "lmstudio_local",
      "api_shape": "openai_chat_completions",
      "model": "google/gemma-4-e4b"
    }
  }
}
```

Alias resolution rules:

- aliases can come from bundled model metadata or user configuration;
- aliases resolve before provider runtime construction;
- user aliases can point to a full backend selection, not just a model string;
- exact model IDs win, followed by user aliases and then bundled aliases;
- duplicate aliases across merged config files currently follow normal settings
  precedence: higher layers replace lower layers by alias name;
- origin-aware duplicate diagnostics would be useful later, but should be added
  across the settings loader rather than only for model aliases;
- TUI model pickers and help should show aliases beside their resolved model.

### Provider Runtime

The Rust implementation that turns Norn's internal request/event model into the
selected API shape.

The runtime should convert between:

```text
Norn ProviderRequest
  -> API-shape request body / stream request
  -> provider stream events
  -> Norn ProviderEvent
```

Everything above the provider runtime should consume Norn's internal
capability/event model rather than knowing whether the model came from OpenAI,
Anthropic, LM Studio, Ollama, Claude Code, or an internal gateway.

## Field-Complete API Options

Each API shape needs two layers of options:

1. Norn-owned normalized options that the agent loop understands.
2. API-shape-specific options that are serialized to the provider request.

Norn should not flatten every provider into the smallest common denominator.
That would lose important controls for local/open-source and hosted models.

For OpenAI-compatible Chat Completions, the implementation goal is field
complete support for the current OpenAI Chat Completions request shape, plus a
safe path for provider-specific compatible extensions.

Important examples include:

- `logprobs` and `top_logprobs`, for monitored decoding and token-level
  inspection;
- `logit_bias`, `seed`, `n`, `stop`, `temperature`, `top_p`,
  `presence_penalty`, and `frequency_penalty`;
- `max_completion_tokens` and legacy `max_tokens` handling;
- `response_format`, including JSON mode and JSON schema;
- `tools`, `tool_choice`, `parallel_tool_calls`, and deprecated
  `functions`/`function_call` compatibility where needed;
- `stream`, `stream_options`, and usage-in-stream behavior;
- `store`, `metadata`, and stored-completion retrieval fields where supported;
- `modalities`, `audio`, image input, and other multimodal fields when the
  selected model/backend declares support;
- model-specific fields such as reasoning effort, verbosity, service tier,
  prompt-cache controls, prediction/static content, and web-search options when
  the API shape and backend expose them.

This should be enforced mechanically. For every supported API shape, maintain a
field coverage matrix generated from or checked against the authoritative schema.
Every request field must be classified as one of:

| Classification | Meaning |
| --- | --- |
| `core` | Norn sets it directly from normalized request state |
| `typed_option` | Exposed as a typed config/CLI/library option |
| `capability_gated` | Exposed only when selected model/backend declares support |
| `passthrough` | Accepted under an API-shape-specific options bag and serialized unchanged |
| `unsupported` | Rejected loudly with a documented reason |

Unclassified fields should fail tests. This prevents silent loss when OpenAI,
Anthropic, LM Studio, or another provider adds a useful control.

The pass-through bag should be scoped by API shape, for example:

```json
{
  "api_options": {
    "openai_chat_completions": {
      "logprobs": true,
      "top_logprobs": 5,
      "seed": 1234
    }
  }
}
```

Pass-through does not mean unvalidated raw request mutation. It means:

- only JSON-object request fields for the selected API shape;
- no override of fields Norn must own for correctness, such as `model`,
  `messages`, `tools`, `stream`, or tool-result correlation, unless explicitly
  marked safe;
- no secret interpolation except typed auth sources;
- unknown fields allowed only when the provider profile permits compatible
  extensions;
- debug dumps clearly show normalized options and pass-through options.

## CLI and Config Direction

The CLI now splits the overloaded provider flag for the implemented OpenAI
Responses and OpenAI-compatible Chat Completions paths. `--provider` remains as
a compatibility alias.

Current spelling:

```bash
norn --api-shape openai-chat-completions \
  --provider-profile lmstudio \
  -m google/gemma-4-e4b \
  -c base_url=http://127.0.0.1:1234/v1
```

```bash
norn --api-shape openai-responses \
  --provider-profile openai-api \
  -m gpt-5.5
```

```bash
norn --api-shape anthropic-messages \
  --provider-profile anthropic-api \
  -m claude-sonnet-...
```

Keep backward-compatible aliases during migration:

| Current flag | Compatibility mapping |
| --- | --- |
| `--provider openai` | current default OpenAI/Codex Responses profile |
| `--provider openai-compatible` | `--api-shape openai-chat-completions --provider-profile openai-compatible` |
| `--provider claude-runner` | Claude Code adapter/provider profile |

Do not remove the current spellings until config migration, docs, TUI help, and
library constructors have all moved to the new terminology.

Implemented now:

- `--api-shape openai-responses` maps to the existing OpenAI Responses runtime.
- `--api-shape openai-chat-completions` maps to the existing
  OpenAI-compatible Chat Completions runtime.
- `settings.provider_profiles.<name>` can provide `api_shape` plus the same
  connection fields as top-level `provider`.
- `settings.model_aliases` supports both string aliases and object aliases that
  select `provider_profile`, `api_shape`, and `model`.
- Bundled `assets/models.json` aliases resolve on the startup `-m` / `--model`
  path after exact model IDs and user-defined aliases.
- OpenAI Responses can use API-key auth when the selected provider settings set
  `api_key_env`; otherwise it keeps the Codex/ChatGPT OAuth path.
- Shape-scoped provider options pass through under
  `api_options.openai_responses` and
  `api_options.openai_chat_completions`, with Norn-owned fields protected.

Still reserved:

- `anthropic-messages`;
- `openai-harmony`;
- `lmstudio-native`;
- `agent-rpc`;
- `agent-client-protocol`.

## Catalog Shape

The catalog should separate API-shape definitions from provider profiles and
model capabilities.

Illustrative shape:

```json
{
  "schema_version": 2,
  "default": {
    "provider_profile": "codex_subscription",
    "api_shape": "openai_responses",
    "model": "gpt-5.5"
  },
  "model_aliases": {
    "55": {
      "provider_profile": "codex_subscription",
      "api_shape": "openai_responses",
      "model": "gpt-5.5"
    }
  },
  "api_shapes": {
    "openai_responses": {
      "display_name": "OpenAI Responses",
      "runtime": "openai_responses"
    },
    "openai_chat_completions": {
      "display_name": "OpenAI-compatible Chat Completions",
      "runtime": "openai_chat_completions"
    },
    "anthropic_messages": {
      "display_name": "Anthropic Messages",
      "runtime": "anthropic_messages"
    }
  },
  "provider_profiles": {
    "lmstudio_local": {
      "display_name": "LM Studio local server",
      "provider": "lmstudio",
      "api_shapes": ["openai_chat_completions", "openai_responses", "lmstudio_native"],
      "auth": "api_key_env_optional",
      "base_url": "http://127.0.0.1:1234/v1"
    }
  },
  "models": {
    "lmstudio_local:openai_chat_completions:google/gemma-4-e4b": {
      "model": "google/gemma-4-e4b",
      "context_window": 32768,
      "supports_tools": false,
      "recommended_tool_profile": "minimal"
    }
  }
}
```

The exact JSON can be cleaner than this, but the identity must preserve the
three-part key.

## Custom Provider Support

There are three levels of custom support. They should not be treated as equally
safe.

### Level 1: Custom Provider Profile Over a Supported API Shape

This should be the main path.

Users provide:

- `api_shape`;
- `base_url`;
- auth env var;
- model IDs;
- capability metadata.

This covers most local and hosted gateways, including LM Studio, Ollama,
llama.cpp server, vLLM, LiteLLM, OpenRouter-style gateways, company proxies, and
specialized hosted inference services.

This is safe because the serialization/parser code remains owned by Norn.

### Level 2: Declarative Request/Response Mapping

Possible, but experimental.

Users provide a constrained mapping from a custom HTTP shape into Norn's
internal request/event model. This should be schema-validated, versioned, and
limited.

Constraints:

- no arbitrary code execution;
- no unrestricted raw request mutation in core provider paths;
- no secret interpolation except through typed auth sources;
- no tool calling until the mapper can prove stable tool-call identity,
  argument streaming, and terminal events;
- explicit capability declaration required;
- loud failure for unknown event shapes.

This is useful for unusual gateways, but it should not become the primary
extension mechanism.

### Level 3: Adapter Process / RPC Provider

This is the right path for Claude Code, agent SDK wrappers, Pi-like agent RPCs,
and future non-HTTP agent runtimes.

Instead of trying to mutate raw requests behind a binary's back, Norn should run
or connect to an adapter with a typed contract:

```text
initialize -> capabilities
start_turn(request) -> stream events
send_tool_result(...)
cancel(...)
shutdown(...)
```

The adapter may internally call Claude Code, an SDK, an agent binary, or a
remote service. Norn still receives typed events and capabilities.

## ACP Position

ACP should be treated as a surface integration protocol, not as an LLM provider
shape.

There are two plausible future directions:

1. Norn as an ACP agent server so editors can connect to Norn.
2. Norn as an ACP client for delegating work to another ACP-compatible coding
   agent.

Both are useful, but they live beside the provider runtime rather than inside
it. ACP coordinates an agent with an editor/surface; it does not replace the
model provider abstraction underneath Norn's own agent loop.

## Tool Surface and Context Budget

The provider/API split will not be enough unless tool and prompt assembly become
capability-aware.

The local LM Studio failure came from a large runtime prompt plus a full tool
schema set. That is correct for large coding models, but wrong for many local
models. Norn should add a prompt/tool budget planner that can select:

- full coding tool surface;
- minimal read/search shell-free surface;
- no-tool chat surface;
- profile-defined tool groups;
- model-specific schema budget caps.

The planner should fail loudly when the requested tool surface cannot fit the
declared context window. It should not silently drop critical tools.

## TUI and Help Requirements

The TUI must stop hardcoding GPT/Codex-specific assumptions.

Capability-driven surfaces:

- `/model` should list models from the selected provider profile and user model
  catalog.
- `/model` should accept user and bundled aliases and show the resolved
  model/backend before switching.
- `/effort` should appear only when the active model/backend exposes a reasoning
  control.
- `/fast` and `/service-tier` should appear only when the active model/backend
  exposes service tiers.
- help text should name the active API shape and provider profile rather than
  implying every backend is Codex/OpenAI Responses.
- status badges should reflect actual selected capabilities.
- unsupported flags should fail with a typed, actionable error.

## Migration Plan

### Phase 0: Lock Terminology

- Rename "API surface" to "API shape" in docs and new code.
- Keep existing config keys readable.
- Add docs explaining the four concepts: API shape, provider profile, model
  capability profile, provider runtime.

### Phase 1: Internal Types

- Add explicit `ApiShape`, `ProviderProfileId`, and `ModelCapabilityProfile`
  types.
- Add explicit `ModelAlias` resolution before backend construction.
- Resolve runtime config into a single immutable selection:

```text
ResolvedBackend {
  api_shape,
  provider_profile,
  provider_runtime,
  model,
  capabilities
}
```

- Ensure library callers can construct the same structure without CLI parsing.

### Phase 2: Catalog v2

- Extend `assets/models.json` to separate API shapes, provider profiles, and
  model capabilities.
- Add user-defined model aliases in settings/profile config.
- Generate Rust code from the catalog at build time or via the existing
  generation path, whichever matches the repository convention.
- Add compatibility loading for schema v1.

### Phase 3: CLI Compatibility Layer

- Add `--api-shape` and `--provider-profile`.
- Keep `--provider` as a compatibility alias.
- Emit a warning only when a deprecated spelling is ambiguous.
- Update completions and help output.

### Phase 4: Capability-Gated UX

- Update TUI commands, help, autocomplete, and status rendering to use resolved
  capabilities.
- Remove GPT-specific hardcoded model and reasoning help.
- Add focused tests for unsupported effort/service-tier combinations.

### Phase 5: Provider Implementations

Recommended order:

1. stabilize `openai_chat_completions`;
2. add `anthropic_messages`;
3. add OpenAI API-key Responses profile separately from Codex subscription;
4. add LM Studio native only if the OpenAI-compatible path cannot expose the
   needed stateful/local features;
5. defer Harmony until direct gpt-oss inference or a tokenizer-level runtime is
   a real target;
6. treat Claude Code and other agents as adapter/RPC providers, not raw request
   patch targets.

### Phase 6: Custom Extension Layer

- Support user-defined provider profiles over known API shapes first.
- Add an experimental declarative mapper only after the typed provider/event IR
  is stable.
- Add adapter-process support for agent runtimes before allowing broad custom
  HTTP mappings.

## Non-Negotiable Invariants

- Never infer model capabilities from provider name alone.
- Never silently reinterpret a model alias as a provider model ID when both
  exist.
- Never show or accept `/effort`, `/fast`, hosted tools, or image controls unless
  the active model/backend declares support.
- Never silently drop tools to fit a context window.
- Never silently drop provider request fields. Every API-shape field must be
  classified as core, typed option, capability-gated, pass-through, or
  unsupported.
- Never pass provider-specific fields to a backend that does not declare support.
- Never let custom provider config execute code inside the Norn process.
- Always map provider stream errors into typed, retry-aware Norn errors.
- Preserve old sessions and config files through compatibility loading.

## Test Plan

Required test areas:

- config resolution for old and new CLI spellings;
- model alias resolution, ambiguity diagnostics, and TUI display;
- catalog v1 compatibility and v2 loading;
- `(provider_profile, api_shape, model_id)` capability lookup;
- API-shape field coverage matrix checks against authoritative schemas;
- `openai_chat_completions` request serialization for advanced fields such as
  `logprobs`, `top_logprobs`, `logit_bias`, `seed`, `response_format`,
  `stream_options`, and service/reasoning/cache controls;
- rejected unsupported reasoning/service-tier flags;
- TUI help and slash-command gating;
- provider request bodies for Responses, Chat Completions, and Anthropic
  Messages;
- stream parsing for text, thinking, tool calls, terminal events, and error
  frames;
- local-model narrow tool-surface smoke path;
- context-budget planner behavior for small context windows;
- library construction without CLI flags.

Standard gates:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
git diff --check
```

## Open Decisions

- Should the user-facing term be `api_shape`, `wire_api`, or `protocol`?
  Recommendation: `api_shape` in config/code, with help text saying "wire API
  shape."
- Should `--provider` remain forever as a high-level convenience alias?
  Recommendation: yes, if it resolves unambiguously to a provider profile.
- Should LM Studio native be first-class soon?
  Recommendation: not before OpenAI-compatible and Anthropic Messages are solid,
  unless stateful LM Studio chat or model load/unload control becomes a core
  requirement.
- Should custom HTTP mapping be supported?
  Recommendation: yes eventually, but only as an experimental declarative layer.
  The safer near-term extension point is custom provider profiles over known API
  shapes.
- Should ACP be part of provider selection?
  Recommendation: no. ACP belongs to agent/editor surface integration.

## References

- OpenAI Responses API: `https://api.openai.com/v1/responses`
- OpenAI Chat Completions API: `https://api.openai.com/v1/chat/completions`
- OpenAI Harmony Response Format:
  `https://developers.openai.com/cookbook/articles/openai-harmony`
- LM Studio REST API:
  `https://lmstudio.ai/docs/developer/rest`
- LM Studio OpenAI-compatible endpoints:
  `https://lmstudio.ai/docs/developer/openai-compat`
- LM Studio native chat endpoint:
  `https://lmstudio.ai/docs/developer/rest/chat`
- Anthropic Messages API:
  `https://platform.claude.com/docs/en/build-with-claude/working-with-messages`
- Anthropic features overview:
  `https://platform.claude.com/docs/en/build-with-claude/overview`
- Agent Client Protocol:
  `https://agentclientprotocol.com/get-started/introduction`
- ACP project:
  `https://github.com/agentclientprotocol/agent-client-protocol`
