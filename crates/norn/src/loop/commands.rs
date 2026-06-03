//! Slash-command preprocessing for the agent loop.
//!
//! User input prefixed with `/` is intercepted before reaching the model and
//! expanded into a deterministic sequence of conversation messages. Three
//! handler kinds cover the common shapes:
//!
//! - [`SlashCommandHandler::Skill`] — emits a user-role message that asks
//!   the agent to call the `skill` tool with a given name and free-form
//!   argument string. The argument is the slash command's remainder
//!   verbatim (everything after the first whitespace), so callers receive
//!   the same text the user typed without further parsing.
//! - [`SlashCommandHandler::Tool`] — emits an assistant-role message that
//!   makes a single tool call against the named tool with caller-supplied
//!   JSON arguments. Useful for built-in shortcuts like `/help` that map to
//!   a deterministic tool invocation.
//! - [`SlashCommandHandler::Custom`] — yields control to a caller-supplied
//!   closure that builds the expansion messages itself. The closure receives
//!   the slash command's argument string (the remainder after the command
//!   name) and returns the full message vec or a [`NornError`].
//!
//! Unknown commands and inputs that do not begin with `/` pass through
//! unchanged so the model can react. Empty or whitespace-only commands also
//! pass through.

use std::collections::HashMap;
use std::sync::Arc;

use crate::error::NornError;
use crate::provider::request::{AssistantToolCall, Message, MessageRole};

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
                        role: MessageRole::User,
                        content: Some(
                            "[/compact] Summarize the session so far and discard the prior \
                             tool transcripts. Continue from the summary."
                                .to_owned(),
                        ),
                        thinking: String::new(),
                        tool_calls: Vec::new(),
                        tool_call_id: None,
                        tool_name: None,
                        tool_call_kind: None,
                    }])
                }),
            },
        });
        self.register(SlashCommand {
            name: "help".to_owned(),
            handler: SlashCommandHandler::Custom {
                handler: Arc::new(|_arg| {
                    Ok(vec![Message {
                        role: MessageRole::User,
                        content: Some(
                            "[/help] Built-in commands: /compact, /help, /status. Custom \
                             commands are registered per profile."
                                .to_owned(),
                        ),
                        thinking: String::new(),
                        tool_calls: Vec::new(),
                        tool_call_id: None,
                        tool_name: None,
                        tool_call_kind: None,
                    }])
                }),
            },
        });
        self.register(SlashCommand {
            name: "status".to_owned(),
            handler: SlashCommandHandler::Custom {
                handler: Arc::new(|_arg| {
                    Ok(vec![Message {
                        role: MessageRole::User,
                        content: Some(
                            "[/status] Report current task progress: what you have done so \
                             far, what is in flight, and what is blocked."
                                .to_owned(),
                        ),
                        thinking: String::new(),
                        tool_calls: Vec::new(),
                        tool_call_id: None,
                        tool_name: None,
                        tool_call_kind: None,
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
                role: MessageRole::User,
                content: Some(body),
                thinking: String::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                tool_name: None,
                tool_call_kind: None,
            }]
        }
        SlashCommandHandler::Tool { tool_name, args } => {
            let arguments = serde_json::to_string(args).map_err(|e| {
                NornError::Config(crate::error::ConfigError::InvalidConfig {
                    reason: format!("slash command '/{name}' has non-serializable args: {e}"),
                })
            })?;
            vec![Message {
                role: MessageRole::Assistant,
                content: None,
                thinking: String::new(),
                tool_calls: vec![AssistantToolCall {
                    call_id: format!("slash-{name}"),
                    name: tool_name.clone(),
                    arguments,
                    kind: crate::provider::request::ToolCallKind::Function,
                }],
                tool_call_id: None,
                tool_name: None,
                tool_call_kind: None,
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
                        role: MessageRole::User,
                        content: Some(format!("custom:{arg}")),
                        thinking: String::new(),
                        tool_calls: Vec::new(),
                        tool_call_id: None,
                        tool_name: None,
                        tool_call_kind: None,
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
