Use when work can be delegated to an independent agent that does not need the parent's conversation history. The sub-agent starts with a clean EventStore — it sees only the task string, not prior turns. Provide a clear, self-contained task description since the sub-agent has no other context.

Spawn is asynchronous: it returns immediately with the agent_id and registry path while the child runs in the background. Continue with your own work after spawning — do not block. When the child completes, its result is delivered back to you automatically. To check whether a child is still running, use the agents tool.

Pass a bare profile name (e.g. "developer", "code-reviewer") in the profile parameter to resolve a markdown profile from $WORKSPACE/.norn/profiles, $WORKSPACE/.meridian/profiles, or ~/.norn/profiles. The profile supplies the child's system instructions, tool allow-list, and reasoning config. Omit profile for a minimal default whose system instruction is built from the task itself.

Use the tools parameter to restrict which tools the sub-agent may call; it takes precedence over the profile's tool list. Omit it to inherit the profile's tools, or the full parent registry when no profile is given.

To parallelise work, spawn several children for independent subtasks. Results are delivered automatically when each child completes.

Delegation is budgeted, not flat: every agent carries a granted policy with a delegation budget (remaining_depth, max_concurrent_children). You can spawn only while your own remaining_depth is at least 1, and a child you create always receives strictly less depth than you hold — by default your own policy with remaining_depth reduced by one. remaining_depth 0 means the child is a leaf and cannot delegate at all. A spawn that exceeds your budget (depth exhausted, or too many concurrently live children) fails with a typed error naming the budget.

Use the optional child_policy parameter to narrow a child's grant below the default: a smaller delegation budget, a tighter messaging scope, or a smaller inbound channel. Narrowing only — any field wider than your own grant is refused. Omit child_policy to grant your own policy with depth decremented.

Each child's results are delivered to its own parent, one hop at a time: your children report to you; their children report to them, never to you directly. Use the agents tool to see your whole descendant subtree.

Children run with default loop limits: a child does not inherit your max_iterations, step_timeout, or linger configuration. Budget the task accordingly — a long-running child is bounded by the defaults, not by your own limits. Because children cannot linger, a child that finishes its own run before its children do loses their results (the late results are error-logged and the registry stays truthful, but nothing delivers them) — when you delegate from a delegated position, keep working until your children's results arrive before producing your final answer.

If you genuinely need a blocking sub-agent that shares the current conversation context, prefer fork over spawn_agent.

The path parameter is a hierarchical registry path (e.g. "/research/phase-1"), not a file path — it controls where the sub-agent appears in the agent tree. Omit path to auto-generate one nested under your own registry path ("{your_path}/spawn/{uuid}"), so the agents tree reads as a real tree at any depth.

Use the output_schema parameter to require structured output: pass a JSON Schema object and the sub-agent's final answer must validate against it. The schema is an explicit per-spawn decision — a sub-agent never inherits your own output schema implicitly. Omit output_schema for free-form output.
