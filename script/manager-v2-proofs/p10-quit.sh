#!/bin/bash
# P10 What a quit does to running agents.
#
#   p10-quit.sh <claude-config-dir> <codex-home> <scratch-dir> proxy [normal|force]
#   p10-quit.sh <claude-config-dir> <codex-home> <scratch-dir> watch
#
# proxy (2 prompts per mode): Glasscode runs each session in a pseudo-terminal
# of its own (alacritty_terminal: the agent is a session leader on that
# terminal). The proxy does the same with pty-drive.py: it starts a Claude and
# a Codex session, gives each a turn that runs `sleep 45` in its shell, and
# 8 s into the turn ends the terminal's owner:
#   normal: the owner closes the terminal and exits (what a quit that runs
#           destructors does: alacritty_terminal's Pty drop sends SIGHUP);
#   force:  the owner gets SIGKILL (a force quit; the kernel closes the master).
# It then records, for 20 s, whether each agent process (and the `sleep` it
# started) lives on, and the last lines of the transcript and the rollout.
#
# watch (owner step, no prompt from this script): with the real app, in a
# scratch GLASSCODE_HOME, start one Claude and one Codex session on a long
# task, run `watch`, then quit the app (Cmd-Q), start it again, repeat with a
# force quit (Activity Monitor > Force Quit, or `kill -9 <app pid>`). `watch`
# prints every claude/codex process under the scratch accounts each second.
. "$(dirname "$0")/lib.sh"
proof_args "$@"
MODE=${4:-proxy}
KIND=${5:-force}
WORK="$SCRATCH/work"
[ -d "$WORK/.git" ] || make_repo "$WORK"
claude_trust "$WORK"

if [ "$MODE" = watch ]; then
  while true; do
    date +%T
    ps -ax -o pid,ppid,stat,etime,command | grep -E "CLAUDE_CONFIG_DIR|claude|codex" | grep -F -e "$CLAUDE_DIR" -e "$CODEX_DIR" | grep -v grep | cut -c1-160
    sleep 1
  done
fi

printf 'python3 -c "import time; time.sleep(45)"\necho P10-DONE\n' > "$WORK/wait45.sh"
ASK='Run the shell command sh wait45.sh with your shell tool in the foreground, not in the background, wait for it, then say done.'
STEPS=$(python3 - "$KIND" <<'PY'
import json, sys
kind = sys.argv[1]
print(json.dumps([
  {"wait_for": "forshortcuts|automodeon|/effort|context left|tab to queue|Ask Codex|›", "timeout": 40}, {"sleep": 3},
  {"send": "__ASK__"}, {"sleep": 1}, {"key": "enter"}, {"mark": "sent"},
  {"wait_for": "wait45", "timeout": 60, "new_only": True}, {"sleep": 8}, {"mark": "end-owner"}]))
PY
)
STEPS=${STEPS/__ASK__/$ASK}

run_owner() { # <name> <cli...>: start the owner, return its pid
  local name=$1; shift
  ( cd "$WORK" && "$@" ) > "$RAW/$name.timeline.out" 2> "$RAW/$name.steps.log" &
  echo $!
}

S=$(python3 "$HERE/claude-hooks.py" "$RAW/claude-$KIND.hooks.log")
# The owner is pty-drive.py. In "normal" mode it reaches the end of its steps
# and closes the terminal (SIGHUP to the agent); in "force" mode it is killed.
if [ "$KIND" = normal ]; then
  STEPS_N=$(python3 -c 'import json,sys; s=json.loads(sys.argv[1]); print(json.dumps(s))' "$STEPS")
else
  STEPS_N=$(python3 -c 'import json,sys; s=json.loads(sys.argv[1]); s.append({"sleep": 120}); print(json.dumps(s))' "$STEPS")
fi
C_OWNER=$(run_owner "claude-$KIND" claude_env python3 "$HERE/pty-drive.py" "$RAW/claude-$KIND" "$STEPS_N" -- \
  claude --model "$CLAUDE_MODEL" --strict-mcp-config --allowedTools "Bash(sh wait45.sh)" --settings "$S")
X_OWNER=$(run_owner "codex-$KIND" codex_env python3 "$HERE/pty-drive.py" "$RAW/codex-$KIND" "$STEPS_N" -- \
  codex "${CODEX_CHEAP[@]}" "${CODEX_TUI[@]}" -c "projects.\"$WORK\".trust_level=\"trusted\"" --sandbox workspace-write --ask-for-approval never)
tally P10 claude "proxy $KIND: turn running sleep 45"
tally P10 codex "proxy $KIND: turn running sleep 45"

# Wait until both owners reached "end-owner".
for i in $(seq 1 90); do
  c=$(grep -c end-owner "$RAW/claude-$KIND.steps.log" 2>/dev/null); x=$(grep -c end-owner "$RAW/codex-$KIND.steps.log" 2>/dev/null)
  [ "${c:-0}" -ge 1 ] && [ "${x:-0}" -ge 1 ] && break
  sleep 1
done
# Only this run's processes: the agents under the two drivers, and the
# commands they started (wait45.sh), never the person's own sessions.
DRIVERS=$(pgrep -f "pty-drive.py $RAW/(claude|codex)-$KIND" | tr '\n' ' ')
AGENTS=$(for d in $DRIVERS; do pgrep -P "$d"; done | tr '\n' ' ')
agents() {
  for p in $AGENTS; do
    if kill -0 "$p" 2>/dev/null; then echo "  agent $p alive: $(ps -o stat=,command= -p "$p" | cut -c1-60)"; else echo "  agent $p gone"; fi
  done
  echo "  commands still running: $(pgrep -f 'time.sleep\(45\)' | wc -l | tr -d ' ')"
}
echo "== before ($KIND): drivers $DRIVERS"; agents
T_END=$(ms_now)
if [ "$KIND" = force ]; then
  kill -9 "$C_OWNER" "$X_OWNER" 2>/dev/null
  # The owners ran under env -i: kill the python children too.
  pkill -9 -f "pty-drive.py $RAW/claude-$KIND" ; pkill -9 -f "pty-drive.py $RAW/codex-$KIND"
fi
for t in 1 3 10 20; do
  sleep $(( t == 1 ? 1 : (t == 3 ? 2 : (t == 10 ? 7 : 10)) ))
  echo "== +${t}s"; agents
done
echo "== claude hooks after the end:"; jq -c "select(.t_ms >= $T_END) | {label, reason: .payload.reason}" "$RAW/claude-$KIND.hooks.log"
TX=$(ls -t "$CLAUDE_DIR/projects/$(echo "$WORK" | sed 's#[^A-Za-z0-9]#-#g')"/*.jsonl | head -1)
echo "== claude transcript, last records:"; tail -6 "$TX" | jq -c '{type, sub: (.subtype // .attachment.type // null), text: ((.message.content // "") | tostring | .[0:100])}'
RO=$(ls -t $(find "$CODEX_DIR/sessions" -name 'rollout-*.jsonl') | head -1)
echo "== codex rollout, last records:"; tail -6 "$RO" | jq -c '{type, pt: .payload.type, text: ((.payload.message // .payload.output // .payload.reason // "") | tostring | .[0:100])}'
