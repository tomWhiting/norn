//! Slash-command preprocessing for the agent loop.
//!
//! User input prefixed with `/` is intercepted before reaching the model and
//! expanded into a deterministic sequence of conversation messages. Three
//! handler kinds cover the common shapes:
//!
//! Handlers cover skills, tools, and custom caller-owned expansion.
//!
//! Unknown commands and inputs that do not begin with `/` pass through
//! unchanged so the model can react. Empty or whitespace-only commands also
//! pass through.

use std::collections::HashMap;
use std::sync::Arc;

use crate::error::NornError;
use crate::provider::request::{AssistantToolCall, Message, MessageRole};

#[path = "command_options.rs"]
mod command_options;
pub use command_options::{
    EffortCommand, ServiceTierCommand, effort_label, parse_effort_command,
    parse_service_tier_command, reasoning_effort_supported_for_model,
    service_tier_supported_for_model, unsupported_reasoning_effort_message,
    unsupported_service_tier_message,
};

#[cfg(test)]
use crate::provider::request::ReasoningEffort;

/// UI surface a built-in slash command is available on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SlashSurface {
    /// Headless/REPL CLI slash-command surface.
    Cli,
    /// Interactive TUI slash-command surface.
    Tui,
}

/// Stable semantic identity for built-in slash commands.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuiltinSlashKind {
    /// `/new`.
    New,
    /// `/clear`.
    Clear,
    /// `/compact`.
    Compact,
    /// `/exit`.
    Exit,
    /// `/quit`.
    Quit,
    /// `/help`.
    Help,
    /// `/model`.
    Model,
    /// `/effort` and `/reasoning-effort`.
    Effort,
    /// `/service-tier`.
    ServiceTier,
    /// `/fast`.
    Fast,
    /// `/tools`.
    Tools,
    /// `/mcp`.
    Mcp,
    /// `/schema`.
    Schema,
    /// `/session`.
    Session,
    /// `/name`.
    Name,
    /// `/variables`.
    Variables,
}

/// Shared metadata for one built-in slash command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BuiltinSlashCommand {
    /// Side-effecting handler kind selected by each UI adapter.
    pub kind: BuiltinSlashKind,
    /// Command name without the leading slash.
    pub name: &'static str,
    /// Usage text rendered in help surfaces.
    pub usage: &'static str,
    /// Help text rendered in detailed help.
    pub help: &'static str,
    /// Short description rendered by autocomplete.
    pub autocomplete: &'static str,
    /// Compact description rendered by CLI `/help`.
    pub cli_description: &'static str,
    cli: bool,
    tui: bool,
}

impl BuiltinSlashCommand {
    /// Whether this command belongs to `surface`.
    #[must_use]
    pub const fn supports(self, surface: SlashSurface) -> bool {
        match surface {
            SlashSurface::Cli => self.cli,
            SlashSurface::Tui => self.tui,
        }
    }
}

/// Built-in slash command metadata shared by CLI and TUI adapters.
pub const BUILTIN_SLASH_COMMANDS: &[BuiltinSlashCommand] = &[
    BuiltinSlashCommand {
        kind: BuiltinSlashKind::New,
        name: "new",
        usage: "/new",
        help: "Start a new session (rotates the JSONL file)",
        autocomplete: "Start a new session",
        cli_description: "Start a new session",
        cli: false,
        tui: true,
    },
    BuiltinSlashCommand {
        kind: BuiltinSlashKind::Clear,
        name: "clear",
        usage: "/clear",
        help: "Reset the conversation history",
        autocomplete: "Reset the conversation history",
        cli_description: "Reset the conversation history",
        cli: true,
        tui: true,
    },
    BuiltinSlashCommand {
        kind: BuiltinSlashKind::Compact,
        name: "compact",
        usage: "/compact",
        help: "Compact older conversation context",
        autocomplete: "Compact conversation history",
        cli_description: "Compact older conversation context",
        cli: true,
        tui: true,
    },
    BuiltinSlashCommand {
        kind: BuiltinSlashKind::Exit,
        name: "exit",
        usage: "/exit",
        help: "Exit the REPL",
        autocomplete: "Exit",
        cli_description: "Exit the REPL",
        cli: true,
        tui: true,
    },
    BuiltinSlashCommand {
        kind: BuiltinSlashKind::Quit,
        name: "quit",
        usage: "/quit",
        help: "Exit the REPL",
        autocomplete: "Exit",
        cli_description: "Exit the REPL",
        cli: true,
        tui: true,
    },
    BuiltinSlashCommand {
        kind: BuiltinSlashKind::Help,
        name: "help",
        usage: "/help",
        help: "Show available commands",
        autocomplete: "Show help",
        cli_description: "Show available commands",
        cli: true,
        tui: true,
    },
    BuiltinSlashCommand {
        kind: BuiltinSlashKind::Model,
        name: "model",
        usage: "/model <name>",
        help: "Show or switch the active model",
        autocomplete: "Switch model",
        cli_description: "Show or switch the active model",
        cli: true,
        tui: true,
    },
    BuiltinSlashCommand {
        kind: BuiltinSlashKind::Effort,
        name: "effort",
        usage: "/effort <none|low|medium|high|xhigh|max|default>",
        help: "Show, set, or clear reasoning effort",
        autocomplete: "Set reasoning effort",
        cli_description: "Show, set, or clear the active reasoning effort",
        cli: true,
        tui: true,
    },
    BuiltinSlashCommand {
        kind: BuiltinSlashKind::Effort,
        name: "reasoning-effort",
        usage: "/reasoning-effort <none|low|medium|high|xhigh|max|default>",
        help: "Alias for /effort",
        autocomplete: "Set reasoning effort",
        cli_description: "Alias for /effort",
        cli: true,
        tui: true,
    },
    BuiltinSlashCommand {
        kind: BuiltinSlashKind::ServiceTier,
        name: "service-tier",
        usage: "/service-tier <fast|none>",
        help: "Show, set, or clear the active service tier",
        autocomplete: "Set service tier",
        cli_description: "Show, set, or clear the active service tier",
        cli: true,
        tui: true,
    },
    BuiltinSlashCommand {
        kind: BuiltinSlashKind::Fast,
        name: "fast",
        usage: "/fast",
        help: "Use the fast service tier",
        autocomplete: "Use fast service tier",
        cli_description: "Use the fast service tier",
        cli: true,
        tui: true,
    },
    BuiltinSlashCommand {
        kind: BuiltinSlashKind::Tools,
        name: "tools",
        usage: "/tools",
        help: "List available tools",
        autocomplete: "List tools available to the model",
        cli_description: "List available tools",
        cli: true,
        tui: true,
    },
    BuiltinSlashCommand {
        kind: BuiltinSlashKind::Mcp,
        name: "mcp",
        usage: "/mcp <help|list|inspect|add|remove|enable|disable|approve|revoke|reload>",
        help: "Inspect or change live MCP servers for this session",
        autocomplete: "Manage live session MCP servers",
        cli_description: "Manage live session MCP servers",
        cli: true,
        tui: true,
    },
    BuiltinSlashCommand {
        kind: BuiltinSlashKind::Schema,
        name: "schema",
        usage: "/schema <json-or-path>",
        help: "Show or set the output schema",
        autocomplete: "Set output schema",
        cli_description: "Show or set the output schema",
        cli: true,
        tui: false,
    },
    BuiltinSlashCommand {
        kind: BuiltinSlashKind::Session,
        name: "session",
        usage: "/session",
        help: "Show session ID, name, turn count, and token totals",
        autocomplete: "Show session state",
        cli_description: "Show session ID, name, turn count, and token totals",
        cli: true,
        tui: false,
    },
    BuiltinSlashCommand {
        kind: BuiltinSlashKind::Name,
        name: "name",
        usage: "/name <text>",
        help: "Set the session's human-readable name",
        autocomplete: "Set session name",
        cli_description: "Set the session's human-readable name",
        cli: true,
        tui: false,
    },
    BuiltinSlashCommand {
        kind: BuiltinSlashKind::Variables,
        name: "variables",
        usage: "/variables",
        help: "List active session variables",
        autocomplete: "List session variables",
        cli_description: "List active session variables",
        cli: true,
        tui: false,
    },
];

/// Iterate shared built-ins available on `surface` in display order.
pub fn builtin_slash_commands(
    surface: SlashSurface,
) -> impl Iterator<Item = &'static BuiltinSlashCommand> {
    BUILTIN_SLASH_COMMANDS
        .iter()
        .filter(move |command| command.supports(surface))
}

/// Find a built-in slash command by name for `surface`.
#[must_use]
pub fn find_builtin_slash_command(
    surface: SlashSurface,
    name: &str,
) -> Option<&'static BuiltinSlashCommand> {
    builtin_slash_commands(surface).find(|command| command.name == name)
}

/// Closure shape used by [`SlashCommandHandler::Custom`].
///
/// Receives the slash command's argument string (everything after the first
/// whitespace, never including the leading `/<command>` token) and returns
/// the messages to splice into the conversation. The closure is held behind
/// an `Arc` so the registry stays cheap to clone across loop iterations.
pub type CustomSlashHandler = Arc<dyn Fn(&str) -> Result<Vec<Message>, NornError> + Send + Sync>;

/// How a slash command expands when matched.
#[derive(Clone)]
pub enum SlashCommandHandler {
    /// Expand to a user-role message asking the agent to invoke the `skill`
    /// tool with the given name. The slash command's argument string is
    /// included verbatim so the agent can route it to the skill.
    Skill {
        /// Name of the skill to activate.
        skill_name: String,
    },
    /// Expand to a single assistant-role tool call against the named tool
    /// with the given arguments.
    Tool {
        /// Tool to invoke.
        tool_name: String,
        /// Arguments serialized to JSON for the tool call.
        args: serde_json::Value,
    },
    /// Yield control to a caller-supplied closure.
    Custom {
        /// The closure that builds the expansion.
        handler: CustomSlashHandler,
    },
}

impl std::fmt::Debug for SlashCommandHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Skill { skill_name } => f
                .debug_struct("Skill")
                .field("skill_name", skill_name)
                .finish(),
            Self::Tool { tool_name, args } => f
                .debug_struct("Tool")
                .field("tool_name", tool_name)
                .field("args", args)
                .finish(),
            Self::Custom { .. } => f.debug_struct("Custom").finish_non_exhaustive(),
        }
    }
}

/// A registered slash command.
#[derive(Clone, Debug)]
pub struct SlashCommand {
    /// Command name without the leading `/`.
    pub name: String,
    /// Handler invoked when the command matches.
    pub handler: SlashCommandHandler,
}

/// Map of command name → registered command. Lookup is case-sensitive.
#[derive(Clone, Default, Debug)]
pub struct SlashCommandRegistry {
    commands: HashMap<String, SlashCommand>,
}

impl SlashCommandRegistry {
    /// Construct an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a command, replacing any prior entry with the same name.
    pub fn register(&mut self, command: SlashCommand) {
        self.commands.insert(command.name.clone(), command);
    }

    /// Look up a command by name without the leading `/`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&SlashCommand> {
        self.commands.get(name)
    }

    /// Number of registered commands.
    #[must_use]
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    /// True when no commands are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    /// Iterator over registered command names (without leading `/`).
    ///
    /// Ordering is unspecified — backed by [`HashMap::keys`]. Callers that
    /// need a stable ordering should collect and sort. The accessor is the
    /// minimal surface needed by external consumers (CLI tab completion,
    /// `/help` enumeration, registry-merge logic) to inspect the registry
    /// without holding a reference to the underlying map.
    pub fn names(&self) -> impl Iterator<Item = &str> + '_ {
        self.commands.keys().map(String::as_str)
    }

    /// Register `/compact`, `/help`, and `/status` as no-op custom commands
    /// that emit a single user-role message describing what would happen.
    /// These exist so the loop has a working set of built-ins even when the
    /// orchestrator does not register richer handlers.
    pub fn register_builtins(&mut self) {
        self.register(SlashCommand {
            name: "compact".to_owned(),
            handler: SlashCommandHandler::Custom {
                handler: Arc::new(|_arg| {
                    Ok(vec![Message {
                        response_items: Vec::new(),
                        role: MessageRole::User,
                        content: Some(
                            "[/compact] Summarize the session so far and discard the prior \
                             tool transcripts. Continue from the summary."
                                .to_owned(),
                        ),
                        thinking: String::new(),
                        reasoning: Vec::new(),
                        tool_calls: Vec::new(),
                        tool_call_id: None,
                        tool_name: None,
                        tool_call_kind: None,
                        tool_call_caller: crate::provider::request::ToolCallCaller::Absent,
                    }])
                }),
            },
        });
        self.register(SlashCommand {
            name: "help".to_owned(),
            handler: SlashCommandHandler::Custom {
                handler: Arc::new(|_arg| {
                    Ok(vec![Message {
                        response_items: Vec::new(),
                        role: MessageRole::User,
                        content: Some(
                            "[/help] Built-in commands: /compact, /help, /status. Custom \
                             commands are registered per profile."
                                .to_owned(),
                        ),
                        thinking: String::new(),
                        reasoning: Vec::new(),
                        tool_calls: Vec::new(),
                        tool_call_id: None,
                        tool_name: None,
                        tool_call_kind: None,
                        tool_call_caller: crate::provider::request::ToolCallCaller::Absent,
                    }])
                }),
            },
        });
        self.register(SlashCommand {
            name: "status".to_owned(),
            handler: SlashCommandHandler::Custom {
                handler: Arc::new(|_arg| {
                    Ok(vec![Message {
                        response_items: Vec::new(),
                        role: MessageRole::User,
                        content: Some(
                            "[/status] Report current task progress: what you have done so \
                             far, what is in flight, and what is blocked."
                                .to_owned(),
                        ),
                        thinking: String::new(),
                        reasoning: Vec::new(),
                        tool_calls: Vec::new(),
                        tool_call_id: None,
                        tool_name: None,
                        tool_call_kind: None,
                        tool_call_caller: crate::provider::request::ToolCallCaller::Absent,
                    }])
                }),
            },
        });
    }
}

/// Result of pre-processing user input.
#[derive(Clone, Debug)]
pub enum PreprocessResult {
    /// Input was not a slash command (or referenced an unknown one) — pass
    /// through unchanged.
    Passthrough(String),
    /// Input was a recognized slash command — splice these messages into the
    /// conversation in place of the literal `/command …` user message.
    Expanded {
        /// Messages produced by the handler.
        messages: Vec<Message>,
    },
}

/// Strip the leading `/` and split into `(name, argument)` where argument is
/// the verbatim remainder of the input. Returns `None` when the input is not
/// a slash command or is empty after the slash.
fn split_command(input: &str) -> Option<(&str, &str)> {
    let after_slash = input.strip_prefix('/')?;
    let trimmed = after_slash.trim_start();
    if trimmed.is_empty() {
        return None;
    }
    let (name, rest) = match trimmed.find(char::is_whitespace) {
        Some(idx) => (&trimmed[..idx], trimmed[idx..].trim_start()),
        None => (trimmed, ""),
    };
    Some((name, rest))
}

/// Pre-process `input` through `registry`.
///
/// Returns [`PreprocessResult::Expanded`] when the input is a recognized
/// slash command and the handler succeeds. Returns
/// [`PreprocessResult::Passthrough`] for non-slash inputs, empty `/`, or
/// unknown commands so the model can deal with them directly.
///
/// # Errors
///
/// Returns the error produced by a [`SlashCommandHandler::Custom`] closure
/// or a JSON serialization failure when expanding a
/// [`SlashCommandHandler::Tool`] handler.
pub fn preprocess_input(
    input: &str,
    registry: &SlashCommandRegistry,
) -> Result<PreprocessResult, NornError> {
    let Some((name, arg)) = split_command(input) else {
        return Ok(PreprocessResult::Passthrough(input.to_owned()));
    };

    let Some(command) = registry.get(name) else {
        return Ok(PreprocessResult::Passthrough(input.to_owned()));
    };

    let messages = match &command.handler {
        SlashCommandHandler::Skill { skill_name } => {
            let body = if arg.is_empty() {
                format!(
                    "[/{name}] Activate the '{skill_name}' skill by calling the `skill` \
                     tool with name=\"{skill_name}\"."
                )
            } else {
                format!(
                    "[/{name} {arg}] Activate the '{skill_name}' skill by calling the \
                     `skill` tool with name=\"{skill_name}\". Argument: {arg}"
                )
            };
            vec![Message {
                response_items: Vec::new(),
                role: MessageRole::User,
                content: Some(body),
                thinking: String::new(),
                reasoning: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                tool_name: None,
                tool_call_kind: None,
                tool_call_caller: crate::provider::request::ToolCallCaller::Absent,
            }]
        }
        SlashCommandHandler::Tool { tool_name, args } => {
            let arguments = serde_json::to_string(args).map_err(|e| {
                NornError::Config(crate::error::ConfigError::InvalidConfig {
                    reason: format!("slash command '/{name}' has non-serializable args: {e}"),
                })
            })?;
            vec![Message {
                response_items: Vec::new(),
                role: MessageRole::Assistant,
                content: None,
                thinking: String::new(),
                reasoning: Vec::new(),
                tool_calls: vec![AssistantToolCall {
                    call_id: format!("slash-{name}"),
                    name: tool_name.clone(),
                    arguments,
                    kind: crate::provider::request::ToolCallKind::Function,
                    caller: crate::provider::request::ToolCallCaller::Absent,
                }],
                tool_call_id: None,
                tool_name: None,
                tool_call_kind: None,
                tool_call_caller: crate::provider::request::ToolCallCaller::Absent,
            }]
        }
        SlashCommandHandler::Custom { handler } => handler(arg)?,
    };

    Ok(PreprocessResult::Expanded { messages })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_const_for_fn,
    clippy::needless_pass_by_value
)]
mod tests {
    use super::*;

    fn registry_with_review() -> SlashCommandRegistry {
        let mut reg = SlashCommandRegistry::new();
        reg.register(SlashCommand {
            name: "review".to_owned(),
            handler: SlashCommandHandler::Skill {
                skill_name: "review".to_owned(),
            },
        });
        reg
    }

    #[test]
    fn non_slash_input_passes_through() {
        let reg = registry_with_review();
        let out = preprocess_input("hello world", &reg).unwrap();
        match out {
            PreprocessResult::Passthrough(s) => assert_eq!(s, "hello world"),
            PreprocessResult::Expanded { .. } => {
                panic!("non-slash input must pass through unchanged")
            }
        }
    }

    #[test]
    fn unknown_slash_passes_through() {
        let reg = registry_with_review();
        let out = preprocess_input("/no-such-command foo", &reg).unwrap();
        match out {
            PreprocessResult::Passthrough(s) => assert_eq!(s, "/no-such-command foo"),
            PreprocessResult::Expanded { .. } => {
                panic!("unknown slash command must pass through unchanged")
            }
        }
    }

    #[test]
    fn empty_slash_passes_through() {
        let reg = registry_with_review();
        let out = preprocess_input("/   ", &reg).unwrap();
        assert!(matches!(out, PreprocessResult::Passthrough(_)));
    }

    #[test]
    fn slash_skill_expands_with_arg() {
        let reg = registry_with_review();
        let out = preprocess_input("/review foo.rs", &reg).unwrap();
        match out {
            PreprocessResult::Expanded { messages } => {
                assert_eq!(messages.len(), 1, "skill expansion produces one message");
                let body = messages[0]
                    .content
                    .as_ref()
                    .expect("skill message has content");
                assert!(
                    body.contains("review"),
                    "body must mention skill name: {body}"
                );
                assert!(
                    body.contains("foo.rs"),
                    "body must mention argument: {body}"
                );
            }
            PreprocessResult::Passthrough(_) => panic!("expected expansion"),
        }
    }

    #[test]
    fn slash_skill_without_arg_still_expands() {
        let reg = registry_with_review();
        let out = preprocess_input("/review", &reg).unwrap();
        match out {
            PreprocessResult::Expanded { messages } => {
                let body = messages[0].content.as_ref().unwrap();
                assert!(body.contains("review"));
            }
            PreprocessResult::Passthrough(_) => panic!("expected expansion"),
        }
    }

    #[test]
    fn slash_tool_expands_as_tool_call() {
        let mut reg = SlashCommandRegistry::new();
        reg.register(SlashCommand {
            name: "noop".to_owned(),
            handler: SlashCommandHandler::Tool {
                tool_name: "noop_tool".to_owned(),
                args: serde_json::json!({"k": 1}),
            },
        });
        let out = preprocess_input("/noop", &reg).unwrap();
        match out {
            PreprocessResult::Expanded { messages } => {
                assert_eq!(messages.len(), 1);
                assert_eq!(messages[0].role, MessageRole::Assistant);
                assert_eq!(messages[0].tool_calls.len(), 1);
                assert_eq!(messages[0].tool_calls[0].name, "noop_tool");
                let parsed: serde_json::Value =
                    serde_json::from_str(&messages[0].tool_calls[0].arguments).unwrap();
                assert_eq!(parsed, serde_json::json!({"k": 1}));
            }
            PreprocessResult::Passthrough(_) => panic!("expected expansion"),
        }
    }

    #[test]
    fn slash_custom_receives_argument() {
        let mut reg = SlashCommandRegistry::new();
        reg.register(SlashCommand {
            name: "echo".to_owned(),
            handler: SlashCommandHandler::Custom {
                handler: Arc::new(|arg| {
                    Ok(vec![Message {
                        response_items: Vec::new(),
                        role: MessageRole::User,
                        content: Some(format!("custom:{arg}")),
                        thinking: String::new(),
                        reasoning: Vec::new(),
                        tool_calls: Vec::new(),
                        tool_call_id: None,
                        tool_name: None,
                        tool_call_kind: None,
                        tool_call_caller: crate::provider::request::ToolCallCaller::Absent,
                    }])
                }),
            },
        });
        let out = preprocess_input("/echo hello there", &reg).unwrap();
        match out {
            PreprocessResult::Expanded { messages } => {
                assert_eq!(messages[0].content.as_deref(), Some("custom:hello there"));
            }
            PreprocessResult::Passthrough(_) => panic!("expected expansion"),
        }
    }

    #[test]
    fn reasoning_effort_support_uses_model_catalog() {
        assert!(reasoning_effort_supported_for_model(
            "gpt-5.5",
            ReasoningEffort::High,
        ));
        assert!(reasoning_effort_supported_for_model(
            "gpt-5.5",
            ReasoningEffort::XHigh,
        ));
        assert!(!reasoning_effort_supported_for_model(
            "gpt-5.5",
            ReasoningEffort::Max,
        ));
        for model in ["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"] {
            assert!(
                reasoning_effort_supported_for_model(model, ReasoningEffort::Max),
                "{model} must support max reasoning effort",
            );
        }
        assert!(!reasoning_effort_supported_for_model(
            "unknown-local-model",
            ReasoningEffort::High,
        ));
    }

    #[test]
    fn effort_command_uses_canonical_xhigh_and_accepts_max() {
        assert_eq!(
            parse_effort_command("xhigh"),
            Some(EffortCommand::Set(ReasoningEffort::XHigh)),
        );
        assert_eq!(
            parse_effort_command("max"),
            Some(EffortCommand::Set(ReasoningEffort::Max)),
        );
        assert_eq!(parse_effort_command("x-high"), None);
        assert_eq!(parse_effort_command("ultra"), None);
    }

    #[test]
    fn unsupported_reasoning_effort_message_names_model_and_effort() {
        let message = unsupported_reasoning_effort_message("local", "high");
        assert!(message.contains("local"));
        assert!(message.contains("high"));
    }

    #[test]
    fn builtins_register_all_three() {
        let mut reg = SlashCommandRegistry::new();
        reg.register_builtins();
        assert!(reg.get("compact").is_some());
        assert!(reg.get("help").is_some());
        assert!(reg.get("status").is_some());
        let out = preprocess_input("/help", &reg).unwrap();
        assert!(matches!(out, PreprocessResult::Expanded { .. }));
    }

    #[test]
    fn names_iterates_all_registered_commands() {
        let mut reg = SlashCommandRegistry::new();
        reg.register_builtins();
        let mut names: Vec<&str> = reg.names().collect();
        names.sort_unstable();
        assert_eq!(names, vec!["compact", "help", "status"]);
    }

    #[test]
    fn names_is_empty_for_fresh_registry() {
        let reg = SlashCommandRegistry::new();
        assert_eq!(reg.names().count(), 0);
    }
}
