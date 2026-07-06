//! [`NornWrappedClaudeCode`] — launches Claude Code in bare-metal mode with
//! native tools disabled, Norn's tools exposed via MCP, and a replaced
//! system prompt.
//!
//! Each call to [`NornWrappedClaudeCode::run`] spawns a fresh process,
//! replays its stream-json events into Norn [`SessionEvent`]s, and tracks
//! the Claude session id for legitimate resumption.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use claude_runner::events::ContentItem;
use claude_runner::types::OutputFormat;
use claude_runner::{ClaudeCommand, ClaudeEvent};
use serde_json::Value;

use crate::error::IntegrationError;
use crate::session::events::{EventBase, SessionEvent, ToolCallEvent};

use super::adapter::{spawn_and_collect, tool_data_pair};

/// Configuration for [`NornWrappedClaudeCode`].
#[derive(Clone, Debug)]
pub struct NornWrappedClaudeConfig {
    /// Path to the Claude Code binary or runner script.
    pub claude_code_path: PathBuf,
    /// Names of Norn tools to expose via MCP. Empty means expose every
    /// registered tool — callers that want a narrower set list it here.
    pub norn_tools: Vec<String>,
    /// System prompt that fully replaces Claude Code's default.
    pub system_prompt: String,
    /// MCP server address (e.g. a stdio command line) that the wrapper
    /// passes to Claude Code via `--mcp-config`.
    pub mcp_server_address: String,
}

/// Wraps Claude Code in bare-metal mode: native tools disabled, Norn's
/// tools provided via MCP, system prompt replaced. Each call to
/// [`NornWrappedClaudeCode::run`] spawns a fresh process, replays its
/// stream-json events into Norn [`SessionEvent`]s, and tracks the Claude
/// session id for legitimate resumption.
pub struct NornWrappedClaudeCode {
    config: NornWrappedClaudeConfig,
    claude_session_id: Arc<parking_lot::Mutex<Option<String>>>,
}

impl NornWrappedClaudeCode {
    /// Construct a new wrapper from configuration.
    #[must_use]
    pub fn new(config: NornWrappedClaudeConfig) -> Self {
        Self {
            config,
            claude_session_id: Arc::new(parking_lot::Mutex::new(None)),
        }
    }

    /// Last observed Claude Code session id, if any.
    #[must_use]
    pub fn claude_session_id(&self) -> Option<String> {
        self.claude_session_id.lock().clone()
    }

    /// Build the [`ClaudeCommand`] used for one invocation.
    ///
    /// The command strips native tools via `--tools ""` (the literal CLI
    /// flag the brief refers to as `--no-tools`), replaces the system
    /// prompt, and registers the Norn MCP server via `--mcp-config` +
    /// `--strict-mcp-config`.
    pub(crate) fn build_command(&self, prompt: &str) -> ClaudeCommand {
        let mcp_config = serde_json::json!({
            "mcpServers": {
                "norn": {
                    "command": self.config.mcp_server_address,
                }
            }
        })
        .to_string();
        ClaudeCommand::new()
            .binary(self.config.claude_code_path.to_string_lossy().into_owned())
            .prompt(prompt.to_owned())
            .print_mode()
            .output_format(OutputFormat::StreamJson)
            .include_partial_messages()
            .system_prompt(self.config.system_prompt.clone())
            .tools(String::new())
            .mcp_config(mcp_config)
            .strict_mcp_config()
            .dangerously_skip_permissions()
    }

    /// Run one prompt against the wrapped Claude Code instance, returning
    /// the [`SessionEvent`] stream captured from its output.
    ///
    /// # Errors
    ///
    /// Returns [`IntegrationError::ClaudeRunnerError`] when spawning or
    /// reading the wrapped process fails.
    pub fn run(&self, prompt: &str) -> Result<Vec<SessionEvent>, IntegrationError> {
        let cmd = self.build_command(prompt);
        let claude_events = spawn_and_collect(&cmd)?;
        let session_events = self.capture_session_events(&claude_events);
        Ok(session_events)
    }

    /// Map a slice of [`ClaudeEvent`]s into [`SessionEvent`]s, threading the
    /// Claude session id into the wrapper for resumption. Public for tests.
    pub fn capture_session_events(&self, events: &[ClaudeEvent]) -> Vec<SessionEvent> {
        let mut out = Vec::new();
        for ev in events {
            if let Some(session_id) = claude_session_id_of(ev) {
                let mut guard = self.claude_session_id.lock();
                if guard.is_none() {
                    *guard = Some(session_id);
                }
            }
            match ev {
                ClaudeEvent::Assistant { message, .. } => {
                    let tool_calls = message
                        .content
                        .iter()
                        .filter_map(|item| {
                            if let ContentItem::ToolUse { id, tool_data } = item {
                                let (name, input) = tool_data_pair(tool_data);
                                Some(ToolCallEvent {
                                    call_id: id.clone(),
                                    name,
                                    arguments: input,
                                    kind: crate::provider::request::ToolCallKind::Function,
                                })
                            } else {
                                None
                            }
                        })
                        .collect();
                    let content = message.text_items().collect::<Vec<&str>>().join("\n");
                    out.push(SessionEvent::AssistantMessage {
                        base: EventBase::new(None),
                        content,
                        thinking: String::new(),
                        // Claude passthrough carries no OpenAI Responses
                        // reasoning items.
                        reasoning: Vec::new(),
                        tool_calls,
                        usage: crate::session::events::EventUsage::default(),
                        stop_reason: String::new(),
                        response_id: None,
                    });
                }
                ClaudeEvent::User { message, .. } => {
                    for item in &message.content {
                        if let ContentItem::ToolResult {
                            tool_use_id,
                            content,
                            ..
                        } = item
                        {
                            out.push(SessionEvent::ToolResult {
                                base: EventBase::new(None),
                                tool_call_id: tool_use_id.clone(),
                                tool_name: String::new(),
                                output: content.clone(),
                                spool_ref: None,
                                duration_ms: 0,
                            });
                        } else if let ContentItem::Text { text } = item {
                            out.push(SessionEvent::UserMessage {
                                base: EventBase::new(None),
                                content: text.clone(),
                            });
                        }
                    }
                }
                ClaudeEvent::Result {
                    result: Some(value),
                    ..
                } => {
                    let mut data = HashMap::new();
                    data.insert("result".to_owned(), value.clone());
                    out.push(SessionEvent::Custom {
                        base: EventBase::new(None),
                        event_type: "claude_code.result".to_owned(),
                        data: serde_json::to_value(&data).unwrap_or(Value::Null),
                    });
                }
                _ => {}
            }
        }
        out
    }
}

fn claude_session_id_of(event: &ClaudeEvent) -> Option<String> {
    match event {
        ClaudeEvent::System(data) => data.session_id.clone(),
        ClaudeEvent::Assistant { session_id, .. }
        | ClaudeEvent::User { session_id, .. }
        | ClaudeEvent::StreamEvent { session_id, .. }
        | ClaudeEvent::Result { session_id, .. } => session_id.clone(),
        _ => None,
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::no_effect_underscore_binding,
    clippy::useless_vec,
    clippy::missing_const_for_fn,
    clippy::duration_suboptimal_units,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::unnecessary_trailing_comma,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants
)]
mod tests {
    use super::*;
    use claude_runner::events::{ClaudeMessage, ContentItem, SystemEventData};

    fn wrapper_config() -> NornWrappedClaudeConfig {
        NornWrappedClaudeConfig {
            claude_code_path: PathBuf::from("/usr/local/bin/claude"),
            norn_tools: vec!["read".to_owned(), "write".to_owned()],
            system_prompt: "you are norn-wrapped".to_owned(),
            mcp_server_address: "norn-mcp-stdio".to_owned(),
        }
    }

    // R2 acceptance: launch configuration includes stripped tools, replaced
    // system prompt, and MCP server registration.
    #[test]
    fn norn_wrapped_command_includes_stripped_tools_and_mcp_config() {
        let wrapper = NornWrappedClaudeCode::new(wrapper_config());
        let cmd = wrapper.build_command("task");
        let args = cmd.build_args();
        let joined = args.join(" ");
        assert!(joined.contains("--tools"), "tools flag set: {joined}");
        assert!(
            joined.contains("--system-prompt"),
            "system prompt: {joined}"
        );
        assert!(joined.contains("--mcp-config"), "mcp-config: {joined}");
        // The MCP config payload includes the server address verbatim.
        assert!(
            args.iter().any(|a| a.contains("norn-mcp-stdio")),
            "address propagated: {joined}",
        );
    }

    // R2 acceptance: Claude Code stream events convert into Norn SessionEvents.
    #[test]
    fn capture_session_events_maps_assistant_and_result() {
        let wrapper = NornWrappedClaudeCode::new(wrapper_config());
        let claude_events = vec![
            ClaudeEvent::System(Box::new(SystemEventData {
                subtype: Some("init".to_owned()),
                session_id: Some("claude-session-1".to_owned()),
                ..Default::default()
            })),
            ClaudeEvent::Assistant {
                message: ClaudeMessage {
                    id: Some("m1".to_owned()),
                    message_type: Some("message".to_owned()),
                    role: "assistant".to_owned(),
                    model: None,
                    content: vec![ContentItem::Text {
                        text: "ok".to_owned(),
                    }],
                    stop_reason: Some("end_turn".to_owned()),
                    usage: None,
                },
                session_id: Some("claude-session-1".to_owned()),
                parent_tool_use_id: None,
                uuid: None,
            },
            ClaudeEvent::Result {
                subtype: Some("success".to_owned()),
                is_error: Some(false),
                duration_ms: None,
                duration_api_ms: None,
                result: Some(serde_json::json!({"final": true})),
                error: None,
                num_turns: None,
                session_id: Some("claude-session-1".to_owned()),
                structured_output: None,
                stop_reason: Some("end_turn".to_owned()),
                total_cost_usd: None,
                usage: None,
            },
        ];
        let events = wrapper.capture_session_events(&claude_events);
        assert!(events.iter().any(
            |e| matches!(e, SessionEvent::AssistantMessage { content, .. } if content == "ok")
        ));
        assert!(events.iter().any(|e| matches!(
            e,
            SessionEvent::Custom { event_type, .. } if event_type == "claude_code.result"
        )));
        assert_eq!(
            wrapper.claude_session_id().as_deref(),
            Some("claude-session-1")
        );
    }
}
