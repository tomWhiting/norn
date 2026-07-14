//! Persistent and approval-aware `norn mcp` configuration operations.

use std::collections::BTreeMap;

use norn::config::{
    McpApprovalState, McpApprovalStore, McpConfigLayer, McpConfigSource, McpConfigState,
    McpLayerEntry, McpPersistentMutation, McpPersistentScope, McpRuntimeOverrides,
    McpServerSettings, ResolvedMcpServer,
};

use crate::cli::{BuildError, Cli, ExitCode, McpPersistenceScope};
use crate::config::{apply_working_dir, collect_extension_servers};

pub(super) enum ConfigCommand {
    List,
    Inspect {
        name: String,
    },
    Add {
        name: String,
        scope: McpPersistenceScope,
        command: Option<String>,
        args: Vec<String>,
        url: Option<String>,
        env: Vec<String>,
        header: Vec<String>,
    },
    Remove {
        name: String,
        scope: McpPersistenceScope,
    },
    SetEnabled {
        name: String,
        scope: McpPersistenceScope,
        enabled: bool,
    },
    Approve {
        name: Option<String>,
        all: bool,
    },
    Revoke {
        name: Option<String>,
        all: bool,
    },
}

pub(super) fn run_config_command(cli: &Cli, command: ConfigCommand) -> ExitCode {
    match execute_config_command(cli, command) {
        Ok(()) => ExitCode::Success,
        Err(error) => {
            eprintln!("norn: {error}");
            error.exit_code()
        }
    }
}

fn execute_config_command(cli: &Cli, command: ConfigCommand) -> Result<(), BuildError> {
    apply_working_dir(cli)?;
    let cwd = std::env::current_dir()?;
    match command {
        ConfigCommand::List | ConfigCommand::Approve { .. } | ConfigCommand::Revoke { .. } => {
            execute_resolved_command(cli, &cwd, command)
        }
        ConfigCommand::Inspect { name } => {
            let state = load_state(cli, &cwd)?;
            render_inspection(&state.inspect(&name).map_err(config_error)?)
        }
        ConfigCommand::Add {
            name,
            scope,
            command,
            args,
            url,
            env,
            header,
        } => {
            let definition = build_definition(command, args, url, env, header)?;
            persist(
                cli,
                &cwd,
                scope,
                &McpPersistentMutation::Upsert { name, definition },
            )
        }
        ConfigCommand::Remove { name, scope } => {
            persist(cli, &cwd, scope, &McpPersistentMutation::Remove { name })
        }
        ConfigCommand::SetEnabled {
            name,
            scope,
            enabled,
        } => persist(
            cli,
            &cwd,
            scope,
            &McpPersistentMutation::SetEnabled { name, enabled },
        ),
    }
}

fn execute_resolved_command(
    cli: &Cli,
    cwd: &std::path::Path,
    command: ConfigCommand,
) -> Result<(), BuildError> {
    let overrides = McpRuntimeOverrides {
        cli: collect_extension_servers(&cli.extension)?,
        session: BTreeMap::new(),
    };
    let resolved = norn::config::load_resolved_settings(cwd, &overrides).map_err(config_error)?;
    match command {
        ConfigCommand::List => {
            render_list(&resolved);
            Ok(())
        }
        ConfigCommand::Approve { name, all } => approve(&resolved, name.as_deref(), all),
        ConfigCommand::Revoke { name, all } => revoke(&resolved, name, all),
        _ => Err(BuildError::Argument(
            "internal MCP command routing mismatch".to_owned(),
        )),
    }
}

fn render_list(resolved: &norn::config::ResolvedSettings) {
    let store = McpApprovalStore::open();
    if let Err(error) = &store {
        eprintln!(
            "norn: project MCP approvals could not be read; shared servers remain pending: {error}"
        );
    }
    for server in resolved.mcp_servers.iter() {
        let state = match &store {
            Ok(store) => store
                .state(&resolved.project_root, server)
                .unwrap_or(McpApprovalState::Pending),
            Err(_) if server.source() == McpConfigSource::Project => McpApprovalState::Pending,
            Err(_) => McpApprovalState::NotRequired,
        };
        eprintln!(
            "{}\t{}\t{}\t{}\t{}",
            server.name(),
            source_label(server.source()),
            approval_label(state),
            transport_label(server.definition()),
            target_label(server.definition()),
        );
    }
}

fn approve(
    resolved: &norn::config::ResolvedSettings,
    name: Option<&str>,
    all: bool,
) -> Result<(), BuildError> {
    let store = McpApprovalStore::open().map_err(config_error)?;
    for server in selected_project_servers(&resolved.mcp_servers, name, all)? {
        store
            .approve(&resolved.project_root, server)
            .map_err(config_error)?;
        eprintln!("Approved MCP server '{}' for this project.", server.name());
    }
    Ok(())
}

fn revoke(
    resolved: &norn::config::ResolvedSettings,
    name: Option<String>,
    all: bool,
) -> Result<(), BuildError> {
    let store = McpApprovalStore::open().map_err(config_error)?;
    if let Some(name) = name {
        store
            .revoke(&resolved.project_root, &name)
            .map_err(config_error)?;
        eprintln!("Revoked MCP server '{name}' for this project.");
        return Ok(());
    }
    for server in selected_project_servers(&resolved.mcp_servers, None, all)? {
        store
            .revoke(&resolved.project_root, server.name())
            .map_err(config_error)?;
        eprintln!("Revoked MCP server '{}' for this project.", server.name());
    }
    Ok(())
}

fn load_state(cli: &Cli, cwd: &std::path::Path) -> Result<McpConfigState, BuildError> {
    McpConfigState::load(cwd, collect_extension_servers(&cli.extension)?).map_err(config_error)
}

fn persist(
    cli: &Cli,
    cwd: &std::path::Path,
    scope: McpPersistenceScope,
    mutation: &McpPersistentMutation,
) -> Result<(), BuildError> {
    let mut state = load_state(cli, cwd)?;
    let change = state
        .persist(persistent_scope(scope), mutation)
        .map_err(config_error)?;
    let status = if change.changed() {
        "Updated"
    } else {
        "Unchanged"
    };
    eprintln!("{status} MCP settings at {}.", change.path().display());
    if change.requires_project_approval() {
        eprintln!("The effective project definition remains inactive until explicitly approved.");
    }
    Ok(())
}

fn build_definition(
    command: Option<String>,
    args: Vec<String>,
    url: Option<String>,
    env: Vec<String>,
    header: Vec<String>,
) -> Result<McpServerSettings, BuildError> {
    match (command, url) {
        (Some(command), None) => Ok(McpServerSettings {
            transport: Some("stdio".to_owned()),
            command: Some(command),
            args: (!args.is_empty()).then_some(args),
            env: parse_entries("environment", env)?,
            ..McpServerSettings::default()
        }),
        (None, Some(url)) => Ok(McpServerSettings {
            transport: Some("http".to_owned()),
            url: Some(url),
            headers: parse_entries("header", header)?,
            ..McpServerSettings::default()
        }),
        _ => Err(BuildError::Argument(
            "MCP add requires exactly one of --command or --url".to_owned(),
        )),
    }
}

fn parse_entries(
    kind: &str,
    entries: Vec<String>,
) -> Result<Option<BTreeMap<String, String>>, BuildError> {
    let mut parsed = BTreeMap::new();
    for entry in entries {
        let Some((key, value)) = entry.split_once('=') else {
            return Err(BuildError::Argument(format!(
                "MCP {kind} entries must use KEY=VALUE"
            )));
        };
        if key.is_empty() {
            return Err(BuildError::Argument(format!(
                "MCP {kind} keys cannot be empty"
            )));
        }
        if parsed.insert(key.to_owned(), value.to_owned()).is_some() {
            return Err(BuildError::Argument(format!(
                "MCP {kind} key '{key}' is specified more than once"
            )));
        }
    }
    Ok((!parsed.is_empty()).then_some(parsed))
}

fn render_inspection(inspection: &norn::config::McpServerInspection) -> Result<(), BuildError> {
    if inspection.chain().is_empty() {
        return Err(BuildError::Argument(format!(
            "no MCP server named '{}' exists in any configuration layer",
            inspection.name()
        )));
    }
    for entry in inspection.chain() {
        match entry {
            McpLayerEntry::Definition { layer, definition } => eprintln!(
                "{}\tdefinition\t{}\t{}",
                layer_label(*layer),
                transport_label(definition),
                target_label(definition),
            ),
            McpLayerEntry::DisabledInherited => {
                eprintln!("session\tdisabled-inherited\t-\t-");
            }
            McpLayerEntry::EnabledInherited => {
                eprintln!("session\tenabled-inherited\t-\t-");
            }
            McpLayerEntry::DisabledDefinition(definition) => eprintln!(
                "session\tdisabled-session\t{}\t{}",
                transport_label(definition),
                target_label(definition),
            ),
        }
    }
    if let Some(effective) = inspection.effective() {
        eprintln!(
            "effective\t{}\t{}\t{}\t{}",
            layer_label(effective.source()),
            if effective.enabled() {
                "enabled"
            } else {
                "disabled"
            },
            transport_label(effective.definition()),
            target_label(effective.definition()),
        );
    }
    Ok(())
}

fn selected_project_servers<'a>(
    servers: &'a norn::config::ResolvedMcpServers,
    name: Option<&str>,
    all: bool,
) -> Result<Vec<&'a ResolvedMcpServer>, BuildError> {
    let selected: Vec<_> = servers
        .iter()
        .filter(|server| server.source() == McpConfigSource::Project)
        .filter(|server| all || name == Some(server.name()))
        .collect();
    if selected.is_empty() {
        return Err(BuildError::Argument(match name {
            Some(name) => format!(
                "no effective shared-project MCP server named '{name}'; run `norn mcp list`",
            ),
            None => "no effective shared-project MCP servers to update".to_owned(),
        }));
    }
    Ok(selected)
}

const fn persistent_scope(scope: McpPersistenceScope) -> McpPersistentScope {
    match scope {
        McpPersistenceScope::User => McpPersistentScope::User,
        McpPersistenceScope::Project => McpPersistentScope::SharedProject,
        McpPersistenceScope::WorkspaceLocal => McpPersistentScope::WorkspaceLocal,
        McpPersistenceScope::Local => McpPersistentScope::PrivateLocal,
    }
}

const fn layer_label(layer: McpConfigLayer) -> &'static str {
    match layer {
        McpConfigLayer::User => "user",
        McpConfigLayer::SharedProject => "project",
        McpConfigLayer::WorkspaceLocal => "workspace-local",
        McpConfigLayer::PrivateLocal => "local",
        McpConfigLayer::Cli => "cli",
        McpConfigLayer::Session => "session",
    }
}

const fn source_label(source: McpConfigSource) -> &'static str {
    match source {
        McpConfigSource::User => "user",
        McpConfigSource::Project => "project",
        McpConfigSource::Local => "local",
        McpConfigSource::Cli => "cli",
        McpConfigSource::Session => "session",
    }
}

const fn approval_label(state: McpApprovalState) -> &'static str {
    match state {
        McpApprovalState::NotRequired => "ready",
        McpApprovalState::Approved => "approved",
        McpApprovalState::Pending => "pending",
    }
}

fn transport_label(definition: &McpServerSettings) -> &'static str {
    if definition.command.is_some() {
        "stdio"
    } else if definition.url.is_some() {
        "http"
    } else {
        "disabled"
    }
}

fn target_label(definition: &McpServerSettings) -> String {
    if let Some(command) = definition.command.as_deref() {
        return std::path::Path::new(command)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("<command>")
            .to_owned();
    }
    let Some(raw_url) = definition.url.as_deref() else {
        return "disabled".to_owned();
    };
    reqwest::Url::parse(raw_url).map_or_else(
        |_| "<url>".to_owned(),
        |url| {
            let mut label = url.origin().ascii_serialization();
            if url.path() != "/" && !url.path().is_empty() {
                label.push_str("/<redacted-path>");
            } else {
                label.push('/');
            }
            label
        },
    )
}

fn config_error(error: impl std::fmt::Display) -> BuildError {
    BuildError::Argument(error.to_string())
}

#[cfg(test)]
#[path = "mcp_config_tests.rs"]
mod tests;
