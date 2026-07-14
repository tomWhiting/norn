use std::sync::Arc;

use super::*;
use crate::integration::mcp_runtime::tests::runtime_with_servers;
use crate::provider::mock::MockProvider;
use crate::provider::traits::Provider;

#[test]
fn builder_publishes_only_the_selected_root_view() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = Arc::new(runtime_with_servers(&["alpha", "beta"]));
    let alpha = runtime.tool_names_for_servers(&["alpha".to_owned()])?;
    let beta = runtime.tool_names_for_servers(&["beta".to_owned()])?;
    let alpha_name = alpha.first().ok_or("alpha fixture exposed no tool")?;
    let beta_name = beta.first().ok_or("beta fixture exposed no tool")?;
    let selection = crate::model_catalog::default_selection();
    let context_window = crate::model_catalog::smallest_context_window_for_model(selection.model)
        .ok_or("catalogued test model has no context window")?;
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
    let working_dir = tempfile::tempdir()?;

    let parts = AgentBuilder::new(provider)
        .model(selection.model)
        .context_window_limit(context_window)
        .working_dir(working_dir.path())
        .mcp_runtime_for_servers(Arc::clone(&runtime), &["alpha".to_owned()])?
        .build()?
        .into_parts();

    assert!(parts.registry.get(alpha_name).is_some());
    assert!(parts.registry.get(beta_name).is_none());
    assert!(parts.registry.get_registered(beta_name).is_some());
    assert!(
        parts
            .tool_defs
            .iter()
            .any(|definition| definition.name == *alpha_name)
    );
    assert!(
        parts
            .tool_defs
            .iter()
            .all(|definition| definition.name != *beta_name)
    );
    Ok(())
}

#[tokio::test]
#[serial_test::serial]
async fn built_handle_controls_live_mcp_state_without_an_initial_runtime()
-> Result<(), Box<dyn std::error::Error>> {
    let home = tempfile::tempdir()?;
    let working_dir = tempfile::tempdir()?;
    temp_env::async_with_vars([("NORN_HOME", Some(home.path().as_os_str()))], async {
        let state = crate::config::McpConfigState::from_layers(
            working_dir.path().to_path_buf(),
            std::array::from_fn(|_| std::collections::BTreeMap::new()),
            std::collections::BTreeMap::new(),
        )?;
        let selection = crate::model_catalog::default_selection();
        let context_window =
            crate::model_catalog::smallest_context_window_for_model(selection.model)
                .ok_or("catalogued test model has no context window")?;
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        let agent = AgentBuilder::new(provider)
            .model(selection.model)
            .context_window_limit(context_window)
            .working_dir(working_dir.path())
            .mcp_config_state(state)
            .build()?;
        let control = agent
            .handle()
            .mcp_control()
            .ok_or("live MCP control was not attached")?;

        assert!(control.list().await?.is_empty());
        let mutation = control
            .session_add(
                "offline".to_owned(),
                crate::config::McpServerSettings {
                    transport: Some("stdio".to_owned()),
                    command: Some("/definitely/not/a/real/norn-mcp-server".to_owned()),
                    ..crate::config::McpServerSettings::default()
                },
            )
            .await?;
        let statuses = control.list().await?;

        assert!(mutation.changed);
        assert_eq!(mutation.revision, 1);
        assert_eq!(statuses.len(), 1);
        assert!(!statuses[0].active);
        assert!(statuses[0].failure_present);
        Ok::<(), Box<dyn std::error::Error>>(())
    })
    .await
}
