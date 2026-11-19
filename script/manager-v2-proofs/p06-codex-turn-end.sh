#!/bin/bash
# P06 Codex turn ends and hooks without the trust bypass; also P03's Codex half.
#
#   p06-codex-turn-end.sh <claude-config-dir> <codex-home> <scratch-dir> trust
#   p06-codex-turn-end.sh <claude-config-dir> <codex-home> <scratch-dir> live start|resume
#
# trust (no prompt): user hooks.json and -c session-flag hooks, and the
#   hooks.state trust entry, read back with app-server `hooks/list`.
# live start (1 prompt): an interactive TUI session in a pseudo-terminal, as
#   agents::launch starts it, with -c developer_instructions (DEV-V1), Glasscode's
#   hooks trusted through hooks.state (no bypass), a "person's" notify program
#   in config.toml and Glasscode's notify program as -c notify. It records when
#   the Stop hook, the notify program and the rollout's task_complete see the
#   turn end.
# live resume (1 prompt): `codex … resume <thread>` with DEV-V2, same question.
# The account's config.toml and hooks.json are written by this script and
# removed at the end (they belong to the scratch account).
. "$(dirname "$0")/lib.sh"
proof_args "$@"
MODE=${4:-trust}
PHASE=${5:-start}
WORK="$SCRATCH/work"
[ -d "$WORK/.git" ] || make_repo "$WORK"
CONF="$CODEX_DIR/config.toml"

FLAGS=()
while IFS= read -r l; do FLAGS+=(-c "$l"); done < <(python3 "$HERE/codex-hooks.py" flags "$RAW/hooks.log" flag)
python3 "$HERE/codex-hooks.py" json "$RAW/hooks.log" user > "$CODEX_DIR/hooks.json"

hooks_list() { # <flags...>: one JSON line per hook
  : > "$RAW/as-list.jsonl"
  codex_env python3 "$HERE/appserver-client.py" "$RAW/as-list.jsonl" \
    "[{\"request\":\"initialize\",\"params\":{\"clientInfo\":{\"name\":\"glasscode-proofs\",\"version\":\"0.0.1\"}}},{\"notify\":\"initialized\"},{\"request\":\"hooks/list\",\"params\":{\"cwds\":[\"$WORK\"]},\"save\":\"hooks\"}]" \
    --stdio -- codex app-server "$@" 2>/dev/null | jq -c '.saved.hooks.data[0].hooks[]'
}

trust_entries() { # <hooks-list lines> <source>: hooks.state TOML for every hook of that source
  jq -r --arg s "$2" 'select(.source==$s) | "[hooks.state.\"\(.key)\"]\ntrusted_hash = \"\(.currentHash)\"\n"' <<< "$1"
}

if [ "$MODE" = trust ]; then
  rm -f "$CONF"
  ALL=$(hooks_list "${FLAGS[@]}")
  pick() { jq -r --arg s "$1" --arg f "$2" 'select(.source==$s and .eventName=="stop") | .[$f]' <<< "$ALL"; }
  UK=$(pick user key); UH=$(pick user currentHash); FK=$(pick sessionFlags key); FH=$(pick sessionFlags currentHash)
  show() { hooks_list "$@" | jq -c 'select(.eventName=="stop") | {source, trustStatus}'; }
  echo "== as written"; show "${FLAGS[@]}"
  echo "== trust by a -c session flag"; show "${FLAGS[@]}" -c "hooks.state.\"$FK\".trusted_hash=\"$FH\"" -c "hooks.state.\"$UK\".trusted_hash=\"$UH\""
  printf '[hooks.state."%s"]\ntrusted_hash = "%s"\n' "$UK" "$UH" > "$CONF"
  echo "== trust of the hooks.json hook in config.toml"; show "${FLAGS[@]}"
  sed -i '' 's/user-Stop/user-Stop-changed/' "$CODEX_DIR/hooks.json"
  echo "== after its command changed"; show "${FLAGS[@]}"
  sed -i '' 's/user-Stop-changed/user-Stop/' "$CODEX_DIR/hooks.json"
  printf '[hooks.state."%s"]\ntrusted_hash = "%s"\n' "$FK" "$FH" >> "$CONF"
  echo "== also the session-flag hook in config.toml"; show "${FLAGS[@]}"
  hooks_list "${FLAGS[@]}" | jq -c '{source, eventName, key}' | head -1
  rm -f "$CONF" "$CODEX_DIR/hooks.json"
  exit 0
fi

# live: trust Glasscode's hooks (the hooks.json ones) the way Glasscode would.
rm -f "$CONF"
ALL=$(hooks_list)
{
  printf 'notify = ["%s", "%s", "%s", "person"]\n\n' "$(command -v python3)" "$HERE/notify-log.py" "$RAW/notify.log"
  printf '[projects."%s"]\ntrust_level = "trusted"\n\n' "$WORK"
  trust_entries "$ALL" user
} > "$CONF"
echo "trust: $(hooks_list | jq -r '.trustStatus' | sort | uniq -c | tr '\n' ' ')"
NOTIFY=(-c "notify=[\"$(command -v python3)\",\"$HERE/notify-log.py\",\"$RAW/notify.log\",\"glasscode\"]")

Q='Which DEV-V marker is in your developer instructions right now? Do not run any command. Reply exactly as: dev=<markers>'
STEPS=$(python3 - "$RAW/hooks.log" "$Q" "$PHASE" <<'PY'
import json, sys
log, q, phase = sys.argv[1:4]
n = 1 if phase == "start" else 2
print(json.dumps([
  {"wait_for": "›|context left|Ask Codex|esc to|Tip", "timeout": 40}, {"sleep": 4},
  {"send": q}, {"sleep": 1}, {"key": "enter"}, {"mark": "sent"},
  {"wait_file": log, "contains": "\"label\": \"user-Stop\"", "min_count": n, "timeout": 180},
  {"sleep": 3}, {"key": "ctrl-c"}, {"sleep": 1}, {"key": "ctrl-c"}, {"wait_exit": 15}]))
PY
)
python3 "$HERE/transcript-watch.py" "$CODEX_DIR/sessions/*/*/*/rollout-*.jsonl" "$RAW/$PHASE.rollout-watch.jsonl" 300 &
WATCH=$!
if [ "$PHASE" = start ]; then
  ( cd "$WORK" && codex_env python3 "$HERE/pty-drive.py" "$RAW/$PHASE" "$STEPS" -- \
      codex -c 'developer_instructions="Glasscode brief marker: DEV-V1"' "${CODEX_CHEAP[@]}" "${NOTIFY[@]}" \
      --sandbox workspace-write --ask-for-approval on-request ) > "$RAW/$PHASE.timeline.out" 2> "$RAW/$PHASE.steps.log"
  tally P06 codex "live start: TUI turn (also P03 Codex start)"
else
  TID=$(jq -r 'select(.label=="user-SessionStart") | .payload.session_id' "$RAW/hooks.log" | head -1)
  ( cd "$WORK" && codex_env python3 "$HERE/pty-drive.py" "$RAW/$PHASE" "$STEPS" -- \
      codex -c 'developer_instructions="Glasscode brief marker: DEV-V2"' "${CODEX_CHEAP[@]}" "${NOTIFY[@]}" \
      --sandbox workspace-write --ask-for-approval on-request resume "$TID" ) > "$RAW/$PHASE.timeline.out" 2> "$RAW/$PHASE.steps.log"
  tally P06 codex "live resume: TUI turn (also P03 Codex resume)"
fi
sleep 2; kill $WATCH 2>/dev/null
cat "$RAW/$PHASE.steps.log"
echo "== hooks"; jq -c '{label, t_ms, source: .payload.source, turn: .payload.turn_id, lam: (.payload.last_assistant_message // null)}' "$RAW/hooks.log"
echo "== notify"; jq -c '{t_ms, who, type: .payload.type, turn: (.payload["turn-id"] // null), last: (.payload["last-assistant-message"] // null)}' "$RAW/notify.log" 2>/dev/null
echo "== rollout task_complete / turn_aborted"; jq -c 'select(.subtype=="task_complete" or .subtype=="turn_aborted" or .subtype=="user_message") | {t_ms, subtype}' "$RAW/$PHASE.rollout-watch.jsonl"
RO=$(ls -t $(find "$CODEX_DIR/sessions" -name 'rollout-*.jsonl') | head -1)
echo "== developer messages in the rollout"; grep -o 'DEV-V[0-9]' "$RO" | sort | uniq -c
rm -f "$CONF" "$CODEX_DIR/hooks.json"
