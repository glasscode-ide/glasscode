#!/bin/bash
# P02 The Codex brain profile (also P11's Codex wake numbers).
#
#   p02-codex-brain.sh <claude-config-dir> <codex-home> <scratch-dir> [probe|live]
#
# probe (no prompt): `codex debug prompt-input` with each candidate flag set,
#   listing the model-visible tools (code mode nests them in `exec`).
# live (2 prompts): one `codex exec` with the 4,000-character situation and
#   the stub MCP server, then `codex exec resume <id>` asking what it said.
. "$(dirname "$0")/lib.sh"
proof_args "$@"
MODE=${4:-probe}
MODE_TAG=""; [ "$MODE" = wake-trim ] && MODE_TAG="-trim"
WORK="$SCRATCH/work"
[ -d "$WORK/.git" ] || make_repo "$WORK"
PY=$(command -v python3)
STUB_LOG="$RAW/stub.log"

OFF=(--disable shell_tool --disable apps --disable plugins --disable multi_agent --disable memories)
# Found by this proof: without it the prompt lists the person's own skills
# (~/.agents/skills), whatever CODEX_HOME is.
SKILLS_OFF=(-c skills.include_instructions=false -c skills.bundled.enabled=false)
SANDBOX_C=(-c 'sandbox_mode="read-only"' -c 'sandbox_workspace_write.network_access=false')
MCP=(-c "mcp_servers.glasscode.command=\"$PY\""
     -c "mcp_servers.glasscode.args=[\"$HERE/stub-mcp.py\",\"$STUB_LOG\"]"
     -c 'mcp_servers.glasscode.default_tools_approval_mode="approve"'
     -c 'mcp_servers.glasscode.required=true')

probe() { # <name> <flags...>
  local name=$1; shift
  ( cd "$WORK" && codex_env codex debug prompt-input -c 'model="gpt-5.6-luna"' "$@" "hello" ) > "$RAW/pi-$name.json" 2> "$RAW/pi-$name.err"
  echo "== $name (exit $?)"
  python3 "$HERE/codex-tools.py" < "$RAW/pi-$name.json"
  [ -s "$RAW/pi-$name.err" ] && head -5 "$RAW/pi-$name.err"
}

if [ "$MODE" = probe ]; then
  probe default
  probe off "${OFF[@]}"
  probe off-sandbox "${OFF[@]}" "${SANDBOX_C[@]}"
  probe off-sandbox-mcp "${OFF[@]}" "${SANDBOX_C[@]}" "${MCP[@]}"
  probe off-sandbox-mcp-noskills "${OFF[@]}" "${SKILLS_OFF[@]}" "${SANDBOX_C[@]}" "${MCP[@]}"
  for f in "$RAW"/pi-*.json; do
    echo "$(basename "$f"): $(wc -c < "$f") chars, person's skills listed: $(grep -c '.agents/skills' "$f")"
  done
  exit 0
fi

# wake-trim (1 prompt): P11's re-measure of the Codex wake with the three trim
# keys of p11-codex-trim.sh, one `codex exec`, no resume.
TRIM=()
if [ "$MODE" = wake-trim ]; then
  printf 'You are the Glasscode manager brain. Use only the glasscode tools, which are MCP tools reachable from exec as tools.mcp__glasscode__<name>. Answer in one short line.\n' > "$SCRATCH/brain-instructions.md"
  TRIM=(-c include_permissions_instructions=false -c include_environment_context=false -c "model_instructions_file=\"$SCRATCH/brain-instructions.md\"")
fi

# live
SITUATION=$(python3 "$HERE/situation.py" 4000)
PROMPT="$SITUATION

You were woken with the situation above. Call the glasscode status tool once, then answer with one line: the marker it returned."
: > "$STUB_LOG"
T0=$(ms_now)
( cd "$WORK" && codex_env codex exec "${CODEX_CHEAP[@]}" "${OFF[@]}" "${SKILLS_OFF[@]}" ${TRIM[@]+"${TRIM[@]}"} -s read-only -c 'sandbox_workspace_write.network_access=false' \
    --ignore-user-config --ignore-rules --skip-git-repo-check --color never --json "${MCP[@]}" "$PROMPT" < /dev/null ) > "$RAW/exec$MODE_TAG.jsonl" 2> "$RAW/exec$MODE_TAG.err"
echo "exec exit $? in $(( $(ms_now) - T0 )) ms"
tally "$([ "$MODE" = wake-trim ] && echo P11 || echo P02)" codex "exec: wake with 4,000-character situation${MODE_TAG}"
TID=$(jq -r 'select(.type=="thread.started") | .thread_id' "$RAW/exec$MODE_TAG.jsonl" | head -1)
jq -c 'select(.type!="item.updated") | {type, item_type: .item.type, tool: (.item.tool // .item.server // null), status: .item.status, text: (.item.text // .item.result // null | tostring | .[0:120]), usage}' "$RAW/exec$MODE_TAG.jsonl"
echo "stub tool calls: $(grep -c '"tools/call"' "$STUB_LOG")"
RO=$(find "$CODEX_DIR/sessions" -name "*$TID*.jsonl" | head -1)
echo "per request (from the rollout):"
jq -c 'select(.type=="event_msg" and .payload.type=="token_count") | .payload.info | {last_total: .last_token_usage.total_tokens, input: .last_token_usage.input_tokens, cached: .last_token_usage.cached_input_tokens, running_total: .total_token_usage.total_tokens}' "$RO"
[ "$MODE" = wake-trim ] && exit 0

T0=$(ms_now)
( cd "$WORK" && codex_env codex exec "${CODEX_CHEAP[@]}" "${OFF[@]}" "${SKILLS_OFF[@]}" -s read-only -c 'sandbox_workspace_write.network_access=false' \
    --ignore-user-config --ignore-rules --skip-git-repo-check --color never --json "${MCP[@]}" \
    resume "$TID" "First, what marker did you answer with in your previous turn? Then try once to create the file probe.txt containing the word x with apply_patch, and say whether it worked. Reply in two short lines." < /dev/null ) > "$RAW/resume.jsonl" 2> "$RAW/resume.err"
echo "resume exit $? in $(( $(ms_now) - T0 )) ms"
tally P02 codex "exec resume: what did you answer, and try apply_patch"
echo "probe.txt written: $([ -e "$WORK/probe.txt" ] && echo yes || echo no)"
jq -c 'select(.type!="item.updated") | {type, item_type: .item.type, text: (.item.text // null | tostring | .[0:120]), usage, thread_id}' "$RAW/resume.jsonl"
head -5 "$RAW/exec.err" "$RAW/resume.err"
