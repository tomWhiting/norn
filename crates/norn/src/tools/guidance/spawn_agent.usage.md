Use when work can be delegated to an independent agent that does not need the parent's conversation history. The sub-agent starts with a clean EventStore — it sees only the task string, not prior turns. Provide a clear, self-contained task description since the sub-agent has no other context.

Spawn is asynchronous: it returns immediately with the agent_id and registry path while the child runs in the background. Continue with your own work after spawning — do not block. When the child completes, its result is delivered back to you automatically.

Pass a bare profile name (e.g. "developer", "code-reviewer") in the profile parameter to resolve a markdown profile from $WORKSPACE/.norn/profiles, $WORKSPACE/.meridian/profiles, or ~/.norn/profiles. The profile supplies the child's system instructions, tool allow-list, and reasoning config. Omit profile for a minimal default whose system instruction is built from the task itself.

Use the tools parameter to restrict which tools the sub-agent may call; it takes precedence over the profile's tool list. Omit it to inherit the profile's tools, or the full parent registry when no profile is given.

To parallelise work, spawn several children for independent subtasks. Results are delivered automatically when each child completes.

Delegation is one layer deep: children — whether spawned or forked — cannot create their own children, so give each child a task it can finish alone and plan any further delegation yourself, one layer at a time.

If you genuinely need a blocking sub-agent that shares the current conversation context, prefer fork over spawn_agent.

The path parameter is a hierarchical registry path (e.g. "/research/phase-1"), not a file path — it controls where the sub-agent appears in the agent tree. Omit path to auto-generate one under /spawn/.

Use the output_schema parameter to require structured output: pass a JSON Schema object and the sub-agent's final answer must validate against it. The schema is an explicit per-spawn decision — a sub-agent never inherits your own output schema implicitly. Omit output_schema for free-form output.
