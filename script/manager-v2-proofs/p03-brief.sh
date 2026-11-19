#!/bin/bash
# P03 Delivering the memory brief to interactive sessions (Claude phases).
# The same sessions carry P05's hooks (every event is logged) and P12's
# transcript checks. Codex phases are in p06-codex-turn-end.sh, which shares
# its sessions with P03's Codex half.
#
#   p03-brief.sh <claude-config-dir> <codex-home> <scratch-dir> <phase...>
#
# Phases, run in this order (each is one interactive session in a PTY):
#   start      --session-id S, --append-system-prompt APPEND-V1, hook brief HOOK-V1   (1 prompt)
#   resume     --resume S, APPEND-V2, HOOK-V2                                          (1 prompt)
#   snapoff    --resume S --system-prompt-snapshot off, APPEND-V3, HOOK-V3; then the
#              brief becomes HOOK-V4, /compact, and the question again                 (3 prompts: ask, compact, ask)
. "$(dirname "$0")/lib.sh"
proof_args "$@"
shift 3
SID=${SID:-33333333-0000-4000-8000-000000000003}
WORK="$SCRATCH/work"
[ -d "$WORK/.git" ] || make_repo "$WORK"
claude_trust "$WORK"
TX_DIR="$CLAUDE_DIR/projects/$(echo "$WORK" | sed 's#[^A-Za-z0-9]#-#g')"
TX="$TX_DIR/$SID.jsonl"

Q='Answer from your instructions and context, not from earlier answers in this chat: which APPEND-V marker is in your system prompt now, and which HOOK-V markers were given to you as SessionStart hook context? Reply exactly as: system=<markers or none>; hook=<markers or none>'

steps_ask() { # <hook-log> <stop-count>
  python3 - "$1" "$2" "$Q" <<'PY'
import json, sys
log, n, q = sys.argv[1], int(sys.argv[2]), sys.argv[3]
print(json.dumps([
  {"wait_for": "forshortcuts|automodeon|/effort", "timeout": 40}, {"sleep": 3},
  {"send": q}, {"sleep": 1}, {"key": "enter"}, {"mark": "sent"},
  {"wait_file": log, "contains": "\"label\": \"Stop\"", "min_count": n, "timeout": 180},
  {"sleep": 2}, {"send": "/exit"}, {"sleep": 1.5}, {"key": "enter"}, {"wait_exit": 20}]))
PY
}

run_phase() { # <name> <steps-json> <claude args...>
  local name=$1 steps=$2; shift 2
  python3 "$HERE/transcript-watch.py" "$TX" "$RAW/$name.tx-watch.jsonl" 400 &
  local watch=$!
  ( cd "$WORK" && claude_env python3 "$HERE/pty-drive.py" "$RAW/$name" "$steps" -- claude "$@" ) > "$RAW/$name.timeline.out" 2> "$RAW/$name.steps.log"
  kill $watch 2>/dev/null
  echo "== $name"; cat "$RAW/$name.steps.log"
  jq -c '{label, t_ms, source: .payload.source, tx: .transcript}' "$RAW/$name.hooks.log" 2>/dev/null
  echo "-- answers in the transcript:"
  python3 - "$TX" <<'PY'
import json, sys
for line in open(sys.argv[1]):
    j = json.loads(line)
    if j.get("type") == "assistant":
        for c in j["message"].get("content", []):
            if c.get("type") == "text":
                print("  assistant:", c["text"][:200])
PY
}

for phase in "$@"; do
  case $phase in
    start)
      echo "Glasscode hook brief marker: HOOK-V1" > "$SCRATCH/brief.txt"
      S=$(python3 "$HERE/claude-hooks.py" "$RAW/start.hooks.log" --brief "$SCRATCH/brief.txt")
      run_phase start "$(steps_ask "$RAW/start.hooks.log" 1)" --session-id "$SID" --model "$CLAUDE_MODEL" --strict-mcp-config \
        --append-system-prompt "Glasscode brief marker: APPEND-V1" --settings "$S"
      tally P03 claude "start: ask"
      ;;
    resume)
      echo "Glasscode hook brief marker: HOOK-V2" > "$SCRATCH/brief.txt"
      S=$(python3 "$HERE/claude-hooks.py" "$RAW/resume.hooks.log" --brief "$SCRATCH/brief.txt")
      run_phase resume "$(steps_ask "$RAW/resume.hooks.log" 1)" --resume "$SID" --model "$CLAUDE_MODEL" --strict-mcp-config \
        --append-system-prompt "Glasscode brief marker: APPEND-V2" --settings "$S"
      tally P03 claude "resume: ask"
      ;;
    snapoff)
      echo "Glasscode hook brief marker: HOOK-V3" > "$SCRATCH/brief.txt"
      LOG="$RAW/snapoff.hooks.log"
      S=$(python3 "$HERE/claude-hooks.py" "$LOG" --brief "$SCRATCH/brief.txt")
      STEPS=$(python3 - "$LOG" "$Q" "$SCRATCH/brief.txt" <<'PY'
import json, sys
log, q, brief = sys.argv[1], sys.argv[2], sys.argv[3]
stop = "\"label\": \"Stop\""
print(json.dumps([
  {"wait_for": "forshortcuts|automodeon|/effort", "timeout": 40}, {"sleep": 3},
  {"send": q}, {"sleep": 1}, {"key": "enter"}, {"mark": "sent-1"},
  {"wait_file": log, "contains": stop, "min_count": 1, "timeout": 180}, {"sleep": 2},
  {"write_file": brief, "text": "Glasscode hook brief marker: HOOK-V4\n"},
  {"send": "/compact"}, {"sleep": 1.5}, {"key": "enter"}, {"mark": "compact"},
  {"wait_file": log, "contains": "\"label\": \"PostCompact\"", "min_count": 1, "timeout": 240}, {"sleep": 4},
  {"send": q}, {"sleep": 1}, {"key": "enter"}, {"mark": "sent-2"},
  {"wait_file": log, "contains": stop, "min_count": 2, "timeout": 180}, {"sleep": 2},
  {"send": "/exit"}, {"sleep": 1.5}, {"key": "enter"}, {"wait_exit": 20}]))
PY
)
      run_phase snapoff "$STEPS" --resume "$SID" --model "$CLAUDE_MODEL" --strict-mcp-config --system-prompt-snapshot off \
        --append-system-prompt "Glasscode brief marker: APPEND-V3" --settings "$S"
      tally P03 claude "snapoff: ask"
      tally P03 claude "snapoff: /compact (one summary request)"
      tally P03 claude "snapoff: ask after compact"
      ;;
    *) echo "unknown phase $phase" >&2; exit 2 ;;
  esac
done
cp "$TX" "$RAW/transcript.jsonl" 2>/dev/null
