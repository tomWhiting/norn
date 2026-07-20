---
name: norn
description: Delegate substantial repository work from Claude Code to a persistent headless Norn session using GPT-5.6 Sol. Use this skill whenever the user asks Norn to scout, research, review, or implement work; wants an independent GPT second opinion; wants to replace a Claude sub-agent; or needs structured, auditable delegation with a session ID and JSON Schema result.
argument-hint: "[scout|research|review|dev] [task] [preset] [medium|high|xhigh|max]"
compatibility: Requires `norn` and `jq` on PATH and a Norn Codex-subscription login.
---

# Norn Delegation

Use Norn as a persistent external worker for one of four modes:

| Mode | Default preset | Purpose |
|---|---|---|
| `scout` | `repository-map` | Locate code, map flows, and narrow an investigation |
| `research` | `evidence-synthesis` | Establish facts, compare options, and answer a question with evidence |
| `review` | `correctness` | Independently assess existing work without changing it |
| `dev` | `implementation` | Make a bounded implementation and verify it |

Every invocation persists a Norn session. Never add `--no-session`. Always
return the session ID and saved envelope path to the caller so the work can be
audited, resumed, or exported later.

## Defaults

- Model: `gpt-5.6-sol`
- Reasoning: `high`
- Service tier: `--fast`
- Output: mode-specific structured value inside Norn's JSON envelope
- Session: persistent, with an explicit ID published before launch
- Working directory and built-in file-tool boundary: repository root

Use Sol only at `medium`, `high`, `xhigh`, or `max`. Default to `high`.
Choose `xhigh` for difficult security, concurrency, persistence, architecture,
or cross-module work. Reserve `max` for genuinely phase-decisive work. If the
job is too small to justify `medium`, use a smaller model rather than Sol at
low effort. Keep `--fast` enabled for now.

## Presets

Each mode loads `instructions/<mode>/base.md` plus one specialist preset:

### Scout

- `repository-map`: identify entry points, responsibilities, and major flows
- `change-impact`: trace the consequences of a proposed change
- `incident-triage`: localize a concrete failure from symptom to source

### Research

- `evidence-synthesis`: answer a question from verified evidence
- `implementation-options`: compare viable implementation paths and tradeoffs
- `external-landscape`: combine repository facts with current primary sources

### Review

- `mechanical`: find incomplete refactors, stale paths, policy violations, and drift
- `correctness`: find behavioral defects and unsupported invariants
- `safety`: assess trust boundaries, credentials, disclosure, and unsafe authority
- `concurrency`: inspect races, ordering, cancellation, and durability
- `architecture`: assess ownership, boundaries, coupling, and migration shape
- `test-evidence`: assess whether tests and evidence actually prove the claims

### Dev

- `implementation`: implement a bounded feature end to end
- `bug-fix`: reproduce, repair, and regression-test a defect
- `refactor`: improve structure without changing intended behavior
- `test-hardening`: add meaningful regression and adversarial coverage

## Run Norn

Fill in `MODE`, `TASK`, and `SCOPE`. Override `PRESET`, `EFFORT`, or
`SCHEMA_PATH` only when the task requires it.

```bash
set -euo pipefail
umask 077

MODE="<scout|research|review|dev>"
TASK="<the concrete outcome Norn must produce>"
SCOPE="<paths, commit range, requirements, symptoms, or constraints>"
EFFORT="${EFFORT:-high}"

WORKSPACE="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
SKILL_DIR="${NORN_SKILL_DIR:-$HOME/.claude/skills/norn}"
NORN_STATE_HOME="${NORN_HOME:-$HOME/.norn}"

case "$MODE" in
  scout)
    DEFAULT_PRESET="repository-map"
    ALLOWED_TOOLS="read,search,lsp,bash"
    ;;
  research)
    DEFAULT_PRESET="evidence-synthesis"
    ALLOWED_TOOLS="read,search,lsp,bash,web_search,web_fetch"
    ;;
  review)
    DEFAULT_PRESET="correctness"
    ALLOWED_TOOLS="read,search,lsp,bash"
    ;;
  dev)
    DEFAULT_PRESET="implementation"
    ALLOWED_TOOLS="read,search,lsp,bash,write,edit,apply_patch,action_log"
    ;;
  *)
    printf 'unknown Norn mode: %s\n' "$MODE" >&2
    exit 2
    ;;
esac

case "$EFFORT" in
  medium|high|xhigh|max) ;;
  *)
    printf 'unsupported Sol reasoning effort: %s\n' "$EFFORT" >&2
    exit 2
    ;;
esac

PRESET="${PRESET:-$DEFAULT_PRESET}"
SCHEMA_PATH="${SCHEMA_PATH:-$SKILL_DIR/schemas/$MODE.schema.json}"
BASE_INSTRUCTIONS="$SKILL_DIR/instructions/$MODE/base.md"
PRESET_INSTRUCTIONS="$SKILL_DIR/instructions/$MODE/$PRESET.md"

for required_file in "$SCHEMA_PATH" "$BASE_INSTRUCTIONS" "$PRESET_INSTRUCTIONS"; do
  if [[ ! -f "$required_file" ]]; then
    printf 'missing Norn skill resource: %s\n' "$required_file" >&2
    exit 2
  fi
done

if [[ "$NORN_STATE_HOME" != /* ]]; then
  printf 'NORN_HOME must be absolute for persistent delegation: %s\n' "$NORN_STATE_HOME" >&2
  exit 2
fi

ENVELOPE_DIR="$NORN_STATE_HOME/delegations"
mkdir -p "$ENVELOPE_DIR"
RESULT_FILE="$(mktemp "$ENVELOPE_DIR/claude-$MODE.XXXXXX")"
SESSION_ID="$(basename "$RESULT_FILE")"
STATUS_FILE="$RESULT_FILE.status.json"
SESSION_FILE="$NORN_STATE_HOME/session-store/$SESSION_ID.jsonl"
SESSION_NAME="claude-$MODE-$(date -u +%Y%m%dT%H%M%SZ)-$$"
APPENDED_SYSTEM_PROMPT="$(cat "$BASE_INSTRUCTIONS" "$PRESET_INSTRUCTIONS")"
STARTED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

write_delegation_status() {
  local status="$1"
  local exit_code="$2"
  local stop_reason="$3"
  local updated_at
  updated_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  jq -n \
    --arg status "$status" \
    --arg session_id "$SESSION_ID" \
    --arg session_name "$SESSION_NAME" \
    --arg result_file "$RESULT_FILE" \
    --arg session_file "$SESSION_FILE" \
    --arg started_at "$STARTED_AT" \
    --arg updated_at "$updated_at" \
    --arg wrapper_pid "$$" \
    --arg exit_code "$exit_code" \
    --arg stop_reason "$stop_reason" \
    '{
      status: $status,
      session_id: $session_id,
      session_name: $session_name,
      result_file: $result_file,
      session_file: $session_file,
      started_at: $started_at,
      updated_at: $updated_at,
      wrapper_pid: ($wrapper_pid | tonumber),
      exit_code: (if $exit_code == "" then null else ($exit_code | tonumber) end),
      stop_reason: (if $stop_reason == "" then null else $stop_reason end)
    }' >"$STATUS_FILE.tmp"
  mv "$STATUS_FILE.tmp" "$STATUS_FILE"
}

PROMPT="$(cat <<PROMPT
Delegation mode: $MODE
Specialist preset: $PRESET
Repository: $WORKSPACE

Task:
$TASK

Scope and constraints:
$SCOPE

Work from the current repository state. Return only the value required by the
supplied output schema. Do not claim completion or verification without direct
evidence.
PROMPT
)"

write_delegation_status "running" "" ""
printf 'Norn session: %s\n' "$SESSION_ID" >&2
printf 'Norn envelope: %s\n' "$RESULT_FILE" >&2
printf 'Norn status: %s\n' "$STATUS_FILE" >&2
printf 'Norn timeline: %s\n' "$SESSION_FILE" >&2

set +e
norn --print \
  --model gpt-5.6-sol \
  --reasoning-effort "$EFFORT" \
  --fast \
  --working-dir "$WORKSPACE" \
  --workspace-root "$WORKSPACE" \
  --allowed-tools "$ALLOWED_TOOLS" \
  --append-system-prompt "$APPENDED_SYSTEM_PROMPT" \
  --session-id "$SESSION_ID" \
  --session-name "$SESSION_NAME" \
  --quiet \
  --output-schema "$SCHEMA_PATH" \
  --output-format json \
  >"$RESULT_FILE" <<<"$PROMPT"
NORN_EXIT=$?
set -e

STOP_REASON="$(jq -er '.stop.reason' "$RESULT_FILE" 2>/dev/null || printf 'unavailable')"
if [[ "$NORN_EXIT" -eq 0 ]]; then
  FINAL_STATUS="finished"
else
  FINAL_STATUS="exited"
fi
write_delegation_status "$FINAL_STATUS" "$NORN_EXIT" "$STOP_REASON"

if [[ "$NORN_EXIT" -ne 0 ]]; then
  printf 'Norn exited with status %s; inspect the status and durable timeline above.\n' "$NORN_EXIT" >&2
  exit "$NORN_EXIT"
fi

ENVELOPE_SESSION_ID="$(jq -er '.session_id' "$RESULT_FILE")"
if [[ "$ENVELOPE_SESSION_ID" != "$SESSION_ID" ]]; then
  printf 'Norn envelope session mismatch: expected %s, got %s\n' "$SESSION_ID" "$ENVELOPE_SESSION_ID" >&2
  exit 1
fi

jq -e '
  if .stop.reason == "completed" then
    .output
  else
    error("Norn delegation did not complete; inspect the saved envelope")
  end
' "$RESULT_FILE"
```

The runner prints `SESSION_ID`, `RESULT_FILE`, `STATUS_FILE`, and `SESSION_FILE`
before Norn starts. Retain and report them; do not delete the envelope or status
record automatically. Terminal JSON is written only when the run ends, so an
empty `RESULT_FILE` does not mean the worker made no progress. Inspect the
status record and durable session timeline, then resume the exact session when
appropriate.

Do not wrap delegation in a broad `pkill`, caller-side hard timeout, or a
pipeline that hides Norn's exit status. Use Norn's own explicit timeout when a
bounded run is required, or driven-mode cancellation when graceful in-band
cancellation is required.

`--workspace-root` confines Norn's built-in file tools. It does not sandbox
`bash`. Scout, research, and review instructions prohibit repository mutation,
but use an OS sandbox or disposable read-only worktree when enforcement rather
than instruction is required.

## Custom Output Schemas

`--output-schema` constrains and locally validates the model's final value.
`--output-format json` wraps that value in `.output` alongside `.stop`,
`.usage`, `.session_id`, `.events`, and `.diagnostics`. Use both for delegation.

To customize a mode, copy its schema and set `SCHEMA_PATH` to the new file. A
portable schema should:

1. Be complete and self-contained. Do not use external `$ref` URLs or depend on
   another schema document.
2. Use a root object and set `additionalProperties: false` on every object.
3. Put every declared property in that object's `required` array. Prefer empty
   arrays or strings over optional object properties.
4. Use the portable structured-output subset: objects, arrays, strings,
   numbers, booleans, nulls, enums, descriptions, and simple bounds.
5. Avoid comments, trailing commas, `patternProperties`, conditionals, and
   advanced schema features unless the active backend is verified to support them.

The schema applies to `.output`, not to the surrounding CLI envelope.

## Resume For Follow-Up

Resume the persisted worker when the caller needs clarification or another pass.
Use the session ID printed by the original command and repeat all behavioral
flags explicitly; a new process does not inherit them.

```bash
set -euo pipefail
umask 077

SESSION_ID="<persisted Norn session id>"
FOLLOW_UP="<question, correction, or next bounded task>"
MODE="<same mode>"
PRESET="<same or new preset>"
EFFORT="${EFFORT:-high}"
WORKSPACE="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
SKILL_DIR="${NORN_SKILL_DIR:-$HOME/.claude/skills/norn}"
NORN_STATE_HOME="${NORN_HOME:-$HOME/.norn}"
SCHEMA_PATH="${SCHEMA_PATH:-$SKILL_DIR/schemas/$MODE.schema.json}"
BASE_INSTRUCTIONS="$SKILL_DIR/instructions/$MODE/base.md"
PRESET_INSTRUCTIONS="$SKILL_DIR/instructions/$MODE/$PRESET.md"
APPENDED_SYSTEM_PROMPT="$(cat "$BASE_INSTRUCTIONS" "$PRESET_INSTRUCTIONS")"
ENVELOPE_DIR="$NORN_STATE_HOME/delegations"
mkdir -p "$ENVELOPE_DIR"
RESULT_FILE="$(mktemp "$ENVELOPE_DIR/claude-$MODE-follow-up.XXXXXX")"
STATUS_FILE="$RESULT_FILE.status.json"
SESSION_FILE="$NORN_STATE_HOME/session-store/$SESSION_ID.jsonl"
STARTED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

write_delegation_status() {
  local status="$1"
  local exit_code="$2"
  local stop_reason="$3"
  local updated_at
  updated_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  jq -n \
    --arg status "$status" \
    --arg session_id "$SESSION_ID" \
    --arg result_file "$RESULT_FILE" \
    --arg session_file "$SESSION_FILE" \
    --arg started_at "$STARTED_AT" \
    --arg updated_at "$updated_at" \
    --arg wrapper_pid "$$" \
    --arg exit_code "$exit_code" \
    --arg stop_reason "$stop_reason" \
    '{
      status: $status,
      session_id: $session_id,
      result_file: $result_file,
      session_file: $session_file,
      started_at: $started_at,
      updated_at: $updated_at,
      wrapper_pid: ($wrapper_pid | tonumber),
      exit_code: (if $exit_code == "" then null else ($exit_code | tonumber) end),
      stop_reason: (if $stop_reason == "" then null else $stop_reason end)
    }' >"$STATUS_FILE.tmp"
  mv "$STATUS_FILE.tmp" "$STATUS_FILE"
}

case "$MODE" in
  scout|review) ALLOWED_TOOLS="read,search,lsp,bash" ;;
  research) ALLOWED_TOOLS="read,search,lsp,bash,web_search,web_fetch" ;;
  dev) ALLOWED_TOOLS="read,search,lsp,bash,write,edit,apply_patch,action_log" ;;
  *) exit 2 ;;
esac

write_delegation_status "running" "" ""
printf 'Norn session: %s\n' "$SESSION_ID" >&2
printf 'Norn envelope: %s\n' "$RESULT_FILE" >&2
printf 'Norn status: %s\n' "$STATUS_FILE" >&2
printf 'Norn timeline: %s\n' "$SESSION_FILE" >&2

set +e
norn --print \
  --model gpt-5.6-sol \
  --reasoning-effort "$EFFORT" \
  --fast \
  --working-dir "$WORKSPACE" \
  --workspace-root "$WORKSPACE" \
  --allowed-tools "$ALLOWED_TOOLS" \
  --append-system-prompt "$APPENDED_SYSTEM_PROMPT" \
  --resume "$SESSION_ID" \
  --quiet \
  --output-schema "$SCHEMA_PATH" \
  --output-format json \
  >"$RESULT_FILE" <<<"$FOLLOW_UP"
NORN_EXIT=$?
set -e

STOP_REASON="$(jq -er '.stop.reason' "$RESULT_FILE" 2>/dev/null || printf 'unavailable')"
if [[ "$NORN_EXIT" -eq 0 ]]; then
  FINAL_STATUS="finished"
else
  FINAL_STATUS="exited"
fi
write_delegation_status "$FINAL_STATUS" "$NORN_EXIT" "$STOP_REASON"

if [[ "$NORN_EXIT" -ne 0 ]]; then
  printf 'Norn follow-up exited with status %s; inspect the status and durable timeline above.\n' "$NORN_EXIT" >&2
  exit "$NORN_EXIT"
fi

jq -e 'if .stop.reason == "completed" then .output else error("follow-up did not complete") end' "$RESULT_FILE"
```

For a free-form follow-up, omit `--output-schema` and still inspect `.output`,
`.stop.reason`, and `.diagnostics` in the envelope.
