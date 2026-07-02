# Task Management Tools for AI Coding Agents

Date: 2026-05-10

## Executive Summary

AI coding-agent task tools are converging on a few common needs:

- Persistent state outside the context window.
- Dependency-aware work queues so agents can ask "what is ready?"
- Machine-readable task data, usually JSON or JSONL.
- Lightweight coordination across sessions, subagents, or parallel agents.
- Links back to source material: files, requirements, specs, external tickets, review notes.

The current market splits into three rough tiers:

1. **Native session task systems** such as Claude Code Tasks: simple JSON files, good for active work, built into the agent UX, but not a full project tracker.
2. **Repo-local graph trackers** such as Beads: designed as persistent agent memory with dependencies, issue types, audit trail, and sync.
3. **External project-management integrations** through MCP or APIs: Linear, Jira/Atlassian, Asana, ClickUp, etc. These are useful for product/team source of truth, but less ideal as the agent's fine-grained execution queue.

## 1. Beads

### What It Is

Beads (`bd`) is a distributed issue/task tracker explicitly built for AI-supervised coding workflows. The current upstream repo is:

- GitHub: https://github.com/gastownhall/beads
- Docs: https://gastownhall.github.io/beads/

The docs describe Beads as a Dolt-powered issue tracker for coding agents. It positions itself against Jira/GitHub Issues for agent workflows, emphasizing hash IDs, dependency-aware execution, formula templates, and multi-agent coordination.

### Core Features

Key upstream features:

- **Dependency graph**: issues can block other issues; `bd ready` surfaces unblocked work.
- **Graph relationships**: documented relationship types include `blocks`, `parent-child`, `discovered-from`, and `related`; the README also mentions `relates_to`, `duplicates`, `supersedes`, and `replies_to`.
- **Hierarchical IDs / epics**: examples include `bd-a3f8`, `bd-a3f8.1`, and `bd-a3f8.1.1`.
- **Agent-optimized output**: common commands support `--json`.
- **Hash-based IDs**: intended to avoid ID collisions during parallel agent work.
- **Persistent project memory**: `bd prime` injects workflow context and remembered project facts.
- **Compaction / memory decay**: old closed issues can be summarized to reduce context load.
- **Messaging**: upstream now includes message issue types and threading.
- **Multi-agent features**: routing, gates, molecules, formulas, work graphs.
- **Integrations**: Claude Code setup, MCP server, GitHub/GitLab/Jira/Linear/Notion-related command surfaces appear in the CLI docs.

Representative commands:

```bash
bd init
bd create "Fix authentication bug" --description="..." -t bug -p 1 --json
bd ready --json
bd show bd-42 --json
bd update bd-42 --claim --json
bd dep add <child> <parent>
bd close bd-42 --reason "Fixed in commit abc123" --json
bd prime
bd dolt push
```

### Task Structure

Beads is issue-oriented rather than just checklist-oriented. A Beads issue commonly includes:

- ID, title, description.
- Type, such as task or bug.
- Priority.
- Status/state.
- Assignee/claim ownership.
- Labels/tags.
- Dependencies and graph links.
- Comments/notes/audit history.
- Optional workflow/formula metadata.

The important structural difference from basic agent todos is that Beads treats tasks as graph nodes, not flat checklist rows.

### Storage Format: Upstream Beads

The current upstream architecture uses **Dolt** as the source of truth:

- Dolt is a version-controlled SQL database.
- Default mode is embedded Dolt in `.beads/embeddeddolt/`.
- Server mode connects to `dolt sql-server`; data lives in `.beads/dolt/`.
- Dolt gives SQL queries, cell-level merge, branching, push/pull, backup/restore.
- `bd export` can produce JSONL for migration/interoperability, but JSONL is no longer the primary upstream storage model.

This is an important update: older Beads material and derivatives often describe "JSONL + SQLite"; current Beads docs say Dolt is the sole storage backend.

### Classic / Rust Implementation: `beads_rust`

There is also a Rust port preserving the classic Beads architecture:

- GitHub: https://github.com/Dicklesworthstone/beads_rust
- Binary: `br`

The Rust port says it is frozen at the "classic" SQLite + JSONL architecture:

```text
.beads/
  beads.db        # SQLite database, primary storage
  issues.jsonl    # JSONL export for git collaboration
  config.yaml
  routes.jsonl
  metadata.json
```

Rust implementation characteristics:

- SQLite primary storage for local queries.
- JSONL export/import for Git-friendly collaboration.
- Mutations update SQLite and auto-flush JSONL by default.
- Explicit sync commands: `br sync --flush-only`, `--import-only`, `--merge`, `--rebuild`.
- No automatic git commit/push/pull.
- No background daemon.
- Agent-first CLI: `--json`, JSON schemas, `robot-docs`, MCP server support behind an optional Rust feature.
- Architecture includes WAL mode, dirty tracking, blocked cache, atomic writes, content hashing, merge support.

The Rust port is useful as a clean reference for a simpler local-first design: SQL for fast queries, JSONL for review/merge, and explicit sync semantics.

## 2. Claude Code Tasks

### Overview

Claude Code Tasks are the built-in task-management system for interactive Claude Code sessions. Official docs list these tools:

- `TaskCreate`: creates a task in the task list.
- `TaskGet`: retrieves full details for a task.
- `TaskList`: lists tasks and statuses.
- `TaskUpdate`: updates task status, dependencies, details, or deletes tasks.

Official Claude Code docs also expose hooks:

- `TaskCreated`
- `TaskCompleted`

These hooks can enforce naming conventions, require descriptions, or block task completion until checks pass.

### On-Disk Storage

Community reverse-engineering shows tasks stored under:

```text
~/.claude/tasks/{task-list-id-or-session-id}/
  .lock
  .highwatermark
  1.json
  2.json
  ...
```

Some articles refer to `.tasks` generically, but observed Claude Code storage is under `~/.claude/tasks/...`. Each task is a standalone JSON file. `.lock` is used for filesystem locking, and `.highwatermark` tracks the next task ID.

Shared task lists can be enabled with `CLAUDE_CODE_TASK_LIST_ID`, for example:

```bash
CLAUDE_CODE_TASK_LIST_ID=my-project claude
```

### Schema

Observed task file shape:

```json
{
  "id": "1",
  "subject": "Analyze frontend architecture",
  "description": "Deep-dive into the frontend codebase...",
  "activeForm": "Analyzing frontend architecture",
  "status": "completed",
  "owner": "frontend-engineer",
  "blocks": [],
  "blockedBy": [],
  "metadata": {
    "_internal": true
  }
}
```

Core fields:

- `id`: string task ID.
- `subject`: short title.
- `description`: detailed requirements/context.
- `activeForm`: present-progress label used by the UI/spinner.
- `status`: usually `pending`, `in_progress`, `completed`; some references also mention `deleted`.
- `owner`: optional agent/user owner; absent on unclaimed tasks in observed files.
- `blocks`: task IDs this task blocks.
- `blockedBy`: task IDs that must finish first.
- `metadata`: optional object; internal lifecycle tasks may use `{"_internal": true}`.

Tool parameter surface from public references:

```text
TaskCreate:
  subject: string
  description: string
  activeForm?: string
  metadata?: object

TaskGet:
  taskId: string

TaskList:
  no required parameters

TaskUpdate:
  taskId: string
  subject?: string
  description?: string
  activeForm?: string
  status?: "pending" | "in_progress" | "completed" | "deleted"
  owner?: string
  addBlockedBy?: string[]
  addBlocks?: string[]
```

### Strengths

- Native UX in Claude Code, including `/tasks` and `Ctrl+T` references in community docs.
- Persistent across session termination if using a task list ID.
- Dependency edges are first-class enough for unblocking.
- Multiple sessions/subagents can share a task list.
- Hooks can enforce creation/completion policy.
- Simple file format is easy to inspect.

### Limitations

- Task model is shallow: no native parent/child hierarchy beyond dependency edges.
- No rich requirement/file/brief linkage in the base schema.
- No durable closed-task archive in some observed versions; completed task files may be removed after all tasks complete.
- Context is not automatically shared; task state persists, but session conversation context remains isolated.
- No built-in merge/consolidation for completed tasks.
- No rich query language or views comparable to a project tracker.
- Limits are unclear; at least one early write-up observed a 10-task list limit.

## 3. Common Problems With Current AI Agent Task Tools

### Flatness

Many task tools are still glorified todo lists. They track title/status and maybe dependencies, but not:

- Epic > task > subtask structure.
- Requirement coverage.
- Design-doc traceability.
- Acceptance criteria per task.
- Review evidence.
- File ownership / module boundaries.

The result is that agents can mark checkboxes while losing sight of the architectural or product intent.

### Weak Traceability

Common missing links:

- Requirement IDs.
- Brief/design sections.
- Source files expected to change.
- Test files expected to cover the task.
- External ticket IDs.
- Pull requests/commits.
- Decisions made during implementation.

Without traceability, a completed task is often just a status bit, not evidence that the intended requirement was satisfied.

### Poor Long-Horizon Memory

Public discussion around Beads and Claude Code Tasks repeatedly frames the problem as context-window loss: the agent forgets decisions, blockers, discovered bugs, and next steps when a session compacts or restarts.

Native Tasks improve this, but a task list alone does not preserve enough rationale unless the descriptions and updates are disciplined.

### Coordination Gaps

With multiple agents:

- Duplicate work can happen without atomic claiming.
- Blocked agents may silently stall.
- Sessions may need to pull/refresh task state.
- Real-time collaboration is often absent or tool-specific.
- File-level ownership is rarely enforced by the task system itself.

Beadbox's blocked-task write-up captures a practical failure mode: agents get stuck and do not naturally escalate like humans would.

### Too Much Or Too Little Integration

Native task systems are convenient but isolated from product systems. External systems like Jira/Linear are authoritative for teams, but often too heavy for the agent's fine-grained work queue.

The agent needs both:

- a local execution graph, and
- links/sync to the product tracker.

### Insufficient Completion Semantics

Status changes such as `completed` are often under-specified. A better system should distinguish:

- Code changed.
- Tests added.
- Tests passed.
- Review completed.
- Requirement satisfied.
- Follow-up discovered.
- External ticket updated.

Hooks can partially enforce this, but the task schema itself often has nowhere structured to store the proof.

## 4. Better Approaches

### Hierarchical Graph, Not Flat List

A stronger task tool should model:

- Epic / feature / requirement / task / subtask.
- Dependencies independent of hierarchy.
- Relationship types: blocks, parent-child, discovered-from, duplicates, supersedes, relates-to, implements, verifies.
- Multiple views: ready queue, blocked queue, per-owner queue, requirement coverage, file ownership.

### Requirement And Artifact Linkage

Each task should optionally link to:

- Requirement IDs.
- Design/brief files and section anchors.
- Source files expected to change.
- Test files expected to change.
- API/schema names.
- External tickets.
- Commits/PRs.
- Review findings.

This makes the task graph useful as an implementation control surface, not just a progress widget.

### Structured Completion Evidence

Recommended fields:

```json
{
  "acceptanceCriteria": [],
  "verification": {
    "commands": [],
    "results": [],
    "skipped": [],
    "knownFailures": []
  },
  "artifacts": {
    "filesChanged": [],
    "commits": [],
    "pullRequests": [],
    "screenshots": [],
    "logs": []
  },
  "review": {
    "status": "not_requested | requested | changes_requested | approved",
    "findings": []
  }
}
```

### Dynamic Tool Availability

Agents do better when the tool surface reflects the current state. Examples:

- Hide or de-prioritize `task_update` until tasks exist.
- Show `task_claim` only for unclaimed ready tasks.
- Show `task_complete` only for owned/in-progress tasks.
- Surface `task_unblock` only for blocked tasks.
- Add specialized tools only when relevant metadata exists, e.g. `requirement_link`, `attach_verification`, `sync_linear`.

Claude Code's MCP `list_changed` support is relevant here: MCP servers can dynamically refresh available tools/prompts/resources.

### Local-First Plus External Sync

A practical architecture:

- Local graph store for agent execution speed and offline use.
- Git-friendly export for review and branch/worktree collaboration.
- SQL query layer for readiness, coverage, and audits.
- Optional MCP/API sync with Linear/Jira/GitHub Issues.
- Explicit conflict and sync semantics.

Beads and `beads_rust` are both useful references:

- Beads current: version-controlled SQL with Dolt.
- Rust classic: SQLite primary store plus JSONL export.

### Treat Blockers As First-Class Signals

Agents need a way to escalate:

- blocked reason
- blocking task or external dependency
- owner to ask
- last progress timestamp
- stale claim detection
- automatic "needs human" gates

This is especially important for multi-agent runs where silence is not a useful signal.

## 5. Linear / Jira / Project Management Integration

### MCP Landscape

Claude Code supports MCP servers for external tools. Official docs list project-management/documentation integrations, including:

- Atlassian MCP for Jira/Confluence.
- Linear MCP.
- Asana MCP.
- ClickUp MCP.

Claude's MCP docs give example prompts such as implementing a feature from a Jira issue and creating a PR.

### Linear

Linear has an official MCP server:

- Docs: https://linear.app/docs/mcp
- Integration page: https://linear.app/integrations/claude
- Claude Code command:

```bash
claude mcp add --transport http linear-server https://mcp.linear.app/mcp
```

Linear says its MCP server lets compatible AI models/agents access Linear data securely, and supports finding, creating, and updating objects like issues, projects, and comments.

### Jira / Atlassian

Atlassian provides the Rovo MCP Server:

- Getting started: https://support.atlassian.com/atlassian-rovo-mcp-server/docs/getting-started-with-the-atlassian-remote-mcp-server/
- Usage docs: https://support.atlassian.com/atlassian-rovo-mcp-server/docs/use-atlassian-rovo-mcp-server/

Atlassian describes it as a cloud-hosted bridge for Jira, Compass, and Confluence Cloud. It supports read/write operations such as searching issues, summarizing pages, and creating/updating issues/pages via natural language. It uses OAuth 2.1 and respects existing user permissions.

Claude's docs previously showed:

```bash
claude mcp add --transport sse atlassian https://mcp.atlassian.com/v1/sse
```

Atlassian's current support page notes that after 2026-06-30, the old `/v1/sse` endpoint will no longer be supported and recommends `/mcp`: `https://mcp.atlassian.com/v1/mcp/authv2`.

### Design Takeaway

External trackers should be treated as product/source-of-truth systems, while agent task tools should be treated as execution/state systems. The best integration pattern is bidirectional linkage, not forcing one system to do both jobs:

- Linear/Jira issue defines product intent and team workflow.
- Local task graph decomposes execution into agent-sized work.
- Each local task records external issue IDs and sync status.
- Completion evidence can be summarized back into the external issue.

## Sources

- Beads GitHub README: https://github.com/gastownhall/beads
- Beads docs introduction: https://gastownhall.github.io/beads/
- Beads architecture docs: https://gastownhall.github.io/beads/architecture
- Beads Claude Code integration: https://gastownhall.github.io/beads/integrations/claude-code
- Beads CLI reference: https://gastownhall.github.io/beads/cli-reference
- Beads Rust port: https://github.com/Dicklesworthstone/beads_rust
- Claude Code tools reference: https://code.claude.com/docs/en/tools-reference
- Claude Code hooks reference: https://code.claude.com/docs/en/hooks
- Claude Code MCP docs: https://code.claude.com/docs/en/mcp
- Claude Code task tool reference article: https://claudearchitect.com/docs/claude-code/taskcreate-taskupdate-reference/
- Claude Code task storage exploration: https://zenn.dev/is0383kk/articles/f6de6ac8cf3a1a?locale=en
- Claude Code filesystem architecture: https://www.diljitpr.net/blog-post-2026-02-24-inside-dot-claude-filesystem-architecture.html
- Claude Code agent teams task files: https://www.claudecodecamp.com/p/claude-code-agent-teams-how-they-work-under-the-hood
- Claude Code task architecture/tradeoffs: https://claudecode.jp/en/news/engineer/claude-code-tasks-system
- Linear MCP docs: https://linear.app/docs/mcp
- Linear Claude integration: https://linear.app/integrations/claude
- Atlassian Rovo MCP getting started: https://support.atlassian.com/atlassian-rovo-mcp-server/docs/getting-started-with-the-atlassian-remote-mcp-server/
- Atlassian Rovo MCP usage docs: https://support.atlassian.com/atlassian-rovo-mcp-server/docs/use-atlassian-rovo-mcp-server/
- Beadbox blocked-task discussion: https://beadbox.app/blog/triage-blocked-tasks-parallel-development
