#!/bin/bash
# P01 The Claude brain profile (also P11's Claude wake numbers).
#
#   p01-claude-brain.sh <claude-config-dir> <codex-home> <scratch-dir> [variant...]
#
# Variants: none (--tools ""), toolsearch (--tools ToolSearch). Default: both.
# MODEL=<alias> picks the model (default haiku); MODEL= (empty) uses the
# account's default model, for P11.
# Live prompts: one per variant.
. "$(dirname "$0")/lib.sh"
proof_args "$@"
shift 3
VARIANTS=${*:-none toolsearch}
MODEL=${MODEL-$CLAUDE_MODEL}

make_repo "$SCRATCH/work"
cat > "$SCRATCH/mcp.json" <<EOF
{"mcpServers":{"glasscode":{"command":"$(command -v python3)","args":["$HERE/stub-mcp.py","$RAW/stub.log"]}}}
EOF

SITUATION=$(python3 "$HERE/situation.py" 4000)
PROMPT="$SITUATION

You were woken with the situation above. Call the glasscode status tool once, then answer with one line: the marker it returned."
MSGS=$(python3 -c 'import json,sys; print(json.dumps([sys.argv[1]]))' "$PROMPT")

for v in $VARIANTS; do
  case $v in
    none) TOOLS="" ;;
    toolsearch) TOOLS="ToolSearch" ;;
    *) echo "unknown variant $v" >&2; exit 2 ;;
  esac
  tag="$v-${MODEL:-default}"
  model_args=()
  [ -n "$MODEL" ] && model_args=(--model "$MODEL")
  : > "$RAW/stub.log"
  ( cd "$SCRATCH/work" && claude_env python3 "$HERE/stream-drive.py" "$RAW/$tag.jsonl" "$MSGS" -- \
      claude -p --input-format stream-json --output-format stream-json --verbose \
      --include-partial-messages \
      --restricted --tools "$TOOLS" --strict-mcp-config --mcp-config "$SCRATCH/mcp.json" \
      --allowedTools "mcp__glasscode__*" --permission-mode dontAsk --permission-prompts none \
      --disable-slash-commands ${model_args[@]+"${model_args[@]}"} ) > "$RAW/$tag.summary.json"
  tally P01 claude "$tag"
  cp "$RAW/stub.log" "$RAW/$tag.stub.log"
  echo "== $tag"
  jq -c '{init_tools: .init.tools, mcp: .init.mcp_servers, apiKeySource: .init.apiKeySource, model: .init.model, version: .init.claude_code_version, init_after_spawn_ms, turns: [.turns[] | {first_token_ms, result_ms, subtype: .result.subtype, is_error: .result.is_error, duration_ms: .result.duration_ms, duration_api_ms: .result.duration_api_ms, num_turns: .result.num_turns, cost: .result.total_cost_usd, usage: .result.usage, result: .result.result}], exit}' "$RAW/$tag.summary.json"
  echo "stub tool calls: $(grep -c '"tools/call"' "$RAW/$tag.stub.log")"
  [ -s "$RAW/$tag.jsonl.err" ] && { echo "stderr:"; head -20 "$RAW/$tag.jsonl.err"; }
done
