# Responses API implementation review (2026-07-10)

**Status:** Review complete for the snapshot below, including the P0 follow-up
threat-model and Responses event-matrix passes completed on 2026-07-11. The P0
findings now have an implementation candidate tracked in the remediation plan;
closure awaits machine verification, residual-risk disposition, the P0 phase
gate, and owner-arranged independent review.

**Reviewed snapshot:** `main` at `263cc4f`.

**Primary scope:** the OpenAI Responses implementation used by both the ChatGPT
/ Codex subscription login and the direct Responses API. The review covers
credential routing, request construction, prompt roles, prompt caching,
conversation state, streaming events, response-item persistence and replay,
usage accounting, tool/schema serialization, working-directory file and command
authority, session-artifact confidentiality, and automation that can bypass the
provider credential boundary.

**Sources:** current Norn source and tests, the OpenAI Responses and prompt-cache
documentation as published on 2026-07-10, and the current `openai/codex` request,
SSE, and protocol-model source. Public API details are time-sensitive and should
be rechecked when remediation begins.

---

## Executive verdict

Norn has several strong Responses-specific foundations. It uses top-level
`instructions` for the stable System prompt, sends Developer and tool items in
the Responses `input` array, keeps function/custom call IDs distinct, uses
`store: false` for the ChatGPT/Codex subscription backend, requests encrypted
reasoning for stateless replay, and now places volatile per-iteration context at
the request tail rather than ahead of all history.

The central weakness is architectural: Norn still models a Responses transcript
as a Chat-Completions-like sequence of flat messages. A Responses turn is an
ordered array of typed output items. Norn reduces it to one text string, a group
of reasoning items, and a group of local tool calls, then synthesizes a new wire
transcript on the next request. That is not lossless enough for the ChatGPT
`store: false` path. It drops assistant `phase`, item ordering, refusals,
annotations, hosted-tool records, and most current or future item types.

After the credential boundary, the highest-priority correction is therefore not
"add every missing SSE event." It is to make an ordered, durable Responses item
transcript canonical and derive Norn's display text and executable local calls
from that transcript.

The July 9 prompt-cache fix closes the known GPT-5.5 prefix-placement defect, but
its benefit has not been demonstrated on the wire. It is specifically uncertain
for GPT-5.6, whose default implicit cache breakpoint is the latest message. In
Norn, that latest message is volatile and is removed and recreated on the next
iteration. Norn also reports every GPT-5.6 cache write as zero, so the current
telemetry cannot establish whether the new layout saves money or creates
billable cache writes that are never reused.

The P0 follow-up also widened the security boundary beyond provider settings.
Norn automatically reads repository context, rules, profiles, capabilities,
skills, variants, and conventions. Several paths followed symlinks, re-resolved a
mutable working directory, or could translate repository data into a subprocess.
Session and spool artifacts can also contain full prompts, tool results, and
process output. P0 is therefore a workspace-authority and private-artifact phase,
not only an endpoint allowlist.

---

## Finding index

| ID | Severity | Status | Finding |
|---|---|---|---|
| `SEC-01` | Critical | P0 candidate; review pending | Project-controlled `base_url` can redirect a Codex OAuth bearer token and account header. |
| `SEC-02` | Critical | P0 candidate; review pending | Working-directory settings can select an arbitrary environment secret and send it to a selected endpoint. |
| `SEC-07` | Critical | P0 candidate; review pending | Working-directory hooks, rule `shell_source`, and profile prompt commands can execute with inherited credentials before provider containment matters. |
| `SEC-12` | Critical | P0 candidate; review pending | Workspace skills and `CONVENTIONS.toml` can translate repository content into shell/LSP/diagnostic processes. |
| `SEC-13` | Critical | P0 candidate; review pending | A model-selected trusted profile can execute its `prompt_commands`, turning model choice into ambient process authority. |
| `SEC-03` | High | P0 candidate; review pending | Working-directory settings can silently enable raw API dumps at a selected filesystem path. |
| `SEC-04` | High | P0 candidate; review pending | Working-directory settings can choose the executable launched by the Claude Runner provider. |
| `SEC-05` | High | P0 candidate; review pending | The public `test-utils` feature exposes arbitrary OAuth token-authority constructors. |
| `SEC-09` | High | P0 candidate; review pending | Working-directory variant `prompt_file` can eagerly read an arbitrary file outside the workspace. |
| `SEC-11` | High | P0 candidate; review pending | Automatically discovered workspace files can escape or change trust roots through symlinks, aliases, and repeated path resolution. |
| `SEC-14` | High | P0 candidate; review pending | Project `provider.options`, profile `api_shape`, and cross-layer name collisions can select request/backend authority indirectly. |
| `SEC-15` | High | Partially addressed; P0 blocker | Session logs, full-output spools, process spools, locks, and temporary artifacts are not uniformly private against permissive umasks or hostile links. |
| `STATE-01` | High | Open | Stateless ChatGPT replay is lossy and changes the provider's ordered output-item transcript. |
| `STATE-02` | High | Open | Replaceable Developer context accumulates server-side under `previous_response_id`. |
| `STATE-03` | High | Open | A stored Responses thread can lose its reasoning state when local compaction resets the anchor and replays history. |
| `ROLE-01` | High | Open | Repository context is promoted inconsistently to System or Developer authority instead of one explicit lower-trust role policy. |
| `EVT-01` | High | Open | A refusal can become a successful empty response. |
| `EVT-02` | High | Open | Assistant `phase`, message boundaries, content indices, and item ordering are destroyed. |
| `EVT-03` | High | Open | Hosted web-search actions, sources, and annotations are discarded and not replayed. |
| `EVT-04` | High | Open | Complete text/item events cannot repair a dropped or malformed delta. |
| `EVT-06` | High | Open | Tool completions are merged by arrival/order rather than stable output-item identity, permitting cross-binding and duplicate application. |
| `EVT-07` | High | Open | Incomplete or malformed call/terminal/reasoning events can default into executable calls, empty content, zero usage, or ordinary success. |
| `REQ-01` | High | Open | A tool-backed slash command forges an assistant call in history but does not dispatch the tool or persist a matching result. |
| `CODEX-01` | High | Open | ChatGPT/Codex `end_turn` is ignored. |
| `CODEX-02` | High | Open | ChatGPT/Codex `x-codex-turn-state` is ignored. |
| `TRANS-01` | High | Open | Cancellation drops the consumer future but can leave the detached HTTP task running. |
| `TRANS-02` | High | Open | In-stream rate limits bypass both the HTTP-429 retry loop and the default loop retry policy. |
| `CACHE-01` | High | Unproven | The tail-placement cache win is not established for GPT-5.6 implicit breakpoints. |
| `CACHE-02` | High | Open | GPT-5.6 `cache_write_tokens` and cache-write cost are recorded as zero. |
| `EVT-05` | Medium | Open | Unknown actionable item variants fail open and may be silently omitted. |
| `CACHE-03` | Medium | Open | Ephemeral roots, `--no-session`, spawned agents, and forks can omit `prompt_cache_key`. |
| `CACHE-04` | Medium | Open | Per-iteration variable expansion can mutate tool definitions and invalidate the whole prefix. |
| `CACHE-05` | Medium | Open | Current GPT-5.6 cache controls and content breakpoints have no typed representation. |
| `MODEL-01` | Medium | Open | Catalog reasoning defaults/capabilities and newer reasoning controls are not resolved before wire construction. |
| `ROLE-02` | Medium | Open | The compatible Chat Completions serializer silently collapses Developer messages into System messages. |
| `TOOL-01` | Medium | Open | Catalog `apply_patch_tool_type` and `web_search_tool_type` values do not control the tool envelopes sent on the wire. |
| `BACKEND-01` | Medium | P0 candidate; review pending | An explicit ChatGPT URL is classified as the direct API rather than the Codex backend. |
| `CONFIG-01` | Medium | Open | `provider.auth` is declared and merged but ignored when runtime authentication is selected. |
| `SEC-06` | Medium | P0 candidate; review pending | Credential claims, response metadata, and OAuth authority error bodies can leak through diagnostics. |
| `SEC-08` | Medium | P0 candidate; review pending | A working-directory model or alias can activate a backend-selecting alias and trusted credential bundle. |
| `SEC-10` | Medium | P0 candidate; review pending | A working-directory setting can re-enable skill shell expansion over a trusted user restriction. |
| `SCHEMA-01` | Medium | Open | Schema downleveling can drop root `$defs` while retaining dangling `$ref` values. |
| `USAGE-01` | Medium | Open | Usage from failed attempts is discarded; missing usage silently becomes zero. |
| `AUTH-01` | Medium | Open | Norn's own login reads a flat account claim instead of the namespaced Codex claim. |
| `AUTH-02` | Medium | Partially fixed | Refresh is single-flight in-process but still races across Norn/Codex processes. |
| `AUTH-03` | Medium | Open | Credential-load and proactive-refresh failures are hidden as absence or stale-token fallback. |
| `AUTH-04` | Low/Medium | Open | The browser reports login complete before token exchange and durable storage. |
| `AUTH-05` | Low/Medium | Open | A remote revoke failure prevents local credential deletion. |
| `STRUCT-01` | Medium | Design tradeoff | Native Responses structured output is replaced with a synthetic function tool. |

Two previously suspected transport issues are in better shape at this snapshot:
`server_is_overloaded` and `slow_down` now carry a retryable 503 classification,
and a clean EOF without a terminal event now becomes `StreamInterrupted`. Those
should remain regression tests, not open findings.

---

## 1. Credential and backend boundary

### SEC-01: OAuth credentials can be sent to a project-selected origin

**Severity:** Critical.

Norn automatically loads project settings from `.norn/settings.json`
(`crates/norn/src/config/loader.rs:59-69`). `provider.base_url` flows through CLI
assembly without an origin trust check
(`crates/norn-cli/src/config/overrides.rs:295-317`). When no API-key environment
variable is selected, the OpenAI Responses provider still chooses OAuth
(`crates/norn-cli/src/print/provider.rs:154-181`). The explicit URL becomes the
request endpoint (`crates/norn/src/provider/openai/provider.rs:106-116`), after
which `Authorization: Bearer ...` and `chatgpt-account-id` are attached
(`crates/norn/src/provider/auth.rs:153-174`).

A cloned repository can therefore select an attacker-controlled HTTPS endpoint
and receive the user's Codex bearer token. Atomic auth storage and fixed OAuth
token/revoke endpoints do not mitigate this request-origin problem.

**Recommendation:** bind ChatGPT OAuth credentials to a compiled allowlist of
normalized HTTPS origins and paths. Arbitrary `base_url` must require API-key
auth, or an explicit user-level trusted-provider configuration that cannot be
introduced by repository config. Do not solve this with a warning alone.

### SEC-02: working-directory config can select and exfiltrate an ambient secret

**Severity:** Critical.

The same project and local settings layers can supply both `provider.api_key_env`
and `provider.base_url`, including through provider profiles. The merge and CLI
override path carries the selected environment-variable name into provider
construction. `build_provider` then reads that variable and uses its value as a
Bearer credential for either the Responses or compatible-provider path.

A cloned repository can therefore name a likely ambient variable such as
`GITHUB_TOKEN` and pair it with an attacker-controlled endpoint. The attacker
does not need to know the value in advance. This is independent of `SEC-01`:
restricting Codex OAuth alone still leaves arbitrary process secrets exposed.

**Recommendation:** treat credential source and credential destination as one
provenance-bound authority decision. Working-directory layers must not set
`base_url`, `api_key_env`, or `auth`; those fields may originate only from
trusted user configuration or an explicit CLI override. Reject the untrusted
layer before merge and before any selected environment variable is read.

### SEC-03: working-directory config can silently enable raw API dumps

**Severity:** High.

`provider.debug_dump_dir` is accepted from both CWD settings layers and provider
profiles. Print and TUI assembly turn its presence into an active JSONL sink
without a separate CLI opt-in. The dump contains full serialized request bodies
and parsed SSE data, including private prompts, tool inputs/outputs, model
content, cache keys, and reusable state. The baseline append path follows
symlinks and uses default file permissions.

A cloned repository can therefore cause sensitive runtime data to be written to
a repository, synced directory, FIFO, symlink target, or other selected path.
The field is a data-sink authority, not a harmless provider tuning value.

**Recommendation:** allow the sink only from trusted user config or explicit
CLI input. Reject raw working-directory presence before merge. Create dump files
with owner-only permissions, refuse symlinks and non-regular files, and never
record untrusted response-header values.

### SEC-04: working-directory config can select a provider executable

**Severity:** High.

`provider.runner_path` is also merged from project and local settings and is
used as the Claude Runner executable. A user explicitly selecting that provider
can therefore execute a repository-selected path rather than the expected
trusted binary. Although this is outside Responses wire semantics, it was found
through the same provider-settings trust-boundary audit and represents local
code execution authority.

**Recommendation:** treat executable paths as trusted-only provider authority.
Reject the field from both CWD layers and provider profiles; retain trusted
user-level selection. Any future CLI override must be an explicit surface.

### SEC-05: `test-utils` exposes a production OAuth authority override

**Severity:** High.

`AuthManager::shared_for_tests` and
`AuthManager::from_static_auth_with_token_url` are compiled and public when the
Cargo `test-utils` feature is enabled. The former can load a real shared
`auth.json`; both can direct a refresh exchange, including its refresh token, to
an arbitrary URL. A Cargo feature is a production build mode, so this
contradicts the source claim that production physically cannot redirect the
token authority.

**Recommendation:** compile arbitrary-authority constructors only under
`#[cfg(test)]` and keep them crate-private. A public feature may expose mocks,
but must not expose a path that reads real credentials or changes their
authority.

### SEC-06: diagnostics expose credential and identity material

**Severity:** Medium.

The baseline derives `Debug` for OAuth token/claim structures containing raw ID,
access, and refresh tokens, email, user ID, account ID, and PKCE values. Response
dumps retain all header values, so a server can echo a credential under an
unanticipated header name. Refresh failures copy arbitrary token-authority
response bodies into displayable errors.

The follow-up pass found the same class outside OAuth: malformed SSE logging can
copy provider-controlled payload fragments, non-2xx Responses/compatible bodies
can flow into ordinary errors, and `response.failed.error.message` is
provider-controlled text. Besides leaking echoed prompt or account data, those
strings can carry terminal control sequences into CLI/TUI logs.

**Recommendation:** use structural presence-only `Debug` implementations,
redact every response-header value rather than maintaining a denylist, and
classify OAuth errors by status plus an allowlisted error code without
propagating authority text. Responses errors should likewise retain only typed
status, retry class, and locally authored messages; malformed-frame diagnostics
should contain event type, size, and parser classification but no payload bytes.

The P0 candidate streams and discards non-2xx bodies under the existing request
timeout before returning its structural status/classification. This deliberately
preserves the established behavior that a stalled 4xx or 5xx body becomes the
typed retryable timeout; P0 does not introduce a broad status-only early return.

The P0 candidate narrows `Debug` for credential-bearing runtime/auth/request
types, including free-form request options. It does **not** claim that every raw
configuration container is structurally redacted: the legacy raw provider
settings container still derives `Debug`, and no reachable logging call was
found. That residual must remain documented and must not be erased by a broader
claim such as "all provider settings are safe to debug."

### SEC-07: working-directory automation bypasses provider containment

**Severity:** Critical.

P0 adversarial review found that the same untrusted project and local settings
layers can declare shell hooks. Runtime assembly turns those entries into
`ShellCommandHook` instances, and unconditional lifecycle hooks can run `sh -c`
before the first provider request with the parent process environment
inherited. Project rules under `.norn/rules`, `.claude/rules`, and
`.meridian/rules` can likewise declare `shell_source`, which runs through
`sh -c` when the rule matches. Bare-name workspace profiles add a third path:
TOML/JSON `prompt_commands` run before provider calls, and a workspace profile
can shadow a same-name user profile. Child-agent profile selection reaches the
same resolver.

A cloned repository can therefore read `$CODEX_HOME/auth.json`, export a likely
ambient secret, or run another executable without relying on `base_url`,
`api_key_env`, or `runner_path`. Endpoint containment is not a meaningful
repository trust boundary while an automatic repository command can escape it.

**Recommendation:** reject command-bearing hooks in both working-directory
settings layers before merge. Preserve source provenance while scanning rules
and resolving profiles; reject `shell_source` from all working-directory rule
tiers and reject `prompt_commands` from bare-name workspace profiles before
building loop state. User-level hooks, rules, and profiles, programmatic hooks,
explicit profile paths, and an explicit `--rules` file remain trusted surfaces;
future project-command support requires a separate explicit consent design.

### SEC-08: model aliases can activate a trusted credential bundle indirectly

**Severity:** Medium.

P0 provenance review found a weaker confused-deputy path after the five direct
provider fields were rejected. A project can set its default `model` to a
backend-selecting alias defined in user settings, define a model alias that
selects `provider_profile` or `api_shape`, or put that model into a bare-name
workspace profile. Runtime resolution then activates the named user provider
profile and its trusted `base_url` and `api_key_env`.

The repository cannot invent a new destination or obtain the credential value,
so this is not the original `SEC-02` exfiltration path. It can nevertheless
choose which pre-authorized deployment and credential bundle is used, with
confused-deputy and spending consequences, contrary to D1's provenance rule.

**Recommendation:** reject `provider_profile` and `api_shape` selectors in
working-directory model aliases. A working-directory default model must not
activate a backend-selecting alias from a trusted layer, whether it came from
settings or a workspace profile. An explicit CLI model selection or trusted
user default/profile remains permitted.

### SEC-09: variant prompt files can escape the workspace read boundary

**Severity:** High.

Working-directory settings can define `variants.<name>.prompt_file`. Variant
catalog assembly reads that path eagerly, accepts absolute paths, and follows
filesystem links before any child is launched. A cloned repository can point a
variant prompt at a predictable credential or private file outside the
workspace; later spawning that variant places the contents into model input.

**Recommendation:** reject `prompt_file` from both working-directory settings
layers before merge and redact the variant name/path in the diagnostic. Inline
workspace variant prompts remain available; user-level prompt files remain a
trusted surface pending a separate user-relative anchoring decision.

### SEC-10: project settings can re-enable skill shell expansion

**Severity:** Medium.

`tools.skill.shell_execution` used ordinary scalar precedence, so a project or
local setting of `true` could override a trusted user setting of `false`.
Skill-authored shell expansion is model-mediated rather than an automatic
startup command, but an untrusted layer still must not relax an explicit user
restriction.

**Recommendation:** permit working-directory layers to disable skill shell
execution but reject any attempt to enable it. This preserves deny-only project
policy while preventing a repository from widening the user boundary.

### SEC-11: workspace reads do not share one immutable filesystem boundary

**Severity:** High.

The follow-up review found that settings, root and nested `NORN.md`, rules,
profiles, capabilities, skills and their resources, variant prompt files, and
`CONVENTIONS.toml` were discovered through several independent path APIs. Some
canonicalized a path and later opened its spelling, some followed a final or
intermediate symlink, and some canonicalized the working directory again after
launch. A repository path could therefore point outside the reviewed workspace,
or the launch path could be replaced between validation and use. A user search
path that was initially a symlink into the workspace could also be re-pointed
after its trust tier was classified.

The P0 candidate establishes one canonical launch root and forwards it to root,
spawn, and fork assembly instead of re-reading a mutable process CWD. On Unix,
workspace reads and directory enumeration walk from a pinned descriptor with
no-follow semantics, reject symlinks at every component, require regular final
files, and recognize alternate physical spellings such as macOS `/var` and
`/private/var` without following the candidate's final component. Search paths
that physically resolve under the workspace are normalized once at launch.
Trusted home roots and explicit user paths must be absolute.

This is an intentional compatibility break: repository symlinks are not a
supported indirection mechanism, even when they point elsewhere inside the same
repository. The current implementation is Unix-specific; when workspace input
is present on a non-Unix target it fails closed rather than silently falling back
to link-following APIs. A narrow exception exists for validated `.git` metadata
needed to display branch/commit identity; it is not a general workspace-read
escape hatch.

Two residuals require explicit gate disposition. Workspace text reads are still
unbounded; any size policy needs an owner-approved streaming or limit design, not
an arbitrary constant. The public `Scanner`, `scan_rule_dirs`, and
`discover_skills` convenience APIs remain trusted-input-only interfaces; secure
runtime assembly does not use them with repository-controlled path roots.

### SEC-12: two further repository surfaces can launch processes

**Severity:** Critical.

A workspace skill body can contain shell expansion. A global user setting that
permits skill shell execution therefore grants every newly cloned repository's
skill text process authority when the model activates it. Separately,
`CONVENTIONS.toml` can name LSP, diagnostic, remediation, and report adapters
that run after a mutation. Those commands are especially dangerous because they
look like quality gates and can run with Norn's inherited environment.

The P0 candidate deliberately disables shell expansion for every skill whose
physical source is under the workspace, regardless of the user-level global
setting. It loads `CONVENTIONS.toml` through the same no-follow workspace reader,
removes all process-bearing `lsp`, `diagnostics`, `remediation`, and `reports`
definitions, and asserts that only non-process LOC and pattern checks remain.
Both policies are intentional behavior breaks. Restoring either feature requires
an explicit, provenance-preserving repository-consent design; ordinary config
precedence or a global enable bit is not consent for a new repository.

### SEC-13: model-selected profiles are a command confused deputy

**Severity:** Critical.

Rejecting `prompt_commands` from a bare-name workspace profile does not close the
whole path. The model can select a trusted user profile through the spawn tool.
If that profile carries prompt commands, Norn executes them before the child
provider call. The user trusted the profile as configuration, but did not
necessarily authorize untrusted model output to invoke its ambient shell
authority.

The P0 candidate rejects `prompt_commands` on every model-selected profile,
including a user-tier profile. Prompt commands remain available only where a
trusted operator or programmatic caller selected the profile. This is an
intentional compatibility break, not a fallback to a same-name profile.

### SEC-14: raw provider fields and cross-layer names bypass typed authority

**Severity:** High.

Working-directory `provider.options` can inject free-form request fields after
typed payload construction. A provider profile's `api_shape` can switch between
Responses and compatible Chat Completions serialization. Same-name project
aliases or profiles can also collide with a trusted user definition and cause a
trusted backend bundle to be selected through an untrusted name. These paths do
not all expose an endpoint directly, but they let repository data choose request
semantics, deployment, credentials, or spending indirectly.

The P0 candidate rejects project/local provider options, profile `api_shape`,
backend-bearing aliases, and cross-layer alias/profile collisions before merge
or environment lookup. Request-level provider options also reject collisions
with Norn-owned typed fields. The `mcp_servers` settings surface, including its
environment map, remains dormant in the current production runtime; it is not a
safe precedent. Provenance and explicit consent are prerequisites before any
future MCP wiring consumes those merged project values.

### SEC-15: durable session and spool artifacts are credential-adjacent

**Severity:** High.

Session JSONL and its index can contain prompts, reasoning summaries, tool
arguments, and outputs. The full-output session spool deliberately stores the
uncapped original tool result, and background-process spools retain stdout and
stderr. Lock and temporary files share the same directory tree. Relying on the
caller's umask, following an existing link, or creating a world-searchable
directory can disclose materially more than ordinary diagnostic output.

P0 therefore treats the session data root, session/index files, lock files,
atomic-write temporaries, full-output spool directories/files, and process spool
directories/files as private artifacts. The required Unix policy is private
directories (`0700`), private regular files (`0600`), no-follow final opens, and
failure on non-regular or link targets. The independent reviewer must verify the
complete creation, reopen, rewrite, resume, and cleanup matrix before this
finding is closed; the source-review status does not claim that gate has passed.

### BACKEND-01: backend identity is inferred from the absence of an override

`is_chatgpt_backend` returns true only for OAuth plus `base_url: None`
(`crates/norn/src/provider/openai/provider.rs:118-137`). Explicitly setting the
canonical ChatGPT URL therefore changes behavior to direct-API semantics:
response threading and server compaction are enabled, service-tier lookup uses
the direct backend, and `store` changes.

Backend identity, credential authority, and endpoint URL are separate concepts.
They should be represented separately. A normalized URL comparison is better
than the current heuristic, but an explicit backend enum with an allowlisted
endpoint is the safer long-term model.

### CONFIG-01: `provider.auth` does not control runtime authentication

`ProviderSettings.auth` documents `oauth`, `api_key`, and `env`, and the settings
merge retains the field. Provider override assembly does not consume it, while
`build_provider` chooses API-key authentication solely from the presence of
`api_key_env` and otherwise chooses OAuth. A trusted configuration can therefore
state `auth: "oauth"` without pinning OAuth, or state another mode without
changing runtime behavior.

P0 rejects this field when it originates in a working-directory layer, closing
its role in the repository trust-boundary attack. The remaining user-level
semantic mismatch should be fixed in P2 with typed validation and an explicit
auth-source resolution contract rather than another presence heuristic.

### AUTH-01 through AUTH-05

Norn's self-hosted login decodes `chatgpt_account_id` as a flat JWT claim
(`crates/norn/src/provider/openai_oauth/jwt.rs:29-40`). Current Codex tokens place
that field under the `https://api.openai.com/auth` object. The token endpoint's
top-level `account_id`, or an existing Codex `tokens.account_id`, masks the bug;
Norn's fallback path does not (`login_server.rs:339-350`).

In-process refresh is now correctly single-flight through a mutex and epoch
(`openai_oauth/manager.rs:215-250`). Cross-process instances still load separate
snapshots and can rotate the same refresh token concurrently. Atomic rename in
`storage.rs` protects file integrity, not refresh-token ownership.

Credential-load errors are discarded by `.ok().flatten()`
(`openai_oauth/manager.rs:129-133`). Proactive transient refresh failures are
also ignored while the stale credential is returned (`manager.rs:202-212`). The
resulting user error can say "no OAuth token found" instead of reporting a
malformed or unreadable auth file.

The browser receives "Login complete" before code exchange and storage
(`openai_oauth/login_server.rs:153-171,219-226`). Logout deletes `auth.json` only
after successful remote revocation (`openai_oauth/revoke.rs:43-50`), so a network
or authority failure leaves the local credential installed.

**Recommendations:** parse both namespaced and legacy flat claim shapes; add an
interprocess reload-lock-refresh-save transaction; preserve typed storage and
refresh errors; show browser success only after durable save; and always clear
local credentials while separately reporting remote revocation status.

---

## 2. Request construction and role semantics

### Current wire shapes

For the ChatGPT/Codex OAuth backend, capabilities disable response threading.
Each request is effectively:

```text
instructions: stable System prompt
input:        full locally reconstructed transcript
              + current user/tool input
              + fresh managed Developer context at the tail
tools:        current resolved tool definitions
store:        false
include:      ["reasoning.encrypted_content"]
cache key:    persisted session id when available
```

For the direct Responses backend after the first stored response:

```text
instructions: stable System prompt, resent every request
previous_response_id: last response id
input:        only local items after the response-thread cursor
              + fresh managed Developer context at the tail
store:        true
```

### What is correct

System messages are concatenated into top-level `instructions`
(`crates/norn/src/provider/openai/request.rs:109-121`). This is the right use of
the field. OpenAI explicitly states that prior `instructions` do not carry over
with `previous_response_id`, so resending the System prompt is required rather
than redundant.

Developer messages remain typed input messages (`request.rs:122-124`), user
messages use `input_text`, and function/custom call outputs retain the provider
`call_id`. `store: false` plus `reasoning.encrypted_content` for stateless
ChatGPT replay is also correct (`request.rs:144-163`).

Putting current dynamic context in a Developer role after the user message is
not an authority inversion. Role priority is more important than chronological
position. The role is appropriate for Norn-controlled environment and harness
instructions that must outrank user content. It does not by itself justify
promoting repository-controlled prose; that separate defect is `ROLE-01`.

One naming issue remains: sections delivered through Norn's
`SystemContextAppend` path are ultimately combined into the managed Developer
message, not serialized as a System message. If callers rely on the name as an
authority guarantee, either the naming or the wire role should be made explicit.

### ROLE-01: repository source trust is promoted inconsistently

**Severity:** High.

Root project `NORN.md` content is folded into the base System instruction, while
nested `NORN.md` and matched rule content enter the managed Developer tail.
Workspace profile instructions can also become a child's base instruction. The
same repository trust tier therefore receives different wire authority depending
on discovery path, and root repository prose can outrank the user's current
request. Current Codex source treats repository guidance as contextual input
rather than product-level System policy.

P0 removes repository *process* authority but intentionally leaves static
repository guidance available. A later owner-approved authority matrix must
classify product invariants, trusted operator configuration, repository context,
rules, profiles, tool output, and user input separately. Repository prose should
be quoted as lower-trust contextual data unless a deliberate consent surface
promotes it; moving it between files must not raise its role.

### ROLE-02: compatible serialization silently collapses Developer into System

**Severity:** Medium.

The compatible Chat Completions serializer maps both `MessageRole::System` and
`MessageRole::Developer` to wire role `system`
(`provider/openai_compatible/request.rs:155-163`). That gives dynamic harness
context the same representation as immutable product instructions and loses the
role distinction that the Responses path preserves. Some compatible backends do
not support `developer`, but silently upgrading it is not a safe universal
fallback.

Resolve role support as an API-shape/backend capability. A capable compatible
backend should preserve Developer. An incapable one needs an explicit,
owner-approved downgrade or local rejection with snapshots proving the effective
authority; provider compatibility alone must not silently change role semantics.

### STATE-02: local replacement is not provider-side replacement

`ManagedDevMessage::detach` removes the old Developer item only from Norn's
local vector (`crates/norn/src/loop/dev_context.rs:84-107`). A fresh item is
appended at the tail (`runner/prompt.rs:175-183`). In provider-threaded mode,
`request_messages` sends only the System prefix plus items after the local cursor
(`loop/conversation_state.rs:162-171`).

The provider referenced by `previous_response_id` still retains every prior
Developer input item. Each timestamp, collaboration mode, rule set,
prompt-command output, and environment snapshot therefore becomes append-only
server history. Norn's token estimate and local prompt view cannot see those
stale items.

This does not affect the default ChatGPT OAuth path because that path correctly
sets `response_threading: false` (`provider/openai/provider.rs:163-170`). It does
affect direct Responses users and any explicit ChatGPT URL currently
misclassified as direct.

**Recommendation:** give replaceable context explicit state semantics. In
threaded mode, either place replaceable material on a truly replaceable request
surface, reset the response anchor and replay a cleaned transcript when it
changes, or disable threading for this prompt design. Local deletion must not be
treated as deletion from provider state.

### STATE-03: local replay cannot reconstruct stored reasoning after anchor reset

**Severity:** High.

For `store:true`, Norn omits `reasoning.encrypted_content` from `include` on the
assumption that the provider thread retains reasoning. That is valid only while
the `previous_response_id` chain remains authoritative. Local compaction or
context replacement can invalidate the anchor and force a replay from Norn's
session view; stored responses did not return replayable encrypted reasoning, so
the rebuilt request cannot preserve the model's reasoning state.

Choose one explicit contract. Either stored calls also return and persist the
replay material required for a later anchor reset, where the backend supports
that shape, or a threaded session must never fall back to local replay after
compaction. Server-side compaction, a semantically fresh thread, and stateless
full replay are distinct transitions and need separate persistence/resume tests.
Resetting the anchor while claiming continuity is not acceptable.

### STATE-01: stateless replay violates the ordered-item contract

OpenAI's conversation-state guidance says a stateless reasoning client should
append every item from `response.output` to the next input, preserving encrypted
reasoning and assistant `phase` values.

Norn instead creates one `AssembledResponse` containing flat `text`, flat
`thinking`, a `Vec<ReasoningItem>`, and a `Vec<AssembledToolCall>`
(`crates/norn/src/loop/assembly.rs:32-52`). It persists one flat
`SessionEvent::AssistantMessage` and later serializes all reasoning first, one
assistant text message second, and all calls last
(`provider/openai/request.rs:348-389`).

That reconstruction can differ materially from the original output sequence.
A valid provider sequence such as:

```text
reasoning -> commentary message -> function call -> reasoning -> final message
```

becomes:

```text
all reasoning -> one phase-less combined message -> all function calls
```

The problem is correctness first and cache fidelity second. The reconstructed
suffix becomes a future request prefix, but it is not the prefix the provider
originally produced.

**Recommendation:** persist an ordered `Vec<ResponseItem>` or equivalent raw
tagged representation per response. Treat normalized assistant text, reasoning
display, and executable local calls as derived projections. Preserve unknown
items as raw JSON so protocol additions do not silently disappear.

### MODEL-01 and STRUCT-01

The model catalog says current models default reasoning summaries to `none` and
support parallel tool calls (`assets/models.json:17-40`). Request construction
turns a missing summary into `auto` and hard-codes `parallel_tool_calls: false`
(`provider/openai/request.rs:139-142,174-188`). The latter is currently required
by assembly's order-based call-completion correlation
(`loop/assembly.rs:81-95`), but it should be described as a Norn limitation, not
as provider capability.

Request construction also always sends a `reasoning` object and requests
encrypted reasoning on every stateless Responses call, including a trusted
custom or unknown model for which reasoning support has not been established.
Newer model-specific reasoning efforts and controls should not arrive through
raw options around that unconditional shape. Resolve an immutable model request
profile first: catalog default effort/summary, supported effort values, summary
support, encrypted-reasoning replay support, and backend-specific reasoning
fields. An unknown model requires explicit trusted capability configuration; it
must not inherit the current frontier model's request shape by assumption. See
the [Responses create reference](https://developers.openai.com/api/reference/resources/responses/methods/create)
for the time-sensitive wire contract.

Norn implements requested structured output as a synthetic function tool,
whereas current Codex uses the Responses `text.format` field. The synthetic tool
is defensible for provider portability and loop-level validation, but it expands
the cached tool prefix, forces tool/nudge semantics onto final output, and passes
through the function-schema downleveler. Responses-native structured output
should be evaluated as the primary path, with the tool strategy retained only
where its control-flow behavior is intentional.

---

## 3. Prompt caching

### Confirmed improvement from `aecae78`

Before July 9, the managed dynamic Developer message sat near the start of the
input. Its second-resolution timestamp changed every iteration, so exact-prefix
caching could not extend into the growing history. Commit `aecae78` moved the
whole dynamic container to the tail. This fixes the placement class rather than
one volatile field and is the right direction for pre-GPT-5.6 automatic prefix
caching.

The incident and measured ANKS impact are retained in
`docs/PROMPT-CACHE-INVALIDATION-FIX.md`. That document should remain the incident
record; this review adds the model-version and transcript-fidelity constraints.

### CACHE-01: GPT-5.6 changes the acceptance question

OpenAI's current GPT-5.6 behavior uses an implicit breakpoint on the latest
message unless request-wide mode is `explicit`. Norn's latest message is the
volatile Developer tail, and Norn deletes that item before constructing the next
request. The prior breakpoint prefix therefore does not necessarily occur in the
next request at all.

For GPT-5.5, "stable history before a changing tail" is enough to expect a
longer matching prefix. For GPT-5.6, it is not enough to infer that the service
will read the stable prefix or avoid a new billable write. Official Codex still
uses a thread-derived cache key without explicit breakpoints, so blindly adding
public-API cache fields to the private ChatGPT backend is also not justified.

**Verdict:** do not revert the tail change, but do not call it validated for the
new default model family. Measure it against the real backend before choosing a
5.6 policy. The candidate alternatives are a typed request-wide
`prompt_cache_options` mode and a breakpoint on a stable content block, both
capability-gated. Do not infer support on the private ChatGPT/Codex backend from
the public Responses schema.

### CACHE-02: current telemetry cannot answer the question

GPT-5.6 reports cache writes separately and bills them at a higher rate than
ordinary uncached input. Norn parses `cached_tokens` but hard-codes
`cache_write_tokens: 0` (`provider/openai/sse.rs:465-491`). The system therefore
cannot distinguish:

- a useful write followed by repeated reads;
- a write that is never reused;
- a full miss;
- a backend that does not report the field.

Missing usage should not collapse to the same value as a reported zero. Capture
presence as well as value, and preserve attempt-level usage before changing
cache policy.

### CACHE-03: cache keys are not universal

Managed persisted sessions correctly use the session ID as
`prompt_cache_key` (`agent/builder.rs:454-466`). This matches current Codex, which
defaults the key to its thread ID.

`AgentLoopConfig::default()` leaves the key unset. `--no-session` installs an
in-memory store without assigning one (`norn-cli/src/runtime/from_cli.rs:172-191`).
Spawned and forked agents resolve a default child loop config even though each
has a stable child UUID and, for persisted parents, a real child session
(`tools/agent/spawn.rs:383,503-530`; `tools/agent/fork_tool.rs:270,297-324`).

Current GPT-5.6 guidance says a key is needed for its more reliable matching
path. Every agent execution should therefore have a stable runtime/thread key
independent of disk durability. Persistent session IDs are suitable; ephemeral
roots and children can use their already-minted runtime IDs.

### CACHE-04: tool definitions are part of the cache prefix

Norn expands tool-description variables and rebuilds the provider tool surface
before every request (`loop/runner/prompt.rs:121-130`;
`loop/expansion.rs:37-53`). Shell variables with no TTL, computed variables, or a
changing `working_dir` can mutate the serialized `tools` array. OpenAI requires
tools to remain identical for a cache hit, so this invalidates the prompt before
message placement matters.

Resolve session-stable tool definitions once, or explicitly classify variables
allowed in tool descriptions as stable. Fingerprint the serialized tool surface
per request and include that fingerprint in cache diagnostics.

### CACHE-05: typed cache controls lag the current API

`ResponsesApiPayload` contains legacy `prompt_cache_retention` but always sets it
to `None`. There is no typed `prompt_cache_options`, and Norn's string-only
message content cannot attach `prompt_cache_breakpoint` to an `input_text`
block. Raw provider options can inject request-wide fields, but they cannot add a
content-block marker through the current message model.

For GPT-5.6, add typed, capability-gated controls only after the ChatGPT backend
has been tested. Keep legacy retention limited to models/backends that support
it. The current OpenAI guide and API reference disagree on whether 50 or 80
historical breakpoints are considered; Norn should encode neither number as a
client invariant.

### Required cache experiment

Run a real 20-call tool loop against both `gpt-5.5` and the current GPT-5.6
Codex-login model. Record one row per request with:

| Field | Purpose |
|---|---|
| model/backend/request number | Separate backend and model behavior. |
| prompt-cache key hash | Verify stable routing without logging the raw key. |
| instructions hash | Detect accidental System drift. |
| tool-surface hash | Detect schema, order, or description drift. |
| ordered input-item type/hash list | Prove the actual prefix, not a normalized message approximation. |
| input/output tokens | Establish total work. |
| cached-read tokens | Measure reuse. |
| cache-write tokens and field presence | Measure write cost and reporting support. |
| latency to first event and completion | Measure user-visible benefit. |

Compare at least four variants: current implicit tail, no dynamic message,
stable Developer message, and an explicit stable breakpoint where the backend
accepts it. Include hosted search, reasoning, and a variable-expanded tool
description in separate cases. Do not combine all volatility into one run or the
source of a miss will be unknowable.

The experiment must compare the private ChatGPT/Codex backend with the public
Responses backend rather than transferring a result between them. Record actual
request timestamps and throttle each cache key to the current prompt-cache
guide's approximately 15 requests per minute per-key guidance; the exact cadence
must be rechecked when D6 is approved. Warm-up, cooldown/retention, concurrency,
service tier, output limit, reasoning effort, and key reuse/isolation belong in
the preregistration, not post-hoc interpretation. See the
[OpenAI prompt-caching guide](https://developers.openai.com/api/docs/guides/prompt-caching).

---

## 4. Streaming events and output items

### Coverage summary

The public Responses reference checked on 2026-07-10 documents 52 streaming
event types. Norn maps 12, explicitly ignores or partially consumes 13, and
lets 27 fall through the unknown-event path. The raw count is not itself the
bug; many lifecycle/progress events are safe to ignore.

The public `ResponseOutputItem` union currently has 28 variants. Norn's
`response.output_item.done` switch recognizes four: `function_call`,
`custom_tool_call`, `reasoning`, and compaction aliases. Compaction is then
discarded during assembly. A completed `message` is not preserved as an item.

| Coverage | Event families | Assessment |
|---|---|---|
| Mapped | `response.completed`, `failed`, `incomplete` | Terminal handling exists, except Codex `end_turn` metadata is lost. |
| Mapped but lossy | output-text and reasoning `delta`/`done` | Text survives, but IDs, indices, parts, and phase do not. |
| Mapped | function/custom tool-input deltas and completions | Call-ID handling is comparatively strong. |
| Partially consumed | `output_item.added` | Used only for tool `item_id` to `call_id` correlation. |
| Explicitly ignored | response/content/reasoning lifecycle events | Mostly harmless UI/progress loss. |
| Explicitly ignored | hosted web-search lifecycle | Material because Norn advertises hosted search. |
| Unknown | refusal and annotation events | Immediate correctness and provenance defects. |
| Unknown | file search, code interpreter, image, audio, computer, MCP, native shell | Unsupported capabilities; must stay unadvertised until end-to-end support exists. |

### EVT-01: refusal becomes empty success

`response.refusal.delta` and `.done` are unknown. The completed message item is
also ignored by the `output_item.done` discriminator
(`provider/openai/sse.rs:267-361,458-460`). `response.completed` then produces an
ordinary `EndTurn`; a response with no tool calls is classified as a valid text
stop. A provider refusal can therefore surface as a successful empty answer.

Refusal should be a typed terminal outcome carrying the refusal text and policy
metadata available on the wire. It must never be indistinguishable from a model
that intentionally returned an empty answer.

### EVT-02: phases and order are operational data

Text deltas lose `item_id`, `output_index`, and `content_index`
(`sse.rs:198-205`) and are globally concatenated (`loop/assembly.rs:98-113`). The
durable `Message` has no `phase` or content-part model
(`provider/request.rs:151-190`).

OpenAI specifically warns that missing `phase` can make GPT-5.5 treat an
intermediate update as a final answer. Current Codex retains
`MessagePhase::Commentary` and `MessagePhase::FinalAnswer` in its canonical
`ResponseItem` model. Norn should do the same and preserve multiple assistant
messages around calls rather than merging them.

### EVT-03: hosted web search is advertised but not preserved

Norn serializes a native `web_search` tool (`provider/openai/tools.rs:33-65`) but
explicitly ignores its lifecycle events (`provider/openai/sse.rs:451-453`) and
does not handle the `web_search_call` output item. It also skips
`response.output_text.annotation.added`.

The final answer's plain text generally survives. The search action, sources,
URL citations, and item needed for exact stateless replay do not. This is an
end-to-end capability mismatch on a tool Norn currently exposes, not a future
feature request.

### EVT-04: authoritative completion data is thrown away

Norn maps `response.output_text.done` into `TextComplete`, but assembly ignores
that event (`loop/assembly.rs:203-211`). It also ignores the completed message
item. Malformed SSE JSON is warned and dropped (`provider/openai/sse.rs:162-185`).
One missing delta can therefore truncate text even when the server later sends
the complete text in both a `.done` event and a completed item.

Assembly should reconcile deltas against authoritative completion data. A
mismatch should be observable; complete data should repair a missing delta where
safe.

### EVT-05: actionable protocol additions fail open

Unknown events and unknown `output_item.done` variants return `None`. That is
appropriate for informational lifecycle events, but unsafe for a new actionable
item: the loop can report completion after silently omitting something the model
asked to execute.

Preserve unknown output items as raw tagged values. Capability gating should
require complete request serialization, stream parsing, persistence, replay,
execution, and UI behavior before a tool family is advertised.

### EVT-06: tool completion is not reconciled by stable item identity

**Severity:** High.

Tool deltas use output-item IDs, while `ToolCallComplete` retains only `call_id`,
name, and arguments. Assembly then completes the next matching/incomplete call by
arrival order rather than proving that the completion belongs to that item's
deltas. With interleaved calls, a completion for call two can finalize call one.
Duplicate `.done`/item-completed frames can also be applied more than once because
there is no per-item idempotency record.

Every added, delta, done, and completed-item event must carry and validate its
item ID, call ID, output index, kind, and content index as applicable. Reconcile
them in a per-response state machine; conflicting identities are typed protocol
errors, exact duplicates are idempotent, and no order-based fallback is allowed.
This is required before the catalog's parallel-call capability can be honored.

### EVT-07: incomplete or malformed events become fabricated success

**Severity:** High.

A call assembled from deltas can survive to `response.completed` and be executed
even when its authoritative `output_item.done` never arrived. Several wire reads
also use empty-string, zero-usage, missing-response-ID, or ordinary-EndTurn
defaults when a required field is absent. A malformed `response.completed` can
therefore look like a successful zero-usage response, while a missing delta can
become empty text/arguments. An unrecognized reasoning summary/content part can
fail deserialization and drop the replay item even though the terminal response
is accepted.

Use typed event schemas with required fields and an explicit distinction between
absent, zero, empty, incomplete, and successful. Only an authoritative completed
item may become executable. Unknown reasoning parts must survive opaquely in the
canonical transcript; malformed terminal or required completion data must end in
a typed protocol error, never a synthesized normal outcome.

### Recommended event architecture

Do not add 52 bespoke handlers to the current flat model. Use two coordinated
streams of information:

1. Canonical ordered items from `output_item.added/done`, persisted in provider
   order and replayed unchanged for `store: false`.
2. Incremental deltas for live UI and partial-output recovery, keyed by item and
   content indices and reconciled with canonical completion items.

The reconciler must be identity-based and idempotent across duplicate and
out-of-order frames, and it must refuse to promote a delta-only call into an
executable item.

This is the shape current Codex uses. It ignores many low-value lifecycle events
too, but it drives the turn from typed `ResponseItem` values rather than losing
them.

---

## 5. Codex-specific turn and transport behavior

### CODEX-01: `end_turn` is ignored

Current Codex parses optional `response.completed.response.end_turn`. Norn
instead infers `ToolUse` only when the last output item type is
`function_call`; every other completed response becomes `EndTurn`
(`provider/openai/sse.rs:503-521`).

For the ChatGPT backend, `end_turn` is explicit server guidance about whether
the client should finish or continue the current turn. Preserve it in
`ProviderEvent::Done` and define how it interacts with local tool calls,
refusals, and no-output continuations.

### CODEX-02: `x-codex-turn-state` is ignored

Current Codex captures `x-codex-turn-state` from HTTP headers or
`response.metadata` and replays it on later requests within the same user turn.
Norn does not expose response headers to the Responses mapper and explicitly
ignores `response.metadata`. Multi-request tool loops therefore omit the
backend's sticky-routing token.

Add typed turn metadata with an explicit lifetime. It must be reused within a
turn and cleared between turns; treating it as session-global would be another
routing bug.

### TRANS-01: cancellation does not own the detached producer

The loop comment says dropping the provider future aborts reqwest
(`loop/runner/provider_call.rs:36-69`). `OpenAiProvider::stream`, however,
detaches `sender.execute` with `tokio::spawn`
(`provider/openai/provider.rs:195-203`). Dropping the receiver is noticed only
on a later send. A task blocked on headers or the next SSE chunk can continue
until its timeout, consuming resources after the user sees cancellation.

Return a stream guard that aborts or cancels the producer on drop, avoid the
detached task, or select all transport waits against receiver closure. Current
Codex's `ResponseStream::drop` cancels the mapper task and is a useful reference.

### TRANS-02: retry ownership is split incorrectly

HTTP 429 responses are retried inside `StreamExecutor`. An in-band
`response.failed` with `rate_limit_exceeded` becomes `ProviderError::RateLimited`,
but the default loop retry policy excludes rate-limited errors. The same
condition is retried or not retried solely based on whether it arrived before or
after SSE began.

Choose one retry owner and carry attempt/budget metadata so an in-band error is
not mistaken for a provider-exhausted HTTP retry. `server_is_overloaded` and
`slow_down` now have retryable 503 classification; retain that improvement.

`emit_mapped` also stops only for `Done`, not `Err`
(`provider/exec.rs:454-477`). The consumer returns on the error, but the detached
producer can continue until its next failed send, EOF, or timeout. Treat a
mapped error as terminal immediately after delivery.

---

## 6. Schema, usage, and payload gaps

### SCHEMA-01: downleveling can produce dangling references

The schema flattener copies nested property schemas unchanged
(`provider/openai/schema_downlevel.rs:236-272`) and builds a new root containing
only type, description, properties, required, and optional
`additionalProperties` (`schema_downlevel.rs:284-310`). A property that still
contains `$ref: "#/$defs/..."` loses the root `$defs` it references.

Preserve referenced definitions or resolve/inline local references before
flattening. When a schema cannot be lowered, fail locally with a typed diagnostic
instead of knowingly sending a shape the provider rejects.

### USAGE-01: attempted spend is not represented

`response.failed` discards any terminal usage. Loop retries return only the
successful attempt's usage. Missing values silently become zero, and cost is
always `None` in the Responses parser (`provider/openai/sse.rs:465-491`).

Separate successful-response usage from total attempted/billed usage. Preserve
field presence and provider-reported detail, including cache reads, cache writes,
and failed-attempt usage. This is necessary for both budget enforcement and the
cache investigation.

### TOOL-01: model-catalog tool envelope choices are not wired

`assets/models.json` records `apply_patch_tool_type` and
`web_search_tool_type`, including freeform apply-patch and text-and-image search
for current Codex models. Request construction does not resolve those values into
the advertised tool definitions: apply-patch remains an ordinary function tool
and web search uses one generic shape, even though the stream/parser already has
partial custom-call support.

Resolve tool envelope type from the same immutable backend/model request profile
as reasoning and service tier. A catalog capability is not documentation-only:
it must control serialization, stream parsing, local dispatch, output echo,
persistence, and replay, or be removed/marked unavailable.

### REQ-01: tool-backed slash commands create an orphan assistant call

`SlashCommandHandler::Tool` expands user input into an assistant-role tool-call
message. It does not execute the local tool registry at that point and does not
append the matching tool result. The next provider request can therefore contain
a model-authored-looking call that the model never emitted, with no completed
local dispatch or correlated output.

A slash command must either dispatch through the normal local tool execution,
authorization, persistence, and result-echo path, or remain a user request for
the model to act on. It must not forge provider transcript history. The
regression must cover tool success, rejection, cancellation, and resume.

### Other payload capabilities

Norn does not currently expose the full public request surface: native
`text.format`, truncation, prompt templates, input image/file content, most
hosted tools, background mode, and newer reasoning/cache controls are absent or
available only through raw provider options. That is acceptable if the provider
advertises only the subset Norn handles end to end.

The problem is not incomplete API feature count. It is inconsistency between
advertised capability and round-trip support. Hosted web search currently crosses
that line; unadvertised image, audio, MCP, computer, file-search, and code
interpreter events do not yet.

---

## 7. Conceptual remediation sequence

The numbered steps in this source review describe dependency order, not the
execution plan's `P0`-`P9` phase identifiers. The maintained phase ownership and
gates live in `docs/RESPONSES-API-REMEDIATION-PLAN.md`.

### Phase 0: credential and workspace-authority containment

1. Reject OAuth plus non-allowlisted `base_url` before constructing a provider.
2. Separate backend identity from endpoint override and auth source.
3. Reject credential-source and destination fields from working-directory
   settings before merge or environment lookup.
4. Reject working-directory debug sinks and executable paths in both CLI and
   shared-library runtime loaders.
5. Validate trusted custom API-key endpoints, require HTTPS except for loopback,
   and disable redirects on every credential-bearing HTTP client.
6. Remove arbitrary OAuth-authority seams from production feature builds.
7. Redact credential material from `Debug`, response metadata, and OAuth
   authority errors; harden raw dump files as private regular files.
8. Resolve one immutable workspace root, use Unix descriptor-relative no-follow
   reads/enumeration for every automatic workspace file, and fail closed for
   workspace input on non-Unix targets.
9. Disable shell expansion for workspace skills, remove process-bearing
   convention categories, and reject prompt commands on model-selected profiles.
10. Reject project/local raw provider options, profile API-shape selection, and
    cross-layer backend-name collisions.
11. Make session, index, lock, temporary, full-output spool, and process-spool
    directories/files private and non-link-following.

This phase should ship independently and first. The account-claim,
cross-process locking, and typed auth-load findings remain in the separate OAuth
lifecycle phase tracked by the remediation plan.

### Phase 1: canonical Responses transcript

1. Introduce a provider Responses item type with typed core variants and an
   opaque unknown variant.
2. Persist ordered output items on the session event, with a versioned migration
   or backward-compatible optional field.
3. Derive display text, reasoning summaries, local tool calls, and stop behavior
   from the item transcript.
4. Replay original items in original order for `store: false`, removing only
   server-internal IDs that the target backend rejects.
5. Preserve message phase, content-part indices, annotations, refusal, hosted
   search, and compaction items.

This phase fixes multiple findings at once and should precede broad event-family
expansion.

### Phase 2: conversation-state semantics

1. Decide whether replaceable dynamic context is compatible with provider
   threading.
2. Reset or disable threads when local context removal cannot be reflected on
   the provider.
3. Add a two-turn test proving stale Developer context is absent from effective
   state.
4. Preserve `end_turn` and turn-scoped sticky metadata.
5. Ratify one role-authority matrix for repository context/rules/profiles and
   compatible Developer-role downgrade behavior.
6. Preserve reasoning across every valid anchor-reset/compaction transition or
   forbid the transition that cannot be replayed losslessly.

### Phase 3: cache instrumentation and policy

1. Parse cache-write usage before changing request policy.
2. Assign stable keys to ephemeral roots, children, and forks.
3. Hash instructions, tools, and ordered input items in debug telemetry.
4. Run the GPT-5.5/GPT-5.6 A/B matrix described above.
5. Add explicit breakpoints only for model/backend pairs proven to accept and
   benefit from them.

### Phase 4: transport and model controls

1. Make stream cancellation own the HTTP producer.
2. Unify HTTP and in-band retry policy.
3. Terminate producers immediately after mapped errors.
4. Resolve reasoning-summary defaults from the selected catalog entry.
5. Correlate tool completions by item ID before enabling parallel calls.
6. Evaluate native `text.format` for Responses-native structured output.
7. Wire catalog-selected apply-patch/search envelopes end to end.
8. Replace orphan slash-command assistant calls with real local dispatch or a
   user-role request.

---

## 8. Required conformance tests

| Test | Required assertion |
|---|---|
| OAuth origin containment | Project config cannot cause a Codex bearer token to leave the allowlisted ChatGPT origin. |
| Ambient-secret containment | Working-directory config cannot select an environment variable or cause its value to be transmitted. |
| Provider side-effect provenance | Working-directory config cannot enable raw dumps or select a provider executable; trusted user settings and supported CLI options retain explicit control. |
| OAuth authority feature surface | A production build, including `test-utils`, exposes no arbitrary token-authority constructor. |
| Diagnostic redaction | Tokens, account claims, response-header values, and authority error text do not appear in diagnostics; dump files are private regular files. |
| Immutable workspace root | Every automatic workspace read/enumeration refuses final and ancestor symlinks, survives root-path replacement without escaping the launch root, and recognizes platform path aliases without following the candidate. |
| Repository command containment | Workspace skills and conventions cannot launch processes; model-selected profiles cannot carry prompt commands; trusted operator-selected command surfaces remain explicit. |
| Provider authority provenance | CWD provider options/API shape and cross-layer alias/profile collisions fail before backend selection, environment lookup, or network I/O. |
| Private session artifacts | Session/index/lock/temp/full-output/process-spool paths are private regular files under private directories and reject symlink/non-regular targets on create and reopen. |
| Ordered stateless replay | Every prior output item is replayed in original order with phase and encrypted reasoning intact. |
| Multi-message phase | Commentary and final-answer messages remain distinct across a tool iteration. |
| Refusal | Refusal text becomes a typed non-success outcome, never empty success. |
| Hosted web search | Search-call item, sources, annotations, and answer survive persistence and the next stateless request. |
| Unknown item | An unknown actionable item is persisted opaquely and prevents unsupported execution from being reported as ordinary success. |
| Delta reconciliation | A missing text delta is repaired by `.done`/completed item data and emits a mismatch diagnostic. |
| Tool-call reconciliation | Interleaved, duplicate, conflicting, and delta-only calls are keyed by stable identity; only an authoritative completed item can execute. |
| Malformed terminal | Missing required completion fields, unknown reasoning parts, and absent usage remain typed/opaque rather than becoming empty, zero, or successful defaults. |
| Threaded dynamic context | The second request cannot see the first request's replaceable environment/rules after they are removed locally. |
| Threaded compaction reasoning | Every supported anchor reset preserves replayable reasoning, or the unsupported reset is rejected before local/provider state diverges. |
| Role authority | Root/nested repository context, rules, profiles, and compatible serialization obey the approved source-to-wire-role matrix. |
| Codex end-turn | `end_turn: false` and `true` drive distinct, explicit loop behavior. |
| Turn-state lifetime | Sticky state is reused within one turn and never leaked into the next. |
| Cancellation | Dropping/canceling the stream promptly terminates the server-observed request or connection. |
| In-band rate limit | A streamed rate-limit failure consumes the intended retry budget exactly once. |
| Cache-key coverage | Persistent, ephemeral, spawn, and fork paths all send a stable non-secret key. |
| GPT-5.6 accounting | Reported cache-write tokens survive SSE parsing, persistence, and total-usage aggregation. |
| Tool stability | Tool surface hash remains stable unless a deliberate tool/schema/config change occurs. |
| Catalog tool envelopes | Apply-patch and search envelope types match the selected backend/model catalog and round-trip through local dispatch/replay. |
| Tool-backed slash command | Slash execution produces a real authorized dispatch and correlated result, or remains user-role text; it never forges an orphan assistant call. |
| `$defs` schema | Lowering never emits a dangling local `$ref`. |
| Cross-process refresh | Two processes sharing one rotating refresh token perform one authority exchange and converge on one stored credential. |

---

## 9. Official references

- [OpenAI prompt caching](https://developers.openai.com/api/docs/guides/prompt-caching)
- [OpenAI conversation state](https://developers.openai.com/api/docs/guides/conversation-state)
- [OpenAI reasoning and assistant phase](https://developers.openai.com/api/docs/guides/reasoning#phase-parameter)
- [OpenAI Responses create reference](https://developers.openai.com/api/reference/resources/responses/methods/create)
- [OpenAI Responses streaming events](https://developers.openai.com/api/reference/resources/responses/streaming-events)
- [Current Codex request builder](https://github.com/openai/codex/blob/main/codex-rs/core/src/client.rs)
- [Current Codex Responses SSE parser](https://github.com/openai/codex/blob/main/codex-rs/codex-api/src/sse/responses.rs)
- [Current Codex response-item model](https://github.com/openai/codex/blob/main/codex-rs/protocol/src/models.rs)
- [Current Codex login server](https://github.com/openai/codex/blob/main/codex-rs/login/src/server.rs)

The official prompt-cache guide and Responses API reference currently disagree
on the historical-breakpoint lookback count. This review deliberately avoids
depending on either number.
