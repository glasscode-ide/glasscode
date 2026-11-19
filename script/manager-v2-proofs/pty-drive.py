#!/usr/bin/env python3
"""Drive an interactive CLI (claude, codex) in a pseudo-terminal, as Glasscode's
terminal view would, from a list of steps. There is no tmux on the Mac.

Usage: pty-drive.py <out-prefix> <steps-json> -- <command> [args...]

Writes <out-prefix>.raw (every byte the program wrote) and prints a JSON
timeline of the steps to stdout, with wall-clock ms.

Steps (a JSON list):
  {"wait_for": "<regex>", "timeout": 60}   on the ANSI-stripped screen text, and
                                            on the same text with whitespace removed
                                            (TUIs move the cursor instead of printing
                                            spaces); "new_only": true looks only at
                                            output after the step starts
  {"sleep": 1.5}
  {"send": "text"}                          typed as is
  {"key": "enter" | "esc" | "ctrl-c" | "ctrl-d" | "tab" | "up" | "down"}
  {"kill": 9}                               signal to the child
  {"wait_exit": 30}
  {"mark": "name"}                          just a timestamp in the timeline
  {"wait_file": path, "contains": "<regex>", "min_count": 1, "timeout": 60}
  {"write_file": path, "text": "..."}       e.g. change the brief between turns

The driver answers the terminal queries a TUI sends at start (cursor position,
device attributes, colours, keyboard protocol), as a real terminal does.
"""
import fcntl
import json
import os
import pty
import re
import select
import signal
import struct
import sys
import termios
import time

out_prefix, steps_json = sys.argv[1], sys.argv[2]
cmd = sys.argv[sys.argv.index("--") + 1:]
steps = json.loads(steps_json)

pid, fd = pty.fork()
if pid == 0:
    os.execvp(cmd[0], cmd)

fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 50, 160, 0, 0))
raw = open(out_prefix + ".raw", "wb")
buf = b""
timeline = []
ANSI = re.compile(rb"\x1b\[[0-9;?<>=]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(\x07|\x1b\\)|\x1b[@-Z\\-_]")
KEYS = {"enter": b"\r", "esc": b"\x1b", "ctrl-c": b"\x03", "ctrl-d": b"\x04",
        "tab": b"\t", "up": b"\x1b[A", "down": b"\x1b[B"}
exited = None


def now():
    return int(time.time() * 1000)


def answer_queries(chunk):
    replies = []
    if b"\x1b[6n" in chunk:
        replies.append(b"\x1b[1;1R")
    if b"\x1b[c" in chunk or b"\x1b[0c" in chunk:
        replies.append(b"\x1b[?62;22c")
    if b"\x1b[?u" in chunk:
        replies.append(b"\x1b[?0u")
    for m in re.finditer(rb"\x1b\](1[01]);\?(\x07|\x1b\\)", chunk):
        replies.append(b"\x1b]" + m.group(1) + b";rgb:0000/0000/0000\x1b\\")
    for r in replies:
        os.write(fd, r)


def pump(timeout):
    global buf, exited
    r, _, _ = select.select([fd], [], [], timeout)
    if r:
        try:
            chunk = os.read(fd, 65536)
        except OSError:
            chunk = b""
        if not chunk:
            finish()
            return False
        raw.write(chunk)
        raw.flush()
        buf += chunk
        answer_queries(chunk)
    reap(block=False)
    return True


def reap(block):
    global exited
    if exited is not None:
        return
    try:
        p, status = os.waitpid(pid, 0 if block else os.WNOHANG)
    except ChildProcessError:
        exited = {"t_ms": now(), "status": None}
        return
    if p == pid:
        exited = {"t_ms": now(), "status": status,
                  "signal": os.WTERMSIG(status) if os.WIFSIGNALED(status) else None,
                  "code": os.WEXITSTATUS(status) if os.WIFEXITED(status) else None}


def finish():
    """Reap the child while still draining the terminal: on macOS an exiting
    process waits until its terminal output is read, so a blocking waitpid
    without reading would hang both."""
    global exited
    while exited is None:
        try:
            r, _, _ = select.select([fd], [], [], 0.1)
            if r:
                chunk = os.read(fd, 65536)
                if chunk:
                    raw.write(chunk)
        except OSError:
            pass
        reap(block=False)


def screen():
    return ANSI.sub(b"", buf).decode("utf-8", "replace")


for step in steps:
    entry = {"step": step, "t_ms": now()}
    if "wait_for" in step:
        pat = re.compile(step["wait_for"])
        deadline = time.time() + step.get("timeout", 60)
        start_len = len(screen()) if step.get("new_only") else 0
        ok = False
        while time.time() < deadline and exited is None:
            scr = screen()[start_len:]
            if pat.search(scr) or pat.search(re.sub(r"\s+", "", scr)):
                ok = True
                break
            pump(0.2)
        entry["ok"] = ok
    elif "wait_file" in step:
        pat = re.compile(step.get("contains", "."))
        need = step.get("min_count", 1)
        deadline = time.time() + step.get("timeout", 60)
        ok = False
        while time.time() < deadline and exited is None:
            try:
                n = len(pat.findall(open(step["wait_file"]).read()))
            except OSError:
                n = 0
            if n >= need:
                ok = True
                break
            pump(0.2)
        entry["ok"] = ok
    elif "write_file" in step:
        with open(step["write_file"], "w") as wf:
            wf.write(step["text"])
    elif "sleep" in step:
        deadline = time.time() + step["sleep"]
        while time.time() < deadline and exited is None:
            pump(min(0.2, max(0.0, deadline - time.time())))
    elif "send" in step:
        for ch in step["send"]:
            os.write(fd, ch.encode())
            time.sleep(0.01)
    elif "key" in step:
        os.write(fd, KEYS[step["key"]])
    elif "kill" in step:
        try:
            os.kill(pid, step["kill"])
        except ProcessLookupError:
            pass
    elif "wait_exit" in step:
        deadline = time.time() + step["wait_exit"]
        while time.time() < deadline and exited is None:
            pump(0.2)
        entry["exited"] = exited
    entry["t_done_ms"] = now()
    timeline.append(entry)
    with open(out_prefix + ".timeline.json", "w") as tf:
        json.dump(timeline, tf)
    sys.stderr.write("step %s -> %s\n" % (json.dumps(step), entry.get("ok", entry.get("exited", ""))))

# Drain, then make sure nothing is left running.
for _ in range(10):
    if exited is not None:
        break
    pump(0.1)
if exited is None:
    try:
        os.kill(pid, signal.SIGHUP)
    except ProcessLookupError:
        pass
    deadline = time.time() + 3
    while exited is None and time.time() < deadline:
        pump(0.1)
    if exited is None:
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
    finish()
raw.close()
with open(out_prefix + ".txt", "w") as f:
    f.write(screen())
print(json.dumps({"timeline": timeline, "exited": exited}, indent=1))
