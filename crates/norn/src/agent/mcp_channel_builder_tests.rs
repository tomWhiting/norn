//! Channels belong to the resolved root, never a pre-registration builder identity.

use std::collections::BTreeMap;
use std::sync::Arc;

use super::{AgentBuilder, McpAttachment};
use crate::agent::child_policy::{ChildPolicy, DelegationBudget, MessagingScope};
use crate::agent::registry::AgentRegistry;
use crate::agent_loop::config::ToolExecutor;
use crate::config::{McpConfigState, McpServerSettings};
use crate::integration::{
    McpChannelLimits, McpChannelOverflow, McpChannelPolicy, McpChannelSettings,
    McpChannelSourcePolicy, McpRuntime,
};
use crate::r#loop::LoopContext;
use crate::provider::mock::MockProvider;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

fn settings() -> Result<McpChannelSettings, crate::error::ConfigError> {
    let limits = McpChannelLimits::new(2, 1024).map_err(|error| {
        crate::error::ConfigError::InvalidConfig {
            reason: error.to_string(),
        }
    })?;
    McpChannelSettings::new(
        limits,
        McpChannelSourcePolicy::Off,
        BTreeMap::from([(
            "messages".to_owned(),
            McpChannelSourcePolicy::Delivery(McpChannelPolicy::Wake),
        )]),
        McpChannelOverflow::RejectNew,
    )
}

fn state(path: &std::path::Path) -> Result<McpConfigState, crate::error::ConfigError> {
    McpConfigState::from_layers(
        path.to_path_buf(),
        std::array::from_fn(|_| BTreeMap::new()),
        BTreeMap::from([(
            "messages".to_owned(),
            McpServerSettings {
                command: Some("/not-launched-by-synchronous-builder".to_owned()),
                transport: Some("stdio".to_owned()),
                ..McpServerSettings::default()
            },
        )]),
    )
}

#[tokio::test]
#[serial_test::serial]
async fn inbox_uses_registered_root_instead_of_builder_hint() -> TestResult {
    let home = tempfile::tempdir()?;
    let directory = tempfile::tempdir()?;
    temp_env::async_with_vars([("NORN_HOME", Some(home.path().as_os_str()))], async {
        let registry = AgentRegistry::shared();
        let hint = uuid::Uuid::new_v4();
        let selection = crate::model_catalog::default_selection();
        let agent = AgentBuilder::new(Arc::new(MockProvider::new(Vec::new())))
            .model(selection.model)
            .working_dir(directory.path())
            .agent_id(hint)
            .agent_registry(Arc::clone(&registry))
            .child_policy(ChildPolicy {
                messaging: MessagingScope::SiblingsAndParent,
                delegation: DelegationBudget {
                    remaining_depth: 1,
                    max_concurrent_children: 2,
                },
                inbound_capacity: 2,
                loop_config: None,
            })
            .child_result_capacity(2)
            .register_root("/root".to_owned(), "lead".to_owned())
            .mcp_config_state(state(directory.path())?)
            .mcp_channels(settings()?)
            .build()?;
        let actual = agent.agent_id();
        assert_ne!(actual, hint);
        let registered = registry
            .read()
            .get_by_path("/root")
            .ok_or("root not registered")?;
        assert_eq!(registered.id, actual);
        let parts = agent.into_parts();
        let inbox = parts
            .loop_context
            .mcp_channel_session
            .as_ref()
            .ok_or("inbox absent")?;
        assert_eq!(inbox.recipient_id(), actual);
        let runtime = parts
            .registry
            .shared_context()
            .ok_or("tool context absent")?
            .get_extension::<crate::integration::McpRuntimeStore>()
            .ok_or("runtime store absent")?;
        assert!(runtime.snapshot().runtime().is_empty());
        assert_eq!(runtime.snapshot().revision(), 0);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })
    .await
}

#[test]
fn missing_config_and_preconnected_runtime_cannot_install_channels() -> TestResult {
    let directory = tempfile::tempdir()?;
    let mut context = LoopContext::new("fixture");
    context.agent_id = Some(uuid::Uuid::new_v4());
    let mut attachment = McpAttachment {
        channels: Some(settings()?),
        ..McpAttachment::default()
    };
    assert!(attachment.install_channels(&mut context).is_err());
    assert!(context.mcp_channel_session.is_none());
    attachment.state = Some(state(directory.path())?);
    attachment.runtime = Some(Arc::new(
        crate::integration::mcp_runtime::tests::runtime_with_servers(&["messages"]),
    ));
    assert!(attachment.install_channels(&mut context).is_err());
    assert!(context.mcp_channel_session.is_none());
    attachment.runtime = Some(Arc::new(McpRuntime::empty()));
    assert!(attachment.install_channels(&mut context)?.is_some());
    assert!(attachment.install_channels(&mut context).is_err());
    Ok(())
}

#[test]
fn unknown_and_http_sources_are_refused_before_connection() -> TestResult {
    let directory = tempfile::tempdir()?;
    let empty = McpConfigState::from_layers(
        directory.path().to_path_buf(),
        std::array::from_fn(|_| BTreeMap::new()),
        BTreeMap::new(),
    )?;
    assert!(settings()?.validate_startup(&empty.snapshot()?).is_err());
    let mut state = state(directory.path())?;
    // The CLI definition remains, so a session tombstone supplies a disabled source.
    state.session_disable("messages")?;
    assert!(settings()?.validate_startup(&state.snapshot()?).is_err());
    state.session_add(
        "messages".to_owned(),
        McpServerSettings {
            transport: Some("http".to_owned()),
            url: Some("http://127.0.0.1:1/mcp".to_owned()),
            ..McpServerSettings::default()
        },
    )?;
    assert!(settings()?.validate_startup(&state.snapshot()?).is_err());
    Ok(())
}
