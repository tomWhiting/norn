#!/usr/bin/env bash
#
# C7 measured resource-whisper harness (retry-forever design, D10 /
# hard requirement A).
#
# Runs the REAL `norn` binary in print mode against a localhost port with
# nothing listening. Every connect fails with ECONNREFUSED, which the
# provider maps to `ProviderError::ConnectionFailed { kind:
# ConnectionReset }` and the retry policy classifies as retryable, so the
# run retries forever by design. While it retries, the harness samples the
# process at a fixed interval and records RSS and cumulative CPU time.
# Finally it sends SIGTERM (which the branch's print signal handling treats
# as the user cancel) and records the raw exit code.
#
# Every parameter is supplied on the command line. The harness invents
# nothing: no default duration, no default interval, no default port, no
# thresholds. It measures and records; the numbers are read by a human.
#
# Usage:
#   resource-whisper.sh BINARY PORT DURATION_S INTERVAL_S GRACE_S OUT_DIR
#
#   BINARY      path to the built norn binary
#   PORT        localhost TCP port that MUST have nothing listening on it
#   DURATION_S  how long to let the run retry before signalling it
#   INTERVAL_S  sampling period
#   GRACE_S     how long to wait for a graceful exit after SIGTERM
#   OUT_DIR     directory to create and write the run artifacts into
#
# Outputs, all inside OUT_DIR:
#   meta.txt      versions, sha256, verbatim command line, timestamps
#   samples.tsv   elapsed_s / rss_kb / pcpu / utime / stime per sample
#   stdout.jsonl  the run's stdout (stream-json events, incl. stream_retry)
#   stderr.log    the run's stderr
#   exit.txt      the raw exit status of the run and of the signal path
#
# No credential is ever used: authentication is pinned to an API key read
# from NORN_C7_DUMMY_KEY, which this script sets to a literal placeholder.

set -u
set -o pipefail

if [ "$#" -ne 6 ]; then
    echo "usage: $0 BINARY PORT DURATION_S INTERVAL_S GRACE_S OUT_DIR" >&2
    exit 64
fi

BINARY="$1"
PORT="$2"
DURATION_S="$3"
INTERVAL_S="$4"
GRACE_S="$5"
OUT_DIR="$6"

if [ ! -x "$BINARY" ]; then
    echo "harness: '$BINARY' is not an executable file" >&2
    exit 66
fi

# The measurement is only meaningful if the endpoint genuinely refuses the
# connection. Abort loudly rather than measure against something live.
if nc -z -G 1 127.0.0.1 "$PORT" >/dev/null 2>&1; then
    echo "harness: something is LISTENING on 127.0.0.1:$PORT — refusing to measure" >&2
    exit 69
fi

mkdir -p "$OUT_DIR" || exit 73
OUT_DIR="$(cd "$OUT_DIR" && pwd)" || exit 73

RUN_HOME="$OUT_DIR/norn-home"
RUN_CWD="$OUT_DIR/workdir"
mkdir -p "$RUN_HOME" "$RUN_CWD" || exit 73

BASE_URL="http://127.0.0.1:$PORT/v1"
PROMPT="C7 resource whisper probe: this request can never reach a provider."

# The one and only credential in play: a literal placeholder, never a real
# key. `auth=api_key` pins the provider away from any OAuth path so no
# stored credential can be picked up.
export NORN_C7_DUMMY_KEY="dummy-key-not-a-credential"
export NORN_HOME="$RUN_HOME"

{
    echo "harness_started_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "uname=$(uname -a)"
    echo "binary=$BINARY"
    echo "binary_sha256=$(shasum -a 256 "$BINARY" | awk '{print $1}')"
    echo "binary_version=$("$BINARY" --version 2>&1)"
    echo "rustc=$(rustc --version 2>&1)"
    echo "cargo=$(cargo --version 2>&1)"
    echo "port=$PORT"
    echo "duration_s=$DURATION_S"
    echo "interval_s=$INTERVAL_S"
    echo "grace_s=$GRACE_S"
    echo "norn_home=$NORN_HOME"
    echo "working_dir=$RUN_CWD"
    echo "command=$BINARY -p <PROMPT> -m sol -f stream-json -C $RUN_CWD -c base_url=$BASE_URL -c auth=api_key -c api_key_env=NORN_C7_DUMMY_KEY"
} >"$OUT_DIR/meta.txt"

"$BINARY" \
    -p "$PROMPT" \
    -m sol \
    -f stream-json \
    -C "$RUN_CWD" \
    -c "base_url=$BASE_URL" \
    -c "auth=api_key" \
    -c "api_key_env=NORN_C7_DUMMY_KEY" \
    >"$OUT_DIR/stdout.jsonl" 2>"$OUT_DIR/stderr.log" &
RUN_PID=$!

printf 'elapsed_s\trss_kb\tpcpu\tutime\tstime\n' >"$OUT_DIR/samples.tsv"

START_EPOCH=$(date +%s)
ELAPSED=0
DIED_EARLY=0
while [ "$ELAPSED" -lt "$DURATION_S" ]; do
    SAMPLE="$(ps -o rss=,pcpu=,utime=,stime= -p "$RUN_PID" 2>/dev/null)"
    PS_STATUS=$?
    if [ "$PS_STATUS" -ne 0 ] || [ -z "$SAMPLE" ]; then
        echo "harness: process $RUN_PID vanished at ${ELAPSED}s (ps exit $PS_STATUS)" >&2
        DIED_EARLY=1
        break
    fi
    printf '%s\t%s\n' "$ELAPSED" "$(echo "$SAMPLE" | awk '{print $1"\t"$2"\t"$3"\t"$4}')" \
        >>"$OUT_DIR/samples.tsv"
    sleep "$INTERVAL_S"
    ELAPSED=$(( $(date +%s) - START_EPOCH ))
done

SIGNAL_SENT_AT="$ELAPSED"
if [ "$DIED_EARLY" -eq 0 ]; then
    kill -TERM "$RUN_PID"
    KILL_STATUS=$?
else
    KILL_STATUS="not-sent"
fi

# Bounded wait for the graceful path, polled once a second so the harness
# itself can never hang.
WAITED=0
while [ "$WAITED" -lt "$GRACE_S" ] && kill -0 "$RUN_PID" 2>/dev/null; do
    sleep 1
    WAITED=$(( WAITED + 1 ))
done

ESCALATED=0
if kill -0 "$RUN_PID" 2>/dev/null; then
    echo "harness: still alive ${GRACE_S}s after SIGTERM; escalating to SIGKILL" >&2
    kill -KILL "$RUN_PID"
    ESCALATED=1
fi

wait "$RUN_PID"
RUN_EXIT=$?

{
    echo "run_pid=$RUN_PID"
    echo "died_before_signal=$DIED_EARLY"
    echo "signal_sent_at_elapsed_s=$SIGNAL_SENT_AT"
    echo "kill_term_status=$KILL_STATUS"
    echo "seconds_waited_for_graceful_exit=$WAITED"
    echo "escalated_to_sigkill=$ESCALATED"
    echo "run_exit_code=$RUN_EXIT"
    echo "harness_finished_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
} >"$OUT_DIR/exit.txt"

cat "$OUT_DIR/exit.txt"
exit 0
