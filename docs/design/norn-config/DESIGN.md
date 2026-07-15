---
type: design
cluster: norn-config
title: "Norn Configuration: Layered Settings, Paths, and Shared Utilities"
---

# Norn Configuration: Layered Settings, Paths, and Shared Utilities

## Intention

When this work is done, Norn has a configuration system that works the way
infrastructure should: predictable layering, no surprises, no hidden state.
A user sets a preference once and it applies everywhere. A project overrides
that preference where it needs to. A CLI flag overrides both for a single
invocation. Nothing is hardcoded that should be configurable. Nothing is
configurable that should be hardcoded.

The experience should feel like a tool that knows its environment: it finds
settings without being told where to look, respects the user's choices
without requiring repetition, and makes the right thing happen by default
while allowing explicit override at every level.

## Problem

Norn has no configuration system. Everything is either a CLI flag, a profile
field, or a hardcoded constant:

- **No settings file.** Every configurable value requires a CLI flag or a
  profile entry. There is no `settings.json` equivalent.

- **No per-project config.** There is no `.norn/` directory convention for
  project-level settings. Tool permissions, skill paths, MCP server
  definitions, and hook registrations are all CLI-only.

- **No config merging.** If a user wants different settings per project,
  they must pass different flags every time.

- **Hardcoded values that should be configurable.** Default model, rate
  limits, prompt command timeout, REPL history capacity, Claude Runner
  binary path, and 10+ other values are compiled into constants with no
  override surface.

- **Duplicated frontmatter parsing.** `split_frontmatter` exists in
  `profile/loader.rs` and `rules/parser.rs`. Skills will need a third copy.
  This is a cross-cutting utility that belongs in a shared location.

- **Path resolution lives in norn-cli.** `norn_dir()`, `profiles_dir()`,
  `session_data_dir()` are in `norn-cli/src/config/paths.rs`. Profile
  resolution already moved to libnorn (NA-002 D4), but path helpers did not
  follow. Anything that needs path resolution from within libnorn cannot
  use the CLI-only helpers.

- **Bugs from inconsistent path resolution.** REPL history ignores
  `NORN_HOME` because it hardcodes `dirs::home_dir().join(".norn")` instead
  of using the centralised path helper.

The other three norn clusters (skills, context, hooks) all depend on a
config schema and directory layout that does not exist yet.

## Solution

### D1: JSON settings files at two levels

Settings live in JSON files at user level and project level:

- **User:** `~/.norn/settings.json`
- **Project:** `.norn/settings.json` (committed to git)
- **Local:** `.norn/settings.local.json` (gitignored, personal overrides)

JSON is chosen over TOML because: (a) `serde_json` is already a dependency,
no new crate needed; (b) matches the Claude Code reference model for
operational familiarity; (c) profiles already serve the human-friendly
editing surface via markdown with YAML frontmatter.

Rejected alternative: TOML for settings. More human-friendly for hand
editing but adds a dependency at the config layer when profiles already
provide the user-facing editing experience.

### D2: Five-layer precedence

Configuration values resolve in this order (highest wins):

1. **Compiled defaults** (in code, the fallback floor)
2. **User settings** (`~/.norn/settings.json`)
3. **Project settings** (`.norn/settings.json`)
4. **Local settings** (`.norn/settings.local.json`)
5. **CLI flags** (`--model`, `-c key=value`, `--allowed-tools`, etc.)

Environment variables (`NORN_HOME`, `OPENAI_API_KEY`) operate outside this
chain — they control runtime behaviour and auth, not settings.

### D3: Merge strategy per field type

- **Scalars** (model, timeout, etc.): higher-precedence wins, lower is
  discarded.
- **Permission deny arrays**: union across all layers. You cannot un-deny
  at project level what user level denies. Deny is additive and absolute.
- **Permission allow arrays**: concatenate across layers, deduplicate.
  Project can add to user's allow list.
- **Hooks**: the typed merge operation concatenates hook groups by event type,
  but runtime loading rejects every non-empty project/local hook slot before
  merge. Only user settings and programmatic construction currently grant hook
  execution authority. Repository hooks require a future explicit-consent
  design; precedence alone is not consent.
- **MCP servers**: merge by name. Project servers add to user servers.
  Same-name project server overrides user server definition.
- **Tool config**: deep merge. Project `tools.write.max_code_lines`
  overrides user's value for that key, but user's `tools.write.length_overrides`
  is preserved if the project does not set it.

### D4: Two permission systems — capability vs consent

Two distinct permission systems serve different purposes:

**Capability boundary (profile).** Controls which tools the model sees.
Profile `tools` array gates the `ToolRegistry` via `set_available` in
`from_profile`. The model literally cannot call a tool that is filtered out.
This stays on the profile where it is today. `--allowed-tools` and
`--disallowed-tools` CLI flags override the profile, not settings.

**Consent boundary (settings).** Controls whether a tool call executes.
The model can call the tool, but the runtime may block it, auto-allow it,
or require confirmation based on pattern matching. Settings `permissions`
section with `allow`, `deny`, and `ask` arrays. Rule syntax follows the
Claude Code pattern: `tool_name`, `tool_name(pattern)`, wildcards.
Evaluation order: deny > ask > allow (first match wins, deny takes
precedence).

These are not unified because they serve different audiences: capability is
model-facing (what the agent can do), consent is operator-facing (what the
operator trusts).

### D5: Hook config aligned to existing traits

The settings `hooks` section maps one-to-one with the five existing
`HookRegistry` traits:

- `pre_tool` — `PreToolHook::before_tool`. Matcher: tool name pattern.
  Can block (exit code 2).
- `post_tool` — `PostToolHook::after_tool`. Matcher: tool name pattern.
  Observation only.
- `pre_llm` — `PreLlmHook::before_llm`. No matcher (fires on every LLM
  call). Can block.
- `post_llm` — `PostLlmHook::after_llm`. No matcher. Observation only.
- `session_event` — `SessionEventHook::on_event`. Matcher: event type
  pattern. Observation only.

Each hook entry is `{ matcher: string, command: string, timeout: u64 }`.
Commands are inline shell strings, not file references. No `hooks/`
directory at either level. If a user wants a script, they reference it by
path in the `command` field.

The config module constructs `HookRegistry` entries from the settings at
load time. Bender's hooks cluster owns the trait implementations and
execution; this cluster owns the config surface that feeds them.

### D6: Tool config via namespaced keys

Per-tool configuration uses a `tools` section with sub-objects keyed by
tool name:

```json
{
  "tools": {
    "write": {
      "max_code_lines": null,
      "length_overrides": []
    },
    "bash": {},
    "edit": {}
  }
}
```

This matches the existing `Profile.settings["tool_config"]["write"]`
pattern (used by `wiring.rs` for `LengthLimit`). The profile remains the
per-profile layer; settings provides the global default. CLI `-c write.max_code_lines=N`
is the per-invocation override.

Precedence: compiled default < settings `tools.*` < profile `tool_config.*`
< CLI `-c tool.key=value`.

### D7: Reasoning config in agent settings

`reasoning_effort` and `reasoning_summary` belong in the `agent` section
of settings alongside `max_turns`, `step_timeout`, and `schema_budget`.

Precedence: compiled default (None) < settings `agent.reasoning_effort` <
profile `reasoning_effort` < CLI `--reasoning-effort`.

Profile wins over settings because reasoning config is model-specific — a
high-reasoning profile for complex work should not be overridden by a global
low-reasoning default.

### D8: Duration format via humantime

Duration fields in settings.json are strings parsed by `humantime::parse_duration`.
Already a dependency (`Cargo.toml`). Accepts `30s`, `2m`, `1h`, `100ms`,
`1h30m`. Consistent with the existing `-c timeout=30s` parsing in
`assembly.rs:58`.

Rejected alternative: integer seconds. Less expressive, inconsistent with
existing CLI surface.

### D9: Path resolution migrates to libnorn

`norn_dir()`, `profiles_dir()`, `session_data_dir()` move from
`norn-cli/src/config/paths.rs` to `norn/src/config/paths.rs`. All
consumers (profile loader, session manager, task store, REPL history)
use the centralised version. `NORN_HOME` is honoured everywhere.

norn-cli retains thin wrappers if needed for CLI-specific concerns but
delegates to `norn::config::paths`.

### D10: Shared frontmatter utility

`split_frontmatter` is extracted to `norn/src/util/frontmatter.rs`. The
profile loader (`profile/loader.rs`) and rules parser (`rules/parser.rs`)
both delegate to this shared implementation. Skills will use it too.

The function signature returns `Result<(&str, &str), FrontmatterError>`
where `FrontmatterError` is a small enum (`MissingOpening`, `MissingClosing`).
Callers map to their own error types (`ConfigError`, `RulesError`).

Rejected alternative: leave duplicated. Three copies with subtly different
error types is a maintenance and correctness risk.

### D11: No auto-creation of directories

Directories are created lazily on first write, not eagerly on startup.
`~/.norn/` is not created by loading settings (it may not exist for a
first-time user). `.norn/` is not created by reading project config (it
may not exist for most projects). This matches the `DiskTaskStore` pattern
established in NA-004.

### D12: Auth mode in settings, secrets in env

Settings `provider.auth` selects the authentication mode (`"oauth"`,
`"api_key"`); no aliases are accepted. The actual secret (API key, token) is
never stored in settings files. API keys come from the environment variable
named by `provider.api_key_env`. OAuth tokens come from Norn-owned
`$NORN_HOME/auth/auth.json` storage (default `~/.norn/auth/auth.json`).

`auth` and `api_key_env` form one precedence unit. A higher-precedence explicit
`auth: "oauth"` clears an inherited lower-layer API-key source. An OAuth mode
and API-key source supplied by the same or a higher layer remain together so
runtime validation rejects the contradictory configuration. Explicit
`auth: "api_key"` retains the effective `api_key_env` left by lower layers;
it does not resurrect a source already cleared by an intervening OAuth layer.
There is no blank-value clearing syntax.

### D13: Existing `-c` overrides as highest-precedence CLI layer

The 23-key `-c key=value` system in `assembly.rs` is preserved. It provides
per-invocation overrides that sit above all settings layers. Settings files
provide defaults; `-c` provides the escape hatch. This matches the Claude
Code model where CLI flags override settings.json.

### D14: Remove unused `config/` subdirectory

`~/.norn/config/` exists in the path system but is currently unused
(profiles moved to `~/.norn/profiles/` per NA-002). The `config_dir()`
helper and `CONFIG_SUBDIR` constant are removed. Settings files live
directly under `~/.norn/`, not in a config subdirectory.

## Goals

G1. `~/.norn/settings.json` is loaded, parsed, validated, and merged with
project-level settings before the runtime is assembled.

G2. All 23 existing `-c key=value` overrides can be expressed as settings
file fields instead of CLI flags, with CLI flags taking precedence.

G3. Tool permissions (consent boundary) are configurable via settings files
with deny > ask > allow evaluation.

G4. Path resolution is available from libnorn, not just norn-cli.

G5. The `split_frontmatter` utility exists in one location and is used by
profiles, rules, and skills.

G6. No hardcoded values that should be configurable remain without a
settings-file override surface.

## Non-Goals

NG1. **Hook trait implementations.** This cluster provides the config
surface for hooks, not the trait implementations or execution. That is
Bender's hooks cluster.

NG2. **Context directory structure.** This cluster provides `context.search_paths`
in settings. The internal structure of context is Harry's context cluster.

NG3. **Skill loading logic.** This cluster provides `skills.search_paths`
in settings. Skill discovery and activation are a separate cluster.

NG4. **MCP server lifecycle.** This cluster provides MCP server definitions
in settings. Connection management, process supervision, and protocol
handling are separate concerns.

NG5. **Profile format changes.** The profile system (markdown+YAML
frontmatter, TOML, JSON) is unchanged. This cluster provides the settings
layer that sits below profiles in the precedence chain.

NG6. **TUI-specific configuration.** The `tui` section in settings is
reserved but not populated. Chop Suey's TUI cluster defines its own config
needs.

## Structure

```
crates/norn/
├── src/
│   ├── config/
│   │   ├── mod.rs              — pub mod + re-exports
│   │   ├── types.rs            — NornSettings, ProviderSettings, AgentSettings,
│   │   │                         PermissionSettings, HookSettings, ToolSettings
│   │   ├── loader.rs           — file discovery, JSON parsing, layer merging
│   │   ├── paths.rs            — directory resolution (migrated from norn-cli)
│   │   ├── merge.rs            — field-by-field merge logic across layers
│   │   └── validate.rs         — cross-field validation
│   ├── util/
│   │   ├── mod.rs              — pub mod + re-exports
│   │   └── frontmatter.rs      — shared split_frontmatter + FrontmatterError
│   └── ...

crates/norn-cli/
├── src/
│   ├── config/
│   │   ├── paths.rs            — MODIFIED: delegates to norn::config::paths
│   │   ├── assembly.rs         — MODIFIED: reads settings before CLI overrides
│   │   ├── profile_loader.rs   — MODIFIED: uses norn::config::paths
│   │   └── ...
│   ├── runtime/
│   │   ├── builder.rs          — MODIFIED: loads settings, merges, passes to runtime
│   │   └── ...
│   └── ...
```

## Current Inventory

### Path Resolution (norn-cli, to be migrated)

| Helper | Current Location | Target |
|--------|-----------------|--------|
| `norn_dir()` | `norn-cli/src/config/paths.rs:35` | `norn/src/config/paths.rs` |
| `config_dir()` | `norn-cli/src/config/paths.rs:50` | Removed (D14) |
| `profiles_dir()` | `norn-cli/src/config/paths.rs:60` | `norn/src/config/paths.rs` |
| `session_data_dir()` | `norn-cli/src/config/paths.rs:71` | `norn/src/config/paths.rs` |
| `NORN_HOME` env var | `norn-cli/src/config/paths.rs:24` | `norn/src/config/paths.rs` |

### Frontmatter Implementations (to be unified)

| Location | Returns | Error Type |
|----------|---------|------------|
| `profile/loader.rs:209` | `Result<(&str, &str), ConfigError>` | `ConfigError::InvalidConfig` |
| `rules/parser.rs:53` | `Result<(String, String), RulesError>` | `RulesError::ParseFailed` |

### ConfigOverrides Keys (23, with settings-field equivalents)

| Key | Settings Path | Type |
|-----|--------------|------|
| `timeout` | `agent.step_timeout` | duration |
| `max_turns` | `agent.max_turns` | u32 |
| `schema_budget` | `agent.schema_budget` | u32 |
| `context_window` | `agent.context_window` | u64 |
| `auto_compact_reserve_tokens` | `agent.auto_compact_reserve_tokens` | u64 or `off` |
| `compact_keep_turns` | `agent.compact_keep_turns` | usize |
| `delegation_depth` | `agent.delegation_depth` | u32 |
| `conversation_state` | `agent.conversation_state` | enum |
| `server_compaction_threshold_tokens` | `agent.server_compaction_threshold_tokens` | u64 |
| `index_lock_deadline_ms` | `agent.index_lock_deadline_ms` | u64 |
| `base_url` | `provider.base_url` | string |
| `max_retries` | `provider.max_retries` | u32 |
| `request_timeout` | `provider.timeout` | duration |
| `rate_limit_interval` | `provider.rate_limit_interval` | duration |
| `retry_backoff` | `provider.retry_backoff` | duration |
| `retry_after_ceiling` | `provider.retry_after_ceiling` | duration |
| `retry_max` | `retry.max_retries` | u32 |
| `retry_base_delay` | `retry.base_delay` | duration |
| `provider_options` | `provider.options` | JSON object |
| `api_key_env` | `provider.api_key_env` | environment variable name |
| `auth` | `provider.auth` | `oauth` or `api_key` |
| `write.max_code_lines` | `tools.write.max_code_lines` | usize |
| `debug_api` | `provider.debug_dump_dir` | path |

### Hardcoded Values (to gain settings overrides)

| Value | Current Location | Settings Path |
|-------|-----------------|--------------|
| Default model `"gpt-5.6-sol"` | `assets/models.json` | `model` |
| Schema budget `3` | `loop/config.rs:92` | `agent.schema_budget` |
| Compact keep turns `10` | `loop/config.rs:98` | `agent.compact_keep_turns` |
| Retry max `2` | `loop/retry.rs:16` | `retry.max_retries` |
| Retry backoff `1s` | `loop/retry.rs:18` | `retry.base_delay` |
| Prompt command timeout `5s` | `loop/loop_context.rs:39` | `agent.prompt_command_timeout` |
| Rate limit `60 req/min` | `openai/mod.rs:26` | `provider.rate_limit` |
| Claude Runner binary `"claude"` | `print/provider.rs:144` | `provider.runner_path` |
| REPL history capacity `1000` | `repl/history.rs:18` | `session.history_capacity` |

## Constraints

- CO1: No `.unwrap()` or `.expect()` in library code (workspace standard).
- CO2: All files under 500 lines of code (excluding tests, comments,
  whitespace).
- CO3: Settings file format is JSON. No TOML, no YAML at the settings layer.
- CO4: Duration strings parsed by `humantime` (existing dependency).
- CO5: Auth secrets never stored in settings files. Environment variables or
  helper scripts only.
- CO6: Deny permissions are additive across layers — you cannot un-deny.
- CO7: No auto-creation of directories on read operations.
- CO8: `NORN_HOME` honoured by all path resolution, including REPL history.
- CO9: Settings types defined in norn crate. CLI wiring stays in norn-cli.

## Security boundary addendum (2026-07-11)

The original merge rules describe value precedence, not authority. P0 adds a
source-aware validation step before project/local values merge:

- `<cwd>/.norn/settings.json` and `settings.local.json` are untrusted even when
  gitignored. They cannot set provider `base_url`, `api_key_env`, `auth`,
  `debug_dump_dir`, `runner_path`, free-form `options`, or provider-profile
  `api_shape`.
- Project model aliases cannot select `provider_profile`/`api_shape`, collide
  with and activate a trusted backend-bearing alias/profile, or make a CWD
  default/workspace-profile model activate a trusted backend bundle. Explicit
  CLI selection remains trusted.
- Every non-empty project/local hook slot is rejected. Project variants cannot
  set `prompt_file`. Project skill policy may narrow shell execution to `false`
  but cannot enable it.
- A profile selected by model output cannot contain `prompt_commands`, even when
  the profile came from trusted user configuration. Operator/programmatic
  selection and model selection are different authority paths.
- `HOME` and `NORN_HOME` must be absolute before they can anchor trusted
  config/credentials. Relative user prompt/search paths are rejected rather
  than interpreted against a repository CWD. A future explicitly selected,
  read-only `$CODEX_HOME/auth.json` source must satisfy the same absolute-root
  rule; `CODEX_HOME` is not Norn's default or fallback credential authority.

These are intentional confused-deputy closures and compatibility breaks.
Ordinary layer precedence is never repository consent.

`mcp_servers` (including its `env` map) is currently merged but dormant in the
production runtime. This design does not authorize a future consumer to launch
it. Runtime wiring must first add source provenance, explicit consent, collision
rules, and credential/redaction tests.

Credential-bearing resolved/runtime/request config uses structural redacted
`Debug`, including free-form request options. The raw legacy provider-settings
container still derives `Debug`; callers must treat it as sensitive and must not
log it. Removing that misuse-prone residual remains a separate reviewed change.
