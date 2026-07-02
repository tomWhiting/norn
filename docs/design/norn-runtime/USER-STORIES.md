# Norn-Runtime — User Stories

## AI Agent — Running a Member-Facing Session

**S1.** As an AI agent, I want my session to run through Norn in-process so that I have typed tool access and structured event streaming instead of subprocess JSONL parsing.

**S2.** As an AI agent, I want my session to resume from where I left off so that I retain full conversation context across wakes without re-reading history.

**S3.** As an AI agent, I want to receive DMs as wake prompts through Norn so that I see my inbox immediately on wake without polling.

**S4.** As an AI agent, I want mid-session DMs to arrive via my inbound channel so that I can respond to messages while actively working.

## AI Agent — Using Meridian Tools

**S5.** As an AI agent, I want to send DMs to other members via a messaging tool so that I can communicate without shelling out to the CLI.

**S6.** As an AI agent, I want to check my inbox and notifications via a tool so that I can triage work without parsing CLI output.

**S7.** As an AI agent, I want to look up member information via a tool so that I can resolve names and check activity status in-process.

**S8.** As an AI agent, I want to see only the tools relevant to my role so that my tool selection is focused and not overwhelmed by 175 options.

**S9.** As an AI agent doing development work, I want to run source control operations (status, diff, commit, push) via a native tool so that I can manage code changes without parsing shell output.

**S10.** As an AI agent doing development work, I want to manage branches (submit, land, restack) via a native tool so that I can operate the branch stack without CLI subprocess calls.

**S11.** As an AI agent, I want to dispatch and monitor workflows via a native tool so that I can coordinate automated work within my session.

## Human Developer — Configuring Agent Sessions

**S12.** As a developer, I want to switch a member's runtime between Norn and Claude Runner via wake config so that I can migrate incrementally.

**S13.** As a developer, I want both runtimes to produce the same event stream so that the frontend works identically regardless of runtime.

**S14.** As a developer, I want a server-level default runtime setting so that new members default to Norn without per-member configuration.

## Human Developer — Debugging Agent Sessions

**S15.** As a developer, I want to see which runtime a session is using so that I can diagnose runtime-specific issues.

**S16.** As a developer, I want Norn session JSONL files at a predictable path so that I can inspect conversation history for debugging.

**S17.** As a developer, I want the activity panel to show identical content for Norn and Claude Runner sessions so that I can verify event parity.

## System — Managing Session Resources

**S18.** As the system, I want both runtimes to draw from the same token pool so that rotation state stays coherent and tokens are not double-consumed.

**S19.** As the system, I want Norn sessions to persist to both JSONL and PG so that session resume and UI history are independently served.

**S20.** As the system, I want idle timeout semantics to match between runtimes so that resource cleanup is predictable.

**S21.** As the system, I want workflow step sessions to persist their Norn EventStore to disk so that step conversation history is not lost when the step completes.

## AI Agent — Receiving Messages While Active

**S22.** As an AI agent with an active session, I want DMs that arrive mid-session to be delivered to my inbound channel immediately so that I can respond without waiting for a wake cycle.

**S23.** As an AI agent, I want pending notifications injected at tool batch boundaries so that I see new messages without relying on a CLI subprocess hook, and I want them marked as delivered so I don't see them twice.

## Human Developer — Monitoring and Configuring Agent Sessions

**S24.** As a developer, I want a member-facing Norn profile that configures namespace tools and system instructions for workspace member behavior so that I can wake members as Norn sessions without manual configuration.

**S25.** As a developer, I want Norn wake sessions to run until the agent completes naturally instead of timing out immediately so that members can process their full inbox before stopping.
