"""Pinned test identities for the P0 retained-evidence contract."""

from typing import Final


EVIDENCE_SELF_TEST_MODULES: Final = (
    "test_p0_evidence_contract.py",
    "test_p0_evidence_paths.py",
    "test_p0_test_output.py",
)
# Updated deliberately whenever the pinned module inventory changes.
EVIDENCE_SELF_TEST_COUNT: Final = 44


DOCTEST_NAMES: Final = (
    "crates/norn/src/agent/builder.rs - agent::builder (line 14) - compile",
    "crates/norn/src/agent/builder_setters.rs - agent::builder_setters::AgentBuilder::child_policy (line 503)",
    "crates/norn/src/agent/child_policy.rs - agent::child_policy::ChildPolicy (line 314)",
    "crates/norn/src/agent/handle.rs - agent::handle (line 30) - compile",
    "crates/norn/src/provider/openai_oauth/manager.rs - provider::openai_oauth::manager::AuthManager (line 45) - compile fail",
    "crates/norn/src/provider/openai_oauth/manager.rs - provider::openai_oauth::manager::AuthManager (line 50) - compile fail",
    "crates/norn/src/runtime_init/base.rs - runtime_init::base::load_merged_settings (line 111) - compile fail",
    "crates/norn/src/runtime_init/base.rs - runtime_init::base::load_merged_settings (line 115) - compile fail",
)

PTY_TEST_NAMES: Final = (
    "run_app_budgets_rows_on_small_terminal",
    "run_app_child_entrypoint",
    "run_app_clears_input_panel_immediately_after_submit",
    "run_app_grows_and_shrinks_input_panel_without_artifacts",
    "run_app_handles_bracketed_paste_and_autocomplete",
    "run_app_handles_resize_during_streaming_output",
    "run_app_keeps_streaming_output_out_of_input_panel_after_typing",
    "run_app_renders_child_activity_rows_in_screen_model",
    "run_app_renders_effort_confirmation_above_input_panel",
    "run_app_renders_provider_output_in_screen_model",
    "run_app_renders_tools_block_above_input_panel",
    "run_app_replays_resumed_session_history_in_screen_model",
    "run_app_soft_wraps_long_output_in_screen_model",
    "run_app_surfaces_child_result_while_turn_is_active",
    "run_app_wakes_idle_root_on_inbound_steer",
    "run_tui_child_entrypoint",
    "run_tui_sets_up_and_restores_terminal_in_pty",
)

SECRET_SENTINELS: Final = (
    "integration::mcp_control::tests::status_and_debug_surfaces_do_not_disclose_definition_secrets",
    "integration::mcp_http::tests::connection_errors_do_not_disclose_endpoint_url_secrets",
    "integration::mcp_http::tests::remote_error_surfaces_do_not_disclose_configured_header_secrets",
    "integration::mcp_live_command::tests::inspect_renderer_redacts_definition_values",
    "integration::mcp_live_command::tests::malformed_secret_input_is_not_echoed",
    "integration::mcp_stdio::tests::server_stderr_metadata_does_not_disclose_configured_secrets",
    "provider::debug::security_tests::response_metadata_redacts_credential_and_redirect_values",
    "provider::exec::tests::endpoint_path_secrets_do_not_reach_errors_or_traces",
    "provider::exec::tests::specialized_401_and_429_paths_never_wait_for_or_disclose_the_body",
    "provider::openai::request::tests::tool_result_missing_call_id_returns_error",
    "provider::openai_compatible::request::tests::missing_tool_result_id_is_hard_error",
    "provider::openai_compatible::sse::tests::malformed_chunk_does_not_disclose_provider_payload",
    "provider::openai_compatible::sse::tests::maps_stream_error_object_to_terminal_error",
    "provider::openai_compatible::sse::tests::maps_unknown_stream_error_without_disclosing_provider_text",
    "provider::openai_compatible::sse::tests::missing_tool_name_does_not_disclose_provider_call_id",
    "provider::openai_compatible::sse::tests::unknown_finish_reason_is_parse_error_without_disclosure",
    "provider::openai_oauth::browser::tests::launcher_errors_do_not_disclose_authorization_url",
    "provider::openai_oauth::browser::tests::macos_launcher_keeps_authorization_url_out_of_argv_and_environment",
    "provider::openai_oauth::login_server::tests::classify_matching_error_callback_fails_without_disclosure",
    "provider::openai_oauth::login_server::tests::matching_error_callback_fails_the_flow_with_a_400_page",
    "provider::openai_oauth::manager::security_tests::manager_debug_redacts_cached_credentials",
    "provider::openai_oauth::pkce::security_tests::pkce_debug_redacts_verifier_and_challenge",
    "provider::openai_oauth::types::security_tests::credential_debug_is_structural_and_redacted",
    "r#loop::assembly::tests::fallback_warning_does_not_disclose_streaming_merge_key",
    "r#loop::tool_dispatch::tests::spool_write_failure_is_typed_and_appends_no_event",
)

MODEL_FACING_SENTINELS: Final = (
    "r#loop::tool_dispatch::batch::tests::spawned_member_panic_surfaces_structured_failure",
)

GATE_CASE_IDS: Final = (
    "fmt",
    "strict_clippy",
    "workspace_check",
    "workspace_all_targets",
    "norn_tests",
    "norn_cli_tests",
    "norn_tui_tests",
    "workspace_docs",
    "norn_test_utils_docs",
    "phase_diff_check",
    "evidence_tooling_self_tests",
    "full_range_policy",
    *SECRET_SENTINELS,
    *MODEL_FACING_SENTINELS,
)

# id, group, run profile, expected count, exact emitted test names
DISTRIBUTION_INVENTORY: Final = (
    (
        "session::manager::tests::open_or_resume_concurrent_same_id_converges_on_one_session",
        "macos_concurrency",
        "concurrency",
        1,
        (
            "session::manager::tests::open_or_resume_concurrent_same_id_converges_on_one_session",
        ),
    ),
    (
        "util::private_fs::tests::concurrent_independent_roots_open_one_shared_lock",
        "macos_concurrency",
        "concurrency",
        1,
        ("util::private_fs::tests::concurrent_independent_roots_open_one_shared_lock",),
    ),
    (
        "util::private_fs::tests::concurrent_create_new_has_exactly_one_winner",
        "macos_concurrency",
        "concurrency",
        1,
        ("util::private_fs::tests::concurrent_create_new_has_exactly_one_winner",),
    ),
    (
        "descriptor_retention",
        "descriptor_admission",
        "other",
        6,
        (
            "tests::descriptor_retention::active_process_permits_release_on_terminal_paths",
            "tests::descriptor_retention::completed_process_registry_stays_bounded",
            "tests::descriptor_retention::lazy_session_reopen_rejects_replaced_inode",
            "tests::descriptor_retention::lazy_spool_reopen_rejects_replaced_inode",
            "tests::descriptor_retention::retained_idle_process_spools_stay_bounded",
            "tests::descriptor_retention::retained_idle_session_sinks_stay_bounded",
        ),
    ),
    *(
        (name, "descriptor_admission", "other", 1, (name,))
        for name in (
            "tools::bash::process::admission_tests::cancelling_foreground_shell_releases_child_drains_and_capacity",
            "tools::bash::tests::timeout_with_partial_output_migrates_and_seeds_spool",
            "process::manager::launch::tests::cancellation_before_adoption_commit_kills_the_process",
            "resource::private_line_log::tests::concurrent_writers_preserve_complete_records",
            "tools::task::disk::admission_tests::exact_weight_supports_nested_work_without_nested_admission",
            "provider::openai_oauth::browser::tests::dropping_launcher_reaps_child_and_releases_spawn_peak",
            "provider::openai_oauth::browser::tests::dropping_delegated_launcher_neither_waits_for_nor_terminates_child",
            "provider::openai_oauth::browser::tests::background_stdin_delivery_releases_its_retained_permit",
        )
    ),
    (
        "held_lock_times_out_typed_with_config_derived_deadline",
        "gate_d_corrections",
        "other",
        1,
        ("held_lock_times_out_typed_with_config_derived_deadline",),
    ),
    (
        "resume_repair_drops_stale_anchor_for_first_healed_request",
        "gate_d_corrections",
        "other",
        1,
        (
            "r#loop::conversation_state::tests::resume_repair_drops_stale_anchor_for_first_healed_request",
        ),
    ),
    ("norn-tui pty_smoke", "pty", "other", 17, PTY_TEST_NAMES),
    *(
        (name, "mcp_startup", "other", 1, (name,))
        for name in (
            "tools::agent::spawn_mcp_tests::variant_child_can_widen_root_mcp_view_and_dispatch_beta",
            "integration::mcp_http::adversarial_tests::real_http_rejects_hostile_initialize_envelopes",
            "integration::mcp_stdio::tests::cancellation_after_write_invalidates_the_channel",
        )
    ),
    (
        "approved_project_server_connects_through_startup",
        "mcp_startup",
        "other",
        1,
        ("runtime::mcp::tests::approved_project_server_connects_through_startup",),
    ),
    *(
        (name, "mcp_live", "other", 1, (name,))
        for name in (
            "integration::mcp_control::refresh_tests::pre_subscription_change_is_refreshed",
            "integration::mcp_control::refresh_tests::change_during_refresh_schedules_the_latest_revision",
            "integration::mcp_control::refresh_tests::failed_refresh_reconnects_without_another_server_revision",
            "integration::mcp_control::refresh_tests::failed_refresh_and_reconnect_publish_an_honest_disconnected_surface",
            "integration::mcp_control::refresh_tests::removed_client_is_not_retained_by_its_watcher",
            "integration::mcp_context_call_tests::public_root_update_cannot_split_contextual_tool_call",
            "integration::mcp_stdio::tests::inherited_stderr_descendant_cannot_retain_transport_capacity",
            "tools::agent::live_tools::tests::new_child_observes_replaced_pool_while_existing_child_keeps_lease",
            "integration::mcp_stdio::tests::dropping_transport_returns_retained_descriptor_capacity",
        )
    ),
    (
        "live_definition_secrets_never_reach_file_backed_history",
        "mcp_live",
        "other",
        1,
        (
            "app::event_loop::tests::live_definition_secrets_never_reach_file_backed_history",
        ),
    ),
    (
        "dropped_ui_waiter_does_not_cancel_an_enqueued_mutation",
        "mcp_live",
        "other",
        1,
        (
            "app::mcp_slash::tests::dropped_ui_waiter_does_not_cancel_an_enqueued_mutation",
        ),
    ),
    *(
        (name, "oauth_callback", "other", 1, (name,))
        for name in (
            "provider::openai_oauth::login_server::tests::accepted_connection_waits_for_delayed_request_bytes",
            "provider::openai_oauth::login_server::tests::accepted_stream_is_normalized_to_blocking_mode",
            "provider::openai_oauth::login_server::tests::cancellation_interrupts_partial_callback_request",
            "provider::openai_oauth::login_server::tests::cancellation_releases_callback_listener_without_waiting_for_deadline",
            "provider::openai_oauth::login_server::tests::matching_error_callback_fails_the_flow_with_a_400_page",
            "provider::openai_oauth::login_server::tests::stray_requests_get_404_and_login_still_completes",
            "provider::openai_oauth::login_server::tests::wait_times_out_when_no_matching_callback_arrives",
        )
    ),
)
