#!/usr/bin/env bash
# The CLI flag probe: does every flag and subcommand that Glasscode passes to `claude` and
# `codex` (cli-flags.json, by call site) still appear in those CLIs' help?
#
#   script/cli-probe.sh <claude help file> <codex help file>
#   script/cli-probe.sh --record <folder>
#
# The first form checks help texts. CI runs it against script/cli-help/, one file per approved
# version. It fails (exit 1) when a flag or subcommand is missing, unless `known_failures` in
# cli-flags.json lists that flag at that call site: those are warnings, with their finding and
# the slice that fixes them. A plain `claude --help` output works as the Claude file.
#
# The second form records the help of the `claude` and `codex` on the PATH into
# <folder>/claude-<version>.txt and <folder>/codex-<version>.txt. They run in a scratch home,
# so no login, setting or file of the person's own is read or written. The owner records and
# probes a new version before approving it in cli-versions.json (docs/architecture.md,
# "Updating the CLIs").
#
# It needs bash 3.2 or newer, a POSIX awk and jq. CLI_FLAGS names another cli-flags.json.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
flags_file="${CLI_FLAGS:-$root/cli-flags.json}"
# Fields of the rows that jq writes: a unit separator, never in a flag or a call site's id.
sep=$'\x1f'

usage() {
  echo "usage: script/cli-probe.sh <claude help file> <codex help file>" >&2
  echo "       script/cli-probe.sh --record <folder>" >&2
  exit 2
}

command -v jq >/dev/null || { echo "cli-probe: jq is needed (brew install jq, apt-get install jq)" >&2; exit 2; }
[ -f "$flags_file" ] || { echo "cli-probe: no $flags_file" >&2; exit 2; }
jq -e . "$flags_file" >/dev/null || { echo "cli-probe: $flags_file is not valid JSON" >&2; exit 2; }

tmp="$(mktemp -d "${TMPDIR:-/tmp}/cli-probe.XXXXXX")"
tmp="$(cd "$tmp" && pwd -P)"
trap 'rm -rf "$tmp"' EXIT

# The heading of one section of a help file: `### $ codex exec --help`.
heading() { # cli command(may be empty) what(--help or --version)
  if [ -n "$2" ]; then echo "### \$ $1 $2 $3"; else echo "### \$ $1 $3"; fi
}

# One section of a help file, on stdout; exit 3 when the file has no such section. A file with
# no headings at all is the plain output of `<cli> --help`.
section() { # file cli command what
  local want
  want="$(heading "$2" "$3" "$4")"
  if ! grep -q '^### \$ ' "$1"; then
    if [ -z "$3" ] && [ "$4" = "--help" ]; then cat "$1"; return 0; fi
    return 3
  fi
  awk -v want="$want" '
    index($0, "### $ ") == 1 { on = ($0 == want); if (on) found = 1; next }
    on { print }
    END { exit found ? 0 : 3 }
  ' "$1"
}

# The options a help text defines, one per line (`-c`, `--config`, ...). Only definition lines
# count: indented one to six spaces, then a dash. Wrapped descriptions are indented further
# (Claude's about 40 spaces, Codex's 10), so a flag a description only mentions is not defined.
defined_options() {
  awk '
    match($0, /^ +-/) {
      if (RLENGTH - 1 > 6) next
      spec = substr($0, RLENGTH)
      # Claude puts the description on the same line, after two spaces or more.
      cut = index(spec, "  ")
      if (cut > 0) spec = substr(spec, 1, cut - 1)
      n = split(spec, words, "[ ,]+")
      for (i = 1; i <= n; i++) {
        w = words[i]
        if (substr(w, 1, 1) != "-") continue
        sub(/=.*/, "", w); sub(/\[.*/, "", w); sub(/<.*/, "", w)
        if (w != "" && w != "-" && w != "--") print w
      }
    }
  '
}

# The subcommands a help text lists under "Commands:", one per line, aliases split
# (`plugin|plugins`).
defined_commands() {
  awk '
    /^[A-Za-z][A-Za-z ]*:$/ { on = ($0 == "Commands:"); next }
    on && /^  [^ ]/ {
      name = substr($0, 3); sub(/ .*/, "", name)
      n = split(name, names, "|")
      for (i = 1; i <= n; i++) print names[i]
    }
  '
}

# Whether the first `Usage:` line of a help text names this command (a check that the right
# help was recorded: `codex exec resume --help`, not `codex --help`).
usage_names() { # cli command
  local want="Usage: $1"
  [ -n "$2" ] && want="$want $2"
  awk -v want="$want" '
    index($0, "Usage: ") == 1 {
      rest = substr($0, length(want) + 1)
      seen = 1; ok = index($0, want) == 1 && (rest == "" || substr(rest, 1, 1) == " ")
      exit
    }
    END { exit (seen && ok) ? 0 : 1 }
  '
}

# The first line of the `--version` section, if the file has one.
recorded_version() { # file cli
  local version
  version="$(section "$1" "$2" "" --version 2>/dev/null | head -n 1 || true)"
  echo "${version:-version not recorded}"
}

# ----- --record ---------------------------------------------------------------------------------

record() { # folder
  local dir="$1" scratch cli program version out path
  mkdir -p "$dir"
  scratch="$tmp/home"
  mkdir -p "$scratch/claude" "$scratch/codex"
  for cli in claude codex; do
    program="$(command -v "$cli")" || { echo "cli-probe: $cli is not on the PATH" >&2; exit 2; }
    version="$(in_scratch "$scratch" "$program" --version | awk '{ for (i = 1; i <= NF; i++) if ($i ~ /^[0-9]+\.[0-9]/) { print $i; exit } }' || true)"
    [ -n "$version" ] || { echo "cli-probe: could not read the version of $cli" >&2; exit 2; }
    out="$dir/$cli-$version.txt"
    {
      echo "# $cli $version: help texts recorded by script/cli-probe.sh --record, in a scratch home."
      heading "$cli" "" --version
      in_scratch "$scratch" "$program" --version
      while IFS= read -r path; do
        heading "$cli" "$path" --help
        # The words of the path are separate arguments: `codex exec resume --help`.
        # shellcheck disable=SC2086
        in_scratch "$scratch" "$program" $path --help
      done < <(jq -r --arg cli "$cli" '.help[$cli][]' "$flags_file")
    } | sed "s#$scratch#<scratch home>#g" > "$out.new"
    while IFS= read -r path; do
      if ! section "$out.new" "$cli" "$path" --help | usage_names "$cli" "$path"; then
        echo "cli-probe: the help of '$cli${path:+ $path}' does not start with its own Usage line; not recorded" >&2
        rm -f "$out.new"; exit 1
      fi
    done < <(jq -r --arg cli "$cli" '.help[$cli][]' "$flags_file")
    if { [ -n "${HOME:-}" ] && grep -qF "$HOME" "$out.new"; } || { [ -n "${USER:-}" ] && grep -qwF "$USER" "$out.new"; }; then
      echo "cli-probe: the help of $cli names this home folder or user; look at $out.new before committing it" >&2
      exit 1
    fi
    mv "$out.new" "$out"
    echo "recorded $out"
  done
}

# A CLI run with nothing of the person's: a scratch home and config folders, no updater.
in_scratch() { # scratch program args...
  local scratch="$1"; shift
  env -i PATH="$PATH" HOME="$scratch" CLAUDE_CONFIG_DIR="$scratch/claude" CODEX_HOME="$scratch/codex" \
    DISABLE_AUTOUPDATER=1 NO_COLOR=1 TERM=dumb "$@" </dev/null 2>&1
}

# ----- the probe --------------------------------------------------------------------------------

probe() { # claude-file codex-file
  local claude_file="$1" codex_file="$2"
  [ -f "$claude_file" ] || { echo "cli-probe: no file $claude_file" >&2; exit 2; }
  [ -f "$codex_file" ] || { echo "cli-probe: no file $codex_file" >&2; exit 2; }

  # One row per thing to check: site, cli, command, kind (flag or command), name.
  jq -r --arg sep "$sep" '
    .call_sites[] | . as $s
    | ($s.command // "" | split(" ") | map(select(. != ""))) as $w
    | ( (range(0; $w | length) as $i | [$s.id, $s.cli, ($w[0:$i] | join(" ")), "command", $w[$i]]),
        (($s.subcommands // [])[] | [$s.id, $s.cli, ($w | join(" ")), "command", .]),
        (($s.flags // [])[] | [$s.id, $s.cli, ($w | join(" ")), "flag", .]) )
    | join($sep)
  ' "$flags_file" > "$tmp/checks"
  jq -r --arg sep "$sep" '.known_failures // [] | .[] | [.site, .cli, (.command // ""), .flag, .finding, .fixed_in] | join($sep)' "$flags_file" > "$tmp/known"

  local sites known_count
  sites="$(jq '.call_sites | length' "$flags_file")"
  known_count="$(jq '.known_failures // [] | length' "$flags_file")"
  echo "CLI flag probe: $sites call sites and $known_count known failures in ${flags_file#"$root"/}"
  echo "  claude: $claude_file ($(recorded_version "$claude_file" claude))"
  echo "  codex:  $codex_file ($(recorded_version "$codex_file" codex))"
  echo

  local failures=0 warnings=0 passed=0 site cli command kind name file key label status
  local current="" oks=""
  flush() {
    if [ -n "$current" ] && [ -n "$oks" ]; then echo "ok    $current:$oks"; fi
    oks=""
  }
  while IFS="$sep" read -r site cli command kind name; do
    [ "$site" = "$current" ] || { flush; current="$site"; }
    if [ "$cli" = claude ]; then file="$claude_file"; else file="$codex_file"; fi
    key="$(printf '%s_%s' "$cli" "$command" | tr ' /' '__')"
    if [ ! -f "$tmp/$key.help" ]; then
      if section "$file" "$cli" "$command" --help > "$tmp/$key.help" && usage_names "$cli" "$command" < "$tmp/$key.help"; then
        defined_options < "$tmp/$key.help" > "$tmp/$key.options"
        defined_commands < "$tmp/$key.help" > "$tmp/$key.commands"
      else
        : > "$tmp/$key.options"; : > "$tmp/$key.commands"; echo missing > "$tmp/$key.missing"
      fi
    fi
    label="$cli${command:+ $command} $name"
    if [ -f "$tmp/$key.missing" ]; then
      flush
      echo "FAIL  $label: '$(heading "$cli" "$command" --help | cut -c 7-)' is not in $file, or is the help of another command ($site)"
      failures=$((failures + 1)); continue
    fi
    if [ "$kind" = flag ]; then
      grep -qxF -- "$name" "$tmp/$key.options" && status=ok || status=missing
    else
      grep -qxF -- "$name" "$tmp/$key.commands" && status=ok || status=missing
    fi
    local known
    known="$(awk -F "$sep" -v s="$site" -v c="$cli" -v m="$command" -v f="$name" -v k="$kind" \
      'k == "flag" && $1 == s && $2 == c && $3 == m && $4 == f { print "known failure (finding: " $5 "; fixed in: " $6 ")"; exit }' "$tmp/known")"
    if [ "$status" = ok ] && [ -z "$known" ]; then
      oks="$oks $name"; passed=$((passed + 1))
    elif [ "$status" = ok ]; then
      flush
      echo "WARN  $label: listed as a $known, but this help has it; remove it from known_failures once no approved version needs it ($site)"
      warnings=$((warnings + 1))
    elif [ -n "$known" ]; then
      flush
      echo "WARN  $label: not in the help: $known ($site)"
      warnings=$((warnings + 1))
    else
      flush
      if [ "$kind" = flag ]; then
        echo "FAIL  $label: not an option of '$cli${command:+ $command}' ($site)"
      else
        echo "FAIL  $label: not a subcommand of '$cli${command:+ $command}' ($site)"
      fi
      failures=$((failures + 1))
    fi
  done < "$tmp/checks"
  flush

  # A known failure that names no flag of a call site is stale: the call site changed, so the
  # entry would excuse nothing, or the wrong thing.
  while IFS="$sep" read -r site cli command name _; do
    if ! awk -F "$sep" -v s="$site" -v c="$cli" -v m="$command" -v f="$name" \
      '$1 == s && $2 == c && $3 == m && $4 == "flag" && $5 == f { found = 1 } END { exit found ? 0 : 1 }' "$tmp/checks"; then
      echo "FAIL  known failure '$cli${command:+ $command} $name' at '$site' matches no flag of that call site; correct or remove it"
      failures=$((failures + 1))
    fi
  done < "$tmp/known"

  echo
  echo "$passed passed, $warnings warnings, $failures failures"
  [ "$failures" -eq 0 ]
}

case "${1:-}" in
  --record) [ $# -eq 2 ] || usage; record "$2" ;;
  -h | --help | "") usage ;;
  *) [ $# -eq 2 ] || usage; probe "$1" "$2" ;;
esac
