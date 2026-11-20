#!/usr/bin/env python3
"""Time a command N times from one process (no per-run interpreter start).

Usage: p11-time.py <n> [--stdin <file>] [--before '<shell>'] -- <command...>
Prints: n, p50, p95, max in ms. --before runs a shell snippet before each run
(not timed), e.g. an edit before a git commit.
"""
import subprocess
import sys
import time

n = int(sys.argv[1])
rest = sys.argv[2:]
stdin_file = before = None
while rest and rest[0] != "--":
    if rest[0] == "--stdin":
        stdin_file = rest[1]
    elif rest[0] == "--before":
        before = rest[1]
    rest = rest[2:]
cmd = rest[1:]
times = []
for _ in range(n):
    if before:
        subprocess.run(["/bin/sh", "-c", before], check=True)
    inp = open(stdin_file, "rb") if stdin_file else subprocess.DEVNULL
    t = time.perf_counter()
    subprocess.run(cmd, stdin=inp, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    times.append((time.perf_counter() - t) * 1000)
    if stdin_file:
        inp.close()
times.sort()
p = lambda q: times[round((len(times) - 1) * q)]
print("n %d, p50 %.1f ms, p95 %.1f ms, max %.1f ms" % (n, p(0.5), p(0.95), times[-1]))
