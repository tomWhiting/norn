Fork splits your session at the current point in time. The fork inherits your complete conversation history and all available tools — it knows everything you know. It runs asynchronously and returns a structured result automatically when it completes.

Provide a requirements array to define what the fork must deliver. Each requirement gets a completion record in the structured output.

Do not fork for work that is independent of your conversation context — use spawn_agent instead.

Forks are children with a budgeted delegation grant, exactly like spawned children: a fork may itself spawn or fork only while its granted remaining_depth is at least 1, and it always receives strictly less depth than you hold — by default your own policy with remaining_depth reduced by one. A fork over budget (depth exhausted, or too many concurrently live children) fails with a typed error naming the budget. Use the optional child_policy parameter to narrow the fork's grant below the default; widening any field beyond your own grant is refused.

A fork's children report their results to the fork, one hop at a time — never to you directly; the fork's own result reaches you when it completes.

Forks run with default loop limits: a fork does not inherit your max_iterations, step_timeout, or linger configuration, even though it inherits your conversation. Budget the work accordingly. Because forks cannot linger, a fork that finishes before its own children do loses their results (error-logged, never silent) — a fork that delegates should keep working until its children's results arrive before producing its structured output.
