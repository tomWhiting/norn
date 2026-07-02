# Norn-Cli — User Stories

## Developer — Using Norn interactively from a terminal

**S1.** As a developer, I want to type `norn` and get an interactive REPL so that I can have a conversation with an agent without writing code.

**S2.** As a developer, I want to see tool calls and their results streamed in real time so that I can follow what the agent is doing while it works.

**S3.** As a developer, I want to type `/tools` to see what tools the agent has access to so that I know what capabilities are available in this session.

**S4.** As a developer, I want to type `/model gpt-5.5` to switch models mid-session so that I can try a different model without restarting.

**S5.** As a developer, I want multi-line input (Shift+Enter) so that I can write complex prompts without having to escape newlines.

**S6.** As a developer, I want command history persisted across sessions so that I can recall previous prompts with the up arrow.

**S7.** As a developer, I want tab completion for slash commands so that I can discover commands without memorising them.

**S8.** As a developer, I want to press Ctrl+C to interrupt a long-running agent step so that I can cancel work that is going in the wrong direction.

## Developer — Resuming and managing sessions

**S9.** As a developer, I want my conversation to be saved automatically so that I can resume it later without losing context.

**S10.** As a developer, I want to type `norn --resume` to pick up my most recent session so that I can continue where I left off.

**S11.** As a developer, I want to type `norn session list` to see my recent sessions so that I can find a specific conversation to resume.

**S12.** As a developer, I want to fork a session so that I can explore a different direction without losing the original conversation.

**S13.** As a developer, I want to name my sessions (`/name refactor-auth`) so that I can find them later by a meaningful label instead of a UUID.

**S14.** As a developer, I want to export a session as markdown so that I can share it with colleagues or include it in documentation.

**S15.** As a developer, I want to type `/compact` when context is getting large so that the conversation is summarised and I can continue without hitting token limits.

## Script Author — Using Norn in shell pipelines and automation

**S16.** As a script author, I want to pipe data through Norn (`cat data.csv | norn -p 'Summarise this'`) so that I can use an agent as part of a Unix pipeline.

**S17.** As a script author, I want to specify an output schema inline (`-s '{...}'`) so that the agent returns structured JSON I can parse with jq.

**S18.** As a script author, I want `--output-format json` to produce a single JSON envelope so that I can programmatically consume the result including token usage and session metadata.

**S19.** As a script author, I want `--output-format stream-json` to produce NDJSON events so that I can process tool calls and text deltas as they happen in a streaming pipeline.

**S20.** As a script author, I want `-q` (quiet) to suppress all progress output on stderr so that my pipeline sees only the final result on stdout.

**S21.** As a script author, I want exit codes to distinguish success (0), agent error (1), argument error (2), and auth error (3) so that my scripts can handle each case.

**S22.** As a script author, I want Norn to auto-detect piped stdin and switch to print mode without requiring `-p` so that `echo 'prompt' | norn` works intuitively.

**S23.** As a script author, I want to send slash commands in print mode (`norn -p '/compact' 'Now summarise'`) so that I can trigger runtime operations from scripts.

## Agent Operator — Configuring agents for specific tasks

**S24.** As an agent operator, I want to load a profile (`--profile coding`) that bundles model, tools, system prompt, and capabilities so that I can switch between pre-configured agent roles.

**S25.** As an agent operator, I want CLI flags to override profile values (`--model gpt-5.5 --profile coding`) so that I can make one-off adjustments without editing the profile file.

**S26.** As an agent operator, I want to restrict tools via `--allowed-tools bash,read` so that the agent can only use the tools I specify.

**S27.** As an agent operator, I want to block specific tools via `--disallowed-tools write,edit` so that the agent cannot modify files in a read-only analysis session.

**S28.** As an agent operator, I want to set per-event output schemas via `--event-schema spoken_response=tts-schema.json` so that each event type conforms to its own contract.

**S29.** As an agent operator, I want to set session variables (`--variables project=yggdrasil --variables env=staging`) so that the system prompt can reference {{project}} and {{env}} dynamically.

**S30.** As an agent operator, I want to connect MCP extensions (`-e stdio://path/to/server`) so that the agent has access to external tool servers.

**S31.** As an agent operator, I want to use `-c key=value` for runtime config overrides so that I can tune timeout, retry, compaction, and context window settings without editing files.

**S32.** As an agent operator, I want to load rules from a YAML file (`--rules coding-rules.yaml`) so that the agent follows project-specific guidelines.

## Orchestrator Developer — Integrating Norn into automated workflows

**S33.** As an orchestrator developer, I want to run Norn as an MCP server (`norn mcp serve`) so that other tools can call Norn's tools via the MCP protocol.

**S34.** As an orchestrator developer, I want JSON output with usage metadata so that I can track token consumption across workflow steps.

**S35.** As an orchestrator developer, I want to write the final output to a file (`-o result.json`) so that downstream workflow steps can read it without parsing stdout.

**S36.** As an orchestrator developer, I want to use `--no-session` for ephemeral runs so that one-shot workflow steps don't pollute the session index.

**S37.** As an orchestrator developer, I want `--provider claude-runner` to route through Claude Code so that I can use Norn's tool framework with Claude's model.

**S38.** As an orchestrator developer, I want diagnostics included in the JSON output envelope so that I can programmatically inspect schema violations, tool failures, and policy violations.

## New User — Setting up Norn for the first time

**S39.** As a new user, I want `norn auth login` to open a browser for OAuth so that I can authenticate without manually managing API keys.

**S40.** As a new user, I want `norn auth status` to tell me whether I'm logged in and when my token expires so that I know if authentication is working.

**S41.** As a new user, I want `norn doctor` to check my setup (auth, provider connectivity, permissions) so that I can diagnose problems before trying to use the agent.

**S42.** As a new user, I want `norn completion zsh` to generate shell completions so that I can tab-complete commands and flags.

**S43.** As a new user, I want clear error messages with remediation advice when something goes wrong so that I can fix the problem without reading source code.

## Developer — Working with structured output

**S44.** As a developer, I want to specify an output schema from a file (`-s analysis-schema.json`) so that the agent returns data conforming to a pre-defined contract.

**S45.** As a developer, I want to specify an output schema inline (`-s '{"type":"object",...}'`) so that I can define quick schemas without creating a file.

**S46.** As a developer, I want to see schema validation feedback when the model produces invalid output so that I understand why a retry is happening.

**S47.** As a developer, I want to type `/schema` to see the current output schema so that I can verify what contract the agent is working against.

**S48.** As a developer, I want to change the output schema mid-session (`/schema '{...}'`) so that I can refine the contract without restarting.

## Developer — Monitoring agent behaviour

**S49.** As a developer, I want to see token usage after each agent step so that I can monitor consumption and know when I'm approaching context limits.

**S50.** As a developer, I want `/session` to show session info (turn count, cumulative tokens, model) so that I can assess the conversation state.

**S51.** As a developer, I want thinking deltas rendered dimmed so that I can distinguish the model's reasoning from its actual output.

**S52.** As a developer, I want long tool output truncated with head/tail display so that verbose results don't flood my terminal.

**S53.** As a developer, I want auto-compaction to fire when context approaches the limit so that long sessions don't fail silently from token overflow.

## Script Author — Configuring tool validation via profile

**S54.** As a script author, I want to set file-length thresholds in my profile's tool_config section so that agents working under this profile enforce my team's code standards.

**S55.** As a script author, I want to override tool-level thresholds from the CLI with -c write.max_code_lines=N so that I can tune validation for a specific run without changing the profile.

**S56.** As a script author, I want the CLI to wire the DiagnosticCollector into LoopContext automatically so that all tool-level violations are captured without explicit setup.
