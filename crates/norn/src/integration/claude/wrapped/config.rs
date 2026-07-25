use std::path::PathBuf;

use claude_runner::ClaudeCommand;
use serde::Serialize;

use super::NornWrappedClaudeError;
use super::command_line::parse_command_line;

const NORN_MCP_SERVER_NAME: &str = "norn";
const MINIMAL_SETTINGS: &str =
    r#"{"disableAllHooks":true,"disableAgentView":true,"disableBundledSkills":true}"#;
const MINIMAL_DISABLE_ENV_VARS: &[&str] = &[
    "CLAUDE_CODE_DISABLE_AGENT_VIEW",
    "CLAUDE_CODE_DISABLE_AUTO_MEMORY",
    "CLAUDE_CODE_DISABLE_BACKGROUND_TASKS",
    "CLAUDE_CODE_DISABLE_BUNDLED_SKILLS",
    "CLAUDE_CODE_DISABLE_CLAUDE_MDS",
    "CLAUDE_CODE_DISABLE_CRON",
];

/// Configuration shared by one-shot compatibility calls and persistent
/// [`NornWrappedClaudeSession`](super::NornWrappedClaudeSession)s.
#[derive(Clone, Debug)]
pub struct NornWrappedClaudeConfig {
    /// Path to the Claude Code binary or runner script.
    pub claude_code_path: PathBuf,
    /// Norn MCP tools to auto-approve. Names are converted to exact
    /// `mcp__norn__<name>` permission rules. An empty list auto-approves all
    /// tools exposed by the strict Norn MCP server.
    pub norn_tools: Vec<String>,
    /// System prompt that fully replaces Claude Code's default.
    pub system_prompt: String,
    /// Shell-style command line that starts Norn's stdio MCP server.
    ///
    /// This compatibility field is parsed into an executable and argument
    /// array before it is serialized into `--mcp-config`; it is never passed
    /// to Claude Code as one executable path.
    pub mcp_server_address: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct McpConfiguration {
    mcp_servers: std::collections::BTreeMap<&'static str, StdioServer>,
}

#[derive(Serialize)]
struct StdioServer {
    command: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    args: Vec<String>,
}

impl NornWrappedClaudeConfig {
    pub(super) fn build_sdk_command(&self) -> Result<ClaudeCommand, NornWrappedClaudeError> {
        let command = ClaudeCommand::minimal_subscription()
            .binary(self.claude_binary()?)
            .system_prompt(self.system_prompt.clone())
            .mcp_config(self.serialized_mcp_configuration()?);
        self.apply_norn_tool_permissions(command)
    }

    pub(super) fn build_legacy_command(
        &self,
        prompt: &str,
    ) -> Result<ClaudeCommand, NornWrappedClaudeError> {
        let mut command = ClaudeCommand::new()
            .binary(self.claude_binary()?)
            .prompt(prompt)
            .print_mode()
            .output_format(claude_runner::OutputFormat::StreamJson)
            .include_partial_messages()
            .system_prompt(self.system_prompt.clone())
            .tools(String::new())
            .mcp_config(self.serialized_mcp_configuration()?)
            .strict_mcp_config()
            .no_setting_sources()
            .settings_json(MINIMAL_SETTINGS)
            .disable_slash_commands();
        for name in MINIMAL_DISABLE_ENV_VARS {
            command = command.env(*name, "1");
        }
        self.apply_norn_tool_permissions(command)
    }

    fn serialized_mcp_configuration(&self) -> Result<String, NornWrappedClaudeError> {
        let parsed = parse_command_line(&self.mcp_server_address)?;
        let configuration = McpConfiguration {
            mcp_servers: std::collections::BTreeMap::from([(
                NORN_MCP_SERVER_NAME,
                StdioServer {
                    command: parsed.command,
                    args: parsed.args,
                },
            )]),
        };
        serde_json::to_string(&configuration)
            .map_err(NornWrappedClaudeError::McpConfigurationSerialization)
    }

    fn claude_binary(&self) -> Result<&str, NornWrappedClaudeError> {
        self.claude_code_path.to_str().ok_or_else(|| {
            NornWrappedClaudeError::NonUtf8ClaudeCodePath {
                path: self.claude_code_path.clone(),
            }
        })
    }

    fn apply_norn_tool_permissions(
        &self,
        command: ClaudeCommand,
    ) -> Result<ClaudeCommand, NornWrappedClaudeError> {
        if self.norn_tools.is_empty() {
            return Ok(command.allowed_tool("mcp__norn__*"));
        }

        let mut permission_rules = Vec::with_capacity(self.norn_tools.len());
        for (index, tool) in self.norn_tools.iter().enumerate() {
            if tool.is_empty() {
                return Err(NornWrappedClaudeError::InvalidNornToolName {
                    index,
                    reason: "tool name must not be empty",
                });
            }
            if tool.len() > 128 {
                return Err(NornWrappedClaudeError::InvalidNornToolName {
                    index,
                    reason: "tool name must contain at most 128 ASCII characters",
                });
            }
            if !tool
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
            {
                return Err(NornWrappedClaudeError::InvalidNornToolName {
                    index,
                    reason: "tool name must use only ASCII letters, digits, underscore, dot, or hyphen",
                });
            }
            permission_rules.push(format!("mcp__norn__{tool}"));
        }
        Ok(command.allowed_tools(permission_rules))
    }
}
