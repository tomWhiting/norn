# Norn-Hooks — User Stories

## Human Developer — Configuring Hooks

**S1.** As a human developer, I want to define pre-tool hooks in .norn/settings.json so that I can validate tool arguments before execution without writing Rust code.

**S2.** As a human developer, I want to define stop hooks so that I can prevent the agent from finishing prematurely when tasks are incomplete.

**S3.** As a human developer, I want to target hooks at specific tools by name pattern so that my validation script only runs for the tools it applies to.

**S4.** As a human developer, I want project-level hooks to combine with user-level hooks so that project-specific validations layer on top of my global hooks.

**S5.** As a human developer, I want local overrides in .norn/settings.local.json so that I can add hooks for my own workflow without affecting the shared project config.

**S6.** As a human developer, I want hook timeouts to be explicit in the config so that I know exactly how long a hook can run before being killed.

## Human Developer — Debugging and Maintaining Hooks

**S7.** As a human developer, I want to see which hook blocked a tool call in the error output so that I can identify which script caused the block and why.

**S8.** As a human developer, I want invalid regex patterns in hook matchers rejected at startup so that I find config errors immediately, not during execution.

**S9.** As a human developer, I want hooks captured at startup so that I can modify settings files without destabilising a running session.

**S10.** As a human developer, I want session event hooks to not block the agent loop so that a slow logging script does not degrade agent performance.

## Shell Script Author — Writing Hook Commands

**S11.** As a shell script author, I want to receive hook context as JSON on stdin so that my script can parse the session ID, tool name, and tool arguments.

**S12.** As a shell script author, I want to block a tool call by exiting with code 2 so that I have a simple protocol for rejecting operations.

**S13.** As a shell script author, I want to modify tool arguments by writing JSON to stdout so that I can rewrite inputs before execution.

**S14.** As a shell script author, I want standard environment variables for the project directory and session ID so that my scripts can locate project files without parsing stdin.

**S15.** As a shell script author, I want a simple exit-code protocol so that I can write basic hooks without complex JSON output parsing.

**S16.** As a shell script author, I want empty stdout on exit 0 treated as proceed so that my simplest hooks are just scripts that check a condition and exit 0 or 2.

## AI Agent — Operating Under Hooks

**S17.** As an AI agent, I want pre-tool hook blocks to include the hook's reason so that I can adjust my approach based on why the tool was rejected.

**S18.** As an AI agent, I want modified tool arguments to flow through pre_validate so that hook modifications are still subject to safety checks.

**S19.** As an AI agent, I want stop hooks to be able to force me to continue working so that external logic can override my stopping decision when tasks remain.

**S20.** As an AI agent, I want hook blocks to surface as clear error messages so that I understand why an operation was prevented and can try an alternative.

## Norn Integrator — Building Programmatic Hooks

**S21.** As a Norn integrator, I want to register trait-based hooks alongside config-driven hooks so that my Rust hook implementations coexist with shell hooks in the same registry.

**S22.** As a Norn integrator, I want the existing 5 hook traits unchanged after module promotion so that my programmatic hooks compile without modification.

**S23.** As a Norn integrator, I want new hook traits for user prompt, stop, sub-agent lifecycle, session lifecycle, and compaction so that I can observe and gate all lifecycle boundaries.
