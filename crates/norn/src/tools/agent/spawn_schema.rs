//! JSON Schema for the `spawn_agent` tool input.

use serde_json::{Value, json};

pub(super) fn input_schema() -> Value {
    json!({
        "type": "object",
        "required": ["task"],
        "additionalProperties": false,
        "properties": {
            "task": {
                "type": "string",
                "description": "Self-contained task description for the sub-agent. The sub-agent sees only this string — it has no access to the parent's conversation history. Required, and not sufficient alone: every call must ALSO pass either variant or role (task-only calls fail)."
            },
            "variant": {
                "type": "string",
                "description": "Optional named agent variant (built-in: \"explorer\", \"reviewer\", \"implementer\"; plus any configured variants.<name> settings). Supplies the child's prompt block, tool subset, default model, and reasoning effort. Mutually exclusive with profile. An unknown name fails with the list of available variants."
            },
            "model": {
                "type": "string",
                "description": "Optional model identifier for the sub-agent (e.g. \"gpt-5.5\"). Omit to use the variant's model, or — when the variant sets none and does not require one, or no variant is given — your own model. The reviewer variant requires a model (variants.reviewer.model or this argument)."
            },
            "role": {
                "type": "string",
                "description": "Optional role label recorded in the agent registry for observability (e.g. \"researcher\", \"code-reviewer\"). Omit to use the variant name; with neither variant nor role the spawn fails."
            },
            "profile": {
                "type": "string",
                "description": "Optional bare profile name (e.g. \"developer\", \"code-reviewer\") resolved as a markdown profile from $WORKSPACE/.norn/profiles, $WORKSPACE/.meridian/profiles, or ~/.norn/profiles. Supplies source-authorized child instructions (workspace profiles are User; user-level profiles are Developer), a tool allow-list, and reasoning config. Mutually exclusive with variant. Omit for a minimal default."
            },
            "tools": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Optional allow-list of tool names the sub-agent may call. Takes precedence over the variant's or profile's tool list. Omit to inherit the variant's/profile's tools, or the full parent registry when neither is given. Your child's granted policy always intersects the final list (a leaf never sees spawn_agent/fork)."
            },
            "mcp_servers": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Optional MCP server subset for this sub-agent. Omit to inherit the variant or parent view; pass an empty list for no MCP servers. This selects from the parent's connected server pool and does not start a duplicate server process."
            },
            "path": {
                "type": "string",
                "description": "Hierarchical registry path for the sub-agent (e.g. \"/workers/phase-1\"). Not a file path. Omit to auto-generate under your own registry path (\"{your_path}/spawn/{uuid}\")."
            },
            "output_schema": {
                "type": "object",
                "description": "Optional JSON Schema the sub-agent's final output must validate against. The sub-agent never inherits the caller's output schema implicitly — supply one here when the result must be structured. Omit for free-form output."
            },
            "child_policy": {
                "type": "object",
                "required": ["messaging", "delegation", "inbound_capacity"],
                "additionalProperties": false,
                "description": "Optional narrowed policy for this child. Omit to grant your own policy with delegation depth reduced by one level. Every field except loop_config must be within your own granted budget — widening fails. Supplying child_policy is a complete replacement: without loop_config it clears any inherited loop overrides — restate them to keep them.",
                "properties": {
                    "messaging": {
                        "type": "string",
                        "enum": ["siblings_and_parent", "parent_only", "none"],
                        "description": "Who the child may message; must not widen your own scope."
                    },
                    "delegation": {
                        "type": "object",
                        "required": ["remaining_depth", "max_concurrent_children"],
                        "additionalProperties": false,
                        "properties": {
                            "remaining_depth": {
                                "type": "integer",
                                "minimum": 0,
                                "description": "Levels of descendants the child may create below itself (0 = leaf). Must be at most your own remaining_depth - 1."
                            },
                            "max_concurrent_children": {
                                "type": "integer",
                                "minimum": 0,
                                "description": "Max non-terminal direct children the child may have at once. Must be at most your own cap."
                            }
                        }
                    },
                    "inbound_capacity": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Bounded capacity of the child's inbound message channel. Must be at most your own granted capacity."
                    },
                    "loop_config": {
                        "type": "object",
                        "additionalProperties": false,
                        "description": "Optional loop-shaping overrides for the child. Not a narrowing axis: any value is accepted regardless of your own loop config. Each field is optional; an unset field keeps the library default (today's behavior). Omit entirely to run the child on default loop limits — and note that supplying child_policy without this key clears any loop overrides the child would have inherited; restate them to keep them.",
                        "properties": {
                            "step_timeout_secs": {
                                "type": "integer",
                                "minimum": 0,
                                "description": "Wall-clock cap in seconds on each of the child's steps. Unset = uncapped."
                            },
                            "linger_secs": {
                                "type": "integer",
                                "minimum": 0,
                                "description": "Linger deadline in seconds: the child waits this long at each would-stop boundary for late messages and its own children's results before stopping. Grant this to a child that delegates, so its children's late results are delivered instead of lost. Unset = the child returns the moment its model stops."
                            },
                            "context_window": {
                                "type": "integer",
                                "minimum": 1,
                                "description": "Explicit context window for the child, in tokens. Unset = filled from the model catalog for the child's resolved model. A value above a catalogued model's maximum is rejected; required for a deliberately uncatalogued model."
                            }
                        }
                    }
                }
            }
        }
    })
}
