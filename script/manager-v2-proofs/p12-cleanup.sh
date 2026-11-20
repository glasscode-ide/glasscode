#!/bin/bash
# P12 How long Claude keeps transcripts. No prompt.
#
#   p12-cleanup.sh <claude-config-dir> <codex-home> <scratch-dir>
#
# Plants transcripts 60 days old (by modification time) in two project folders
# of the scratch account, then starts sessions that send no user line, with
# and without the sweep marker <config-dir>/.last-cleanup and with and without
# --settings {"cleanupPeriodDays": 3650}. After each, it lists which planted
# files are left, in the session's own project (A) and in another one (B).
. "$(dirname "$0")/lib.sh"
proof_args "$@"
PROJ="$CLAUDE_DIR/projects"
A="$SCRATCH/proj-a"; B="$SCRATCH/proj-b"
make_repo "$A"; make_repo "$B"; claude_trust "$A"; claude_trust "$B"
slug() { echo "$1" | sed 's#[^A-Za-z0-9]#-#g'; }
plant() {
  for d in "$A" "$B"; do
    mkdir -p "$PROJ/$(slug "$d")"
    for n in old-1 old-2; do
      f="$PROJ/$(slug "$d")/00000000-0000-4000-8000-0000012a00${n#old-}.jsonl"
      echo '{"type":"user","message":{"role":"user","content":"planted"}}' > "$f"
      touch -t "$(date -v-60d +%Y%m%d%H%M)" "$f"
    done
  done
}
left() { echo "  left: A $(ls "$PROJ/$(slug "$A")" 2>/dev/null | grep -c 0000012a00) of 2, B $(ls "$PROJ/$(slug "$B")" 2>/dev/null | grep -c 0000012a00) of 2"; }
run_p() { # <dir> [settings]
  local s=${2:-'{}'}
  ( cd "$1" && (sleep 6) | claude_env claude -p --input-format stream-json --output-format stream-json --verbose \
      --model "$CLAUDE_MODEL" --strict-mcp-config --settings "$s" ) > /dev/null 2>&1
}

MARK="$CLAUDE_DIR/.last-cleanup"
echo "the sweep marker before: $(cat "$MARK" 2>/dev/null || echo none)"
plant; echo "planted, 60 days old, in projects A and B:"; left
echo "1. marker removed; -p in A with cleanupPeriodDays 3650"; rm -f "$MARK"; run_p "$A" '{"cleanupPeriodDays": 3650}'; sleep 2; left
echo "   marker now: $(cat "$MARK" 2>/dev/null || echo none)"
echo "2. marker kept; -p in A with the default (30 days)"; run_p "$A"; sleep 2; left
echo "3. marker removed; -p in A with the default"; rm -f "$MARK"; run_p "$A"; sleep 2; left
plant
echo "4. marker removed; interactive in B with cleanupPeriodDays 3650, quit after 8 s"; rm -f "$MARK"
( cd "$B" && claude_env python3 "$HERE/pty-drive.py" "$RAW/p12-interactive" \
    '[{"wait_for":"forshortcuts|automodeon|/effort","timeout":40},{"sleep":8},{"send":"/exit"},{"sleep":1.5},{"key":"enter"},{"wait_exit":15}]' \
    -- claude --model "$CLAUDE_MODEL" --strict-mcp-config --settings '{"cleanupPeriodDays": 3650}' ) > /dev/null 2>&1
sleep 2; left
rm -f "$PROJ/$(slug "$A")"/*0000012a00* "$PROJ/$(slug "$B")"/*0000012a00*
