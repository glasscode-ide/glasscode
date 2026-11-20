#!/bin/bash
# P13 isolation, no prompt: where does a plain Codex TUI (no --remote) run its
# server, and what does killing its process group leave?
#
#   p13-plain-tui.sh <claude-config-dir> <codex-home> <scratch-dir>
#
# Starts the TUI for 10 s in the account folder <codex-home> (A) and in an empty
# account folder <scratch>/codex-home-b (B), and lists app-server processes
# and new sockets in /private/tmp/codex-daemon-<uid>/ while it runs. Then
# starts it again in A and sends SIGTERM to its process group. Processes of
# other Codex programs on the Mac (such as the ChatGPT app's) are left out of
# the list and never touched.
. "$(dirname "$0")/lib.sh"
proof_args "$@"
WORK="$SCRATCH/work"
[ -d "$WORK/.git" ] || make_repo "$WORK"
SOCKDIR=/private/tmp/codex-daemon-$(id -u)
mkdir -p "$SCRATCH/codex-home-b"
sockets() { ls "$SOCKDIR" 2>/dev/null | grep -v '\.lock$' | sort; }
BEFORE=$(sockets)
new_sockets() { comm -13 <(echo "$BEFORE") <(sockets) | grep -c .; }
appservers() { ps -ax -o pid=,ppid=,pgid=,command= | grep -v grep | grep codex | grep app-server | grep -v ChatGPT.app | awk '{print $1 "/" $2 "/" $3}' | tr '\n' ' '; }
start_tui() { # <codex-home> <name> <extra steps>: runs in the background
  ( cd "$WORK" && CODEX_DIR=$1 codex_env python3 "$HERE/pty-drive.py" "$RAW/$2" \
      "[{\"sleep\":10}$3,{\"key\":\"ctrl-c\"},{\"wait_exit\":10}]" -- codex "${CODEX_TUI[@]}" ) > /dev/null 2>&1 &
}
tui_pid() { pgrep -P "$(pgrep -f "pty-drive.py $RAW/$1" | head -1)" | head -1; }
echo "sockets before: $(echo "$BEFORE" | grep -c .)"
for acct in a b; do
  home=$CODEX_DIR; [ $acct = b ] && home=$SCRATCH/codex-home-b
  start_tui "$home" "plain-$acct" ""
  sleep 6
  T=$(tui_pid "plain-$acct")
  echo "== account $acct, TUI running: pid $T, pgid $(ps -o pgid= -p "$T" | tr -d ' '), children: [$(pgrep -P "$T" | tr '\n' ' ')], app-servers (pid/ppid/pgid): [$(appservers)], new sockets: $(new_sockets)"
  while pgrep -f "pty-drive.py $RAW/plain-$acct" >/dev/null; do sleep 1; done
  echo "   after it quit: app-servers [$(appservers)], new sockets kept: $(new_sockets)"
done
start_tui "$CODEX_DIR" pg-a ',{"sleep":30}'
sleep 6
T=$(tui_pid pg-a); PG=$(ps -o pgid= -p "$T" | tr -d ' ')
echo "== kill the process group: TUI $T, pgid $PG, members [$(ps -ax -o pid=,pgid= | awk -v g="$PG" '$2==g {print $1}' | tr '\n' ' ')]"
kill -TERM -"$PG"
sleep 3
echo "   after kill -TERM -$PG: TUI alive: $(kill -0 "$T" 2>/dev/null && echo yes || echo no); app-servers [$(appservers)]; new sockets: $(new_sockets)"
while pgrep -f "pty-drive.py $RAW/pg-a" >/dev/null; do sleep 1; done
