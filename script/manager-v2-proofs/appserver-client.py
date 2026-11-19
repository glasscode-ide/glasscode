#!/usr/bin/env python3
"""A small client for `codex app-server` (JSON-RPC, one JSON object per line).

Usage:
  appserver-client.py <log.jsonl> <steps-json> --stdio -- <codex app-server command...>
  appserver-client.py <log.jsonl> <steps-json> --unix <socket-path>

Every message in or out is logged with a wall-clock time. Steps:
  {"request": "<method>", "params": {...}, "save": "<name>", "timeout": 30}
  {"notify": "<method>", "params": {...}}
  {"wait": "<method regex>", "timeout": 60, "save": "<name>", "match": {"path.to.field": value}}
  {"answer": "<server request method regex>", "result": {...}, "timeout": 120}
  {"sleep": 2}
Strings "${name.a.b}" in params are replaced by saved values. A server
request that no "answer" step is waiting for is logged and left unanswered.
"""
import json
import os
import re
import select
import socket
import subprocess
import sys
import time

log_path, steps = sys.argv[1], json.loads(sys.argv[2])
log = open(log_path, "a")
saved = {}
pending = []  # messages read but not yet consumed by a step
buf = b""


def now():
    return int(time.time() * 1000)


def L(direction, msg):
    log.write(json.dumps({"t_ms": now(), "dir": direction, "msg": msg}) + "\n")
    log.flush()


if "--unix" in sys.argv:
    path = sys.argv[sys.argv.index("--unix") + 1]
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect(path)
    rfd = sock.fileno()

    def write(b):
        sock.sendall(b)

    def read():
        return sock.recv(65536)
    proc = None
else:
    cmd = sys.argv[sys.argv.index("--") + 1:]
    proc = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=open(log_path + ".stderr", "w"))
    rfd = proc.stdout.fileno()

    def write(b):
        proc.stdin.write(b)
        proc.stdin.flush()

    def read():
        return os.read(rfd, 65536)

next_id = [1]


def send(obj):
    L("out", obj)
    write((json.dumps(obj) + "\n").encode())


def pump(wait):
    global buf
    r, _, _ = select.select([rfd], [], [], wait)
    if not r:
        return
    chunk = read()
    if not chunk:
        raise SystemExit("connection closed")
    buf += chunk
    while b"\n" in buf:
        line, buf = buf.split(b"\n", 1)
        if not line.strip():
            continue
        try:
            msg = json.loads(line)
        except ValueError:
            L("bad", line.decode("utf-8", "replace"))
            continue
        L("in", msg)
        pending.append(msg)


def subst(o):
    if isinstance(o, str):
        def rep(m):
            v = saved
            for part in m.group(1).split("."):
                v = v[int(part)] if isinstance(v, list) else v[part]
            return v if isinstance(v, str) else json.dumps(v)
        whole = re.fullmatch(r"\$\{([^}]+)\}", o)
        if whole:
            v = saved
            for part in whole.group(1).split("."):
                v = v[int(part)] if isinstance(v, list) else v[part]
            return v
        return re.sub(r"\$\{([^}]+)\}", rep, o)
    if isinstance(o, dict):
        return {k: subst(v) for k, v in o.items()}
    if isinstance(o, list):
        return [subst(v) for v in o]
    return o


def get(o, path):
    for part in path.split("."):
        if isinstance(o, dict):
            o = o.get(part)
        else:
            return None
    return o


def take(pred, timeout):
    deadline = time.time() + timeout
    while True:
        for i, m in enumerate(pending):
            if pred(m):
                return pending.pop(i)
        if time.time() >= deadline:
            return None
        pump(min(0.2, max(0.0, deadline - time.time())))


result = {"steps": []}
for step in steps:
    entry = {"step": step, "t_ms": now()}
    if "request" in step:
        rid = next_id[0]
        next_id[0] += 1
        send({"jsonrpc": "2.0", "id": rid, "method": step["request"], "params": subst(step.get("params", {}))})
        resp = take(lambda m: m.get("id") == rid and "method" not in m, step.get("timeout", 30))
        entry["ok"] = resp is not None and "error" not in resp
        entry["error"] = (resp or {}).get("error")
        if resp and step.get("save"):
            saved[step["save"]] = resp.get("result")
    elif "notify" in step:
        send({"jsonrpc": "2.0", "method": step["notify"], "params": subst(step.get("params", {}))})
    elif "wait" in step:
        pat = re.compile(step["wait"])
        match = step.get("match", {})
        m = take(lambda m: "method" in m and pat.search(m["method"]) and "id" not in m
                 and all(get(m.get("params", {}), k) == subst(v) for k, v in match.items()), step.get("timeout", 60))
        entry["ok"] = m is not None
        if m and step.get("save"):
            saved[step["save"]] = m.get("params")
    elif "answer" in step:
        pat = re.compile(step["answer"])
        m = take(lambda m: "method" in m and "id" in m and pat.search(m["method"]), step.get("timeout", 120))
        entry["ok"] = m is not None
        if m:
            entry["request"] = m["method"]
            send({"jsonrpc": "2.0", "id": m["id"], "result": subst(step.get("result", {}))})
    elif "sleep" in step:
        deadline = time.time() + step["sleep"]
        while time.time() < deadline:
            pump(0.2)
    entry["t_done_ms"] = now()
    result["steps"].append(entry)
    sys.stderr.write("step %s -> %s\n" % (list(step)[0] + ":" + str(step.get(list(step)[0]))[:60], entry.get("ok", "")))
result["saved"] = saved
if proc:
    proc.stdin.close()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
print(json.dumps(result, indent=1)[:20000])
