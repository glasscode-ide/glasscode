#!/bin/bash
# P05 New Claude hook events: the cases that need a turn.
#
#   p05-hooks.sh <claude-config-dir> <codex-home> <scratch-dir> interrupt|apierror|kill9|continue
#
# Every phase has all the hooks of claude-hooks.py, logged to raw/<phase>.hooks.log,
# and watches the transcript (transcript-watch.py) for the lag to Stop.
#   interrupt (1 prompt): interactive; the turn runs `sleep 30`; Esc 2 s after
#             PreToolUse. Records the hooks and the transcript's interrupt marker.
#   apierror  (1 prompt, no model reached): -p with an invalid model name.
#   kill9     (1 prompt): interactive; a 31 s foreground command (Claude refuses
#             a bare `sleep` in the foreground); SIGKILL to claude 2 s after
#             PreToolUse. Records hooks after the kill, the orphaned command,
#             and the transcript's last lines.
#   continue  (1 prompt, 2 requests and more): interactive; a turn that runs
#             `cd` and an Explore subagent; the Stop hook blocks once
#             (hook.py --stop-once), so Claude continues with CONTINUED-58.
. "$(dirname "$0")/lib.sh"
proof_args "$@"
PHASE=$4
WORK="$SCRATCH/work"
[ -d "$WORK/.git" ] || make_repo "$WORK"
claude_trust "$WORK"
LOG="$RAW/$PHASE.hooks.log"
: > "$LOG"
SID=$(python3 -c 'import uuid; print(uuid.uuid4())')
TX="$CLAUDE_DIR/projects/$(echo "$WORK" | sed 's#[^A-Za-z0-9]#-#g')/$SID.jsonl"
READY='{"wait_for":"forshortcuts|automodeon|/effort","timeout":40},{"sleep":3}'
EXIT='{"sleep":2},{"send":"/exit"},{"sleep":1.5},{"key":"enter"},{"wait_exit":20}'
pre() { echo "{\"wait_file\":\"$LOG\",\"contains\":\"\\\"label\\\": \\\"PreToolUse\\\"\",\"min_count\":1,\"timeout\":120}"; }
python3 "$HERE/transcript-watch.py" "$TX" "$RAW/$PHASE.tx-watch.jsonl" 300 &
WATCH=$!
run_i() { # <steps> <claude args...>
  local steps=$1; shift
  ( cd "$WORK" && claude_env python3 "$HERE/pty-drive.py" "$RAW/$PHASE" "$steps" -- claude --session-id "$SID" "$@" ) > "$RAW/$PHASE.timeline.out" 2> "$RAW/$PHASE.steps.log"
}
case $PHASE in
  interrupt)
    S=$(python3 "$HERE/claude-hooks.py" "$LOG")
    ASK='Run the shell command sleep 30 with the Bash tool, then say done.'
    run_i "[$READY,{\"send\":\"$ASK\"},{\"sleep\":1},{\"key\":\"enter\"},$(pre),{\"sleep\":2},{\"mark\":\"esc\"},{\"key\":\"esc\"},{\"sleep\":6},$EXIT]" \
      --model "$CLAUDE_MODEL" --strict-mcp-config --allowedTools "Bash(sleep *)" --settings "$S"
    tally P05 claude "interrupt: Esc during sleep 30"
    ;;
  apierror)
    S=$(python3 "$HERE/claude-hooks.py" "$LOG")
    ( cd "$WORK" && claude_env python3 "$HERE/stream-drive.py" "$RAW/$PHASE.jsonl" '["hi"]' -- \
        claude -p --input-format stream-json --output-format stream-json --verbose --session-id "$SID" \
        --model claude-glasscode-no-such-model --strict-mcp-config --settings "$S" ) > "$RAW/$PHASE.summary.json"
    tally P05 claude "apierror: invalid model name (no model reached)"
    jq -c '[.turns[] | {result_ms, subtype: .result.subtype, is_error: .result.is_error, result: (.result.result|tostring|.[0:200])}]' "$RAW/$PHASE.summary.json"
    ;;
  kill9)
    S=$(python3 "$HERE/claude-hooks.py" "$LOG")
    printf 'python3 -c "import time; time.sleep(31)"\n' > "$WORK/wait31.sh"
    ASK='Run the shell command sh wait31.sh with the Bash tool in the foreground, not in the background, then say done.'
    run_i "[$READY,{\"send\":\"$ASK\"},{\"sleep\":1},{\"key\":\"enter\"},$(pre),{\"sleep\":2},{\"mark\":\"kill\"},{\"kill\":9},{\"wait_exit\":10},{\"sleep\":5}]" \
      --model "$CLAUDE_MODEL" --strict-mcp-config --allowedTools "Bash(sh wait31.sh)" --settings "$S"
    tally P05 claude "kill9: SIGKILL during a 31 s foreground command"
    echo "orphaned command after the kill: $(pgrep -f 'time.sleep\(31\)' | wc -l | tr -d ' ')"
    ;;
  continue)
    touch "$SCRATCH/stop-once.flag"
    S=$(python3 "$HERE/claude-hooks.py" "$LOG" --stop-once "$SCRATCH/stop-once.flag")
    ASK='First run the shell command mkdir -p sub and then cd sub with the Bash tool. Then use the Agent tool with the Explore agent to count the files in this repository. Then say finished.'
    run_i "[$READY,{\"send\":\"$ASK\"},{\"sleep\":1},{\"key\":\"enter\"},{\"wait_file\":\"$LOG\",\"contains\":\"\\\"label\\\": \\\"Stop\\\"\",\"min_count\":2,\"timeout\":240},$EXIT]" \
      --model "$CLAUDE_MODEL" --strict-mcp-config --allowedTools "Bash" "Agent" --settings "$S"
    tally P05 claude "continue: cd, an Explore subagent, one stop-hook continuation"
    ;;
esac
sleep 1; kill $WATCH 2>/dev/null
cat "$RAW/$PHASE.steps.log" 2>/dev/null | grep -v '"send"'
echo "== hooks (time after the first hook, event, fields)"
python3 - "$LOG" <<'PY'
import json, sys
rows = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
t0 = rows[0]["t_ms"] if rows else 0
for r in rows:
    p = r.get("payload") or {}
    keep = {k: p[k] for k in ("source", "reason", "error", "error_details", "tool_name", "agent_type", "old_cwd", "new_cwd",
                              "stop_hook_active", "notification_type", "trigger", "last_assistant_message") if k in p}
    if "last_assistant_message" in keep and keep["last_assistant_message"]:
        keep["last_assistant_message"] = str(keep["last_assistant_message"])[:60]
    extra = sorted(set(p) - {"session_id", "transcript_path", "cwd", "hook_event_name", "scratchpad_dir"})
    print("+%6d ms %-18s %s keys=%s" % (r["t_ms"] - t0, r["label"], json.dumps(keep)[:220], ",".join(extra)))
PY
echo "== transcript: interrupt marker and last records"
[ -f "$TX" ] && { grep -o 'Request interrupted[^"]*' "$TX" | sort | uniq -c; tail -4 "$TX" | jq -c '{type, sub: (.subtype // .attachment.type // null), text: ((.message.content // "") | tostring | .[0:90])}'; }
