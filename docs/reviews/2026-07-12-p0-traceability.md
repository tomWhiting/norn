# P0 finding-to-evidence traceability

**Date:** 2026-07-12
**Phase base:** `41ea210`
**Snapshot code head:** `bfa0b8e`
**Status:** candidate matrix and machine Gate C complete; independent acceptance remains open
**Source review:** `docs/reviews/2026-07-10-responses-api-implementation-review.md`

## Evidence rule

The source review is the retained baseline proof where the campaign did not
capture a failing executable fixture before implementation. Such a row is
labelled **source proof**, not retroactively described as a test run. Candidate
tests are named exactly. Their final status comes only from the completed Gate C
run at the final P0 head; listing a test here is not a pass claim.

## Credential, backend, and configuration authority

| Finding | Baseline proof | Candidate regression(s) | State at final Gate C |
|---|---|---|---|
| `SEC-01` | Source proof at review lines 142 onward: project `base_url` can receive OAuth bearer/account headers. | `config/provider_security.rs::project_base_url_is_rejected_without_echoing_value`; `provider/openai/provider.rs::hostile_oauth_destination_is_rejected_before_auth_application`; `provider/openai/backend.rs::oauth_rejects_every_noncanonical_destination_without_echoing_it`; redirect tests in `provider/http_client.rs`. | Candidate present. |
| `SEC-02` | Source proof at lines 165 onward: project endpoint plus `api_key_env` selects ambient secrets. | `config/provider_security.rs::{local_api_key_env_is_rejected_without_echoing_name,project_base_url_is_rejected_without_echoing_value}`; real entrypoints `runtime_init/base.rs::shared_settings_loader_rejects_working_directory_provider_authority`, `norn-cli/runtime/resolve.rs::resolve_invocation_rejects_restricted_working_directory_provider_fields`, and `norn-cli/commands/session.rs::subcommand_settings_reject_working_directory_authority_before_merge`. | Component and real-entrypoint evidence; no claim of one combined hostile endpoint/env fixture. |
| `SEC-03` | Source proof at lines 186 onward: repository chooses raw debug-dump path. | `config/provider_security.rs::debug_sinks_and_executable_paths_are_rejected`; `provider/debug.rs::{dump_files_are_private_and_symlinks_are_rejected,append_rejects_an_ancestor_repoint_without_touching_outside_file}`. | Candidate present. |
| `SEC-04` | Source proof at lines 206 onward: repository chooses Claude Runner executable. | `config/provider_security.rs::debug_sinks_and_executable_paths_are_rejected`; CLI entrypoint rejection in `norn-cli/runtime/resolve.rs`. | Rejection-before-construction evidence; no separate executable sentinel is claimed. |
| `SEC-05` | Source proof at lines 221 onward: `test-utils` shipped arbitrary OAuth authority constructors. | Two external `compile_fail` doctests on `provider/openai_oauth/manager.rs::AuthManager`, run both normally and with `--features test-utils`; constructors themselves are `#[cfg(test)] pub(crate)`. | Final Gate C ran all eight `test-utils` doctests successfully; independent acceptance remains open. |
| `BACKEND-01` | Source proof at lines 469 onward: explicit canonical ChatGPT URL classified as direct API. | `provider/openai/provider.rs::{implicit_and_explicit_canonical_oauth_have_identical_semantics,implicit_and_explicit_canonical_oauth_serialize_identical_payloads}`; `provider/openai/backend.rs::{oauth_without_override_resolves_to_compiled_codex_backend,normalized_canonical_oauth_spellings_use_compiled_url}`. | Candidate present. |
| `BACKEND-02` | Provisional source proof at review lines 535 onward: removing generic auth injection left no constrained embedder path. | `tests/static_codex_provider_api.rs::embedder_can_construct_pinned_non_refreshing_codex_provider`; static-Codex destination/header/401/config tests in `provider/openai/provider.rs`. | Norn contract candidate present; Meridian upgrade remains separately owned downstream evidence. |

## Workspace command and read authority

| Finding | Baseline proof | Candidate regression(s) | State at final Gate C |
|---|---|---|---|
| `SEC-07` | Source proof at lines 273 onward: hooks, rule shell sources, and prompt commands execute from repository data. | `config/provider_security.rs::every_working_directory_shell_hook_slot_is_rejected_without_echoing_commands`; shared-loader hook/rule tests in `runtime_init/base.rs`; workspace profile tests in `profile/loader.rs`; child path in `tools/agent/spawn.rs`. | Candidate present. |
| `SEC-08` | Source proof at lines 301 onward: working-directory model/alias activates trusted backend credentials. | Working-directory alias/model/collision tests in `config/provider_security.rs`; shared and CLI entrypoint tests in `runtime_init/base.rs` and `norn-cli/runtime/resolve.rs`. | Candidate present. |
| `SEC-08A` | Provisional source proof at review lines 545 onward: child inheritance lacked a no-reconstruction regression. | `tools/agent/spawn_context.rs::{spawn_and_fork_inherit_exact_parent_provider_authority,child_entry_paths_cannot_reinterpret_model_as_backend_alias}`. | Candidate present. |
| `SEC-09` | Source proof at lines 323 onward: repository variant `prompt_file` reads arbitrary paths. | `config/provider_security.rs::{working_directory_variant_prompt_files_are_rejected_without_echoing_paths,user_file_sources_must_be_absolute_to_avoid_working_directory_redirects}`; `agent/variants/catalog.rs::prompt_file_inside_workspace_refuses_symlink_even_when_path_is_absolute`. | Candidate present. |
| `SEC-10` | Source proof at lines 338 onward: repository re-enables trusted skill shell expansion. | `config/provider_security.rs::working_directory_cannot_enable_skill_shell_execution`; `tools/skill.rs::workspace_skill_shell_is_disabled_even_when_tool_default_is_enabled`. | Candidate present. |
| `SEC-11` | Source proof at lines 352 onward: repeated path resolution and links escape/change the workspace root. | Secure-file final/intermediate/launch-root tests; settings/context/rule/profile/skill/conventions family tests named in the P0 Gate C handoff and remediation plan. | Candidate family present; final reviewer rechecks the enumerated automatic-reader families. |
| `SEC-12` | Source proof at lines 389 onward: skills and conventions translate repository text into processes. | Workspace-skill disable/symlink tests in `tools/skill.rs`; LOC/pattern-only and symlink tests in `tools/diagnostics_infra.rs`. | Candidate present. |
| `SEC-13` | Source proof at lines 409 onward: model-selected trusted profile executes prompt commands. | `profile/loader.rs::static_workspace_and_user_prompt_command_profiles_keep_their_trust_contract`; child validation through `tools/agent/spawn_context.rs`. | Fixture covers the exact confused-deputy case although the legacy test name is broad. |
| `SEC-14` | Source proof at lines 425 onward: provider options, profile API shape, name collisions, and the dormant merged `mcp_servers` map select authority indirectly. | Provider-profile/options/alias/collision tests in `config/provider_security.rs`; protected compatible-request keys in `provider/openai_compatible/request.rs`; startup provenance/approval/activation fixtures; spawned-child dispatch; hostile HTTP initialization and stdio cancellation; live mutation, contextual-root, catalogue-refresh, generation-swap, watcher-release, history-redaction, and descriptor-release fixtures named in the final distribution manifest. | Startup and live MCP candidates are implemented. The final Gate C is green and the final distribution contains 20/20 observations for each selected MCP seam; independent whole-phase acceptance remains open. |
| `SEC-16` | Provisional source proof at review lines 527 onward and credential review section 01 lines 64 onward: public raw load/merge bypasses validation. | Two external `compile_fail` doctests on `runtime_init/base.rs::load_merged_settings`; implementation validates raw layers before merge. | Candidate present. |

## Disclosure, terminal diagnostics, and redirects

| Finding | Baseline proof | Candidate regression(s) | State at final Gate C |
|---|---|---|---|
| `SEC-06` | Source proof at review lines 238 onward: credentials/claims/metadata/authority bodies can escape diagnostics. | Exact final non-disclosure identities are pinned in `p0_evidence_manifest.py`: credential-bearing OAuth/PKCE/manager `Debug`; MCP definition/history/endpoint rendering; response metadata; request/error-body/provider-payload and malformed SSE paths; OAuth browser/callback errors; tool-result IDs; and loop assembly/dispatch errors. The panic-conversion case is separately classified as model-facing only. | All 23 secret/non-disclosure sentinels and the separately scoped model-facing sentinel passed at final Gate C; external Gate D reproduction remains open. |
| `NF-1` | Provisional source proof at review lines 551 onward and transport review section 02 lines 64 onward: unknown terminal values lose their discriminator. | Opaque unknown failed/incomplete classification/equality/non-disclosure tests in `provider/openai/sse_types.rs`; raw-wire tests in `provider/openai/sse.rs`. | Candidate present. |
| `NF-2` | Same provisional review, transport review lines 85 onward: redirect refusal appears as generic stream/body failure. | `provider/exec_tests.rs::every_redirect_status_is_an_explicit_terminal_policy_refusal`; no-follow redirect client tests. | Candidate present. |
| `NF-4` | Provisional review lines 557 onward and transport review lines 118 onward: blanket redaction removes correlation metadata. | `provider/debug.rs::response_metadata_redacts_credential_and_redirect_values`; `provider/exec_tests.rs::specialized_401_and_429_paths_never_wait_for_or_disclose_the_body`. | Intentional limitation under D1; evidence proves full value redaction, not preserved correlation. |

## Private artifacts and quality policy

| Finding | Baseline proof | Candidate regression(s) | State at final Gate C |
|---|---|---|---|
| `SEC-15` | Source proof at review lines 445 onward plus Gate D finding F2: artifact families had mixed permissions, link behavior, relative roots, and a workspace fetch cache. | Session/index/lock tests in `session/persistence/tests.rs`; session spool tests in `session/spool.rs`; process spool tests; task disk tests; Bash private-output tests; fetched-artifact tests; TUI `resource/private_line_log_tests.rs` covering modes, final links, torn tails, corrupt data, and concurrent writers. Exact writer enumeration and classification: `2026-07-12-p0-artifact-writer-inventory.md` plus `2026-07-14-p0-final-policy-bfa0b8e.json`. | Candidate present; 97 writer candidates enumerated and machine all-target gate green. Layout redesign is explicitly not claimed; final inventory reconciliation remains with the reviewer. |
| `QUAL-01` | Provisional source proof at review lines 559 onward: 177 campaign-added panic/unwrap/expect calls were hidden by inherited test allowances. | `docs/reviews/evidence/run_p0_policy_evidence.py`, `p0-rust-items.yml`, and `2026-07-14-p0-final-policy-bfa0b8e.json`; focused history correction removes its inherited `unwrap_used` allowance rather than adding another. | Full-range final-head generation and mechanical attestation cover 333 changed Rust files with zero added policy matches, zero production files over 500 LOC, and zero thin-entrypoint violations; external Gate D reproduction remains open. |

## Finalization requirements

Before this record supports a P0 closure claim:

1. Independently verify the D1D/SEC-14 provenance, approval, hostile protocol,
   complete-pool/view, live mutation, catalogue refresh, and real child-dispatch
   evidence.
2. Independently reproduce the final policy/writer inventory and repeated
   machine distributions recorded at the tested P0 code head.
3. Dispose the retrospective Gate A and Gate B evidence gaps without inventing
   historical proof.
4. Obtain the fresh independent whole-P0 Gate D verdict. This record does not
   substitute for that review.
