#!/bin/bash
# P11 follow-up: how small can the Codex brain's own prompt get? No prompt:
# `codex debug prompt-input` with the brain flags of P02 and each trim
# candidate, counting the characters of each model-visible item (about four
# characters to a token for this text).
#
#   p11-codex-trim.sh <claude-config-dir> <codex-home> <scratch-dir>
. "$(dirname "$0")/lib.sh"
proof_args "$@"
WORK="$SCRATCH/work"
[ -d "$WORK/.git" ] || make_repo "$WORK"
PY=$(command -v python3)
BRAIN=(--disable shell_tool --disable apps --disable plugins --disable multi_agent --disable memories
       -c skills.include_instructions=false -c skills.bundled.enabled=false
       -c 'sandbox_mode="read-only"'
       -c "mcp_servers.glasscode.command=\"$PY\"" -c "mcp_servers.glasscode.args=[\"$HERE/stub-mcp.py\",\"$RAW/stub.log\"]"
       -c 'mcp_servers.glasscode.default_tools_approval_mode="approve"' -c 'mcp_servers.glasscode.required=true')
printf 'You are the Glasscode manager brain. Use only the glasscode tools. Answer in one short line.\n' > "$SCRATCH/brain-instructions.md"
measure() { # <name> <extra flags...>
  local name=$1; shift
  ( cd "$WORK" && codex_env codex debug prompt-input -c 'model="gpt-5.6-luna"' "${BRAIN[@]}" "$@" "hello" ) > "$RAW/trim-$name.json" 2> "$RAW/trim-$name.err" || { echo "$name: failed: $(head -2 "$RAW/trim-$name.err")"; return; }
  python3 - "$RAW/trim-$name.json" "$name" <<'PY2'
import json, sys
items = json.load(open(sys.argv[1]))
sizes = [len(json.dumps(i)) for i in items]
print("%-34s total %6d chars (~%5d tokens)  items: %s" % (sys.argv[2], sum(sizes), sum(sizes) // 4, " ".join(str(s) for s in sizes)))
PY2
}
measure "P02 brain profile"
measure "+ include_permissions_instructions=false" -c include_permissions_instructions=false
measure "+ include_environment_context=false" -c include_environment_context=false
measure "+ include_apps_instructions=false" -c include_apps_instructions=false
measure "+ model_instructions_file (1 line)" -c "model_instructions_file=\"$SCRATCH/brain-instructions.md\""
measure "all four" -c include_permissions_instructions=false -c include_environment_context=false -c include_apps_instructions=false -c "model_instructions_file=\"$SCRATCH/brain-instructions.md\""
