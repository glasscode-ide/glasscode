#!/usr/bin/env python3
"""A stand-in for `glasscode mcp`: an MCP stdio server with a few tools.

Usage: stub-mcp.py <log-file> [--tools status,note,wait]

Every request and notification is appended to <log-file> as one JSON line with
a wall-clock time in ms, so a proof can see whether a tool call reached it,
when, and whether the client cancelled it.

Tools:
  status          returns a fixed marker string
  note(text)      echoes the text back
  wait(seconds)   answers after <seconds>; used by P15 for client timeouts
"""
import json
import sys
import threading
import time

LOG = sys.argv[1] if len(sys.argv) > 1 else "/dev/null"
TOOLS = ["status", "note"]
if "--tools" in sys.argv:
    TOOLS = sys.argv[sys.argv.index("--tools") + 1].split(",")

lock = threading.Lock()


def log(kind, obj):
    with lock, open(LOG, "a") as f:
        f.write(json.dumps({"t_ms": int(time.time() * 1000), "kind": kind, "msg": obj}) + "\n")


def send(obj):
    with lock:
        sys.stdout.write(json.dumps(obj) + "\n")
        sys.stdout.flush()


DEFS = {
    "status": {
        "name": "status",
        "description": "Glasscode stand-in: returns the current status marker.",
        "inputSchema": {"type": "object", "properties": {}},
    },
    "note": {
        "name": "note",
        "description": "Glasscode stand-in: records a short note and echoes it.",
        "inputSchema": {
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"],
        },
    },
    "wait": {
        "name": "wait",
        "description": "Glasscode stand-in: waits for the person. Answers after `seconds`.",
        "inputSchema": {
            "type": "object",
            "properties": {"seconds": {"type": "integer"}},
            "required": ["seconds"],
        },
    },
}


def call(req):
    params = req.get("params") or {}
    name = params.get("name")
    args = params.get("arguments") or {}
    if name == "status":
        text = "STUB-STATUS-OK-7731"
    elif name == "note":
        text = "noted: " + str(args.get("text", ""))
    elif name == "wait":
        secs = int(args.get("seconds", 30))
        t0 = time.time()
        time.sleep(secs)
        text = "waited %d s" % int(time.time() - t0)
    else:
        send({"jsonrpc": "2.0", "id": req["id"], "error": {"code": -32602, "message": "unknown tool"}})
        return
    log("result", {"id": req["id"], "name": name, "text": text})
    send({"jsonrpc": "2.0", "id": req["id"], "result": {"content": [{"type": "text", "text": text}]}})


for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        req = json.loads(line)
    except ValueError:
        log("bad", line)
        continue
    log("in", req)
    method = req.get("method")
    if method == "initialize":
        pv = (req.get("params") or {}).get("protocolVersion", "2025-06-18")
        send({
            "jsonrpc": "2.0",
            "id": req["id"],
            "result": {
                "protocolVersion": pv,
                "capabilities": {"tools": {"listChanged": False}},
                "serverInfo": {"name": "glasscode-stub", "version": "0.0.1"},
            },
        })
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": req["id"], "result": {"tools": [DEFS[t] for t in TOOLS]}})
    elif method == "tools/call":
        threading.Thread(target=call, args=(req,), daemon=True).start()
    elif method == "ping":
        send({"jsonrpc": "2.0", "id": req["id"], "result": {}})
    elif "id" in req and method is not None:
        send({"jsonrpc": "2.0", "id": req["id"], "result": {}})
log("eof", None)
