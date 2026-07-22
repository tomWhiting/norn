use super::*;

/// Track B finding 3 regression: the compaction guidance in the system
/// prompt must consult the *effective* agent config (runtime base merged
/// with explicit builder overrides) — not one field from each source.
/// Here both compaction fields arrive via the explicit builder config;
/// the guidance must be present even when the runtime base sets neither.
#[test]
fn auto_compact_guidance_follows_effective_config() {
    let temp = tempfile::tempdir().expect("tempdir");
    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(temp.path())
        .load_runtime_base()
        .agent_config(AgentLoopConfig {
            context_window_limit: Some(200_000),
            auto_compact_reserve_tokens: Some(30_000),
            ..AgentLoopConfig::default()
        })
        .build()
        .expect("build succeeds");

    assert_eq!(agent.config.context_window_limit, Some(200_000));
    assert_eq!(agent.config.auto_compact_reserve_tokens, Some(30_000));
    assert!(
        agent
            .loop_context
            .base_system_instruction()
            .contains("automatically summarised or cleared"),
        "compaction guidance must be in the system prompt when the \
         effective config enables auto-compaction",
    );
}

/// Companion to the finding 3 regression: with compaction disabled
/// (reserve off), the guidance must stay out of the system prompt.
/// A windowless build is no longer the way to express "no compaction"
/// — that now hard-errors (2026-07-05 incident guard) — so the honest
/// no-compaction state is an armed window with the reserve disabled.
#[test]
fn no_auto_compact_guidance_without_compaction_config() {
    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .working_dir(std::env::temp_dir())
        .agent_config(AgentLoopConfig {
            context_window_limit: Some(TEST_CONTEXT_WINDOW),
            auto_compact_reserve_tokens: None,
            ..AgentLoopConfig::default()
        })
        .build()
        .expect("build succeeds");
    assert!(
        !agent
            .loop_context
            .base_system_instruction()
            .contains("automatically summarised or cleared"),
        "no compaction config means no compaction guidance",
    );
}

/// Finding 5 regression: a reserve at or above the window makes the
/// runtime trigger disable itself (every step would fire), so the build
/// must not emit compaction guidance the loop will never honour. Both
/// values are known at build time, so `has_auto_compact` is forced false
/// even though both fields are `Some`.
#[test]
fn reserve_at_or_above_window_drops_auto_compact_guidance() {
    for (window, reserve) in [(50_000_u64, 50_000_u64), (50_000, 60_000)] {
        let temp = tempfile::tempdir().expect("tempdir");
        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .working_dir(temp.path())
            .agent_config(AgentLoopConfig {
                context_window_limit: Some(window),
                auto_compact_reserve_tokens: Some(reserve),
                ..AgentLoopConfig::default()
            })
            .build()
            .expect("build succeeds");

        // The config values themselves are preserved verbatim — the build
        // only suppresses the *prompt guidance*, matching the runtime
        // trigger which reads these same values and disables.
        assert_eq!(agent.config.context_window_limit, Some(window));
        assert_eq!(agent.config.auto_compact_reserve_tokens, Some(reserve));
        assert!(
            !agent
                .loop_context
                .base_system_instruction()
                .contains("automatically summarised or cleared"),
            "reserve {reserve} >= window {window} must drop the compaction guidance \
             (the runtime trigger disables in this shape)",
        );
    }
}

/// Reserve-armed default: a catalogued model with no explicit window or
/// reserve must default its context window from the model catalog and
/// keep the reserve knob armed, so auto-compaction is on by default.
#[test]
fn catalog_arms_window_and_reserve_by_default() {
    let temp = tempfile::tempdir().expect("tempdir");
    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("gpt-5.5")
        .working_dir(temp.path())
        .build()
        .expect("build succeeds");

    let catalog_window = crate::model_catalog::smallest_context_window_for_model("gpt-5.5");
    assert!(catalog_window.is_some(), "gpt-5.5 must be in the catalog");
    assert_eq!(
        agent.config.context_window_limit, catalog_window,
        "an unset window must default to the model catalog value",
    );
    assert_eq!(
        agent.config.auto_compact_reserve_tokens,
        Some(30_000),
        "the reserve knob is armed by default",
    );
    assert!(
        agent
            .loop_context
            .base_system_instruction()
            .contains("automatically summarised or cleared"),
        "a catalog-armed window plus the default reserve enables \
         auto-compaction, so the guidance must be present",
    );
}

/// An explicitly configured window — even for a catalogued model — must
/// win over the catalog fill-in (the catalog only fills an unset value).
#[test]
fn explicit_window_beats_catalog() {
    let temp = tempfile::tempdir().expect("tempdir");
    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("gpt-5.5")
        .working_dir(temp.path())
        .agent_config(AgentLoopConfig {
            context_window_limit: Some(50_000),
            ..AgentLoopConfig::default()
        })
        .build()
        .expect("build succeeds");

    assert_eq!(
        agent.config.context_window_limit,
        Some(50_000),
        "an explicit window must not be overwritten by the catalog",
    );
}

/// 2026-07-05 incident guard (owner-ruled): a model absent from the
/// catalog with no explicit window is rejected at build — running with
/// the protections silently disabled is the ruled-against state, and
/// an unknown model "probably means the wrong model code".
#[test]
fn unknown_model_without_window_is_rejected_at_build() {
    let temp = tempfile::tempdir().expect("tempdir");
    let result = AgentBuilder::new(provider_with(vec![]))
        .model("not-in-catalog")
        .working_dir(temp.path())
        .build();
    let Err(err) = result else {
        panic!("uncatalogued model with no window must be rejected");
    };

    let reason = err.to_string();
    assert!(
        reason.contains("not-in-catalog"),
        "names the model: {reason}"
    );
    assert!(
        reason.contains("typo"),
        "leads with the typo hypothesis: {reason}"
    );

    // The explicit-window escape hatch assembles fine.
    let temp = tempfile::tempdir().expect("tempdir");
    AgentBuilder::new(provider_with(vec![]))
        .model("not-in-catalog")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(temp.path())
        .build()
        .expect("explicit window arms an uncatalogued model");
}

/// The 2026-07-05 incident shape through the real assembly funnel: a
/// catalogued 128k model with an explicit 272k window must fail
/// `build()` itself — this pins the guard staying wired into build,
/// not just its unit logic in `arming`.
#[test]
fn oversized_explicit_window_is_rejected_at_build() {
    let temp = tempfile::tempdir().expect("tempdir");
    let result = AgentBuilder::new(provider_with(vec![]))
        .model("gpt-5.3-codex-spark")
        .context_window_limit(272_000)
        .working_dir(temp.path())
        .build();
    let Err(err) = result else {
        panic!("explicit window above the catalog maximum must be rejected");
    };

    let reason = err.to_string();
    assert!(
        reason.contains("gpt-5.3-codex-spark"),
        "names the model: {reason}"
    );
    assert!(
        reason.contains("272000"),
        "names the configured value: {reason}"
    );
    assert!(
        reason.contains("128000"),
        "names the catalog maximum: {reason}"
    );
}

/// The catalog fill-in resolves each agent's own model — a sub-agent
/// built with a different catalogued model gets that model's window, not
/// a shared or parent value.
#[test]
fn catalog_window_is_resolved_per_model() {
    let temp = tempfile::tempdir().expect("tempdir");
    let big = AgentBuilder::new(provider_with(vec![]))
        .model("gpt-5.5")
        .working_dir(temp.path())
        .build()
        .expect("build succeeds");
    let small = AgentBuilder::new(provider_with(vec![]))
        .model("gpt-5.3-codex-spark")
        .working_dir(temp.path())
        .build()
        .expect("build succeeds");

    assert_eq!(
        big.config.context_window_limit,
        crate::model_catalog::smallest_context_window_for_model("gpt-5.5"),
    );
    assert_eq!(
        small.config.context_window_limit,
        crate::model_catalog::smallest_context_window_for_model("gpt-5.3-codex-spark"),
    );
    assert_ne!(
        big.config.context_window_limit, small.config.context_window_limit,
        "each agent resolves its own model's catalog window",
    );
}

/// Track B finding 2 regression: with the runtime base loaded, the
/// merged `settings.permissions` must compile into a
/// [`crate::config::PermissionPolicy`] published on the registry's
/// shared tool context — the embedded path previously installed
/// nothing, so settings-declared deny rules were never enforced.
#[test]
fn runtime_base_installs_permission_policy_on_tool_context() {
    use crate::config::{PermissionDecision, PermissionPolicy};

    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(temp.path().join(".norn")).expect("mkdir .norn");
    std::fs::write(
        temp.path().join(".norn").join("settings.json"),
        r#"{"permissions": {"deny": ["bash"]}}"#,
    )
    .expect("write settings");

    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(temp.path())
        .load_runtime_base()
        .build()
        .expect("build succeeds");

    let ctx = agent
        .registry
        .shared_context()
        .expect("registry exposes its shared tool context");
    let policy = ctx
        .get_extension::<PermissionPolicy>()
        .expect("settings.permissions must be installed on the embedded path");
    assert!(
        matches!(
            policy.evaluate("bash", &serde_json::json!({"command": "ls"})),
            PermissionDecision::Deny { .. }
        ),
        "the settings-declared deny rule must be active",
    );
}

/// Track B finding 2, end to end: a deny rule in the project settings
/// blocks the tool through a real embedded dispatch — the loop's gating
/// phase refuses the call and records the block as the tool result.
#[tokio::test]
async fn settings_deny_rule_blocks_tool_through_embedded_dispatch() {
    use crate::provider::request::ToolCallKind;
    use crate::session::events::SessionEvent;

    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(temp.path().join(".norn")).expect("mkdir .norn");
    std::fs::write(
        temp.path().join(".norn").join("settings.json"),
        r#"{"permissions": {"deny": ["bash"]}}"#,
    )
    .expect("write settings");

    let provider = provider_with(vec![
        vec![
            ProviderEvent::ToolCallComplete {
                call_id: "call-denied".to_owned(),
                name: "bash".to_owned(),
                arguments: r#"{"command": "echo hi"}"#.to_owned(),
                kind: ToolCallKind::Function,
            },
            ProviderEvent::Done {
                stop_reason: StopReason::ToolUse,
                usage: Usage::default(),
                response_id: None,
            },
        ],
        text_completion("acknowledged")
            .pop()
            .expect("one scripted turn"),
    ]);
    let output = AgentBuilder::new(provider)
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(temp.path())
        .load_runtime_base()
        .run("run a command")
        .await
        .expect("run completes");

    let store = output
        .into_output()
        .event_store
        .expect("event store returned");
    let blocked = store.events().iter().any(|event| {
        matches!(
            event,
            SessionEvent::ToolResult { tool_name, output, .. }
                if tool_name == "bash"
                    // Permission denials persist as the typed
                    // `permission_denied` payload, not a collapsed
                    // string.
                    && output["error"]["kind"] == "permission_denied"
                    && output["error"]["message"]
                        .as_str()
                        .is_some_and(|m| m.contains("blocked by permissions"))
        )
    });
    assert!(
        blocked,
        "the bash call must be refused by the settings deny rule through \
         real dispatch; events: {:?}",
        store.events(),
    );
}

/// Fix 7 regression: resuming a session rebuilds the action log (and its
/// mutation ledger) from the persisted events, restoring the
/// session-lifetime queryability contract.
#[test]
fn resumed_session_rebuilds_action_log() {
    use crate::session::events::{EventBase, EventUsage, SessionEvent, ToolCallEvent};

    let store = EventStore::new();
    store
        .append(SessionEvent::AssistantMessage {
            response_items: Vec::new(),
            base: EventBase::new(None),
            content: String::new(),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: vec![ToolCallEvent {
                call_id: "tc-resume".to_owned(),
                name: "read".to_owned(),
                arguments: serde_json::json!({
                    "path": "src/lib.rs",
                    "tool_use_description": "inspect entry point",
                }),
                kind: crate::provider::request::ToolCallKind::Function,
                caller: crate::provider::request::ToolCallCaller::Absent,
            }],
            usage: EventUsage::default(),
            stop_reason: "tool_use".to_owned(),
            response_id: None,
        })
        .expect("append assistant message");
    store
        .append(SessionEvent::ToolResult {
            base: EventBase::new(None),
            tool_call_id: "tc-resume".to_owned(),
            tool_name: "read".to_owned(),
            output: serde_json::json!({"lines": 12}),
            spool_ref: None,
            duration_ms: 4,
        })
        .expect("append tool result");

    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .session(Arc::new(store))
        .build()
        .expect("build succeeds");

    let log = agent
        .loop_context
        .action_log
        .as_ref()
        .expect("action log installed");
    let entry = log
        .entry("tc-resume")
        .expect("resumed tool call must be queryable again");
    assert_eq!(entry.tool_use_description, "inspect entry point");
    assert!(matches!(
        entry.outcome,
        crate::session::action_log::Outcome::Success
    ));
}
