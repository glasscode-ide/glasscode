#!/bin/bash
# P13 The Codex terminal UI attached to an app server, with a second client.
#
#   p13-app-server.sh <claude-config-dir> <codex-home> <scratch-dir>
#
# Starts `codex app-server --listen unix://<scratch>/app.sock` in the work
# folder, the TUI with `--remote unix://…` in a pseudo-terminal (read-only
# sandbox, approvals on request), and a second client (appserver-client.py,
# WebSocket over the socket). Prompts: 3.
#   turn 1 (1 prompt): the TUI asks for `echo ok > p13.txt`, which needs an
#          approval; the second client subscribes to the thread (thread/resume)
#          and tries to answer the approval request with "accept".
#   turn 2 (1 prompt + 1 steer): the TUI runs `sleep 25`; the second client
#          sends turn/steer 3 s in, then turn/interrupt 4 s later.
. "$(dirname "$0")/lib.sh"
proof_args "$@"
WORK="$SCRATCH/work"
make_repo "$WORK"
SOCK="$SCRATCH/app.sock"; rm -f "$SOCK"
CONF="$CODEX_DIR/config.toml"
printf '[projects."%s"]\ntrust_level = "trusted"\n' "$WORK" > "$CONF"
( cd "$WORK" && codex_env codex app-server --listen "unix://$SOCK" > "$RAW/server.out" 2> "$RAW/server.err" ) &
for i in $(seq 1 50); do [ -S "$SOCK" ] && break; sleep 0.2; done
INIT='{"request":"initialize","params":{"clientInfo":{"name":"glasscode-proofs","version":"0.0.1"}}},{"notify":"initialized"}'
Q1='Create the file p13.txt containing ok with the shell command echo ok > p13.txt, then say done.'
Q2='Run the shell command sleep 25 in the foreground, then say done.'
TUI_STEPS=$(python3 - "$Q1" "$Q2" <<'PY'
import json, sys
q1, q2 = sys.argv[1:3]
print(json.dumps([
  {"wait_for": "›|Ask Codex", "timeout": 40}, {"sleep": 3},
  {"send": q1}, {"sleep": 1}, {"key": "enter"}, {"mark": "turn1"},
  {"sleep": 75},
  {"mark": "turn2"}, {"send": q2}, {"sleep": 1}, {"key": "enter"},
  {"sleep": 40}, {"key": "ctrl-c"}, {"wait_exit": 15}]))
PY
)
( cd "$WORK" && codex_env python3 "$HERE/pty-drive.py" "$RAW/tui" "$TUI_STEPS" -- \
    codex --remote "unix://$SOCK" "${CODEX_CHEAP[@]}" "${CODEX_TUI[@]}" --sandbox read-only --ask-for-approval on-request ) > "$RAW/tui.timeline.out" 2> "$RAW/tui.steps.log" &
TPID=$!
# Client 1: after the TUI sent turn 1, subscribe and try to answer the approval.
sleep 14
: > "$RAW/c1.jsonl"
codex_env python3 "$HERE/appserver-client.py" "$RAW/c1.jsonl" \
  "[$INIT,{\"request\":\"thread/loaded/list\",\"params\":{},\"save\":\"loaded\"},{\"request\":\"thread/resume\",\"params\":{\"threadId\":\"\${loaded.data.0}\"},\"save\":\"resumed\"},{\"answer\":\"requestApproval|execCommandApproval\",\"result\":{\"decision\":\"accept\"},\"timeout\":50},{\"wait\":\"^turn/completed\",\"timeout\":40,\"save\":\"done1\"}]" \
  --unix "$SOCK" > "$RAW/c1.result.json" 2> "$RAW/c1.steps"
tally P13 codex "turn 1: a command that needs an approval"
# Client 2: subscribe before turn 2, steer it, then interrupt it.
TID=$(jq -r '.saved.loaded.data[0] // empty' "$RAW/c1.result.json")
sleep $(( 75 + 14 - 14 - 50 > 0 ? 1 : 1 ))
: > "$RAW/c2.jsonl"
codex_env python3 "$HERE/appserver-client.py" "$RAW/c2.jsonl" \
  "[$INIT,{\"request\":\"thread/resume\",\"params\":{\"threadId\":\"$TID\"}},{\"wait\":\"^turn/started\",\"timeout\":90,\"save\":\"t\"},{\"sleep\":3},{\"request\":\"turn/steer\",\"params\":{\"threadId\":\"$TID\",\"expectedTurnId\":\"\${t.turn.id}\",\"input\":[{\"type\":\"text\",\"text\":\"Also end your reply with the word STEERED-13.\"}]},\"save\":\"steer\"},{\"sleep\":4},{\"request\":\"turn/interrupt\",\"params\":{\"threadId\":\"$TID\",\"turnId\":\"\${t.turn.id}\"},\"save\":\"intr\"},{\"wait\":\"^turn/completed\",\"timeout\":40,\"save\":\"done2\"}]" \
  --unix "$SOCK" > "$RAW/c2.result.json" 2> "$RAW/c2.steps"
tally P13 codex "turn 2: sleep 25"
tally P13 codex "turn 2: the second client's steer"
wait $TPID
pkill -f "app-server --listen unix://$SOCK"
rm -f "$CONF"
echo "== client 1 (approval)"; cat "$RAW/c1.steps"
jq -c '.steps[] | {s: (.step|keys[0]), v: (.step.request // .step.answer // .step.wait // null), ok, error, request}' "$RAW/c1.result.json"
echo "  turn 1 status: $(jq -r '.saved.done1.turn.status // "none"' "$RAW/c1.result.json"); p13.txt: $([ -f "$WORK/p13.txt" ] && echo written || echo absent)"
echo "  server requests seen by client 1: $(jq -r 'select(.dir=="in" and .msg.id != null and .msg.method != null) | .msg.method' "$RAW/c1.jsonl" | sort | uniq -c | tr '\n' ' ')"
echo "  notifications seen by client 1: $(jq -r 'select(.dir=="in" and .msg.id == null) | .msg.method' "$RAW/c1.jsonl" | sort | uniq -c | sort -rn | head -12 | tr '\n' ' ')"
echo "== client 2 (steer, interrupt)"; cat "$RAW/c2.steps"
jq -c '.steps[] | {s: (.step|keys[0]), v: (.step.request // .step.wait // null), ok, error}' "$RAW/c2.result.json"
echo "  turn 2 status: $(jq -r '.saved.done2.turn.status // "none"' "$RAW/c2.result.json")"
echo "== what the TUI showed (last lines)"
python3 - "$RAW/tui.txt" "$HOME" <<'PY'
import sys
t = open(sys.argv[1]).read().replace(sys.argv[2], "~")
lines = [l.strip() for l in t.splitlines() if l.strip()]
print("\n".join(lines[-25:])[:2500])
PY
