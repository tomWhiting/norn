# Provider Backends

Norn has three compatibility `--provider` choices today:

| Provider flag | API surface | Auth | Main use |
| --- | --- | --- | --- |
| `--provider openai` | OpenAI Responses | Codex OAuth by default; API key when `api_key_env` is set | Default subscription-backed GPT models, or OpenAI Responses API with an API key |
| `--provider openai-compatible` | OpenAI-compatible Chat Completions | API key env var | Local and third-party servers such as LM Studio, Ollama, llama.cpp server, vLLM, or hosted compatible APIs |
| `--provider claude-runner` | Claude Code CLI adapter | Claude CLI session | Claude Code subscription path |

New configuration should prefer API-shape terminology:

| API shape | Existing runtime | Status |
| --- | --- | --- |
| `openai-responses` | OpenAI Responses provider | implemented |
| `openai-chat-completions` | OpenAI-compatible Chat Completions provider | implemented |
| `anthropic-messages` | Anthropic Messages provider | reserved, not implemented |
| `openai-harmony` | Harmony/gpt-oss prompt format | reserved, not implemented |
| `lmstudio-native` | LM Studio native API | reserved, not implemented |
| `agent-rpc` | Agent adapter/RPC process | reserved, not implemented |
| `agent-client-protocol` | Agent Client Protocol | reserved, not implemented |

The same top-level flags work in print/headless mode and in the TUI. Use `-p`
for headless print mode; omit it for the TUI.

## Model Selection

The model is selected with `-m` / `--model`, or by the active profile/settings
when no CLI model override is supplied.

Norn passes the model string through to the selected backend. It does not
currently auto-discover local models or rewrite local model names. For
OpenAI-compatible servers, use exactly the model identifier that the server
exposes in its UI or `/v1/models` response.

Examples:

```bash
norn -p --provider openai -m gpt-5.5 "Summarise this repository"
```

```bash
norn -p --provider openai-compatible -m qwen2.5-coder:14b \
  -c base_url=http://127.0.0.1:11434/v1 \
  "Summarise this repository"
```

```bash
norn --provider openai-compatible -m local-model-name \
  -c base_url=http://127.0.0.1:1234/v1
```

The local examples above are deliberately generic. For LM Studio, Ollama,
llama.cpp, vLLM, or another server, copy the base URL and model name from that
server's own OpenAI-compatible API screen or model list.

## API Shape and Provider Profiles

`--api-shape` selects the wire API shape. `--provider-profile` selects a named
deployment profile from settings:

```json
{
  "provider_profiles": {
    "lmstudio": {
      "api_shape": "openai_chat_completions",
      "base_url": "http://127.0.0.1:1234/v1",
      "api_key_env": "NORN_OPENAI_COMPAT_API_KEY"
    },
    "openai_api": {
      "api_shape": "openai_responses",
      "api_key_env": "OPENAI_API_KEY"
    }
  }
}
```

Then:

```bash
norn -p --provider-profile lmstudio -m google/gemma-4-e4b "Reply with one sentence."
```

or, without a named profile:

```bash
norn -p --api-shape openai-chat-completions \
  -m google/gemma-4-e4b \
  -c base_url=http://127.0.0.1:1234/v1
```

Top-level `provider` settings still act as defaults. A selected
`provider_profiles.<name>` entry overrides those defaults, and CLI `-c`
overrides both.

## Model Aliases

Bundled models can define short aliases in `assets/models.json`. For example,
`norn -p -m sol "hi"` resolves `sol` to the canonical model id
`gpt-5.6-sol` before provider selection and capability validation.

Settings can also define custom model aliases. A plain string alias changes
only the model id:

```json
{
  "model_aliases": {
    "55": "gpt-5.5"
  }
}
```

Object aliases can select the model and backend together:

```json
{
  "model_aliases": {
    "local": {
      "provider_profile": "lmstudio",
      "model": "google/gemma-4-e4b"
    }
  },
  "provider_profiles": {
    "lmstudio": {
      "api_shape": "openai_chat_completions",
      "base_url": "http://127.0.0.1:1234/v1",
      "api_key_env": "NORN_OPENAI_COMPAT_API_KEY"
    }
  }
}
```

Then `norn -p -m local "hi"` resolves to model
`google/gemma-4-e4b` on the `lmstudio` profile.

Resolution order is:

1. exact bundled model id;
2. a user-defined settings alias;
3. a bundled catalog alias; and
4. an unknown model id passed through unchanged.

Exact bundled model ids therefore cannot be shadowed, while settings aliases
can intentionally override a bundled short alias.

## OpenAI-Compatible Chat Completions

Use this backend when the target server implements the OpenAI Chat Completions
shape:

```text
POST {base_url}/chat/completions
```

That means `base_url` is the API prefix, not the full endpoint. If a server
documents or displays:

```text
http://127.0.0.1:1234/v1/chat/completions
```

configure:

```bash
-c base_url=http://127.0.0.1:1234/v1
```

Do not include `/chat/completions` in `base_url`; Norn appends that path.

### API Key

The OpenAI-compatible backend requires a non-empty API key environment
variable. By default it reads:

```bash
NORN_OPENAI_COMPAT_API_KEY
```

For local servers that do not enforce auth, set a dummy value:

```bash
export NORN_OPENAI_COMPAT_API_KEY=dummy
```

For hosted compatible APIs, point Norn at the env var that contains the real
key:

```bash
export LOCAL_AI_KEY=sk-...
norn -p --provider openai-compatible \
  -m provider-model-name \
  -c base_url=https://provider.example/v1 \
  -c api_key_env=LOCAL_AI_KEY \
  "Run a smoke test"
```

Norn stores only the env var name in settings, never the key value.

### Settings File

The equivalent settings shape is:

```json
{
  "model": "local-model-name",
  "provider": {
    "base_url": "http://127.0.0.1:1234/v1",
    "api_key_env": "NORN_OPENAI_COMPAT_API_KEY"
  }
}
```

CLI `-c` values override settings values for the same field.

### Advanced Chat Completions Fields

Use `provider.options.api_options.openai_chat_completions` in settings, or
`-c provider_options=...` on the CLI, for shape-specific fields that Norn does
not expose as first-class flags:

```json
{
  "provider": {
    "options": {
      "api_options": {
        "openai_chat_completions": {
          "logprobs": true,
          "top_logprobs": 5,
          "seed": 7,
          "response_format": { "type": "json_object" }
        }
      }
    }
  }
}
```

Norn rejects overrides for fields it owns for correctness, including `model`,
`messages`, `tools`, `stream`, `functions`, and `function_call`.

## OpenAI Responses

The default `openai` compatibility provider and `--api-shape openai-responses`
use the Responses wire shape:

```text
POST {base_url}/responses
```

Without `api_key_env`, Norn uses Codex/ChatGPT OAuth credentials. With
`api_key_env`, it reads that environment variable and uses API-key auth:

```json
{
  "provider_profiles": {
    "openai_api": {
      "api_shape": "openai_responses",
      "api_key_env": "OPENAI_API_KEY"
    }
  }
}
```

```bash
OPENAI_API_KEY=sk-... \
norn -p --provider-profile openai_api -m gpt-5.5 "hi"
```

Advanced Responses fields use the Responses option scope:

```json
{
  "provider": {
    "options": {
      "api_options": {
        "openai_responses": {
          "temperature": 0.2,
          "max_output_tokens": 2048,
          "text": { "format": { "type": "json_object" } }
        }
      }
    }
  }
}
```

Norn rejects overrides for fields it owns, including `model`, `instructions`,
`input`, `tools`, `stream`, `store`, `include`, `reasoning`,
`previous_response_id`, and context-management fields.

## Feature Differences

Backends do not have identical capabilities.

The default `openai` backend uses the Responses API and can expose
Responses-specific behavior such as hosted web search, response threading, and
server-side context management when the selected auth/backend supports it.

The `openai-compatible` backend intentionally uses the simpler Chat
Completions shape. It sends local function tools as Chat Completions tools and
maps streamed text/tool-call deltas back into Norn events. It does not send
Responses-only fields such as:

- `store`
- `previous_response_id`
- `context_management`
- `prompt_cache_key`
- hosted tool definitions

Reasoning effort is catalog-gated. `/effort` and `--reasoning-effort` should be
used only for model/backend pairs that declare supported effort levels. Generic
local model ids are treated as unsupported until the model catalog or provider
profile grows explicit capability metadata for them.

`--fast` / `--service-tier fast` is catalog-backed. It should be used only for
models whose selected provider/backend declares a fast service tier. For
generic local OpenAI-compatible models, leave it off unless the model catalog
has been extended for that backend.

## Practical Setup Checklist

1. Start the target server and confirm its OpenAI-compatible API prefix.
2. Confirm the exact model identifier the server exposes.
3. Export an API key env var, or a dummy non-empty value for local unauthenticated servers.
4. Run a small headless smoke test:

```bash
NORN_OPENAI_COMPAT_API_KEY=dummy \
norn -p --provider openai-compatible \
  -m local-model-name \
  -c base_url=http://127.0.0.1:1234/v1 \
  --allowed-tools read \
  "Reply with one sentence."
```

5. If the smoke test works, broaden the tool list as the model/context window
   allows, then use the same flags without `-p` for the TUI.

The default Norn agent surface includes runtime instructions plus the full tool
catalog and JSON schemas. That is correct for large coding models, but it can
exceed the context window of small local models before the user prompt is even
considered. For first contact with LM Studio, Ollama, llama.cpp, or similar
servers, start with a narrow tool surface such as `--allowed-tools read` or a
small profile-specific tool list.

If the server reports a context-length error, reduce the tool list, load the
model with a larger context length, or use a larger-context model. Passing
`--system-prompt ""` removes your profile/system prompt text, but it does not
remove Norn's runtime instructions or tool schemas.

## Library Use

Library callers can construct the provider directly instead of going through
CLI flags. The OpenAI-compatible provider expects a `ProviderConfig` with:

- `auth_source: AuthSource::ApiKey { ... }`
- `base_url: Some(".../v1")`
- timeout/retry/rate-limit values as needed

The model still lives on each `ProviderRequest`. The provider does not validate
that model name locally; the remote server is the source of truth.
