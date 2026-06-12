#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Arc;

    use norn::agent::registry::AgentRegistry;
    use norn::agent_loop::config::ToolExecutor;
    use norn::profile::Profile;
    use norn::provider::traits::Provider;
    use norn::session::store::EventStore;
    use norn::tool::registry::ToolRegistry;
    use norn::tools::agent::AgentToolInfra;
    use serde_json::json;
    use uuid::Uuid;

    use crate::cli::BuildError;
    use crate::runtime::wiring::{
        build_diagnostic_collector, build_slash_state_from_bundle, install_agent_tool_infra,
        iteration_monitor_from_profile, length_limit_from_profile,
    };

    #[test]
    fn install_agent_tool_infra_installs_reachable_infra() {
        use norn::provider::mock::MockProvider;

        let registry = ToolRegistry::new();
        let tool_registry = Arc::new(ToolRegistry::new());
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![]));
        let agent_id = Uuid::new_v4();

        let root_inbound = install_agent_tool_infra(
            &registry,
            provider,
            Arc::new(EventStore::new()),
            agent_id,
            Arc::clone(&tool_registry),
            AgentRegistry::shared(),
            crate::runtime::cli_coordination_envelope(),
        );
        assert!(
            root_inbound.is_some(),
            "a registry with a shared context always wires the root inbound channel",
        );

        let shared = registry
            .shared_context()
            .expect("registry exposes a shared context");
        let infra = shared
            .get_extension::<AgentToolInfra>()
            .expect("AgentToolInfra installed and reachable via get_extension");
        assert_eq!(infra.agent_id, agent_id);
        assert!(infra.parent_id.is_none());
        assert!(infra.grant.is_none(), "the CLI root has no granting parent");
        assert!(
            infra.tool_registry.is_some(),
            "tool_registry must be wired so spawned sub-agents can dispatch tools",
        );

        // Carry-forward 4 (W3.2): the CLI's deliberate envelope is
        // published on the shared context so spawn-time policy reads
        // resolve instead of failing MissingExtension.
        let envelope = shared
            .get_extension::<norn::agent::child_policy::CoordinationEnvelope>()
            .expect("CoordinationEnvelope published by install_agent_tool_infra");
        assert_eq!(
            *envelope,
            crate::runtime::cli_coordination_envelope(),
            "the published envelope carries the CLI's deliberate values verbatim",
        );
    }

    /// A [`norn::agent_loop::inbound::ChannelMessage`] as the
    /// `signal_agent` tool would build it for the root recipient; the
    /// router stamps `to_id` and mints `seq` at delivery.
    fn root_bound_message(content: &str) -> norn::agent_loop::inbound::ChannelMessage {
        norn::agent_loop::inbound::ChannelMessage {
            id: Uuid::new_v4(),
            sender_id: Uuid::new_v4(),
            from: "/root/spawn/child".to_owned(),
            role: Some("worker".to_owned()),
            to_id: Uuid::nil(),
            content: content.to_owned(),
            kind: norn::agent_loop::inbound::MessageKind::Steer,
            seq: None,
            timestamp: chrono::Utc::now(),
        }
    }

    /// Shared installation for the W3.7 root-inbound wire tests: a fresh
    /// registry assembled exactly as the CLI drivers assemble it, with
    /// the CLI's deliberate envelope.
    fn installed_root_inbound(
        agent_id: Uuid,
    ) -> (
        ToolRegistry,
        Option<norn::agent_loop::inbound::InboundChannel>,
    ) {
        use norn::provider::mock::MockProvider;
        let registry = ToolRegistry::new();
        let tool_registry = Arc::new(ToolRegistry::new());
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![]));
        let root_inbound = install_agent_tool_infra(
            &registry,
            provider,
            Arc::new(EventStore::new()),
            agent_id,
            tool_registry,
            AgentRegistry::shared(),
            crate::runtime::cli_coordination_envelope(),
        );
        (registry, root_inbound)
    }

    /// W3.7 root inbound wiring: assembly registers a live root route in
    /// the [`norn::agent::message_router::MessageRouter`] under the
    /// root's id, and the channel's capacity is exactly the published
    /// envelope's `child_policy.inbound_capacity` — proven by filling it
    /// to the brim (every send accepted, sequence numbers minted in
    /// order) and observing the typed `ChannelFull` on the next send.
    /// An unrelated id stays honestly `NotRouted`.
    #[test]
    fn install_agent_tool_infra_registers_root_route_with_envelope_capacity() {
        use norn::agent::message_router::RouteError;

        let root_id = Uuid::new_v4();
        let (registry, root_inbound) = installed_root_inbound(root_id);
        let _root_inbound = root_inbound.expect("root inbound channel wired");

        let shared = registry.shared_context().expect("shared context");
        let infra = shared
            .get_extension::<AgentToolInfra>()
            .expect("AgentToolInfra installed");
        assert!(
            infra.router.is_routed(root_id),
            "the root's inbound sender must be registered under the root id",
        );

        let capacity = crate::runtime::cli_coordination_envelope()
            .child_policy
            .inbound_capacity;
        for n in 1..=capacity {
            let seq = infra
                .router
                .try_deliver(root_id, root_bound_message(&format!("msg {n}")))
                .unwrap_or_else(|err| panic!("send {n} of {capacity} must fit: {err}"));
            let expected = u64::try_from(n).expect("test capacity fits in u64");
            assert_eq!(seq, expected, "sequence numbers mint in send order");
        }
        assert_eq!(
            infra
                .router
                .try_deliver(root_id, root_bound_message("one past capacity")),
            Err(RouteError::ChannelFull { agent_id: root_id }),
            "send {} must report the typed ChannelFull — the channel is sized \
             from the envelope, not an invented constant",
            capacity + 1,
        );

        let stranger = Uuid::new_v4();
        assert_eq!(
            infra
                .router
                .try_deliver(stranger, root_bound_message("nobody home")),
            Err(RouteError::NotRouted { agent_id: stranger }),
            "ids without a route keep the honest NotRouted failure",
        );
    }

    /// W3.7 root inbound wiring: a message routed to the root's id lands
    /// in the very channel the driver drains — the returned
    /// [`norn::agent_loop::inbound::InboundChannel`] that the print
    /// orchestrator and the TUI event loop thread into the root's
    /// `AgentStepRequest.inbound` — stamped with the root as recipient
    /// and the router-minted sequence number.
    #[test]
    fn routed_root_message_lands_in_the_drained_channel() {
        let root_id = Uuid::new_v4();
        let (registry, root_inbound) = installed_root_inbound(root_id);
        let mut root_inbound = root_inbound.expect("root inbound channel wired");

        let shared = registry.shared_context().expect("shared context");
        let infra = shared
            .get_extension::<AgentToolInfra>()
            .expect("AgentToolInfra installed");
        infra
            .router
            .try_deliver(root_id, root_bound_message("status update for parent"))
            .expect("routed delivery to the root succeeds");

        let drained = root_inbound.drain();
        assert_eq!(drained.len(), 1, "exactly the routed message drains");
        assert_eq!(drained[0].content, "status update for parent");
        assert_eq!(drained[0].to_id, root_id, "router stamps the recipient");
        assert_eq!(drained[0].seq, Some(1), "router mints the first sequence");
        assert_eq!(
            drained[0].kind,
            norn::agent_loop::inbound::MessageKind::Steer,
        );
    }

    /// W3.7 route ownership: the root's route lives exactly as long as
    /// the driver-owned receiver. Dropping the receiver (driver
    /// teardown) is detected lazily on the next delivery as the typed
    /// `ChannelClosed`, which removes the stale route so later sends
    /// fail fast as `NotRouted` — the same lifecycle the library root
    /// has (`install_agent_infra`), with no explicit deregistration.
    #[test]
    fn dropping_the_root_receiver_closes_the_route_honestly() {
        use norn::agent::message_router::RouteError;

        let root_id = Uuid::new_v4();
        let (registry, root_inbound) = installed_root_inbound(root_id);
        drop(root_inbound.expect("root inbound channel wired"));

        let shared = registry.shared_context().expect("shared context");
        let infra = shared
            .get_extension::<AgentToolInfra>()
            .expect("AgentToolInfra installed");
        assert_eq!(
            infra
                .router
                .try_deliver(root_id, root_bound_message("after teardown")),
            Err(RouteError::ChannelClosed { agent_id: root_id }),
            "the dropped receiver surfaces as the typed ChannelClosed",
        );
        assert_eq!(
            infra
                .router
                .try_deliver(root_id, root_bound_message("after lazy cleanup")),
            Err(RouteError::NotRouted { agent_id: root_id }),
            "the stale route is removed on the failed delivery",
        );
    }

    /// Regression for the resume/working-dir `ActionLog` bug: the log must
    /// be constructed with the loop context's LIVE `SharedWorkingDir`
    /// (the handle bash's `cd` updates), not `ActionLog::new`'s
    /// process-CWD default — otherwise model-supplied relative paths
    /// hash into the mutation ledger against the wrong directory.
    #[test]
    fn install_action_log_resolves_paths_against_live_working_dir() {
        use norn::session::action_log::{CompletionRecord, Outcome};
        use norn::tool::context::SharedWorkingDir;

        let agent_dir = tempfile::tempdir().unwrap();
        assert_ne!(
            std::env::current_dir().unwrap(),
            agent_dir.path(),
            "precondition: agent working dir must differ from process CWD",
        );

        let registry = ToolRegistry::new();
        let mut loop_context = norn::agent_loop::loop_context::LoopContext {
            working_dir: SharedWorkingDir::new(agent_dir.path().to_path_buf()),
            ..Default::default()
        };

        let store = Arc::new(EventStore::new());
        crate::runtime::install_action_log(&registry, &store, &mut loop_context);

        let log = loop_context
            .action_log
            .as_ref()
            .expect("install_action_log must set LoopContext::action_log");
        log.record_completion(CompletionRecord {
            tool_name: "write",
            tool_call_id: "tc-1",
            tool_use_description: "",
            outcome: Outcome::Success,
            output: &json!({"path": "rel.txt"}),
            args: json!({"path": "rel.txt"}),
            duration_ms: 1,
            follow_ups: Vec::new(),
            post_validate_outcome: None,
            level_1_only: false,
        });

        let entries = log.mutation_entries();
        assert_eq!(entries.len(), 1, "write completion must hit the ledger");
        assert!(
            entries[0].file_path.starts_with(agent_dir.path()),
            "relative path must resolve against the live agent working dir \
             {}, got {}",
            agent_dir.path().display(),
            entries[0].file_path.display(),
        );
    }

    /// Regression for the empty-ledger-on-resume bug: a store that
    /// already carries replayed events (the `--resume` path) must yield
    /// an [`ActionLog`](norn::session::action_log::ActionLog) with the
    /// historical tool calls reconstructed, not an empty ledger.
    #[test]
    fn install_action_log_rebuilds_ledger_from_replayed_events() {
        use norn::session::events::{EventBase, EventUsage, SessionEvent, ToolCallEvent};

        let store = Arc::new(EventStore::new());
        store
            .append(SessionEvent::AssistantMessage {
                base: EventBase::new(None),
                content: String::new(),
                thinking: String::new(),
                tool_calls: vec![ToolCallEvent {
                    call_id: "tc-resumed".to_owned(),
                    name: "read".to_owned(),
                    arguments: json!({"path": "a.txt"}),
                    kind: norn::provider::request::ToolCallKind::Function,
                }],
                usage: EventUsage::default(),
                stop_reason: String::new(),
                response_id: None,
            })
            .unwrap();
        store
            .append(SessionEvent::ToolResult {
                base: EventBase::new(None),
                tool_call_id: "tc-resumed".to_owned(),
                tool_name: "read".to_owned(),
                output: json!({"content": "hello"}),
                duration_ms: 3,
            })
            .unwrap();

        let registry = ToolRegistry::new();
        let mut loop_context = norn::agent_loop::loop_context::LoopContext::default();
        crate::runtime::install_action_log(&registry, &store, &mut loop_context);

        let log = loop_context
            .action_log
            .as_ref()
            .expect("install_action_log must set LoopContext::action_log");
        let entries = log.entries();
        assert_eq!(
            entries.len(),
            1,
            "the replayed tool call must be reconstructed on resume",
        );
        assert_eq!(entries[0].tool_call_id, "tc-resumed");
        assert_eq!(entries[0].tool_name, "read");
    }

    fn empty_profile() -> Profile {
        Profile::default()
    }

    fn profile_with_iteration_monitor(value: serde_json::Value) -> Profile {
        let mut profile = Profile::default();
        profile
            .settings
            .insert("iteration_monitor".to_owned(), value);
        profile
    }

    fn profile_with_tool_config(write_value: &serde_json::Value) -> Profile {
        let mut profile = Profile::default();
        profile.settings.insert(
            "tool_config".to_owned(),
            json!({ "write": write_value.clone() }),
        );
        profile
    }

    #[test]
    fn diagnostic_collector_is_fresh_and_empty() {
        let collector = build_diagnostic_collector();
        assert!(collector.is_empty());
        assert_eq!(Arc::strong_count(&collector), 1);
    }

    #[test]
    fn iteration_monitor_absent_yields_none() {
        let cfg = iteration_monitor_from_profile(&empty_profile()).unwrap();
        assert!(cfg.is_none());
    }

    #[test]
    fn iteration_monitor_full_section_round_trips() {
        let profile = profile_with_iteration_monitor(json!({
            "context_window_tokens": 200_000u64,
            "warn_threshold_pct": 0.75,
            "handoff_threshold_pct": 0.90,
            "handoff_guidance": "wrap up",
            "failure_repeat_window": 3,
            "hedging_patterns": ["I cannot", "I'm unable"],
        }));
        let cfg = iteration_monitor_from_profile(&profile).unwrap().unwrap();
        assert_eq!(cfg.context_window_tokens, 200_000);
        assert!((cfg.warn_threshold_pct - 0.75).abs() < f64::EPSILON);
        assert!((cfg.handoff_threshold_pct - 0.90).abs() < f64::EPSILON);
        assert_eq!(cfg.handoff_guidance, "wrap up");
        assert_eq!(cfg.failure_repeat_window, 3);
        assert_eq!(
            cfg.hedging_patterns,
            vec!["I cannot".to_owned(), "I'm unable".to_owned()],
        );
    }

    #[test]
    fn iteration_monitor_missing_required_field_returns_argument_error() {
        let profile = profile_with_iteration_monitor(json!({
            "warn_threshold_pct": 0.75,
        }));
        let err = iteration_monitor_from_profile(&profile).unwrap_err();
        assert!(matches!(err, BuildError::Argument(_)));
    }

    #[test]
    fn length_limit_absent_profile_and_no_override_yields_none() {
        let limit = length_limit_from_profile(&empty_profile(), None).unwrap();
        assert!(limit.default.is_none());
        assert!(limit.overrides.is_empty());
    }

    #[test]
    fn length_limit_profile_default_lands_on_default() {
        let profile = profile_with_tool_config(&json!({ "max_code_lines": 500 }));
        let limit = length_limit_from_profile(&profile, None).unwrap();
        assert_eq!(limit.default, Some(500));
        assert!(limit.overrides.is_empty());
    }

    #[test]
    fn length_limit_profile_overrides_are_threaded_in_order() {
        use std::path::Path;
        let profile = profile_with_tool_config(&json!({
            "max_code_lines": 500,
            "length_overrides": [
                { "pattern": "tests/**", "limit": 800 },
                { "pattern": "**/*_test.rs", "limit": 1200 },
            ],
        }));
        let limit = length_limit_from_profile(&profile, None).unwrap();
        assert_eq!(limit.default, Some(500));
        assert_eq!(limit.overrides.len(), 2);
        // First-match-wins per LengthLimit::limit_for.
        assert_eq!(limit.limit_for(Path::new("tests/foo.rs")), Some(800));
        // Default applies when no override matches.
        assert_eq!(limit.limit_for(Path::new("src/lib.rs")), Some(500));
    }

    #[test]
    fn length_limit_overrides_without_default_keep_default_none() {
        let profile = profile_with_tool_config(&json!({
            "length_overrides": [
                { "pattern": "tests/**", "limit": 800 },
            ],
        }));
        let limit = length_limit_from_profile(&profile, None).unwrap();
        assert!(limit.default.is_none());
        assert_eq!(limit.overrides.len(), 1);
    }

    #[test]
    fn length_limit_invalid_glob_returns_argument_error_naming_pattern() {
        let profile = profile_with_tool_config(&json!({
            "length_overrides": [
                { "pattern": "[unterminated", "limit": 100 },
            ],
        }));
        match length_limit_from_profile(&profile, None) {
            Ok(_) => panic!("expected Argument error, got Ok"),
            Err(BuildError::Argument(reason)) => {
                assert!(reason.contains("[unterminated"), "reason: {reason}");
            }
            Err(other @ BuildError::Auth(_)) => panic!("expected Argument, got {other:?}"),
        }
    }

    #[test]
    fn length_limit_malformed_profile_section_returns_argument_error() {
        // `max_code_lines` must be a number; supplying a string surfaces
        // a serde failure as BuildError::Argument.
        let profile = profile_with_tool_config(&json!({ "max_code_lines": "five" }));
        match length_limit_from_profile(&profile, None) {
            Ok(_) => panic!("expected Argument error, got Ok"),
            Err(err) => assert!(matches!(err, BuildError::Argument(_))),
        }
    }

    #[test]
    fn length_limit_cli_override_alone_sets_default() {
        let limit = length_limit_from_profile(&empty_profile(), Some(800)).unwrap();
        assert_eq!(limit.default, Some(800));
        assert!(limit.overrides.is_empty());
    }

    #[test]
    fn length_limit_cli_override_replaces_profile_default_but_preserves_overrides() {
        use std::path::Path;
        let profile = profile_with_tool_config(&json!({
            "max_code_lines": 500,
            "length_overrides": [
                { "pattern": "tests/**", "limit": 1500 },
            ],
        }));
        let limit = length_limit_from_profile(&profile, Some(800)).unwrap();
        assert_eq!(limit.default, Some(800));
        assert_eq!(limit.overrides.len(), 1);
        // Override still wins for matching paths.
        assert_eq!(limit.limit_for(Path::new("tests/foo.rs")), Some(1500));
        // CLI default applies to non-matching paths.
        assert_eq!(limit.limit_for(Path::new("src/lib.rs")), Some(800));
    }

    #[test]
    fn iteration_monitor_hedging_patterns_default_empty() {
        let profile = profile_with_iteration_monitor(json!({
            "context_window_tokens": 100u64,
            "warn_threshold_pct": 0.5,
            "handoff_threshold_pct": 0.9,
            "handoff_guidance": "",
            "failure_repeat_window": 0,
        }));
        let cfg = iteration_monitor_from_profile(&profile).unwrap().unwrap();
        assert!(cfg.hedging_patterns.is_empty());
    }

    #[test]
    fn slash_state_builder_snapshots_tools_and_registers_all_builtins() {
        use crate::cli::Cli;
        use crate::commands::slash::CLI_BUILTIN_NAMES;
        use crate::runtime::RuntimeInputs;
        use crate::runtime::build_runtime;
        use clap::Parser;
        let cli = Cli::try_parse_from(["norn"]).unwrap();
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        let store = Arc::new(EventStore::new());
        let (state, registry) =
            build_slash_state_from_bundle(&cli, &bundle, Arc::clone(&store), None);
        assert_eq!(state.model_snapshot(), bundle.model);
        for name in CLI_BUILTIN_NAMES {
            assert!(registry.get(name).is_some(), "missing /{name}");
        }
        assert!(Arc::ptr_eq(&store, &state.current_store()));
    }

    #[test]
    fn slash_state_builder_carries_variable_pairs() {
        use crate::cli::Cli;
        use crate::runtime::RuntimeInputs;
        use crate::runtime::build_runtime;
        use clap::Parser;
        let cli = Cli::try_parse_from(["norn", "--variables", "project=yggdrasil"]).unwrap();
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        let store = Arc::new(EventStore::new());
        let (state, _registry) = build_slash_state_from_bundle(&cli, &bundle, store, None);
        assert_eq!(
            state.variable_pairs,
            vec![("project".to_owned(), "yggdrasil".to_owned())],
        );
    }

    #[test]
    fn slash_state_builder_parses_inline_output_schema() {
        use crate::cli::Cli;
        use crate::runtime::RuntimeInputs;
        use crate::runtime::build_runtime;
        use clap::Parser;
        let cli = Cli::try_parse_from(["norn", "-s", r#"{"type":"object"}"#]).unwrap();
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        let store = Arc::new(EventStore::new());
        let (state, _registry) = build_slash_state_from_bundle(&cli, &bundle, store, None);
        assert_eq!(
            state.output_schema_snapshot(),
            Some(serde_json::json!({"type": "object"})),
        );
    }

    #[test]
    fn slash_state_builder_threads_session_id() {
        use crate::cli::Cli;
        use crate::runtime::RuntimeInputs;
        use crate::runtime::build_runtime;
        use clap::Parser;
        let cli = Cli::try_parse_from(["norn"]).unwrap();
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        let store = Arc::new(EventStore::new());
        let (state, _registry) =
            build_slash_state_from_bundle(&cli, &bundle, store, Some("abc-123".to_owned()));
        assert_eq!(state.session_id.as_deref(), Some("abc-123"));
    }
}
