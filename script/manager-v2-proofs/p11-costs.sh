#!/bin/bash
# P11 Costs that need no model: the hook start, a git commit in a 10 MB memory
# repository, a search over 10 MB of Markdown, a journal append.
#
#   p11-costs.sh <claude-config-dir> <codex-home> <scratch-dir> [glasscode-binary]
#
# [glasscode-binary] is the program `glasscode hook` runs (default: the
# installed app's /Applications/Glasscode.app/Contents/MacOS/Glasscode, called
# as `<binary> cli hook`). It runs with GLASSCODE_HOME and GLASSCODE_EVENTS in
# the scratch folder. No prompt. (The model wakes are P01 and P02.)
. "$(dirname "$0")/lib.sh"
proof_args "$@"
BIN=${4:-/Applications/Glasscode.app/Contents/MacOS/Glasscode}

T="$HERE/p11-time.py"
echo "process start of /usr/bin/true: $(python3 "$T" 50 -- /usr/bin/true)"

PAYLOAD="$SCRATCH/hook-payload.json"
python3 -c 'import json; print(json.dumps({"session_id":"s","hook_event_name":"Stop","last_assistant_message":"x"*1500,"transcript_path":"/tmp/t.jsonl","cwd":"/tmp"}))' > "$PAYLOAD"
export GLASSCODE_EVENTS="$SCRATCH/events/s.jsonl" GLASSCODE_HOME="$SCRATCH/home"
mkdir -p "$SCRATCH/events" "$SCRATCH/home"
echo "hook, today's Mac command (sh -c 'cat >> events; printf'): $(python3 "$T" 50 --stdin "$PAYLOAD" -- /bin/sh -c 'cat >> "$GLASSCODE_EVENTS"; printf "\n" >> "$GLASSCODE_EVENTS"')"
if [ -x "$BIN" ]; then
  echo "hook, '<app binary> cli hook': $(python3 "$T" 50 --stdin "$PAYLOAD" -- "$BIN" cli hook)"
fi

# A 10 MB memory repository: 2,000 Markdown files of 5 KB.
MEM="$SCRATCH/memory"
if [ ! -d "$MEM/.git" ]; then
  make_repo "$MEM"
  python3 - "$MEM" <<'PY'
import os, random, sys
random.seed(7)
words = ("parser test review branch session memory brief release deploy cache index "
         "journal reconcile worktree account project decision owner schedule").split()
root = sys.argv[1]
for i in range(2000):
    d = os.path.join(root, "projects", "p%02d" % (i % 40))
    os.makedirs(d, exist_ok=True)
    with open(os.path.join(d, "entry-%04d.md" % i), "w") as f:
        f.write("---\nid: e%04d\nkind: fact\n---\n# Entry %d\n\n" % (i, i))
        n = 0
        while n < 5000:
            line = " ".join(random.choice(words) for _ in range(12)) + ".\n"
            f.write(line); n += len(line)
PY
  git -C "$MEM" add -A && git -C "$MEM" commit -q -m "10 MB of memory"
fi
echo "memory repository: $(du -sk "$MEM/projects" | cut -f1) KB of Markdown in $(git -C "$MEM" ls-files | wc -l | tr -d ' ') files; .git $(du -sk "$MEM/.git" | cut -f1) KB"
F="$MEM/projects/p00/entry-0000.md"
echo "git commit of one edit (add before, not timed): $(python3 "$T" 20 --before "echo '- edit' >> '$F'; git -C '$MEM' add '$F'" -- git -C "$MEM" commit -q -m "memory: one edit")"
echo "git add + commit as one command: $(python3 "$T" 20 --before "echo '- edit' >> '$F'" -- /bin/sh -c "git -C '$MEM' add '$F' && git -C '$MEM' commit -q -m edit")"
echo "git grep -i -n (one word, whole repo): $(python3 "$T" 20 -- git -C "$MEM" grep -i -n -e journal)"
echo "git grep -i -l (two words, files only): $(python3 "$T" 20 -- git -C "$MEM" grep -i -l --all-match -e decision -e owner)"
echo "grep -r -i -n (one word): $(python3 "$T" 20 -- grep -r -i -n journal "$MEM/projects")"

rustc -O -o "$SCRATCH/p11-journal" "$HERE/p11-journal.rs" 2>/dev/null
rm -f "$SCRATCH/journal.jsonl"
echo "journal append (500 lines of ~400 bytes, from a Rust program, no process start):"
"$SCRATCH/p11-journal" "$SCRATCH/journal.jsonl" 500
