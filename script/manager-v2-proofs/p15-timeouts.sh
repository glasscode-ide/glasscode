#!/bin/bash
# P15 Tool-call timeouts. One prompt per run: the model calls the stub's
# `wait` tool with seconds=600 (the longest case of the proof; a CLI that keeps
# the call open 600 s also keeps it open 30 s, 2 min and 5 min).
#
#   p15-timeouts.sh <claude-config-dir> <codex-home> <scratch-dir> claude|codex|codex-900
#
# The stub logs when the call arrived, any notifications/cancelled, and when
# it answered; the CLI's own output gives the error it showed.
. "$(dirname "$0")/lib.sh"
proof_args "$@"
WHICH=$4
WORK="$SCRATCH/work-$WHICH"
make_repo "$WORK"
PY=$(command -v python3)
LOG="$RAW/stub-$WHICH.log"
: > "$LOG"
ASK='Call the glasscode wait tool exactly once with seconds=600, wait for it, then reply with exactly what it returned, or with the error you got.'
T0=$(ms_now)
case $WHICH in
  claude)
    cat > "$SCRATCH/mcp-$WHICH.json" <<EOF
{"mcpServers":{"glasscode":{"command":"$PY","args":["$HERE/stub-mcp.py","$LOG","--tools","wait"]}}}
EOF
    ( cd "$WORK" && TURN_TIMEOUT=900 claude_env python3 "$HERE/stream-drive.py" "$RAW/$WHICH.jsonl" "$(python3 -c 'import json,sys; print(json.dumps([sys.argv[1]]))' "$ASK")" -- \
        claude -p --input-format stream-json --output-format stream-json --verbose --model "$CLAUDE_MODEL" \
        --restricted --tools "" --strict-mcp-config --mcp-config "$SCRATCH/mcp-$WHICH.json" \
        --allowedTools "mcp__glasscode__*" --permission-mode dontAsk --permission-prompts none ) > "$RAW/$WHICH.summary.json"
    tally P15 claude "wait 600 s, default timeout"
    jq -c '[.turns[] | {result_ms, subtype: .result.subtype, result: .result.result}]' "$RAW/$WHICH.summary.json"
    jq -c 'select(.line.type=="user") | .line.message.content[]? | select(.type=="tool_result") | {is_error, content: (.content|tostring|.[0:200])}' "$RAW/$WHICH.jsonl"
    ;;
  codex|codex-900)
    EXTRA=()
    [ "$WHICH" = codex-900 ] && EXTRA=(-c mcp_servers.glasscode.tool_timeout_sec=900)
    ( cd "$WORK" && codex_env codex exec "${CODEX_CHEAP[@]}" --disable shell_tool --disable apps --disable plugins --disable multi_agent \
        --disable memories -c skills.include_instructions=false -s read-only --ignore-user-config --ignore-rules --skip-git-repo-check --color never --json \
        -c "mcp_servers.glasscode.command=\"$PY\"" -c "mcp_servers.glasscode.args=[\"$HERE/stub-mcp.py\",\"$LOG\",\"--tools\",\"wait\"]" \
        -c 'mcp_servers.glasscode.default_tools_approval_mode="approve"' ${EXTRA[@]+"${EXTRA[@]}"} "$ASK" < /dev/null ) > "$RAW/$WHICH.jsonl" 2> "$RAW/$WHICH.err"
    tally P15 codex "wait 600 s, $([ "$WHICH" = codex-900 ] && echo 'tool_timeout_sec=900' || echo 'default timeout')"
    jq -c 'select(.type=="item.completed" or .type=="turn.completed" or .type=="error") | {type, it: .item.type, status: .item.status, err: (.item.error // null | tostring | .[0:200]), text: (.item.text // null | tostring | .[0:200])}' "$RAW/$WHICH.jsonl"
    grep -i -m3 "timeout\|timed out" "$RAW/$WHICH.err" | cut -c1-200
    ;;
esac
T_END=$(ms_now)
CALL=$(jq -r 'select(.kind=="in" and .msg.method=="tools/call") | .t_ms' "$LOG" | head -1)
CANC=$(jq -r 'select(.kind=="in" and .msg.method=="notifications/cancelled") | .t_ms' "$LOG" | head -1)
RES=$(jq -r 'select(.kind=="result") | .t_ms' "$LOG" | head -1)
c="no"; [ -n "$CANC" ] && c="$(( CANC - CALL )) ms after the call"
r="no"; [ -n "$RES" ] && r="$(( RES - CALL )) ms after the call"
echo "call arrived +$(( ${CALL:-$T0} - T0 )) ms; cancelled: $c; stub answered: $r; run ended $(( (T_END - ${CALL:-$T0}) / 1000 )) s after the call"
