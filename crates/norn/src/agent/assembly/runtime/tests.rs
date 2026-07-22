use std::error::Error;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use uuid::Uuid;

use super::super::paths::profile_prompt_source;
use super::super::tooling::install_agent_infra;
use super::config::merge_agent_config;
use super::*;
use crate::agent::child_policy::CoordinationEnvelope;
use crate::agent::registry::AgentRegistry;
use crate::r#loop::config::{AgentLoopConfig, ConversationStateMode};
use crate::r#loop::linger::LingerPolicy;
use crate::profile::loader::ProfileOrigin;
use crate::provider::mock::MockProvider;
use crate::provider::traits::Provider;
use crate::session::action_log::ActionLog;
use crate::session::action_log_tree::ActionLogTree;
use crate::session::store::EventStore;
use crate::system_prompt::PromptSource;
use crate::tool::context::ToolContext;
use crate::tool::registry::ToolRegistry;

#[test]
fn profile_origin_derives_prompt_source_without_an_independent_role() {
    assert_eq!(profile_prompt_source(None), PromptSource::OperatorProfile);
    assert_eq!(
        profile_prompt_source(Some(ProfileOrigin::User)),
        PromptSource::OperatorProfile
    );
    assert_eq!(
        profile_prompt_source(Some(ProfileOrigin::WorkingDirectory)),
        PromptSource::WorkspaceProfile
    );
}

/// An explicit non-`Option` value equal to the library default must still win
/// over a settings-derived base when its presence flag is set.
#[test]
fn explicit_non_option_default_value_wins_when_present() {
    let base = AgentLoopConfig {
        schema_attempt_budget: 5,
        auto_compact_keep_recent_turns: 20,
        auto_compact_reserve_tokens: Some(90_000),
        ..AgentLoopConfig::default()
    };
    let explicit = AgentLoopConfig {
        schema_attempt_budget: AgentLoopConfig::default().schema_attempt_budget,
        auto_compact_keep_recent_turns: AgentLoopConfig::default().auto_compact_keep_recent_turns,
        auto_compact_reserve_tokens: AgentLoopConfig::default().auto_compact_reserve_tokens,
        ..AgentLoopConfig::default()
    };
    let merged = merge_agent_config(base, explicit, AgentConfigPresence::all());
    assert_eq!(
        merged.schema_attempt_budget, 3,
        "explicit schema_attempt_budget=3 must win over base=5",
    );
    assert_eq!(
        merged.auto_compact_keep_recent_turns, 10,
        "explicit keep_recent_turns=10 must win over base=20",
    );
    assert_eq!(
        merged.auto_compact_reserve_tokens,
        Some(30_000),
        "explicit reserve=Some(30_000) must win over base=Some(90_000)",
    );
}

#[test]
fn unset_non_option_field_defers_to_base() {
    let base = AgentLoopConfig {
        schema_attempt_budget: 5,
        ..AgentLoopConfig::default()
    };
    let explicit = AgentLoopConfig::default();
    let merged = merge_agent_config(base, explicit, AgentConfigPresence::default());
    assert_eq!(
        merged.schema_attempt_budget, 5,
        "no presence flag means the base value stands",
    );
}

/// A fully explicit config overlays the base in its entirety, including
/// fields with meaningful defaults and optional late additions.
#[test]
fn fully_explicit_config_overlays_every_field() {
    let base = AgentLoopConfig::default();
    let explicit = AgentLoopConfig {
        schema_attempt_budget: 9,
        max_iterations: Some(42),
        step_timeout: Some(Duration::from_secs(99)),
        context_window_limit: Some(123_456),
        auto_compact_reserve_tokens: Some(45_000),
        auto_compact_keep_recent_turns: 33,
        schema_tool_name: "custom_output".to_owned(),
        cache_key: Some("ck".to_owned()),
        conversation_state: ConversationStateMode::ManualReplay,
        server_compaction_threshold_tokens: Some(7_000),
        output_schema: Some(serde_json::json!({"type": "object"})),
        prompt_command_timeout: Some(Duration::from_secs(12)),
        linger: Some(LingerPolicy {
            deadline: Duration::from_secs(3),
        }),
    };
    let merged = merge_agent_config(base, explicit.clone(), AgentConfigPresence::all());
    assert_eq!(merged.schema_attempt_budget, explicit.schema_attempt_budget);
    assert_eq!(merged.max_iterations, explicit.max_iterations);
    assert_eq!(merged.step_timeout, explicit.step_timeout);
    assert_eq!(merged.context_window_limit, explicit.context_window_limit);
    assert_eq!(
        merged.auto_compact_reserve_tokens,
        explicit.auto_compact_reserve_tokens
    );
    assert_eq!(
        merged.auto_compact_keep_recent_turns,
        explicit.auto_compact_keep_recent_turns
    );
    assert_eq!(merged.schema_tool_name, explicit.schema_tool_name);
    assert_eq!(merged.cache_key, explicit.cache_key);
    assert_eq!(merged.conversation_state, explicit.conversation_state);
    assert_eq!(
        merged.server_compaction_threshold_tokens,
        explicit.server_compaction_threshold_tokens
    );
    assert_eq!(merged.output_schema, explicit.output_schema);
    assert_eq!(
        merged.prompt_command_timeout, explicit.prompt_command_timeout,
        "prompt_command_timeout must overlay onto the base",
    );
    assert_eq!(
        merged.linger.is_some(),
        explicit.linger.is_some(),
        "linger must overlay onto the base",
    );
}

#[test]
fn install_agent_infra_publishes_action_log_tree_with_root_log() -> Result<(), Box<dyn Error>> {
    let tool_registry = Arc::new(ToolRegistry::new());
    let ctx = ToolContext::empty();
    let action_log = Arc::new(ActionLog::new(Arc::new(EventStore::new())));
    ctx.insert_extension(Arc::clone(&action_log));

    let agent_id = Uuid::new_v4();
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
    let envelope = CoordinationEnvelope {
        child_policy: crate::agent::child_policy::ChildPolicy {
            messaging: crate::agent::child_policy::MessagingScope::SiblingsAndParent,
            delegation: crate::agent::child_policy::DelegationBudget {
                remaining_depth: 1,
                max_concurrent_children: 32,
            },
            inbound_capacity: 32,
            loop_config: None,
        },
        child_result_capacity: 256,
    };
    let root_cancel = tokio_util::sync::CancellationToken::new();
    let _child_rx = install_agent_infra(
        &tool_registry,
        &ctx,
        AgentInfraParts {
            registry: AgentRegistry::shared(),
            provider,
            event_store: Arc::new(EventStore::new()),
            session: Arc::new(crate::session::SessionBinding::ephemeral_root()),
            id: agent_id,
            mailbox_lease: Arc::new(crate::agent::PendingMailboxLease::new()),
            envelope: envelope.clone(),
            root_inbound: None,
            cancel: root_cancel.clone(),
            terminal_reclamation: true,
        },
    );

    let published = ctx
        .get_extension::<CoordinationEnvelope>()
        .ok_or_else(|| io::Error::other("coordination envelope was not published"))?;
    assert_eq!(*published, envelope);

    let published_cancel = ctx
        .get_extension::<crate::tools::agent::AgentCancellation>()
        .ok_or_else(|| io::Error::other("agent cancellation was not published"))?;
    assert!(!published_cancel.0.is_cancelled());
    root_cancel.cancel();
    assert!(published_cancel.0.is_cancelled());

    let tree = ctx
        .get_extension::<ActionLogTree>()
        .ok_or_else(|| io::Error::other("action-log tree was not published"))?;
    assert_eq!(tree.root(), agent_id);
    let root_log = tree
        .log_of(agent_id)
        .ok_or_else(|| io::Error::other("root action log was not registered"))?;
    assert!(Arc::ptr_eq(&root_log, &action_log));
    assert!(tree.children_of(agent_id).is_empty());
    Ok(())
}

#[test]
fn skill_catalog_diagnostics_reach_the_collector_after_assembly() -> Result<(), Box<dyn Error>> {
    let cwd = tempfile::tempdir()?;
    let broken = cwd.path().join(".norn").join("skills").join("broken");
    std::fs::create_dir_all(&broken)?;
    std::fs::write(broken.join("SKILL.md"), "---\nname: broken\n---\nbody")?;

    let mut profile = crate::profile::Profile::default();
    let base = crate::runtime_init::load_runtime_base(cwd.path(), &mut profile, None, None)?;
    let ctx = ToolContext::empty();
    install_runtime_base_extensions(&ctx, &base, None, cwd.path())?;

    let snapshot = base.diagnostics.snapshot();
    assert!(
        snapshot
            .iter()
            .any(|diagnostic| diagnostic.code == "skill-missing-description"),
        "malformed skill diagnostic was not surfaced: {snapshot:?}",
    );
    Ok(())
}

#[test]
fn install_runtime_base_extensions_publishes_variant_catalog_with_builtins()
-> Result<(), Box<dyn Error>> {
    let cwd = tempfile::tempdir()?;
    let mut profile = crate::profile::Profile::default();
    let base = crate::runtime_init::load_runtime_base(cwd.path(), &mut profile, None, None)?;
    let ctx = ToolContext::empty();
    install_runtime_base_extensions(&ctx, &base, None, cwd.path())?;

    let catalog = ctx
        .get_extension::<crate::agent::variants::VariantCatalog>()
        .ok_or_else(|| io::Error::other("variant catalog was not published"))?;
    for name in ["explorer", "reviewer", "implementer"] {
        assert!(
            catalog.get(name).is_some(),
            "built-in variant '{name}' did not resolve",
        );
    }
    Ok(())
}
