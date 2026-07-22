use super::*;

/// NO ASSUMED DEFAULTS: with neither a profile model nor an explicit
/// `.model(..)`, the build must fail with a typed error that tells the
/// embedder exactly what to set — never fall back to a hardcoded model.
#[test]
fn build_without_profile_or_model_is_a_typed_error() {
    let result = AgentBuilder::new(provider_with(vec![]))
        .working_dir(std::env::temp_dir())
        .build();
    match result {
        Err(NornError::Config(ConfigError::InvalidConfig { reason })) => {
            assert!(reason.contains("no model resolved"), "{reason}");
            assert!(reason.contains(".model("), "{reason}");
            assert!(reason.contains(".profile"), "{reason}");
        }
        Err(other) => panic!("expected a typed config error, got: {other}"),
        Ok(_) => panic!("a build with no model must fail, not assume one"),
    }
}

/// ITEM C: the output schema lives on the agent-loop config, so it
/// round-trips through serde with the rest of the config — the
/// serialized form embedders carry across activity boundaries.
#[test]
fn output_schema_round_trips_through_serialized_loop_config() {
    let schema = serde_json::json!({"type": "object", "required": ["verdict"]});
    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .output_schema(schema.clone())
        .build()
        .expect("build succeeds");
    assert_eq!(
        agent.loop_config().output_schema.as_ref(),
        Some(&schema),
        "the effective config is introspectable through the public accessor",
    );

    let json = serde_json::to_string(agent.loop_config()).expect("config serializes");
    let back: AgentLoopConfig = serde_json::from_str(&json).expect("config deserializes");
    assert_eq!(back.output_schema.as_ref(), Some(&schema));
    assert_eq!(back.schema_tool_name, agent.config.schema_tool_name);

    // Partial JSON deserializes with defaults — the activity-input shape.
    let partial: AgentLoopConfig = serde_json::from_str(r#"{"output_schema": {"type": "object"}}"#)
        .expect("partial config deserializes");
    assert_eq!(
        partial.output_schema,
        Some(serde_json::json!({"type": "object"}))
    );
    assert_eq!(
        partial.schema_attempt_budget,
        AgentLoopConfig::default().schema_attempt_budget
    );
}

/// A runtime-base config merges with the explicit schema: the schema
/// is part of the effective config, exactly like every other field.
#[test]
fn output_schema_survives_runtime_base_merge() {
    let temp = tempfile::tempdir().expect("tempdir");
    let schema = serde_json::json!({"type": "string"});
    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(temp.path())
        .load_runtime_base()
        .output_schema(schema.clone())
        .build()
        .expect("build succeeds");
    assert_eq!(agent.config.output_schema.as_ref(), Some(&schema));
    assert_eq!(agent.info().output_schema.as_ref(), Some(&schema));
    assert!(
        agent
            .loop_context
            .base_system_instruction()
            .contains("structured"),
        "schema mode must reach the system prompt through the effective config",
    );
}

#[test]
fn context_window_limit_setter_survives_runtime_base_merge() {
    // C8: the granular setter overrides only this field, overriding the
    // settings-derived runtime base per the explicit-config-wins rule.
    let temp = tempfile::tempdir().expect("tempdir");
    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(temp.path())
        .load_runtime_base()
        .context_window_limit(4_242)
        .build()
        .expect("build succeeds");
    assert_eq!(agent.config.context_window_limit, Some(4_242));
}

#[test]
fn context_window_limit_setter_beats_auto_compaction_catalog_fill() {
    // C8 interaction: the setter's window survives auto-compaction arming,
    // which fills the catalog window only when the merged value is still
    // `None`. A catalogued model would otherwise have its window filled
    // from the catalog during build(); the explicit setter value wins.
    let temp = tempfile::tempdir().expect("tempdir");
    let model = crate::model_catalog::default_selection().model;
    let agent = AgentBuilder::new(provider_with(vec![]))
        .model(model)
        .working_dir(temp.path())
        .load_runtime_base()
        .context_window_limit(4_242)
        .build()
        .expect("build succeeds");
    assert_eq!(
        agent.config.context_window_limit,
        Some(4_242),
        "the setter value must survive catalog fill during arming",
    );
}
