use super::*;
use norn::config::{McpApprovalStore, McpRuntimeOverrides, load_resolved_settings};

#[tokio::test]
#[serial_test::serial]
async fn pending_project_server_causes_no_process_activation()
-> Result<(), Box<dyn std::error::Error>> {
    let home = tempfile::tempdir()?;
    let project = tempfile::tempdir()?;
    let config_dir = project.path().join(".norn");
    std::fs::create_dir_all(&config_dir)?;
    let marker = project.path().join("must-not-exist");
    std::fs::write(
        config_dir.join("settings.json"),
        serde_json::to_vec(&serde_json::json!({
            "mcp_servers": {
                "hostile": {
                    "transport": "stdio",
                    "command": "/bin/sh",
                    "args": ["-c", format!("touch {}", marker.display())]
                }
            }
        }))?,
    )?;

    temp_env::async_with_vars([("NORN_HOME", Some(home.path().as_os_str()))], async {
        let resolved = load_resolved_settings(project.path(), &McpRuntimeOverrides::default())?;
        let startup = connect_mcp_runtime(&resolved.project_root, &resolved.mcp_servers).await?;

        assert!(startup.runtime.is_none());
        assert_eq!(startup.pending_project_servers, ["hostile"]);
        assert!(!marker.exists());
        Ok::<_, Box<dyn std::error::Error>>(())
    })
    .await
}

#[tokio::test]
#[serial_test::serial]
async fn workspace_local_server_is_direct_and_not_approval_gated()
-> Result<(), Box<dyn std::error::Error>> {
    let home = tempfile::tempdir()?;
    let project = tempfile::tempdir()?;
    let config_dir = project.path().join(".norn");
    std::fs::create_dir_all(&config_dir)?;
    let marker = project.path().join("workspace-local-ran");
    std::fs::write(
        config_dir.join("settings.local.json"),
        serde_json::to_vec(&serde_json::json!({
            "mcp_servers": {
                "workspace_local": {
                    "command": "/bin/sh",
                    "args": ["-c", format!("touch {}", marker.display())]
                }
            }
        }))?,
    )?;

    temp_env::async_with_vars([("NORN_HOME", Some(home.path().as_os_str()))], async {
        let resolved = load_resolved_settings(project.path(), &McpRuntimeOverrides::default())?;
        let server = resolved
            .mcp_servers
            .get("workspace_local")
            .ok_or("workspace-local server was not resolved")?;
        assert_eq!(server.source(), norn::config::McpConfigSource::Local);
        let startup = connect_mcp_runtime(&resolved.project_root, &resolved.mcp_servers).await?;
        assert!(startup.pending_project_servers.is_empty());
        assert_eq!(startup.failed_servers.len(), 1);
        assert!(marker.exists());
        Ok::<_, Box<dyn std::error::Error>>(())
    })
    .await
}

#[cfg(unix)]
#[tokio::test]
#[serial_test::serial]
async fn approved_project_server_connects_through_startup() -> Result<(), Box<dyn std::error::Error>>
{
    let home = tempfile::tempdir()?;
    let project = tempfile::tempdir()?;
    let config_dir = project.path().join(".norn");
    std::fs::create_dir_all(&config_dir)?;
    let script = concat!(
        "read initialize; ",
        "printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{\"tools\":{}},\"serverInfo\":{\"name\":\"approved-fixture\",\"version\":\"1\"}}}'; ",
        "read initialized; read list; ",
        "printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[{\"name\":\"echo\"}]}}'",
    );
    std::fs::write(
        config_dir.join("settings.json"),
        serde_json::to_vec(&serde_json::json!({
            "mcp_servers": {
                "approved_fixture": {
                    "transport": "stdio",
                    "command": "/bin/sh",
                    "args": ["-c", script]
                }
            }
        }))?,
    )?;

    temp_env::async_with_vars([("NORN_HOME", Some(home.path().as_os_str()))], async {
        let resolved = load_resolved_settings(project.path(), &McpRuntimeOverrides::default())?;
        let server = resolved
            .mcp_servers
            .get("approved_fixture")
            .ok_or("approved project fixture was not resolved")?;
        McpApprovalStore::open()?.approve(&resolved.project_root, server)?;

        let startup = connect_mcp_runtime(&resolved.project_root, &resolved.mcp_servers).await?;
        let Some(runtime) = startup.runtime else {
            return Err("approved project MCP fixture did not connect".into());
        };
        assert!(startup.pending_project_servers.is_empty());
        assert!(startup.project_approval_error.is_none());
        assert!(startup.failed_servers.is_empty());
        assert_eq!(
            runtime.server_names().collect::<Vec<_>>(),
            ["approved_fixture"]
        );
        assert_eq!(runtime.tool_names().len(), 1);
        Ok::<_, Box<dyn std::error::Error>>(())
    })
    .await
}

#[cfg(unix)]
#[tokio::test]
#[serial_test::serial]
async fn private_local_server_connects_without_project_approval()
-> Result<(), Box<dyn std::error::Error>> {
    let home = tempfile::tempdir()?;
    let project = tempfile::tempdir()?;
    let script = concat!(
        "read initialize; ",
        "printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{\"tools\":{}},\"serverInfo\":{\"name\":\"fixture\",\"version\":\"1\"}}}'; ",
        "read initialized; read list; printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":\"server-ping\",\"method\":\"ping\"}'; read pong; ",
        "printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[{\"name\":\"echo\"}]}}'",
    );
    let local_path = temp_env::with_var("NORN_HOME", Some(home.path().as_os_str()), || {
        norn::config::project_local_mcp_settings_path(project.path())
    })?;
    let Some(local_dir) = local_path.parent() else {
        return Err("private project-local MCP path has no parent".into());
    };
    std::fs::create_dir_all(local_dir)?;
    std::fs::write(
        local_path,
        serde_json::to_vec(&serde_json::json!({
            "mcp_servers": {
                "local_fixture": {
                    "transport": "stdio",
                    "command": "/bin/sh",
                    "args": ["-c", script]
                },
                "missing_fixture": {
                    "transport": "stdio",
                    "command": "/definitely/not/a/norn-mcp-server"
                }
            }
        }))?,
    )?;

    temp_env::async_with_vars([("NORN_HOME", Some(home.path().as_os_str()))], async {
        let resolved = load_resolved_settings(project.path(), &McpRuntimeOverrides::default())?;
        let startup = connect_mcp_runtime(&resolved.project_root, &resolved.mcp_servers).await?;
        let Some(runtime) = startup.runtime else {
            return Err("local MCP fixture did not connect".into());
        };

        assert!(startup.pending_project_servers.is_empty());
        assert_eq!(
            runtime.server_names().collect::<Vec<_>>(),
            ["local_fixture"]
        );
        assert_eq!(startup.failed_servers.len(), 1);
        assert_eq!(startup.failed_servers[0].0, "missing_fixture");
        let all_tool_names = runtime.tool_names();
        assert_eq!(all_tool_names.len(), 1);
        let mut registry = norn::tool::registry::ToolRegistry::new();
        runtime.register_tools(&mut registry)?;
        runtime.restrict_registry_to_servers(&mut registry, &[])?;
        assert!(registry.names().next().is_none());
        Ok::<_, Box<dyn std::error::Error>>(())
    })
    .await
}
