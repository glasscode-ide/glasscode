#!/usr/bin/env bash
# The owner's journeys through a whole Glasscode: a debug build at engine v2, on a scratch home
# with the stand-in agents (script/test-agents), driven over its socket, `ui state` and the debug
# person's keys (GLASSCODE_DEBUG_KEYS). It never reaches a running Glasscode, the real `claude`,
# `codex` or `gh`, or the person's ~/.glasscode, ~/.claude and ~/.codex.
#
#   script/journeys.sh              # build, then run every journey
#   script/journeys.sh 3 5          # only these journeys
#   script/journeys.sh --no-build   # with what target/debug holds
#   script/journeys.sh --keep       # keep the scratch folder (it is printed)
#
# Journeys:
#   1  first run: a home with no project opens the first run; moving in through its ops, its
#      questions answered in the chat's "Moving in" topic
#   2  a stand-in Claude session's turn ends: its report (with its tools) and its work's facts
#   3  CI turns red: a wake, a card approved by its own button's op ("yes" in the chat approves the
#      program's ask before it); the effect runs once, journaled
#   4  "go to the session …" and "what's happening there" in the chat: the focus and the Show me
#      chip
#   5  a memory note is proposed and accepted; a new session's SessionStart brief has it
#   6  data export, forget a project, import: the note goes and comes back
#   8  a smoke test of the chat and Today: a task on the board, Today shown, the chat answering
#      from code and from its brain
#   7  quiet hours hold a notification; when they end (the app's debug clock starts at 06:58, so
#      22-7 ends a couple of minutes in), the digest follows
#   9  the glasses: a paired phone stand-in (the iPhone app's own link code, built for macOS by
#      script/glasses-e2e/build.sh) reaches the device door on loopback, with a door key of the
#      journey's own (never the Keychain): the handshake, home, state, Today, presence, a question
#      the manager answers from code, and a refused method
#
# Needs: macOS or Linux with a display (the app opens a window), python3, git, lsof.
set -uo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
BUILD=1
KEEP=0
ONLY=()
for arg in "$@"; do
  case "$arg" in
    --no-build) BUILD=0 ;;
    --keep) KEEP=1 ;;
    [1-9]) ONLY+=("$arg") ;;
    *) echo "usage: script/journeys.sh [--no-build] [--keep] [1-9]…" >&2; exit 2 ;;
  esac
done
wanted() { [ ${#ONLY[@]} -eq 0 ] && return 0; local j; for j in "${ONLY[@]}"; do [ "$j" = "$1" ] && return 0; done; return 1; }

if [ "$BUILD" = 1 ]; then
  export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-4}
  (cd "$ROOT" && cargo build --locked -p glasscode && cargo build --locked -p glasscode_core --features testkit --bin make-test-home) || exit 1
  if wanted 9 && command -v swiftc > /dev/null; then "$ROOT/script/glasses-e2e/build.sh" > /dev/null || exit 1; fi
fi
DOOR_E2E=$ROOT/script/glasses-e2e/.build/door-e2e
APPBIN=$ROOT/target/debug/glasscode
MAKE_HOME=$ROOT/target/debug/make-test-home
STAND_IN=$ROOT/script/test-agents/claude

T=$(mktemp -d "${TMPDIR:-/tmp}/glasscode-journeys.XXXXXX")
APP=""
PASSED=0
FAILED=0
cleanup() {
  stop_app
  if [ "$KEEP" = 1 ] || [ "$FAILED" -gt 0 ]; then echo "scratch folder kept: $T"; else rm -rf "$T"; fi
}
trap cleanup EXIT

say() { printf '%s\n' "$*"; }
pass() { PASSED=$((PASSED + 1)); say "  pass  $*"; }
fail() { FAILED=$((FAILED + 1)); say "  FAIL  $*"; }
check() { local what=$1; shift; if "$@"; then pass "$what"; else fail "$what"; fi; }
# wait_for <seconds> <command…>: until the command succeeds.
wait_for() {
  local until=$(( $(date +%s) + $1 )); shift
  while ! "$@" 2>/dev/null; do [ "$(date +%s)" -ge "$until" ] && return 1; sleep 1; done
}

# ----- the scratch environment -------------------------------------------------------------------

# A login shell that prints only the environment it was started with (no path_helper puts the real
# agents first), and a shell tab that is bash with no startup files.
cat > "$T/shell" <<'EOF'
#!/bin/bash
case " $* " in *" -c /usr/bin/env -0 "*) exec /usr/bin/env -0 ;; esac
for a in "$@"; do if [ "$a" = "-c" ]; then while [ "$1" != "-c" ]; do shift; done; exec /bin/bash --noprofile --norc "$@"; fi; done
exec /bin/bash --noprofile --norc -i
EOF
chmod +x "$T/shell"
mkdir -p "$T/bin" && ln -sf "$APPBIN" "$T/bin/glasscode"

# scratch <command…>: in the scratch home's environment, never this shell's Glasscode session.
scratch() {
  ( set -a; . "$T/home.env"; set +a
    exec env -u GLASSCODE_CONTROL_SOCKET -u GLASSCODE_SESSION_ID -u GLASSCODE_SESSION_SIG -u GLASSCODE_EVENTS -u GLASSCODE_LIMITS -u GLASSCODE_WORKTREE \
      SHELL="$T/shell" GLASSCODE_TEST_GH="$T/gh.json" "$@" )
}
gc() { scratch "$APPBIN" cli "$@"; }
# keys <line>: the person types it (in the active terminal; `chat:<text>` in the Manager chat box).
keys() { printf '%s\n' "$1" >> "$T/keys"; }
# tab <file> <glasscode args…>: the person types a glasscode command in the shell tab (short, so the
# line does not wrap past what the tab reads back), its output in <file>.
tab() { local out=$1; shift; rm -f "$T/$out"; keys "bin/glasscode cli $* > $out 2>&1"; wait_for 30 grep -qv '^Waiting for you' "$T/$out"; }
journal() { cat "$T"/home/glasscode/journal/*.jsonl 2>/dev/null; }
# has_event <kind> [<words in its summary>]
has_event() { journal | python3 -c "
import json, sys
kind, words = sys.argv[1], sys.argv[2] if len(sys.argv) > 2 else ''
sys.exit(0 if any(json.loads(l)['kind'] == kind and words in json.loads(l)['summary'] for l in sys.stdin) else 1)" "$@"; }
count_events() { journal | python3 -c "
import json, sys
print(sum(1 for l in sys.stdin if json.loads(l)['kind'] == sys.argv[1] and sys.argv[2] in json.loads(l)['summary']))" "$@"; }
session_exists() { test -n "$(session_id "$1")"; }
# shown <words>: the window shows them (`ui state`).
shown() { gc ui state 2>/dev/null | grep -q "$1"; }
session_id() { python3 -c "
import json, sys
s = json.load(open(sys.argv[1]))
print(next((x['id'] for x in s['sessions'] if x.get('title', '').startswith(sys.argv[2])), ''))" "$T/home/glasscode/state.json" "$1"; }

# make_home [--empty]: a fresh scratch home; the manager switched on (or, --empty, no project).
make_home() {
  stop_app
  rm -rf "$T/home"; : > "$T/keys"
  "$MAKE_HOME" "$T/home" > "$T/home.env" 2> "$T/make-home.log" || { cat "$T/make-home.log"; say "make-test-home failed"; exit 1; }
  python3 - "$T/home/glasscode/state.json" "${1:-}" <<'EOF'
import json, sys
path, empty = sys.argv[1], sys.argv[2] == "--empty"
s = json.load(open(path))
if empty:
    s["projects"], s["sessions"] = [], []
    s.pop("active_session", None)
else:
    s["first_run_done"] = True
    s.setdefault("manager", {}).update({"enabled": True, "name": "Alex", "owner": "Sam"})
open(path, "w").write(json.dumps(s, indent=1))
EOF
  # The stand-in GitHub knows the app project as acme/app.
  [ -d "$T/home/projects/app" ] && scratch git -C "$T/home/projects/app" config glasscode.repo acme/app
  gh_fixture running
  brain ""
}

# gh_fixture running|failed: pull request #7 of acme/app, its CI running or failed.
gh_fixture() {
  python3 - "$1" "$T/gh.json" <<'EOF'
import json, sys
state, path = sys.argv[1], sys.argv[2]
check = {"__typename": "CheckRun", "name": "test", "workflowName": "CI", "status": "IN_PROGRESS" if state == "running" else "COMPLETED", "conclusion": "" if state == "running" else "FAILURE", "detailsUrl": "https://github.com/acme/app/actions/runs/1", "startedAt": "2026-11-20T16:00:00Z", "completedAt": "" if state == "running" else "2026-11-20T16:05:00Z"}
pr = {"number": 7, "title": "Fix the typo", "url": "https://github.com/acme/app/pull/7", "headRefName": "x", "baseRefName": "main", "isDraft": False, "state": "OPEN", "author": {"login": "test"}, "reviewRequests": [], "latestReviews": []}
view = {"headRefOid": "abc123", "state": "OPEN", "statusCheckRollup": [check], "mergeable": "MERGEABLE", "baseRefName": "main", "baseRefOid": "def456"}
calls = [
    {"args": ["pr", "list", "--repo", "acme/app", "--head", "main"], "stdout": []},
    {"args": ["pr", "list", "--repo", "acme/app", "--head"], "stdout": [pr]},
    {"args": ["pr", "view", "7", "--repo", "acme/app", "--json", "headRefOid,state,statusCheckRollup,mergeable,baseRefName,baseRefOid"], "stdout": view},
]
json.dump({"calls": calls}, open(path, "w"))
EOF
}

# brain <session id>: the stand-in manager brain. A wake proposes a task for CI on app#7; the chat
# opens the session for "go to the session" and reads it for "what's happening there", through
# the brain's own MCP tools.
brain() {
  python3 - "$T/home/scenarios/claude-print.json" "$1" <<'EOF'
import json, sys
path, session = sys.argv[1], sys.argv[2]
def line(value): return {"at": 0, "to": "stdout", "text": json.dumps(value)}
init = line({"type": "system", "subtype": "init", "session_id": "{{session_id}}", "tools": [], "model": "claude-haiku-5-5"})
def answer(text): return [line({"type": "stream_event", "parent_tool_use_id": None, "event": {"type": "content_block_delta", "delta": {"type": "text_delta", "text": text}}}), line({"type": "result", "subtype": "success", "is_error": False, "result": text, "usage": {"input_tokens": 400, "output_tokens": 40}})]
wake = json.dumps({"tell_person": "CI is red on app#7; I propose a task to look at it.", "matters": [{"key": "ci:app#7", "summary": "CI red on #7", "next_check_minutes": 30}], "proposals": [{"op": "board.add", "args": {"title": "Look at CI on app#7"}, "reason": "CI failed on #7"}], "notify": []})
# A model takes a moment before its first tool call: the app checks the init line first.
think = {"at": 0, "to": "sleep", "ms": 1500}
json.dump({
    "agent": "claude", "version": "2.1.322", "source": "synthesized: the journeys' brain",
    "turns": [[init, *answer(wake)]],
    "replies": [
        {"when": "go to the session", "steps": [init, think, {"at": 0, "to": "mcp", "tool": "ui_open", "arguments": {"place": session}}, *answer("Opened it: {{tool_result}}")]},
        {"when": "what's happening there", "steps": [init, think, {"at": 0, "to": "mcp", "tool": "session_show", "arguments": {"session": session}}, *answer("There: {{tool_result}}")]},
        {"when": "plan my afternoon", "steps": [init, *answer("Start with the release notes, then the review.")]},
    ],
}, open(path, "w"), indent=1)
EOF
}

# start_app [<decide>]: the scratch app; cards are answered with <decide> (allow), or wait for the
# person when it is empty. Its socket must be its own before anything talks to it.
start_app() {
  local decide=${1-allow}
  if [ "$(scratch bash -c 'command -v claude')" != "$STAND_IN" ]; then
    say "the scratch PATH does not find the stand-in claude first; stopping"; exit 1
  fi
  ( set -a; . "$T/home.env"; set +a
    exec env -u GLASSCODE_CONTROL_SOCKET -u GLASSCODE_SESSION_ID -u GLASSCODE_SESSION_SIG -u GLASSCODE_EVENTS -u GLASSCODE_LIMITS -u GLASSCODE_WORKTREE \
      SHELL="$T/shell" GLASSCODE_TEST_GH="$T/gh.json" GLASSCODE_DEBUG_KEYS="$T/keys" GLASSCODE_DEBUG_DECIDE="$decide" GLASSCODE_DEBUG_CLOCK="${CLOCK:-}" RUST_LOG=info \
      "$APPBIN" ) > "$T/app.log" 2>&1 &
  APP=$!
  if ! wait_for 60 own_socket; then say "the scratch app did not open its socket (log: $T/app.log)"; exit 1; fi
  wait_for 30 gc ui state > /dev/null
}
own_socket() { lsof -U -a -p "$APP" 2>/dev/null | grep -q 'control-'; }
stop_app() {
  if [ -n "$APP" ] && kill -0 "$APP" 2>/dev/null; then kill "$APP"; wait "$APP" 2>/dev/null; fi
  APP=""
}
# open_tab: a shell tab in the app project, shown, in the scratch folder (the person's keys go there).
open_tab() {
  gc session new --project app --agent shell --title Shell --open > /dev/null 2>&1 &
  wait_for 30 session_exists Shell || return 1
  sleep 2
  keys "cd $T"
}

# ----- the journeys --------------------------------------------------------------------------------

journey1() {
  say "1. First run, then moving in"
  make_home --empty
  start_app
  check "a home with no project opens the first run" shown "first-run"
  # Moving in, through its ops (the buttons' own), typed by the person in a shell tab.
  make_home
  start_app allow
  open_tab
  tab j1-open.txt move-in open
  check "move-in open opens it" grep -q "Moving in is open" "$T/j1-open.txt"
  check "move-in read reads the ticked sources" wait_for 60 movein_reads
  check "it shows what it proposes" wait_for 60 movein_at "accept"
  tab j1-accept.txt move-in accept
  tab j1-study.txt move-in study
  check "move-in study asks its questions in the chat's topic \"Moving in\"" wait_for 60 movein_asked
  gc ui topic Moving in > "$T/j1-topic.txt" 2>&1
  check "ui topic shows the topic" grep -q "shows the topic “Moving in”" "$T/j1-topic.txt"
  keys "chat:Standard"
  check "an answer typed in the topic answers its question" wait_for 30 movein_answered
}
# movein_reads: the person's `move-in read` started reading (moving in may still be looking).
movein_reads() { tab j1-read.txt move-in read; grep -q "Reading" "$T/j1-read.txt"; }
# movein_at <what>: moving in takes <what> now (accept: it shows its proposals).
movein_at() { tab j1-probe.txt move-in accept nothing-such; grep -q "is no proposal of moving in" "$T/j1-probe.txt"; }
movein_asked() { cat "$T"/home/glasscode/manager/conversation/*.jsonl 2>/dev/null | python3 -c "
import json, sys
items = [json.loads(l) for l in sys.stdin]
topics = {i.get('id') for i in items if i.get('record') == 'topic' and i.get('title') == 'Moving in'}
sys.exit(0 if topics and any(i.get('record') == 'item' and str(i['kind'].get('matter', '')).startswith('movein:') for i in items) else 1)"; }
movein_answered() { python3 -c "
import json, sys
p = json.load(open(sys.argv[1]))
p = p.get('data', p)
# A level answered is said; it is kept once the person confirms it in Settings' own preview.
sys.exit(0 if any(q.get('said') == 'Standard' for q in p.get('questions', [])) else 1)" "$T/home/glasscode/manager/movein.json"; }

turn_reported() { journal | python3 -c "
import json, sys
ok = any(d['kind'] == 'turn.ended' and d.get('session') == sys.argv[1] and not d['data'].get('partial') and ' 0 tools' not in d['summary'] for d in map(json.loads, sys.stdin))
sys.exit(0 if ok else 1)" "$1"; }
work_has_turn() { python3 -c "
import json, sys
items = json.load(open(sys.argv[1]))['data']['items']
sys.exit(0 if any(sys.argv[2] in i['sessions'] and i['facts'].get('last_turn') == 'Completed' for i in items) else 1)" "$T/home/glasscode/manager/work.json" "$1"; }
# chip <words>: a Show me chip in the conversation says them.
chip() { cat "$T"/home/glasscode/manager/conversation/*.jsonl 2>/dev/null | python3 -c "
import json, sys
sys.exit(0 if any(d.get('record') == 'item' and d['kind'].get('type') == 'nudge' and sys.argv[1] in d['text'] for d in map(json.loads, sys.stdin)) else 1)" "$1"; }
brief_has() { python3 -c "
import json, sys
runs = [json.loads(l) for l in open(sys.argv[1])]
sys.exit(0 if any(r['event'] == 'SessionStart' and sys.argv[2] in r['stdout'] for r in runs) else 1)" "$T/home/hook-runs.jsonl" "$1"; }
memory_has() { grep -rqF "$1" "$T/home/glasscode/memory" 2>/dev/null; }
board_count() { gc board 2>/dev/null | grep -cF "$1"; }

# finished <session> <outcome>: the app reported a finish of the session with that outcome.
finished() { journal | python3 -c "
import json, sys
sys.exit(0 if any(d['kind'] == 'session.finished' and d.get('session') == sys.argv[1] and d['data'].get('outcome') == sys.argv[2] for d in map(json.loads, sys.stdin)) else 1)" "$1" "$2"; }
# idle_waits <session>: a needs-input event of the session came from Claude's idle reminder.
idle_waits() { journal | python3 -c "
import json, sys
sys.exit(0 if any(d['kind'] == 'session.needs_input' and d.get('session') == sys.argv[1] and d['data'].get('notification_type') == 'idle_prompt' for d in map(json.loads, sys.stdin)) else 1)" "$1"; }
no_idle_waits() { ! idle_waits "$1"; }
state_is() { gc session show "$1" --json 2>/dev/null | python3 -c "import json, sys; d = json.load(sys.stdin); d = d.get('data', d); sys.exit(0 if d.get('state') == sys.argv[1] else 1)" "$2"; }

journey2() {
  say "2. A stand-in Claude session's turn"
  gc session new --project app --message "Fix the typo in the README" > /dev/null 2>&1
  local id; id=$(session_id "Fix the typo")
  check "the session started" test -n "$id"
  check "its turn's report has its tools, read from its transcript" wait_for 60 turn_reported "$id"
  check "its work item has the turn" wait_for 20 work_has_turn "$id"
  # The finish report (finish_ui): told within seconds, by what the turn's last message says.
  check "its finish is reported at once, as done" wait_for 30 finished "$id" done
  check "the manager hears it finished" wait_for 10 state_is "$id" finished
  gc session send "$id" "and the setup doc?" > /dev/null 2>&1
  check "a turn that ends on a question is reported as needing you" wait_for 60 finished "$id" needs_you
  check "the manager hears it needs you" wait_for 10 state_is "$id" "needs you"
  # The stand-in's idle reminder comes a minute after the turn: it is no wait in the app.
  sleep 75
  check "Claude's idle reminder is no needs-input event" no_idle_waits "$id"
}

# last_approval: the number of the newest card asked.
last_approval() { journal | python3 -c "
import json, sys
asked = [json.loads(l) for l in sys.stdin]
asked = [d['data'].get('approval') for d in asked if d['kind'] == 'approval.asked']
print(asked[-1] if asked else '')"; }

journey3() {
  say "3. CI turns red; a card approved in the chat"
  make_home
  start_app ""
  gc session new --project app --message "Fix the typo in the README" > /dev/null 2>&1 &
  wait_for 30 has_event approval.asked "start a Claude session"
  keys "chat:yes"
  check "\"yes\" in the chat starts the session the program asked for" wait_for 30 session_exists "Fix the typo"
  check "the pull request's CI is seen running" wait_for 120 has_event work.changed "waiting for CI"
  gh_fixture failed
  check "CI turns red: ci.failed" wait_for 180 has_event ci.failed
  check "a wake proposes a card" wait_for 120 has_event approval.asked "Alex asks to add"
  # The card's own Allow: the op its button runs (`approvals.decide`), as the person's click.
  local card; card=$(last_approval)
  keys "op:approvals.decide {\"id\":\"$card\",\"answer\":\"allow\"}"
  check "the card's Allow approves it, and it runs" wait_for 60 has_event op.done "Added “Look at CI on app#7”"
  sleep 20
  check "it ran once" test "$(count_events op.done 'Added “Look at CI on app#7”')" = 1
  check "the board has the task once" test "$(board_count 'Look at CI on app#7')" = 1
}

journey4() {
  say "4. Go to a session, and what's happening there"
  gc session new --project app --message "Fix the docs link" > /dev/null 2>&1
  local id; id=$(session_id "Fix the docs link")
  brain "$id"
  gc manager ask "go to the session Fix the docs link" > "$T/j4-terminal.txt" 2>&1
  check "a program's ask does not move the window" grep -q "Not moved" "$T/j4-terminal.txt"
  check "the Show me chip names the session" chip "Show me the session “Fix the docs link"
  # The person spoke a moment ago, but this question is a program's: still not moved.
  tab j4-hello.txt manager ask "'how are things'"
  gc manager ask "go to the session Fix the docs link" > "$T/j4-after.txt" 2>&1
  check "a program's ask right after the person's own does not move it either" grep -q "Not moved" "$T/j4-after.txt"
  tab j4-person.txt manager ask "'go to the session Fix the docs link'"
  check "the person's ask moves the window there" shown "Fix the docs link"
  gc manager ask "what's happening there" > "$T/j4-there.txt" 2>&1
  check "what's happening there: the session's state" grep -q "Pull request #7" "$T/j4-there.txt"
}

journey5() {
  say "5. A memory note, then a new session's brief"
  tab j5-propose.txt memory propose "'The app tests run with make test'" --file project
  local proposal; proposal=$(grep -o 'Proposal [0-9a-f]*' "$T/j5-propose.txt" | cut -d' ' -f2)
  check "a note is proposed" test -n "$proposal"
  tab j5-accept.txt memory accept "$proposal"
  check "the person accepts it" grep -q "Accepted $proposal" "$T/j5-accept.txt"
  gc session new --project app --message "Run the tests" > /dev/null 2>&1
  check "the new session's SessionStart brief has it" wait_for 30 brief_has "make test"
  # The chat's own answers (M56.2): "remember that …" in the chat is the person's fact in memory.
  keys "chat:remember that releases go out on Tuesdays"
  check "\"remember that …\" in the chat is kept in memory" wait_for 30 memory_has "releases go out on Tuesdays"
  gc manager ask "status" > "$T/j5-status.txt" 2>&1
  check "\"status\" is answered from code" grep -q "session" "$T/j5-status.txt"
}

journey6() {
  say "6. Export, forget, import"
  local memory=$T/home/glasscode/memory
  tab j6-export.txt data export --to export.tar
  check "the export is where it was asked for" test -s "$T/export.tar"
  tab j6-forget.txt data forget --project app
  check "the card named what goes" grep -q "forget the project app" "$T/j6-forget.txt"
  check "forgotten: the note is gone" bash -c "! grep -rqF 'make test' '$memory'"
  tab j6-import.txt data import export.tar
  check "imported: the note is back" memory_has "make test"
}

journey7() {
  say "7. Quiet hours, then the digest"
  # Its own app, whose clock starts at 06:58 today: the quiet hours 22-7 end two minutes in.
  make_home
  CLOCK=$(python3 -c "import datetime; n = datetime.datetime.now().replace(hour=6, minute=58, second=0, microsecond=0); print(int(n.timestamp() * 1000))")
  start_app allow
  CLOCK=""
  open_tab
  tab j7-on.txt settings set notify.quiet 22-7
  check "quiet hours are 22-7" grep -q '"22-7" now' "$T/j7-on.txt"
  gc session new --project app --message "Look at the logs" > /dev/null 2>&1
  check "a notification is held" wait_for 30 has_event decision.made "held for the end of the quiet hours"
  check "not yet in a digest, while it is quiet" bash -c "! grep -q 'While it was quiet' '$T/home/glasscode/manager/delivery.json' 2>/dev/null"
  check "at 07:00 the quiet hours end and the digest follows" wait_for 240 grep -q "While it was quiet" "$T/home/glasscode/manager/delivery.json"
}

# Journeys 2, 4, 5 and 6 share one app (and its shell tab).

# chat_said <words>: the manager said them in the conversation.
chat_said() { cat "$T"/home/glasscode/manager/conversation/*.jsonl 2>/dev/null | python3 -c "
import json, sys
sys.exit(0 if any(d.get('record') == 'item' and d['kind'].get('type') == 'manager' and sys.argv[1] in d['text'] for d in map(json.loads, sys.stdin)) else 1)" "$1"; }

on_board() { [ "$(board_count "$1")" -ge 1 ]; }

journey8() {
  say "8. Smoke: the chat and Today"
  make_home
  start_app allow
  gc board add "Ship the release notes" --due tomorrow > "$T/j8-add.txt" 2>&1
  check "a task goes on the board" on_board "Ship the release notes"
  # A program's "show Today" never takes the window by itself: the gate asks the person first
  # (a program outside Glasscode may not move the window unasked, since #271/#336); the person's
  # own click goes at once.
  gc ui open today > "$T/j8-today.txt" 2>&1
  check "a program's \"show Today\" asks the person before it takes the window" grep -q "Waiting for you to allow this" "$T/j8-today.txt"
  keys 'op:ui.open {"place":"today","go":true}'
  check "the person's Today is shown" wait_for 10 shown "today"
  keys "chat:what needs me?"
  check "the chat answers from code" wait_for 30 chat_said "need"
  keys "chat:plan my afternoon"
  check "the chat answers from its brain" wait_for 60 chat_said "Start with the release notes"
}

# The journey's own keys: the door's (the cross-language vector's) and the stand-in phone's.
DOOR_HEX=$(printf '07%.0s' $(seq 32))
DOOR_PUBLIC="ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBB4YUy/UdUwC8wQdnHXOszuD/9gax85P6ILMscmLxYlupGwxHE4v9A3ZajZT5uRURdMt/khuztdcepDGoYiBwKM="
PHONE_HEX=$(printf '09%.0s' $(seq 32))
j9_line() { python3 -c "
import json, sys
for line in open(sys.argv[1]):
    try: v = json.loads(line)
    except ValueError: continue
    if sys.argv[2] in v:
        print(json.dumps(v[sys.argv[2]])); break" "$T/j9.json" "$1"; }
j9_has() { j9_line "$1" | grep -q "$2"; }
journey9() {
  say "9. The glasses: a paired phone reaches the device door"
  if [ ! -x "$DOOR_E2E" ]; then fail "the phone stand-in is built (script/glasses-e2e/build.sh)"; return; fi
  make_home
  # Pin the stand-in phone, as pairing would.
  MODE=whoami DOOR_ADDR=x DOOR_KEY=x PHONE_RAW="$PHONE_HEX" "$DOOR_E2E" > "$T/j9-whoami.json"
  python3 -c 'import json, os, sys, time
me = json.load(open(sys.argv[2]))
os.makedirs(os.path.dirname(sys.argv[1]), exist_ok=True)
doc = {"store": "authority/devices", "version": 1, "data": {"phones": {me["device"]: {"key": me["key"], "name": "Journey phone", "paired_ms": int(time.time() * 1000)}}}}
json.dump(doc, open(sys.argv[1], "w"))
' "$T/home/glasscode/authority/devices.json" "$T/j9-whoami.json"
  local port=$((45100 + RANDOM % 800))
  export GLASSCODE_DOOR_LOOPBACK=1 GLASSCODE_DOOR_PORT=$port GLASSCODE_DEBUG_DOOR_KEY=$DOOR_HEX
  start_app allow
  unset GLASSCODE_DOOR_LOOPBACK GLASSCODE_DOOR_PORT GLASSCODE_DEBUG_DOOR_KEY
  check "the door listens on loopback" wait_for 30 grep -q "device door: listening" "$T/app.log"
  MODE=live DOOR_ADDR="127.0.0.1:$port" DOOR_KEY="$DOOR_PUBLIC" PHONE_RAW="$PHONE_HEX" "$DOOR_E2E" > "$T/j9.json" 2>&1
  check "the phone gets in (both signatures)" j9_has connected true
  check "manager.home says the Mac is home" j9_has home '"home": "mac"'
  check "state lists this Mac's sessions" j9_has state '"sessions"'
  check "today.get gives Today" j9_has today '"summary"'
  check "device.presence is taken" j9_has presence '"ok": true'
  check "the manager answers the glasses from code" j9_has ask '"say"'
  check "a method outside the list is refused" j9_has refused true
  check "the app says the phone connected" grep -q "device door: Journey phone" "$T/app.log"
  # One place everywhere (docs/lens-everywhere.md): the voice's tools on the Mac, places opened
  # from the phone, the window going there and its page coming back.
  check "the phone is sent the window's place when it connects" j9_has place_first '"kind"'
  check "the voice's session list comes from the Mac, with its count" j9_has tool_list 'Count:'
  check "the list has short refs, never whole ids to read out" j9_has tool_list 'ref '
  check "the phone's voice is told where the person is and what is open" j9_has place_first 'What is open in Glasscode now'
  local page
  for page in tickets usage pull-requests sessions; do
    check "the voice opens $page: it says so" j9_has "tool_$page" 'Shown on the Mac'
    check "the voice opens $page: the window is there" j9_has "window_$page" "\"$page\""
    check "the voice opens $page: the phone gets its page" j9_has "event_$page" true
  done
  local sid; sid=$(j9_line session_id | tr -d '"')
  check "the scratch home has a session to open" test -n "$sid"
  check "a search in its project finds the session" j9_has tool_search 'Best fit'
  check "the voice opens a session by its title and project" j9_has tool_session 'Shown on the Mac'
  check "the window shows that session" j9_has window_session "$sid"
  check "the phone gets the session's page" j9_has event_session true
  check "the phone opens Today: the window is there" j9_has window_today '"today"'
  check "the phone gets Today's page" j9_has event_today true
  check "the phone opens the session by its id" j9_has window_back "$sid"
  check "a ticket that is not there opens nothing" j9_has tool_missing 'Nothing opened'
  check "the window stays where it was" j9_has window_missing "$sid"
  check "words that fit no session open nothing" j9_has tool_nobody 'Nothing opened'
  check "ui state agrees: the session is in the window" shown "$(j9_line session_title | tr -d '"' | cut -c1-20)"
}

say "Journeys in $T"
wanted 1 && journey1
if wanted 2 || wanted 4 || wanted 5 || wanted 6; then
  make_home
  start_app allow
  open_tab
  # Journey 4 moves the window off the shell tab, so it comes last.
  wanted 2 && journey2
  wanted 5 && journey5
  wanted 6 && journey6
  wanted 4 && journey4
fi
wanted 3 && journey3
wanted 7 && journey7
wanted 8 && journey8
wanted 9 && journey9
say ""
say "$PASSED passed, $FAILED failed"
[ "$FAILED" -eq 0 ]
