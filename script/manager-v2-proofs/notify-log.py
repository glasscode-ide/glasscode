#!/usr/bin/env python3
"""A Codex `notify` program for P06: logs who was called and the payload.

Usage (as Codex config): notify = ["python3", "notify-log.py", "<log>", "<who>"]
Codex appends the JSON payload as the last argument.
"""
import json
import sys
import time

log, who = sys.argv[1], sys.argv[2]
payload = sys.argv[3] if len(sys.argv) > 3 else None
try:
    payload = json.loads(payload) if payload else None
except ValueError:
    pass
with open(log, "a") as f:
    f.write(json.dumps({"t_ms": int(time.time() * 1000), "who": who, "payload": payload}) + "\n")
