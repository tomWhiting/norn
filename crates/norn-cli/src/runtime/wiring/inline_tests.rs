#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Arc;

    use norn::agent::{AgentBuilder, AgentParts};
    use norn::profile::Profile;
    use norn::provider::mock::MockProvider;
    use norn::provider::traits::Provider;
    use norn::session::store::EventStore;
    use serde_json::json;

    use crate::cli::BuildError;
    use crate::runtime::wiring::{
        DEFAULT_DELEGATION_DEPTH, SlashStateInputs, build_slash_state_from_bundle,
        cli_coordination_envelope, length_limit_from_profile,
    };

    /// DECISIONS §0.6(d): the CLI's coordination envelope seeds the root's
    /// own delegation depth from the resolved value. The default is `2`
    /// (a child may spawn one level of its own; grandchildren are leaves),
    /// and an explicit `1` restores leaf children.
    #[test]
    fn coordination_envelope_seeds_root_delegation_depth() {
        assert_eq!(DEFAULT_DELEGATION_DEPTH, 2, "owner-ruled default is 2");

        let default_env = cli_coordination_envelope(DEFAULT_DELEGATION_DEPTH);
        assert_eq!(default_env.child_policy.delegation.remaining_depth, 2);
        // The inherit-with-decrement invariant is untouched: a root at
        // depth 2 grants a child depth 1 (can still spawn) and a grandchild
        // depth 0 (a leaf that cannot).
        let child = default_env
            .child_policy
            .grant_for_child(None)
            .expect("root grants a child");
        assert_eq!(
            child.delegation.remaining_depth, 1,
            "child may spawn one level"
        );
        let grandchild = child
            .grant_for_child(None)
            .expect("child grants a grandchild");
        assert_eq!(
            grandchild.delegation.remaining_depth, 0,
            "grandchild is a leaf"
        );
        assert!(
            grandchild.grant_for_child(None).is_err(),
            "a leaf grandchild cannot spawn",
        );

        // Explicit depth 1 restores leaf children (the pre-ruling shape).
        let leaf_env = cli_coordination_envelope(1);
        assert_eq!(leaf_env.child_policy.delegation.remaining_depth, 1);
        let leaf_child = leaf_env
            .child_policy
            .grant_for_child(None)
            .expect("root grants a child");
        assert_eq!(leaf_child.delegation.remaining_depth, 0, "child is a leaf");
        // The concurrency cap is unchanged by the depth knob.
        assert_eq!(leaf_env.child_policy.delegation.max_concurrent_children, 32);
    }

    /// Assemble a headless agent through the library builder and hand back
    /// its parts, so the slash-state tests read the same gated registry,
    /// model, and resolved service tier / reasoning effort the print
    /// orchestrator's `AgentParts` carry.
    fn built_parts() -> AgentParts {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        AgentBuilder::new(provider)
            .model("gpt-x")
            .working_dir(std::env::temp_dir())
            .load_runtime_base()
            .build()
            .expect("build succeeds")
            .into_parts()
    }

    /// The slash-state inputs read off assembled `AgentParts` — the
    /// decoupled surface the unified assembly path feeds.
    fn slash_inputs(parts: &AgentParts) -> SlashStateInputs<'_> {
        SlashStateInputs {
            registry: &parts.registry,
            model: &parts.model,
            service_tier: parts.loop_context.service_tier,
            reasoning_effort: parts.loop_context.reasoning_effort,
        }
    }

    fn empty_profile() -> Profile {
        Profile::default()
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
    fn slash_state_builder_snapshots_tools_and_registers_all_builtins() {
        use clap::Parser;

        use crate::cli::Cli;
        use crate::commands::slash::cli_builtin_names;
        let cli = Cli::try_parse_from(["norn"]).unwrap();
        let parts = built_parts();
        let store = Arc::new(EventStore::new());
        let (state, registry) =
            build_slash_state_from_bundle(&cli, slash_inputs(&parts), Arc::clone(&store), None);
        assert_eq!(state.model_snapshot(), parts.model);
        for name in cli_builtin_names() {
            assert!(registry.get(name).is_some(), "missing /{name}");
        }
        assert!(Arc::ptr_eq(&store, &state.current_store()));
    }

    #[test]
    fn slash_state_builder_carries_variable_pairs() {
        use clap::Parser;

        use crate::cli::Cli;
        let cli = Cli::try_parse_from(["norn", "--variables", "project=yggdrasil"]).unwrap();
        let parts = built_parts();
        let store = Arc::new(EventStore::new());
        let (state, _registry) =
            build_slash_state_from_bundle(&cli, slash_inputs(&parts), store, None);
        assert_eq!(
            state.variable_pairs,
            vec![("project".to_owned(), "yggdrasil".to_owned())],
        );
    }

    #[test]
    fn slash_state_builder_parses_inline_output_schema() {
        use clap::Parser;

        use crate::cli::Cli;
        let cli = Cli::try_parse_from(["norn", "-s", r#"{"type":"object"}"#]).unwrap();
        let parts = built_parts();
        let store = Arc::new(EventStore::new());
        let (state, _registry) =
            build_slash_state_from_bundle(&cli, slash_inputs(&parts), store, None);
        assert_eq!(
            state.output_schema_snapshot(),
            Some(serde_json::json!({"type": "object"})),
        );
    }

    /// F4 regression: the unmatched-tool-flag check reports only names
    /// that match no real tool — a partial typo on `--allowed-tools`
    /// (`read,serch` silently narrowing to `read`) and a bogus
    /// `--disallowed-tools` name — while never mis-flagging a correctly
    /// denied tool (`--disallowed-tools bash`), since deny hides a tool
    /// from dispatch without unregistering it.
    #[test]
    fn unmatched_tool_flag_names_reports_typos_not_denied_tools() {
        use norn::tool::registry::ToolRegistry;
        use norn::tools::registry_builder::register_standard_tools;

        use crate::config::AppliedOverrides;
        use crate::runtime::wiring::unmatched_tool_flag_names;

        let mut registry = ToolRegistry::new();
        register_standard_tools(&mut registry, None);
        // `bash` is physically registered, then gated out of `names()`.
        registry.set_disallowed(vec!["bash".to_owned()]);

        let applied = AppliedOverrides {
            allowed_tools: vec!["read".to_owned(), "serch".to_owned()],
            disallowed_tools: vec!["bash".to_owned(), "nope".to_owned()],
        };
        let unmatched = unmatched_tool_flag_names(&registry, &applied);
        assert!(
            unmatched.contains(&("--allowed-tools", "serch")),
            "the allowed-tools typo must warn: {unmatched:?}",
        );
        assert!(
            unmatched.contains(&("--disallowed-tools", "nope")),
            "the bogus disallowed name must warn: {unmatched:?}",
        );
        assert!(
            !unmatched.iter().any(|(_, name)| *name == "read"),
            "a real allowed tool must not warn: {unmatched:?}",
        );
        assert!(
            !unmatched.iter().any(|(_, name)| *name == "bash"),
            "a correctly denied tool must not warn: {unmatched:?}",
        );
    }

    #[test]
    fn slash_state_builder_threads_session_id() {
        use clap::Parser;

        use crate::cli::Cli;
        let cli = Cli::try_parse_from(["norn"]).unwrap();
        let parts = built_parts();
        let store = Arc::new(EventStore::new());
        let (state, _registry) = build_slash_state_from_bundle(
            &cli,
            slash_inputs(&parts),
            store,
            Some("abc-123".to_owned()),
        );
        assert_eq!(state.current_session_id().as_deref(), Some("abc-123"));
    }
}
