Footnote [1]: Envelopes for all text inputs

It may be worth experimenting with wrapping every text input in some kind of envelope, not just fork or sub-agent outputs. The point would be to help the model distinguish between different lines of communication: direct session messages from Tom, Meridian DMs, sub-agent updates, fork responses, harness requests, and so on.

This matters because agents get conditioned by the channel they have been using. For example, if we are working through Meridian and the server stops compiling, I may reopen the same agent directly to fix it. That agent may still try to reply via Meridian DM because that is what it has been doing, even though the message is now coming directly into the session. An envelope could make the difference explicit: “direct session message from Tom; respond normally,” versus “Meridian DM; reply via DM.”

This might eventually look a bit like a dashboard: current direct message, pending inbox items, snoozed messages, fork updates, and so on. I do not know whether global envelopes are worth doing, but I think they are definitely worth exploring for forks and sub-agents.

Footnote [2]: Applications for sub-agents and teams

If we use the same mechanism for multiple sub-agents or agent teams, we may eventually want event types like broadcast. That may be too much power to put directly in agents’ hands, but it could be useful in some cases. For example, an agent might need to tell the rest of the team to pause briefly while it fixes a branch or workspace issue.

The core event types I keep coming back to are:

- `update`: normal progress update.
- `request_help` / `emergency`: the agent is blocked and needs runtime intervention.
- `game_plan` / `approach`: a short tactical plan before acting.
- `harness_issue` / `feedback`: the agent reports a problem with tools, hooks, permissions, or the runtime.

Together with structured final output, this would be extremely useful. We get relevant updates while work is happening, then a clean structured handoff at the end. If needed, we can still go back through the full logs down to the raw session events.

Footnote [3]: Context construction and temporal relevance

I want us to capture the idea of marking inputs, outputs, and events with context-construction metadata. We keep absolutely everything in the session store, but not everything should remain in active context forever. Events can be tagged with their lifecycle, plan, fork, requirement, relevance window, and whether they should be kept, dropped later, or retained at all costs.

For example, search results and routine shell output are often useful briefly, then become noise. Some should be dropped from future context construction once the agent has moved past that phase. Other things, like design decisions or critical evidence, should be kept. Eventually we could have a trailing marker agent that periodically reviews the session and grades events: keep, can drop, important, critical, irrelevant. It could identify what actually mattered and mark it for future context construction.

That could significantly extend the useful lifetime of long-running agents while preserving full auditability. It also helps with historical replay. If events have temporal relevance markers, we can reconstruct what context the agent had at a particular point in time, which is useful for auditability, evaluation, debugging, and historical forks.

Footnote [4]: Structured output schema generation

The structured output mechanism in the OpenAI Responses API, and I think Anthropic has something similar, effectively gives us a schema-enforced output channel. It is not really a normal tool call because it does not execute a function. It constrains the model to produce output matching a schema.

If that schema must be fixed at request time, fine: that is the constraint. But if there is any way to construct or modify the schema later in the response, that would open useful options.

For example, at the end of a task we often ask the agent, “Which files did you edit?” That works, but the harness already knows the answer. If the harness can provide the edited file list, the final structured output schema could ask the agent to comment on each actual edited file. We could distinguish major edits from minor edits or group trivial changes, but the key is that the schema would be based on what actually happened during the session.

That would let us ask for structured commentary on concrete session artifacts that we could not reasonably predict at request time. It would be extremely useful to have as an option.
