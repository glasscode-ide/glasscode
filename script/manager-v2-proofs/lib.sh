# Shared helpers for the Manager v2 proofs (M01). Source it; do not run it.
#
# Every proof script takes its scratch paths as arguments:
#   pNN-*.sh <claude-config-dir> <codex-home> <scratch-dir>
# The two account folders are signed in once for the proofs (never the person's
# own ~/.claude or ~/.codex). <scratch-dir> is created if missing and holds the
# proof's repositories, homes and raw captures.
#
# Agents are launched with `env -i`, so nothing of the calling shell leaks in:
# no API key, no CLAUDECODE / CLAUDE_CODE_* variables of a parent session, no
# messaging socket of a parent team.

set -u

proof_args() {
  if [ $# -lt 3 ]; then
    echo "usage: $0 <claude-config-dir> <codex-home> <scratch-dir>" >&2
    exit 2
  fi
  CLAUDE_DIR=$(cd "$1" && pwd)
  CODEX_DIR=$(cd "$2" && pwd)
  mkdir -p "$3"
  SCRATCH=$(cd "$3" && pwd)
  RAW="$SCRATCH/raw"
  mkdir -p "$RAW"
  HERE=$(cd "$(dirname "${BASH_SOURCE[1]:-$0}")" && pwd)
  export CLAUDE_DIR CODEX_DIR SCRATCH RAW HERE
}

# The PATH the agents see: the CLIs, git, gh, python3, node. Nothing from the
# caller's session.
clean_path() {
  local p="/usr/bin:/bin:/usr/sbin:/sbin"
  for d in /opt/homebrew/bin /usr/local/bin "$HOME/.cargo/bin"; do
    [ -d "$d" ] && p="$d:$p"
  done
  echo "$p"
}

# claude_env <cmd...>: run with only the scratch Claude account.
# DISABLE_AUTOUPDATER keeps a proof run from updating the installed CLI.
claude_env() {
  env -i HOME="${PROOF_HOME:-$HOME}" USER="$USER" LOGNAME="${LOGNAME:-$USER}" \
    SHELL=/bin/zsh TERM="${TERM_OVERRIDE:-xterm-256color}" LANG=en_US.UTF-8 \
    TMPDIR="${TMPDIR:-/tmp}" PATH="${PROOF_PATH:-$(clean_path)}" \
    CLAUDE_CONFIG_DIR="$CLAUDE_DIR" DISABLE_AUTOUPDATER=1 \
    ${EXTRA_ENV:-} "$@"
}

# codex_env <cmd...>: run with only the scratch Codex account.
codex_env() {
  env -i HOME="${PROOF_HOME:-$HOME}" USER="$USER" LOGNAME="${LOGNAME:-$USER}" \
    SHELL=/bin/zsh TERM="${TERM_OVERRIDE:-xterm-256color}" LANG=en_US.UTF-8 \
    TMPDIR="${TMPDIR:-/tmp}" PATH="${PROOF_PATH:-$(clean_path)}" \
    CODEX_HOME="$CODEX_DIR" \
    ${EXTRA_ENV:-} "$@"
}

# The cheapest models (M01 budget rule).
CLAUDE_MODEL=${CLAUDE_MODEL:-haiku}
CODEX_MODEL=${CODEX_MODEL:-gpt-5.6-luna}
CODEX_CHEAP=(-m "$CODEX_MODEL" -c 'model_reasoning_effort="low"')

# tally <proof> <cli> <what>: one line per submitted user turn (a "prompt").
# The file is shared by all proofs: $PROOF_TALLY, else <scratch>/raw/prompts.tsv.
tally() {
  local f="${PROOF_TALLY:-$RAW/prompts.tsv}"
  printf '%s\t%s\t%s\t%s\n' "$(date -u +%FT%TZ)" "$1" "$2" "$3" >> "$f"
}

# A scratch git repository with one commit, no remote.
make_repo() {
  local dir="$1"
  rm -rf "$dir"
  mkdir -p "$dir"
  git -C "$dir" init -q -b main
  git -C "$dir" config user.email proofs@example.invalid
  git -C "$dir" config user.name "Glasscode proofs"
  echo "# scratch" > "$dir/README.md"
  git -C "$dir" add README.md
  git -C "$dir" commit -q -m init
}

# Pre-accept Claude's folder trust dialog for <dir> in the scratch account, so
# an interactive session starts straight at the prompt.
claude_trust() {
  local dir="$1" f="$CLAUDE_DIR/.claude.json"
  python3 -I - "$f" "$dir" <<'PY'
import json, sys
f, d = sys.argv[1], sys.argv[2]
j = json.load(open(f))
p = j.setdefault("projects", {}).setdefault(d, {})
p["hasTrustDialogAccepted"] = True
p["hasCompletedProjectOnboarding"] = True
p.setdefault("allowedTools", [])
# The one-time offers that would stop an interactive start in a dialog.
j["fullscreenUpsellSeenCount"] = 99
json.dump(j, open(f, "w"), indent=2)
PY
}

# Replace personal values in a capture before it is quoted in the doc.
scrub() {
  sed -E \
    -e "s#$HOME#~#g" \
    -e 's/[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}/<email>/g' \
    -e 's/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/<uuid>/g' \
    -e 's/(sk-ant-|sk-|gh[pousr]_)[A-Za-z0-9_-]{8,}/<token>/g'
}

ms_now() { python3 -c 'import time; print(int(time.time()*1000))'; }
