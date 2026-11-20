#!/bin/bash
# P16 Claude sessions messaging each other.
#
#   p16-cross-session.sh <claude-config-dir> <codex-home> <scratch-dir> accept|refuse
#
# A receiver (interactive, named p16-receiver-<mode>-7f3a, in its own folder)
# starts first; a sender (`claude -p`, same scratch account) is asked to list
# the sessions and send the receiver, by that exact name, a request to create
# p16-done.txt. The receiver runs with the default (accept) or with
# --settings {"crossSessionInbound": "refuse"}.
# Prompts: accept 2 (the sender's, and the receiver's turn), refuse 1.
# Only the number of sessions the sender lists is recorded, never their names.
. "$(dirname "$0")/lib.sh"
proof_args "$@"
MODE=$4
NAME="p16-receiver-$MODE-7f3a"
RB="$SCRATCH/receiver-$MODE"; SA="$SCRATCH/sender-$MODE"
make_repo "$RB"; make_repo "$SA"; claude_trust "$RB"; claude_trust "$SA"
rm -f "$RB/p16-done.txt"
HLOG="$RAW/receiver-$MODE.hooks.log"; : > "$HLOG"
EXTRA='{}'; [ "$MODE" = refuse ] && EXTRA='{"crossSessionInbound": "refuse"}'
S=$(python3 "$HERE/claude-hooks.py" "$HLOG" --extra "$EXTRA")
WAIT=150; [ "$MODE" = refuse ] && WAIT=90
STEPS="[{\"wait_for\":\"forshortcuts|automodeon|/effort\",\"timeout\":40},{\"mark\":\"ready\"},{\"wait_file\":\"$HLOG\",\"contains\":\"\\\"label\\\": \\\"UserPromptSubmit\\\"\",\"min_count\":1,\"timeout\":$WAIT},{\"wait_file\":\"$HLOG\",\"contains\":\"\\\"label\\\": \\\"Stop\\\"\",\"min_count\":1,\"timeout\":120},{\"sleep\":2},{\"send\":\"/exit\"},{\"sleep\":1.5},{\"key\":\"enter\"},{\"wait_exit\":20}]"
( cd "$RB" && claude_env python3 "$HERE/pty-drive.py" "$RAW/receiver-$MODE" "$STEPS" -- \
    claude -n "$NAME" --model "$CLAUDE_MODEL" --strict-mcp-config --permission-mode acceptEdits --settings "$S" ) > "$RAW/receiver-$MODE.timeline.out" 2> "$RAW/receiver-$MODE.steps.log" &
RPID=$!
for i in $(seq 1 60); do grep -q '"label": "SessionStart"' "$HLOG" 2>/dev/null && break; sleep 1; done
sleep 5
ASK="Use the ListAgents tool to list the other sessions. Then use the SendMessage tool to send the session named exactly $NAME this message: Please create the file p16-done.txt containing ok in your working folder. Send it to that session only, and to no other. Then reply with the number of sessions ListAgents showed and the SendMessage result."
MSGS=$(python3 -c 'import json,sys; print(json.dumps([sys.argv[1]]))' "$ASK")
( cd "$SA" && claude_env python3 "$HERE/stream-drive.py" "$RAW/sender-$MODE.jsonl" "$MSGS" -- \
    claude -p -n "p16-sender-$MODE-7f3a" --input-format stream-json --output-format stream-json --verbose --model "$CLAUDE_MODEL" \
    --strict-mcp-config --permission-mode acceptEdits ) > "$RAW/sender-$MODE.summary.json"
tally P16 claude "$MODE: sender lists and sends"
wait $RPID
if grep -q '"label": "UserPromptSubmit"' "$HLOG"; then tally P16 claude "$MODE: the receiver's turn on the message"; fi
echo "== sender's tool calls:"
python3 - "$RAW/sender-$MODE.jsonl" "$NAME" <<'PY'
import json, sys
name = sys.argv[2]; uses = {}
for line in open(sys.argv[1]):
    j = json.loads(line)["line"]
    msg = j.get("message")
    if not isinstance(msg, dict) or not isinstance(msg.get("content"), list):
        continue
    for c in msg["content"]:
        if not isinstance(c, dict):
            continue
        if c.get("type") == "tool_use":
            uses[c["id"]] = (c["name"], c.get("input") or {})
        elif c.get("type") == "tool_result":
            n, inp = uses.get(c.get("tool_use_id"), ("?", {}))
            cont = c.get("content")
            t = cont if isinstance(cont, str) else " ".join(x.get("text", "") if isinstance(x, dict) else str(x) for x in cont or [])
            if n == "ListAgents":
                rows = [l for l in t.splitlines() if l.strip()]
                print("  ListAgents -> %d lines; the receiver listed: %s" % (len(rows), name in t))
            elif n == "SendMessage":
                print("  SendMessage to=%s -> %s %s" % (inp.get("to"), "ERROR" if c.get("is_error") else "ok", t[:220].replace("\n", " ")))
            else:
                print("  %s -> %s" % (n, t[:100].replace("\n", " ")))
PY
echo "== receiver: file created: $([ -f "$RB/p16-done.txt" ] && echo yes || echo no); hooks: $(jq -r '.label' "$HLOG" | sort | uniq -c | tr '\n' ' ')"
jq -r 'select(.label=="UserPromptSubmit") | .payload.prompt' "$HLOG" | head -c 400; echo
