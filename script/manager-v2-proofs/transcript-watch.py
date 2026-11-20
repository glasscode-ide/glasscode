#!/usr/bin/env python3
"""Log every change of a growing JSONL file (a Claude transcript, a Codex
rollout) with a wall-clock time: one line per new record, with its type.

Usage: transcript-watch.py <file-or-glob> <out.jsonl> [seconds]

For a glob, the newest matching file is followed (a rollout's name is not
known before the session starts). Stops after <seconds> (default 600).
"""
import glob
import json
import os
import sys
import time

pattern, out_path = sys.argv[1], sys.argv[2]
until = time.time() + (float(sys.argv[3]) if len(sys.argv) > 3 else 600)
seen = {}
out = open(out_path, "a")
while time.time() < until:
    files = sorted(glob.glob(pattern), key=lambda p: os.path.getmtime(p)) if any(c in pattern for c in "*?[") else [pattern]
    for path in files:
        try:
            data = open(path, "rb").read()
        except OSError:
            continue
        lines = data.splitlines()
        n = seen.get(path, 0)
        if len(lines) > n:
            t = int(time.time() * 1000)
            for line in lines[n:]:
                try:
                    j = json.loads(line)
                except ValueError:
                    j = {}
                payload = j.get("payload") if isinstance(j.get("payload"), dict) else {}
                out.write(json.dumps({
                    "t_ms": t,
                    "file": os.path.basename(path),
                    "type": j.get("type"),
                    "subtype": j.get("subtype") or payload.get("type"),
                    "ts": j.get("timestamp"),
                }) + "\n")
            out.flush()
            seen[path] = len(lines)
    time.sleep(0.05)
