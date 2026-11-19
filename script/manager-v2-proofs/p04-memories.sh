#!/bin/bash
# P04 The native memory switches.
#
#   p04-memories.sh <claude-config-dir> <codex-home> <scratch-dir> codex
#   p04-memories.sh <claude-config-dir> <codex-home> <scratch-dir> claude
#
# codex (no prompt): plants a marked memory in a copy-free way (a
#   $CODEX_HOME/memories/ folder with memory_summary.md and MEMORY.md; the
#   folder is removed again), then reads `codex debug prompt-input` with the
#   memories feature on and each switch, and reports whether the marker is in
#   the model-visible prompt.
# claude (1 prompt + 1 interactive /memory look, no prompt): asks a session
#   with --settings {"autoMemoryDirectory": <scratch>/agent-notes/p/claude}
#   to remember a fact, then checks where it was written and what /memory
#   shows; then /memory in a session with {"autoMemoryEnabled": false}.
. "$(dirname "$0")/lib.sh"
proof_args "$@"
WORK="$SCRATCH/work"
[ -d "$WORK/.git" ] || make_repo "$WORK"

if [ "${4:-codex}" = codex ]; then
  MEM="$CODEX_DIR/memories"
  [ -e "$MEM" ] && { echo "$MEM exists already; not touching it" >&2; exit 1; }
  mkdir -p "$MEM"
  printf '# Memory summary\n- The project codename is CODEX-MEM-MARKER-41.\n' > "$MEM/memory_summary.md"
  printf '# MEMORY\n- The project codename is CODEX-MEM-MARKER-41.\n' > "$MEM/MEMORY.md"
  probe() { # <name> <flags...>
    local name=$1; shift
    ( cd "$WORK" && codex_env codex debug prompt-input -c 'model="gpt-5.6-luna"' "$@" hello ) > "$RAW/pi-$name.json" 2> "$RAW/pi-$name.err"
    printf '%-58s marker in prompt: %s\n' "$name" "$(grep -c 'CODEX-MEM-MARKER-41' "$RAW/pi-$name.json")"
  }
  probe "feature off (default)"
  probe "feature on" --enable memories
  probe "feature on, use_memories=false" --enable memories -c memories.use_memories=false
  probe "feature on, use+generate=false" --enable memories -c memories.use_memories=false -c memories.generate_memories=false
  probe "feature on, disable_on_external_context=true" --enable memories -c memories.disable_on_external_context=true
  probe "feature on, --disable memories again" --enable memories --disable memories
  rm -rf "$MEM"
  exit 0
fi

# claude
NOTES="$SCRATCH/agent-notes/p/claude"
mkdir -p "$NOTES"
claude_trust "$WORK"
S="{\"autoMemoryDirectory\": \"$NOTES\"}"
ASK='Remember this for future sessions in your auto memory: the scratch project codename is CLAUDE-MEM-MARKER-17. Save it now, then reply saved.'
STEPS='[{"wait_for":"forshortcuts|automodeon|/effort","timeout":40},{"sleep":3},{"send":"__ASK__"},{"sleep":1},{"key":"enter"},{"wait_for":"(?i)saved","timeout":120,"new_only":true},{"sleep":3},{"send":"/memory"},{"sleep":1.5},{"key":"enter"},{"sleep":4},{"key":"esc"},{"sleep":1},{"send":"/exit"},{"sleep":1.5},{"key":"enter"},{"wait_exit":15}]'
STEPS=${STEPS/__ASK__/$ASK}
( cd "$WORK" && claude_env python3 "$HERE/pty-drive.py" "$RAW/claude-redirect" "$STEPS" -- \
    claude --model "$CLAUDE_MODEL" --strict-mcp-config --settings "$S" ) > /dev/null 2> "$RAW/claude-redirect.steps.log"
tally P04 claude "autoMemoryDirectory: remember a fact"
echo "== files with the marker:"
grep -rl CLAUDE-MEM-MARKER-17 "$SCRATCH/agent-notes" "$CLAUDE_DIR/projects" 2>/dev/null | grep -v '\.jsonl$' | sed "s#$HOME#~#g"
echo "== permission prompts on screen: $(grep -c -i 'Do you want to' "$RAW/claude-redirect.txt")"
echo "== /memory screen (redirect):"; python3 - "$RAW/claude-redirect.txt" <<'PY'
import sys
t = open(sys.argv[1]).read()
i = t.rfind("memory")
print("\n".join(l for l in t[max(0, t.find("/memory")):][:1500].splitlines() if l.strip())[:1200])
PY

S2='{"autoMemoryEnabled": false}'
STEPS2='[{"wait_for":"forshortcuts|automodeon|/effort","timeout":40},{"sleep":3},{"send":"/memory"},{"sleep":1.5},{"key":"enter"},{"sleep":4},{"key":"esc"},{"sleep":1},{"send":"/exit"},{"sleep":1.5},{"key":"enter"},{"wait_exit":15}]'
( cd "$WORK" && claude_env python3 "$HERE/pty-drive.py" "$RAW/claude-off" "$STEPS2" -- \
    claude --model "$CLAUDE_MODEL" --strict-mcp-config --settings "$S2" ) > /dev/null 2> "$RAW/claude-off.steps.log"
echo "== /memory screen (autoMemoryEnabled false):"; python3 - "$RAW/claude-off.txt" <<'PY'
import sys
t = open(sys.argv[1]).read()
print("\n".join(l for l in t[max(0, t.find("/memory")):][:1500].splitlines() if l.strip())[:1200])
PY
