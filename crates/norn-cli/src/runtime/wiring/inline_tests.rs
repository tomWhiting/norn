#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Arc;

    use norn::agent::registry::AgentRegistry;
    use norn::r#loop::config::ToolExecutor;
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

        install_agent_tool_infra(
            &registry,
            provider,
            Arc::new(EventStore::new()),
            agent_id,
            Arc::clone(&tool_registry),
            AgentRegistry::shared(),
        );

        let shared = registry
            .shared_context()
            .expect("registry exposes a shared context");
        let infra = shared
            .get_extension::<AgentToolInfra>()
            .expect("AgentToolInfra installed and reachable via get_extension");
        assert_eq!(infra.agent_id, agent_id);
        assert!(infra.parent_id.is_none());
        assert!(
            infra.tool_registry.is_some(),
            "tool_registry must be wired so spawned sub-agents can dispatch tools",
        );
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
