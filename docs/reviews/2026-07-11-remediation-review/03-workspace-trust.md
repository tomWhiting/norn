# Trust-Boundary Review — Workspace Authority Containment (P0 / SEC-01…15)

> **Coordinator intake note:** This report is preserved as provisional review
> input against `7d121c9`. The scoped `READY` below is not a P0 phase or Gate D
> verdict. Open findings require tracking, a final fix-round review, and
> complete machine-gate evidence.

**Reviewer:** Fable subagent (workspace trust / config authority ownership).
**Substrate:** frozen snapshot of the working tree (base pin `41ea210` + old mate's P0 work), 2026-07-11.
**Note:** the reviewer returned this report to the coordinator, who transcribed it verbatim to this file.

**Verdict: READY on the surfaces owned by this review.** No reachable Critical or High trust-boundary bypass found across config/context/hook/skill/profile loading. The implementation is coherent, fail-closed, and consistently routed through a single canonical launch root with descriptor-relative no-follow reads. Findings below are Low/Informational.

## 1. Trust model — coherent and sound

- **What is trusted:** user-tier config (`$NORN_HOME`/`~/.norn`), programmatic hooks, and explicit CLI surfaces (`--rules <file>`, `--profile <path>`, `-c key=val`, `--model`). Everything under the working directory (`.norn/`, `.claude/`, `.meridian/`, `NORN.md`, `CONVENTIONS.toml`) is untrusted.
- **No repo can grant itself trust.** Trust tier is determined by *source location*, decided before any repo content is parsed. There is no trust file a repo can write, and `.norn/settings.local.json` is treated as untrusted-equal-to-project (not user-authored) — confirmed in `config/loader.rs` and `provider_security.rs`. The trust decision (`validate_working_directory_authority`) runs on the **raw** layers **before** `merge_settings`, so a higher-precedence override cannot launder a forbidden field (`runtime_init/base.rs:122→124`, `norn-cli/.../resolve.rs:99→101`, `commands/session.rs:93→95` — all three production merge sites gate correctly; these are the *only* production `merge_settings` callers).
- **Relative-root escalation closed:** `NORN_HOME`/`HOME` must be absolute (`config/paths.rs` `validate_norn_home`/`validate_user_home`; `norn_dir`/`trusted_home_dir` ignore relative values), so a repo-relative CWD cannot become the trusted tier. Tested end-to-end in `resolve.rs`.

## 2. Coverage — every load surface accounted for

| Surface | Gate | Verified |
|---|---|---|
| settings.json / settings.local.json | reject-before-merge + no-follow read | `loader.rs` (`read_workspace_text_file`), `provider_security.rs` |
| `base_url`/`api_key_env`/`auth`/`options`/`debug_dump_dir`/`runner_path` (+ profile `api_shape`) | rejected from project/local & profiles | `restricted_provider_field` destructures all fields (new field → compile break) |
| model aliases selecting `provider_profile`/`api_shape` | rejected; indirect via workspace `model` and workspace-profile `model` rejected; catalog-name collision safe | `validate_indirect_backend_selection` + `resolve_model_selection` (catalog-first, so a user alias keyed to a catalog id can never select a backend — the two functions *agree*) |
| trusted backend-bundle collisions (SEC-14) | same-name project/local vs trusted user profile/alias rejected | `validate_trusted_backend_collisions` |
| all 13 hook slots | any non-empty workspace slot rejected | `contains_shell_hooks` (destructured) |
| rules `shell_source` (`.norn`/`.claude`/`.meridian`) | provenance-preserving single scan; untrusted `shell_source` → error, no exec | `context/scanner.rs` + `base.rs merge_discovered_rules` (untrusted-index logic robust to missing user tier) |
| context `NORN.md` (root + nested) | no-follow read; symlink refused incl. staleness refresh | `context/loader.rs`, `scanner.rs NestedScanner` |
| workspace profiles / capabilities | no-follow read; `prompt_commands` rejected; capabilities tier-isolated | `profile/loader.rs resolve_workspace_profile_at_launch_root` |
| model-selected profile prompt_commands (SEC-13) | rejected even for user-tier profile | `spawn.rs:150 validate_model_selected_profile` |
| workspace skills | no-follow read; **shell expansion always disabled** for workspace-sourced skills (`disable_shell = !cfg || workspace_root.is_some()`), independent of the user `shell_execution` bit | `tools/skill.rs:501`, `WorkspaceSkillRoot` wired in `runtime_init/extensions.rs install_skill_infra` |
| `tools.skill.shell_execution=true` | rejected from workspace (deny-only allowed) | `provider_security.rs` |
| variants `prompt_file` | workspace rejected pre-merge; user must be absolute; catalog also reads no-follow (defense-in-depth) | `provider_security.rs` + `agent/variants/catalog.rs` |
| `CONVENTIONS.toml` | no-follow read; `lsp`/`diagnostics`/`remediation`/`reports` stripped + `is_non_executing` fail-closed assertion; symlink refused | `tools/diagnostics_infra.rs` |
| skills/context `search_paths` | workspace rejected; user must be absolute; skill roots pinned at launch against alias-swap | `provider_security.rs`, `base.rs pin_skill_search_path` |
| `--rules` / `--profile <path>` / `-c` | trusted operator surfaces (ordinary read, `prompt_commands`/`shell_source` permitted) | `cli/config/rules.rs`, `profile_loader.rs looks_like_path` |
| `doctor` | reads **no** repo config (auth/connectivity/cwd/PATH only) | `commands/doctor.rs` |

The public `Scanner` / `scan_rule_dirs` / `discover_skills` / `resolve_profile` convenience APIs use ordinary reads but are documented trusted-input-only and have **no production caller with workspace roots** (verified by grep) — this matches the dispositioned SEC-11 residual.

## 3. Untrusted-mode semantics — fail-closed, and the user is told

A forbidden workspace field produces a **typed hard error that aborts assembly**, naming the exact field (`provider.base_url`, `hooks`, `variants.<variant>.prompt_file`, …) and **never echoing the value or the repo-controlled profile name** (extensively asserted). This is fail-closed, not silent-skip — the correct answer to the review's concern. Non-Unix targets fail closed on any present workspace input (`secure_file.rs` `no_nofollow_error`), an intentional, documented compatibility break.

The no-follow primitive (`util/secure_file.rs`) is the security backstop: it walks every component of root+relative from `/` with `O_NOFOLLOW`, requires an absolute root and a regular final file, enumerates via pinned descriptors, and recognizes `/var`↔`/private/var` alternate spellings *without* canonicalizing the final candidate (so inner symlinks stay visible). `workspace_relative_path`'s use of `canonicalize` is classification-only; the authoritative read always re-walks no-follow, so a stale classification cannot grant escape.

## 4. Findings (all Low / Informational)

- **OBS-1 (Info, positive):** Untrusted config is fail-closed with a field-named diagnostic — better than silent skipping. Confirmed intended.
- **OBS-2 (Low):** `base.rs build_shared_task_store` uses `norn_dir().unwrap_or_else(|| PathBuf::from(".norn"))`, and `norn-cli/config/paths.rs session_data_dir` falls back to relative `./.norn/sessions`, when neither `$NORN_HOME` nor `$HOME` resolves (chroot/CI). In such environments credential-adjacent session/task artifacts land inside the workspace tree. Not a trust-*read* escalation, but it intersects SEC-15: the session/spool private-permission policy (0700/0600, no-follow) must hold **regardless of this location fallback**. Belongs to the session-module reviewer to confirm; flagging the shared origin here.
- **OBS-3 (Info):** Context `search_paths` are not pinned like skill roots. Safe by construction — `validate` rejects workspace context search paths outright and requires user ones absolute, so only trusted absolute user paths reach `install_context_search_paths`; there is no workspace/user shell-trust distinction for context reads that a pin would protect.

## 5. Coverage seam routed to the provider/spawn reviewer (not this review's ownership)

A model-selected **user** profile's `model` field can be a backend-selecting alias; whether the spawned child re-resolves a new backend/credential bundle from `profile.model` (vs. inheriting the parent's provider) is decided in `tools/agent` spawn/provider assembly. `prompt_commands` on such profiles are correctly rejected (SEC-13), but the *model→backend* activation on child spawn should be confirmed by that reviewer to fully close the SEC-08 confused-deputy class for the child path.
