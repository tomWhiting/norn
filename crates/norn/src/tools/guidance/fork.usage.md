Fork splits your session at the current point in time. The fork inherits your complete conversation history and all available tools — it knows everything you know. It runs asynchronously and returns a structured result automatically when it completes.

Provide a requirements array to define what the fork must deliver. Each requirement gets a completion record in the structured output.

Do not fork for work that is independent of your conversation context — use spawn_agent instead.

Forks are children and children cannot create their own children (no spawn_agent or fork from inside a fork). Plan delegation one layer at a time.

Forks run with default loop limits: a fork does not inherit your max_iterations, step_timeout, or linger configuration, even though it inherits your conversation. Budget the work accordingly.
