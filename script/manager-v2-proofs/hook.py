#!/usr/bin/env python3
"""A hook command for the proofs, for both Claude and Codex.

Usage: hook.py <log-file> <label> [--brief <file>] [--stop-once <flag-file>]

Appends {"t_ms", "label", "argv", "payload"} to <log-file> as one JSON line
(the payload is the JSON the CLI wrote on stdin).

--brief <file>       on SessionStart, print the file as additionalContext
                     (the brief variant of P03).
--stop-once <flag>   on Stop, when <flag> exists: delete it and block the stop
                     once, with a continuation reason (P05's continuation).
"""
import json
import os
import sys
import time

log_file, label = sys.argv[1], sys.argv[2]
raw = sys.stdin.read()
try:
    payload = json.loads(raw) if raw.strip() else None
except ValueError:
    payload = {"unparsed": raw}
# The transcript as it is when the hook runs, for the lag between an event and
# the transcript's last line (P05).
tx = None
tp = (payload or {}).get("transcript_path") if isinstance(payload, dict) else None
if tp and os.path.exists(tp):
    with open(tp, "rb") as tf:
        lines = tf.read().splitlines()
    last = {}
    try:
        last = json.loads(lines[-1]) if lines else {}
    except ValueError:
        pass
    tx = {"lines": len(lines), "size": os.path.getsize(tp), "last_type": last.get("type"), "last_uuid": last.get("uuid")}
with open(log_file, "a") as f:
    f.write(json.dumps({"t_ms": int(time.time() * 1000), "label": label, "ppid": os.getppid(), "transcript": tx, "payload": payload}) + "\n")

event = (payload or {}).get("hook_event_name") or label


def opt(name):
    return sys.argv[sys.argv.index(name) + 1] if name in sys.argv else None


brief = opt("--brief")
if event == "SessionStart" and brief and os.path.exists(brief):
    text = open(brief).read()
    print(json.dumps({"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": text}}))

flag = opt("--stop-once")
if event == "Stop" and flag and os.path.exists(flag):
    os.remove(flag)
    print(json.dumps({"decision": "block", "reason": "Glasscode check: before you stop, reply with the single word CONTINUED-58."}))
sys.exit(0)
