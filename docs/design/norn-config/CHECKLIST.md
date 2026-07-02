# Norn-Config — Checklist

## Settings Types and Schema

- [ ] **C1** — NornSettings struct defined in norn::config::types with all top-level sections: model, provider, agent, retry, permissions, hooks, tools, mcp_servers, skills, session, tui, env.
- [ ] **C2** — ProviderSettings struct with fields: base_url, timeout, max_retries, options, auth, rate_limit, runner_path, debug_dump_dir.
- [ ] **C3** — AgentSettings struct with fields: max_turns, step_timeout, schema_budget, context_window, compact_threshold, compact_keep_turns, reasoning_effort, reasoning_summary, prompt_command_timeout.
- [ ] **C4** — RetrySettings struct with fields: max_retries, base_delay, backoff_multiplier.
- [ ] **C5** — PermissionSettings struct with fields: allow (Vec<String>), deny (Vec<String>), ask (Vec<String>).
- [ ] **C6** — HookSettings struct with fields matching the five HookRegistry traits: pre_tool, post_tool, pre_llm, post_llm, session_event.
- [ ] **C7** — HookEntry struct with fields: matcher (Option<String>), command (String), timeout (Option<u64>).
- [ ] **C8** — ToolSettings struct with namespaced sub-objects: write, bash, edit (each Option, open to future tools).
- [ ] **C9** — All duration fields in settings structs are Option<String> deserialized via humantime::parse_duration.
- [ ] **C10** — All settings structs derive Serialize, Deserialize, Debug, Default. Default yields all-None/empty fields (no assumed defaults).

## Settings Loading and Merging

- [ ] **C11** — load_settings() discovers and parses settings from up to three files: ~/.norn/settings.json, .norn/settings.json, .norn/settings.local.json.
- [ ] **C12** — Missing settings files are treated as empty (all-None) — no error on absent file.
- [ ] **C13** — Malformed JSON in a settings file returns a typed error with the file path and parse error, not a panic.
- [ ] **C14** — merge_settings() implements five-layer precedence: compiled defaults < user settings < project settings < local settings < CLI overrides.
- [ ] **C15** — Scalar fields: higher-precedence non-None value wins.
- [ ] **C16** — Permission deny arrays: union across all layers (additive, cannot un-deny).
- [ ] **C17** — Permission allow arrays: concatenate across layers, deduplicate.
- [ ] **C18** — Hook arrays: merge by event type. Project hooks extend user hooks, not replace.
- [ ] **C19** — MCP servers: merge by name. Same-name project server overrides user server.
- [ ] **C20** — Tool config: deep merge. Specific key override, sibling keys preserved.

## Path Resolution Migration

- [ ] **C21** — norn_dir() defined in norn::config::paths, returns Option<PathBuf>, honours NORN_HOME env var.
- [ ] **C22** — profiles_dir() defined in norn::config::paths, returns Option<PathBuf>.
- [ ] **C23** — session_data_dir() defined in norn::config::paths, returns Option<PathBuf> (not Result, not panic).
- [ ] **C24** — settings_file() defined in norn::config::paths for user-level settings path (~/.norn/settings.json).
- [ ] **C25** — norn-cli paths.rs delegates to norn::config::paths for all path resolution.
- [ ] **C26** — REPL history path uses norn::config::paths::norn_dir(), honouring NORN_HOME.
- [ ] **C27** — config_dir() helper and CONFIG_SUBDIR constant removed from norn-cli paths.rs.

## Shared Frontmatter Utility

- [ ] **C28** — split_frontmatter defined in norn::util::frontmatter, returns Result<(&str, &str), FrontmatterError>.
- [ ] **C29** — FrontmatterError enum with MissingOpening and MissingClosing variants.
- [ ] **C30** — profile/loader.rs delegates to norn::util::frontmatter::split_frontmatter.
- [ ] **C31** — rules/parser.rs delegates to norn::util::frontmatter::split_frontmatter.
- [ ] **C32** — No duplicate split_frontmatter implementations remain in the codebase.

## Settings Validation

- [ ] **C33** — Duration strings validated via humantime::parse_duration at load time. Invalid duration produces a typed error naming the field and value.
- [ ] **C34** — Permission patterns validated at load time (syntactically well-formed tool patterns).
- [ ] **C35** — Unknown top-level keys in settings.json emit tracing::warn, not an error (forward compatibility).

## Builder Integration

- [ ] **C36** — build_runtime loads and merges settings before constructing the RuntimeBundle.
- [ ] **C37** — Merged settings populate AgentLoopConfig fields that were previously only CLI-settable.
- [ ] **C38** — Merged settings populate ProviderConfigOverrides fields that were previously only CLI-settable.
- [ ] **C39** — Merged settings populate RetryPolicy fields that were previously only CLI-settable.
- [ ] **C40** — CLI -c overrides take precedence over settings file values.
- [ ] **C41** — Profile reasoning_effort takes precedence over settings agent.reasoning_effort.

## Downstream Config Surfaces

- [ ] **C42** — skills.search_paths in settings provides additional skill search directories.
- [ ] **C43** — context.search_paths in settings provides context discovery directories.
- [ ] **C44** — session.history_capacity in settings overrides the REPL history capacity.
- [ ] **C45** — provider.rate_limit in settings overrides the hardcoded 60 req/min (Tom-decision value for default).

## Linting and Quality

- [ ] **C46** — cargo clippy -p norn -p norn-cli -- -D warnings passes clean.
- [ ] **C47** — No file exceeds 500 lines of code (excluding tests, comments, whitespace).
- [ ] **C48** — No .unwrap() or .expect() in library code (norn crate).
