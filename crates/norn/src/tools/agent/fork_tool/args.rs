use serde::Deserialize;

use crate::agent::child_policy::ChildPolicy;
use crate::agent::fork::ForkRequirement;

// A misspelled policy key must fail rather than silently widening a fork's
// inherited grant.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ForkArgs {
    pub(super) request: String,
    pub(super) model: String,
    pub(super) requirements: Vec<ForkRequirement>,
    /// Optional complete replacement for the inherited child policy.
    #[serde(default)]
    pub(super) child_policy: Option<ChildPolicy>,
}

pub(super) fn input_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["request", "model", "requirements"],
        "additionalProperties": false,
        "properties": {
            "request": {
                "type": "string",
                "description": "What you need the forked agent to do. The fork inherits the full conversation context from the parent session."
            },
            "model": {
                "type": "string",
                "minLength": 1,
                "description": "Model identifier for the forked agent. Use a model supported by the current provider/backend."
            },
            "requirements": {
                "type": "array",
                "description": "Requirements the fork must satisfy. When provided, the fork's structured output includes a completion record for each requirement with name, completed (bool), and completion_notes.",
                "items": {
                    "type": "object",
                    "required": ["name", "description"],
                    "additionalProperties": false,
                    "properties": {
                        "name": { "type": "string", "description": "Identifier for this requirement." },
                        "description": { "type": "string", "description": "What must be done to satisfy this requirement." }
                    }
                }
            },
            "child_policy": {
                "type": "object",
                "required": ["messaging", "delegation", "inbound_capacity"],
                "additionalProperties": false,
                "description": "Optional narrowed policy for this fork. Omit to grant your own policy with delegation depth reduced by one level. Every field except loop_config must be within your own granted budget — widening fails. Supplying child_policy is a complete replacement: without loop_config it clears any inherited loop overrides — restate them to keep them.",
                "properties": {
                    "messaging": {
                        "type": "string",
                        "enum": ["siblings_and_parent", "parent_only", "none"],
                        "description": "Who the fork may message; must not widen your own scope."
                    },
                    "delegation": {
                        "type": "object",
                        "required": ["remaining_depth", "max_concurrent_children"],
                        "additionalProperties": false,
                        "properties": {
                            "remaining_depth": {
                                "type": "integer",
                                "minimum": 0,
                                "description": "Levels of descendants the fork may create below itself (0 = leaf). Must be at most your own remaining_depth - 1."
                            },
                            "max_concurrent_children": {
                                "type": "integer",
                                "minimum": 0,
                                "description": "Max non-terminal direct children the fork may have at once. Must be at most your own cap."
                            }
                        }
                    },
                    "inbound_capacity": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Bounded capacity of the fork's inbound message channel. Must be at most your own granted capacity."
                    },
                    "loop_config": {
                        "type": "object",
                        "additionalProperties": false,
                        "description": "Optional loop-shaping overrides for the fork. Not a narrowing axis: any value is accepted regardless of your own loop config. Each field is optional; an unset field keeps the library default (today's behavior). Omit entirely to run the fork on default loop limits — and note that supplying child_policy without this key clears any loop overrides the fork would have inherited; restate them to keep them.",
                        "properties": {
                            "step_timeout_secs": {
                                "type": "integer",
                                "minimum": 0,
                                "description": "Wall-clock cap in seconds on each of the fork's steps. Unset = uncapped."
                            },
                            "linger_secs": {
                                "type": "integer",
                                "minimum": 0,
                                "description": "Linger deadline in seconds: the fork waits this long at each would-stop boundary for late messages and its own children's results before stopping. Grant this to a fork that delegates, so its children's late results are delivered instead of lost. Unset = the fork returns the moment its model stops."
                            },
                            "context_window": {
                                "type": "integer",
                                "minimum": 1,
                                "description": "Explicit context window for the fork, in tokens. Unset = filled from the model catalog for the fork's model. A value above a catalogued model's maximum is rejected; required for a deliberately uncatalogued model."
                            }
                        }
                    }
                }
            }
        }
    })
}
