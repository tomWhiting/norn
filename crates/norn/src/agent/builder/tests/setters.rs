use super::*;

/// `.event_schemas(..)` installs the set on the built loop context.
#[test]
fn event_schemas_setter_installs_on_loop_context() {
    use crate::agent_loop::event_schemas::{EventSchemaSet, EventType};

    let mut schemas = EventSchemaSet::new();
    schemas.set(EventType::Text, serde_json::json!({"type": "string"}));
    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .event_schemas(schemas)
        .build()
        .expect("build succeeds");
    let installed = agent
        .loop_context
        .event_schemas
        .as_ref()
        .expect("event schemas installed on the loop context");
    assert_eq!(
        installed.get(EventType::Text),
        Some(&serde_json::json!({"type": "string"})),
    );
}

/// `.variables(store)` overrides the minted store, and with no
/// `open_session` the supplied store's id becomes the resolved session
/// id so `{{session_id}}` and the environment agree.
#[test]
fn variables_setter_overrides_store_and_pins_session_id() {
    use crate::integration::variables::VariableStore;

    let store = Arc::new(VariableStore::with_builtins().with_session_id("custom-session"));
    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .variables(Arc::clone(&store))
        .build()
        .expect("build succeeds");
    let installed = agent
        .loop_context
        .variables
        .as_ref()
        .expect("variable store installed on the loop context");
    assert!(
        Arc::ptr_eq(installed, &store),
        "the supplied store overrides the minted one",
    );
    assert_eq!(
        agent.info().session_id,
        "custom-session",
        "the supplied store's id becomes the resolved session id",
    );
}

/// A supplied variable store whose id contradicts an `open_session`
/// persisted id fails the build rather than silently diverging.
#[test]
fn variables_setter_conflicting_session_id_fails_build() {
    use crate::integration::variables::VariableStore;

    let temp = tempfile::tempdir().expect("tempdir");
    let manager = SessionManager::new(temp.path());
    let store = Arc::new(VariableStore::with_builtins().with_session_id("not-the-session"));
    let reason = invalid_config_reason(
        AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .working_dir(temp.path())
            .open_session(
                &manager,
                SessionSpec::Create { name: None },
                DurabilityPolicy::Flush,
            )
            .variables(store)
            .build(),
    );
    assert!(
        reason.contains("disagrees with the resolved session id"),
        "{reason}"
    );
}

/// `.disallowed_tools(..)` gates the registry with deny-wins semantics.
#[test]
fn disallowed_tools_setter_denies_named_tools() {
    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .allowed_tools(&["read", "bash"])
        .disallowed_tools(&["bash"])
        .build()
        .expect("build succeeds");
    assert!(
        agent.registry.get("bash").is_none(),
        "deny wins even when the allow-list names the tool",
    );
    assert!(agent.registry.get("read").is_some());
}

/// `.terminal_reclamation(false)` suppresses the reclamation marker the
/// coordination runtime installs by default.
#[test]
fn terminal_reclamation_setter_gates_reclaim_marker() {
    use crate::tools::agent::ReclaimOnResultDelivery;

    let default_agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .agent_registry(AgentRegistry::shared())
        .child_policy(test_child_policy())
        .child_result_capacity(16)
        .build()
        .expect("build succeeds");
    assert!(
        default_agent
            .registry
            .shared_context()
            .expect("shared tool context")
            .get_extension::<ReclaimOnResultDelivery>()
            .is_some(),
        "terminal reclamation is installed by default (headless)",
    );

    let tui_agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .agent_registry(AgentRegistry::shared())
        .child_policy(test_child_policy())
        .child_result_capacity(16)
        .terminal_reclamation(false)
        .build()
        .expect("build succeeds");
    assert!(
        tui_agent
            .registry
            .shared_context()
            .expect("shared tool context")
            .get_extension::<ReclaimOnResultDelivery>()
            .is_none(),
        "terminal_reclamation(false) suppresses the marker for status-panel drivers",
    );
}

/// `.register_root(..)` reserves the registry entry and adopts the
/// reserved id as the agent's id.
#[test]
fn register_root_setter_reserves_and_adopts_id() {
    let registry = AgentRegistry::shared();
    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .agent_registry(Arc::clone(&registry))
        .child_policy(test_child_policy())
        .child_result_capacity(16)
        .register_root("/root".to_string(), "lead".to_string())
        .build()
        .expect("build succeeds");
    let entry = registry
        .read()
        .get_by_path("/root")
        .expect("root entry reserved and confirmed");
    assert_eq!(
        entry.id,
        agent.agent_id(),
        "the agent adopts the reserved root id",
    );
}

/// `.register_root(..)` without `.agent_registry(..)` fails the build —
/// the entry would be silently unregistered.
#[test]
fn register_root_without_registry_fails_build() {
    let reason = invalid_config_reason(
        AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .register_root("/root".to_string(), "lead".to_string())
            .build(),
    );
    assert!(
        reason.contains("register_root is set but agent coordination is not wired"),
        "{reason}"
    );
}
