#!/usr/bin/env python3
"""Run provenance-bearing P0 Gate C and repeated-distribution evidence."""

from __future__ import annotations

import importlib
import json
import sys
from pathlib import Path
from typing import Final

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
manifest = importlib.import_module("p0_evidence_manifest")
cli = importlib.import_module("p0_evidence_cli")
gate_case_support = importlib.import_module("p0_evidence_gate_cases")
policy_contract = importlib.import_module("p0_evidence_policy")
paths = importlib.import_module("p0_evidence_paths")
run_support = importlib.import_module("p0_evidence_run_support")
support = importlib.import_module("p0_evidence_support")
Case = support.Case
evidence_environment = support.evidence_environment
metadata = support.metadata
prepare_fresh_target_dir = support.prepare_fresh_target_dir
repository_state = support.repository_state
run_cases = support.run_cases
rust_test_executions = run_support.rust_test_executions
validate_distribution_inventory = run_support.validate_distribution_inventory
write_result = run_support.write_result


BASE: Final = "41ea210"
TOOLCHAIN: Final = "1.94.0"
MINIMUM_REPEATED_RUNS: Final = 20
MINIMUM_CONCURRENCY_RUNS: Final = 50
MINIMUM_DISTRIBUTION_OBSERVATIONS: Final = 750


def cargo(*args: str) -> tuple[str, ...]:
    return ("cargo", f"+{TOOLCHAIN}", "--locked", *args)


def repository_root() -> Path:
    return Path(__file__).resolve().parents[3]


def gate_cases(
    policy_output: Path, head: str, python_executable: str | None = None
) -> list[Case]:
    return gate_case_support.gate_cases(
        policy_output, head, cargo, BASE, python_executable
    )


def distribution_cases(concurrency_runs: int, other_runs: int) -> list[Case]:
    descriptor_retention_tests = (
        "tests::descriptor_retention::active_process_permits_release_on_terminal_paths",
        "tests::descriptor_retention::completed_process_registry_stays_bounded",
        "tests::descriptor_retention::lazy_session_reopen_rejects_replaced_inode",
        "tests::descriptor_retention::lazy_spool_reopen_rejects_replaced_inode",
        "tests::descriptor_retention::retained_idle_process_spools_stay_bounded",
        "tests::descriptor_retention::retained_idle_session_sinks_stay_bounded",
    )
    cases = [
        Case(
            name,
            "macos_concurrency",
            cargo("test", "-p", "norn", "--lib", name, "--", "--exact"),
            concurrency_runs,
            expected_tests=1,
            expected_test_names=(name,),
        )
        for name in (
            "session::manager::tests::open_or_resume_concurrent_same_id_converges_on_one_session",
            "util::private_fs::tests::concurrent_independent_roots_open_one_shared_lock",
            "util::private_fs::tests::concurrent_create_new_has_exactly_one_winner",
        )
    ]
    cases.append(
        Case(
            "descriptor_retention",
            "descriptor_admission",
            cargo("test", "-p", "norn", "--lib", "tests::descriptor_retention::"),
            other_runs,
            expected_tests=6,
            expected_test_names=descriptor_retention_tests,
        )
    )
    cases.extend(
        repeated_norn_cases(
            "descriptor_admission",
            (
                "tools::bash::process::admission_tests::cancelling_foreground_shell_releases_child_drains_and_capacity",
                "tools::bash::tests::timeout_with_partial_output_migrates_and_seeds_spool",
                "process::manager::launch::tests::cancellation_before_adoption_commit_kills_the_process",
                "resource::private_line_log::tests::concurrent_writers_preserve_complete_records",
                "tools::task::disk::admission_tests::exact_weight_supports_nested_work_without_nested_admission",
                "provider::openai_oauth::browser::tests::dropping_launcher_reaps_child_and_releases_spawn_peak",
                "provider::openai_oauth::browser::tests::dropping_delegated_launcher_neither_waits_for_nor_terminates_child",
                "provider::openai_oauth::browser::tests::background_stdin_delivery_releases_its_retained_permit",
            ),
            other_runs,
        )
    )
    cases.extend(
        (
            Case(
                "held_lock_times_out_typed_with_config_derived_deadline",
                "gate_d_corrections",
                cargo(
                    "test",
                    "-p",
                    "norn-cli",
                    "--test",
                    "index_lock_deadline",
                    "held_lock_times_out_typed_with_config_derived_deadline",
                    "--",
                    "--exact",
                ),
                other_runs,
                expected_tests=1,
                expected_test_names=(
                    "held_lock_times_out_typed_with_config_derived_deadline",
                ),
            ),
            Case(
                "resume_repair_drops_stale_anchor_for_first_healed_request",
                "gate_d_corrections",
                cargo(
                    "test",
                    "-p",
                    "norn",
                    "--lib",
                    "r#loop::conversation_state::tests::resume_repair_drops_stale_anchor_for_first_healed_request",
                    "--",
                    "--exact",
                ),
                other_runs,
                expected_tests=1,
                expected_test_names=(
                    "r#loop::conversation_state::tests::resume_repair_drops_stale_anchor_for_first_healed_request",
                ),
            ),
            Case(
                "norn-tui pty_smoke",
                "pty",
                cargo("test", "-p", "norn-tui", "--test", "pty_smoke"),
                other_runs,
                expected_tests=17,
                expected_test_names=manifest.PTY_TEST_NAMES,
            ),
        )
    )
    cases.extend(
        repeated_norn_cases(
            "mcp_startup",
            (
                "tools::agent::spawn_mcp_tests::variant_child_can_widen_root_mcp_view_and_dispatch_beta",
                "integration::mcp_http::adversarial_tests::real_http_rejects_hostile_initialize_envelopes",
                "integration::mcp_stdio::tests::cancellation_after_write_invalidates_the_channel",
            ),
            other_runs,
        )
    )
    cases.append(
        Case(
            "approved_project_server_connects_through_startup",
            "mcp_startup",
            cargo(
                "test",
                "-p",
                "norn-cli",
                "--lib",
                "runtime::mcp::tests::approved_project_server_connects_through_startup",
                "--",
                "--exact",
            ),
            other_runs,
            expected_tests=1,
            expected_test_names=(
                "runtime::mcp::tests::approved_project_server_connects_through_startup",
            ),
        )
    )
    cases.extend(
        repeated_norn_cases(
            "mcp_live",
            (
                "integration::mcp_control::refresh_tests::pre_subscription_change_is_refreshed",
                "integration::mcp_control::refresh_tests::change_during_refresh_schedules_the_latest_revision",
                "integration::mcp_control::refresh_tests::failed_refresh_reconnects_without_another_server_revision",
                "integration::mcp_control::refresh_tests::failed_refresh_and_reconnect_publish_an_honest_disconnected_surface",
                "integration::mcp_control::refresh_tests::removed_client_is_not_retained_by_its_watcher",
                "integration::mcp_context_call_tests::public_root_update_cannot_split_contextual_tool_call",
                "integration::mcp_stdio::tests::inherited_stderr_descendant_cannot_retain_transport_capacity",
                "tools::agent::live_tools::tests::new_child_observes_replaced_pool_while_existing_child_keeps_lease",
                "integration::mcp_stdio::tests::dropping_transport_returns_retained_descriptor_capacity",
            ),
            other_runs,
        )
    )
    cases.append(
        Case(
            "live_definition_secrets_never_reach_file_backed_history",
            "mcp_live",
            cargo(
                "test",
                "-p",
                "norn-tui",
                "--lib",
                "app::event_loop::tests::live_definition_secrets_never_reach_file_backed_history",
                "--",
                "--exact",
            ),
            other_runs,
            expected_tests=1,
            expected_test_names=(
                "app::event_loop::tests::live_definition_secrets_never_reach_file_backed_history",
            ),
        )
    )
    cases.append(
        Case(
            "dropped_ui_waiter_does_not_cancel_an_enqueued_mutation",
            "mcp_live",
            cargo(
                "test",
                "-p",
                "norn-tui",
                "--lib",
                "app::mcp_slash::tests::dropped_ui_waiter_does_not_cancel_an_enqueued_mutation",
                "--",
                "--exact",
            ),
            other_runs,
            expected_tests=1,
            expected_test_names=(
                "app::mcp_slash::tests::dropped_ui_waiter_does_not_cancel_an_enqueued_mutation",
            ),
        )
    )
    cases.extend(
        repeated_norn_cases(
            "oauth_callback",
            (
                "provider::openai_oauth::login_server::tests::accepted_connection_waits_for_delayed_request_bytes",
                "provider::openai_oauth::login_server::tests::accepted_stream_is_normalized_to_blocking_mode",
                "provider::openai_oauth::login_server::tests::cancellation_interrupts_partial_callback_request",
                "provider::openai_oauth::login_server::tests::cancellation_releases_callback_listener_without_waiting_for_deadline",
                "provider::openai_oauth::login_server::tests::matching_error_callback_fails_the_flow_with_a_400_page",
                "provider::openai_oauth::login_server::tests::stray_requests_get_404_and_login_still_completes",
                "provider::openai_oauth::login_server::tests::wait_times_out_when_no_matching_callback_arrives",
            ),
            other_runs,
        )
    )
    return cases


def repeated_norn_cases(group: str, names: tuple[str, ...], runs: int) -> list[Case]:
    return [
        Case(
            name,
            group,
            cargo("test", "-p", "norn", "--lib", name, "--", "--exact"),
            runs,
            expected_tests=1,
            expected_test_names=(name,),
        )
        for name in names
    ]


def main() -> int:
    args = cli.parse_runner_args(MINIMUM_REPEATED_RUNS, MINIMUM_CONCURRENCY_RUNS)
    root = repository_root()
    requested_policy = args.policy_output if args.mode == "gate" else None
    validated = paths.validate_runner_paths(
        root, args.target_dir, args.output, requested_policy
    )
    prepare_fresh_target_dir(validated.target_dir)
    environment, removed_environment_count = evidence_environment(validated.target_dir)
    result = metadata(
        root,
        environment,
        removed_environment_count,
        args.mode,
        BASE,
        TOOLCHAIN,
    )
    if args.mode == "distributions":
        support.require_macos_apfs_distribution_host(result)
    if args.mode == "gate":
        if validated.policy_output is None:
            raise RuntimeError("gate mode requires a validated policy output")
        cases = gate_cases(validated.policy_output, str(result["head"]))
        if tuple(case.case_id for case in cases) != manifest.GATE_CASE_IDS:
            raise RuntimeError("P0 Gate C identity inventory changed")
    else:
        cases = distribution_cases(args.concurrency_runs, args.other_runs)
        validate_distribution_inventory(cases, args.concurrency_runs, args.other_runs)
        observations = sum(case.runs for case in cases)
        if len(cases) != len(manifest.DISTRIBUTION_INVENTORY):
            raise RuntimeError("P0 distribution case inventory changed")
        if observations < MINIMUM_DISTRIBUTION_OBSERVATIONS:
            raise RuntimeError(
                "P0 distribution observations fell below the gate minimum"
            )
    records = run_cases(root, environment, cases)
    final_state = repository_state(root, environment)
    integrity_passed = final_state == {
        "head": result["head"],
        "worktree_clean": True,
    }
    result["cases"] = records
    result["passed"] = sum(record["passed"] for record in records)
    result["failed"] = sum(record["failed"] for record in records)
    result["runner_observations"] = sum(record["runs"] for record in records)
    result["rust_test_executions"] = rust_test_executions(records)
    if args.mode == "gate":
        policy_artifact, policy_contract_passed = policy_contract.bind_policy_artifact(
            validated.policy_output, root, str(result["head"]), BASE
        )
        policy_artifact.pop("file_name", None)
        result["policy_artifact"] = policy_artifact
        result["policy_contract_passed"] = policy_contract_passed
        integrity_passed = integrity_passed and policy_contract_passed
    result["final_repository_state"] = final_state
    result["repository_integrity_passed"] = integrity_passed
    write_result(validated.output, result)
    print(json.dumps(result, indent=2, sort_keys=True))
    return 1 if result["failed"] or not integrity_passed else 0


if __name__ == "__main__":
    raise SystemExit(main())
