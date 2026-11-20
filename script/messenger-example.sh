#!/usr/bin/env bash
#
# EXAMPLE ONLY: a messenger program for Glasscode's manager that needs no account.
#
# It sends nothing to anyone. What the manager "sends" is appended to
#   ~/.glasscode/manager/outbox-example.log
# and its inbox is always empty. Use it to try the manager's messages end to end, and as a
# starting point for a real one (Slack, email, anything): keep the four calls, replace what
# they do. The protocol is described in docs/manager.md.
#
# To try it: Settings › Manager › Messenger, and enter the full path of this file.
#
#   messenger-example.sh whoami                           who you are, as JSON
#   messenger-example.sh people <query>                   people matching a name, as JSON
#   messenger-example.sh send --to <contact> [--urgent]   the text comes on stdin
#   messenger-example.sh inbox --since <unix seconds>     what others wrote since, as JSON
#
# Exit codes: 0 done, 3 not signed in (never here), anything else failed (why goes to stderr).

set -euo pipefail

log_file="${GLASSCODE_HOME:-$HOME/.glasscode}/manager/outbox-example.log"

# A text as a JSON string, quotes included.
json_string() {
  local text=$1
  text=${text//\\/\\\\}
  text=${text//\"/\\\"}
  text=${text//$'\n'/\\n}
  text=${text//$'\r'/\\r}
  text=${text//$'\t'/\\t}
  printf '"%s"' "$text"
}

usage() {
  echo "usage: $(basename "$0") whoami | people <query> | send --to <contact> [--urgent] | inbox --since <unix seconds>" >&2
  exit 2
}

call=${1:-}
[ $# -gt 0 ] && shift

case "$call" in
  whoami)
    # A real messenger answers with the account that is signed in.
    echo '{"id": "example-me", "name": "Example Person", "email": "me@example.com"}'
    ;;

  people)
    query=${1:-}
    if [ -z "$query" ]; then
      echo '[]'
    else
      # A real messenger searches its directory. This one knows whoever is asked for.
      printf '[{"id": %s, "name": %s, "email": ""}]\n' "$(json_string "$query")" "$(json_string "$query")"
    fi
    ;;

  send)
    to=""
    urgent="no"
    while [ $# -gt 0 ]; do
      case "$1" in
        --to)
          to=${2:-}
          shift
          ;;
        --urgent) urgent="yes" ;;
        *) usage ;;
      esac
      shift
    done
    if [ -z "$to" ]; then
      echo "send needs --to <contact>" >&2
      exit 2
    fi
    text=$(cat)
    if [ -z "$text" ]; then
      echo "there is no text on stdin" >&2
      exit 1
    fi
    mkdir -p "$(dirname "$log_file")"
    {
      printf -- '--- %s · to: %s · urgent: %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" "$to" "$urgent"
      printf '%s\n\n' "$text"
    } >> "$log_file"
    ;;

  inbox)
    # A real messenger prints what others wrote after --since, for example:
    # [{"id": "m1", "chat": "c1", "from": "Sam Example", "text": "Can you look at #42?",
    #   "at": 1794735000, "urgent": false, "handled": false}]
    if [ "${1:-}" != "--since" ] || [ -z "${2:-}" ]; then
      usage
    fi
    echo '[]'
    ;;

  *)
    usage
    ;;
esac
