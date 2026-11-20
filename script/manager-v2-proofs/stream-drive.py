#!/usr/bin/env python3
"""Drive `claude -p --input-format stream-json --output-format stream-json`.

Usage: stream-drive.py <out.jsonl> <messages-json> -- <claude command...>

<messages-json> is a JSON list of user message texts. Each is sent after the
previous turn's `result` line (the first one at once). Every stdout line is
written to <out.jsonl> wrapped as {"t_ms": ..., "line": <parsed json>}, and a
summary is printed: init tools, apiKeySource, time to first token and to the
result for each turn, usage and cost.
"""
import json
import select
import subprocess
import sys
import time

out_path, msgs = sys.argv[1], json.loads(sys.argv[2])
cmd = sys.argv[sys.argv.index("--") + 1:]
timeout = float(__import__("os").environ.get("TURN_TIMEOUT", "900"))


def now():
    return int(time.time() * 1000)


p = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=open(out_path + ".err", "w"), text=True, bufsize=1)
out = open(out_path, "w")
t_start = now()
summary = {"turns": [], "t_spawn_ms": t_start}


pending = b""
fd_out = p.stdout.fileno()


def next_line(wait):
    """One stdout line, None when nothing came within <wait>, "" at EOF."""
    global pending
    while b"\n" not in pending:
        r, _, _ = select.select([fd_out], [], [], wait)
        if not r:
            return None
        chunk = __import__("os").read(fd_out, 65536)
        if not chunk:
            rest, pending = pending, b""
            return rest.decode() if rest else ""
        pending += chunk
    line, pending = pending.split(b"\n", 1)
    return line.decode("utf-8", "replace") + "\n"


def send(text):
    msg = {"type": "user", "message": {"role": "user", "content": [{"type": "text", "text": text}]}}
    p.stdin.write(json.dumps(msg) + "\n")
    p.stdin.flush()
    return now()


for i, text in enumerate(msgs):
    t_sent = send(text)
    turn = {"i": i, "t_sent_ms": t_sent, "first_token_ms": None}
    deadline = time.time() + timeout
    while True:
        if time.time() >= deadline:
            turn["timeout"] = True
            break
        line = next_line(0.5)
        if line is None:
            continue
        if line == "":
            turn["eof"] = True
            break
        t = now()
        try:
            j = json.loads(line)
        except ValueError:
            j = {"unparsed": line}
        out.write(json.dumps({"t_ms": t, "line": j}) + "\n")
        out.flush()
        ty, st = j.get("type"), j.get("subtype")
        if ty == "system" and st == "init" and "init" not in summary:
            summary["init"] = {k: j.get(k) for k in ("tools", "mcp_servers", "apiKeySource", "model", "permissionMode", "slash_commands", "claude_code_version")}
            summary["init_after_spawn_ms"] = t - t_start
        if turn["first_token_ms"] is None and ty == "stream_event":
            ev = j.get("event", {})
            if ev.get("type") == "content_block_delta":
                turn["first_token_ms"] = t - t_sent
        if ty == "assistant" and "first_assistant_ms" not in turn:
            turn["first_assistant_ms"] = t - t_sent
        if ty == "result":
            turn["result_ms"] = t - t_sent
            turn["result"] = {k: j.get(k) for k in ("subtype", "is_error", "duration_ms", "duration_api_ms", "num_turns", "total_cost_usd", "usage", "result", "session_id")}
            break
    summary["turns"].append(turn)
    if turn.get("eof") or turn.get("timeout"):
        break

p.stdin.close()
try:
    p.wait(timeout=30)
except subprocess.TimeoutExpired:
    p.kill()
summary["exit"] = p.returncode
print(json.dumps(summary, indent=1))
