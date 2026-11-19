#!/bin/bash
# P17 Sleep timing and the lease clock. Needs the owner's hands (a real sleep).
#
#   p17-sleep.sh <claude-config-dir> <codex-home> <scratch-dir> [url] [hold-seconds]
#
# Builds p17-sleep-probe.swift into <scratch>, starts it in the background and
# prints what to do. [url] is the request made in the held window (default: the
# Docker test server's health page; any reachable http URL works). No prompt.
#
# Owner steps, one log per case (stop the probe with `kill <pid>` after each):
#   1. On battery: start, wait 10 s, close the lid, wait 2 min, open it.
#   2. On mains:   the same.
#   3. Idle sleep: start, set a short sleep (System Settings > Lock Screen or
#      `pmset -g` to see it), leave the Mac untouched until it sleeps, wake it.
# Then: p17-sleep.sh … report <log> prints the numbers for the proofs doc.
. "$(dirname "$0")/lib.sh"
proof_args "$@"
if [ "${4:-}" = report ]; then
  python3 - "$5" <<'PY'
import json, sys
ev = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
first = {}
for e in ev:
    first.setdefault(e["event"], e)
def gap(a, b, clock="wall_ms"):
    if a in first and b in first:
        return first[b][clock] - first[a][clock]
print("events:", [e["event"] for e in ev if e["event"] != "tick"])
print("workspace notice to I/O Kit will_sleep (ms):", gap("workspace_will_sleep", "iokit_will_sleep"))
print("I/O Kit will_sleep to allowed (ms, the held window):", gap("iokit_will_sleep", "iokit_allowed"))
for k in ("net_ok", "net_error"):
    if k in first:
        print(k, first[k])
w = [e for e in ev if e["event"] in ("iokit_allowed", "iokit_powered_on")]
if len(w) >= 2:
    a, b = w[0], w[-1]
    for c in ("wall_ms", "uptime_raw_ms", "monotonic_ms", "continuous_ms"):
        print("asleep by %-14s %8.1f s" % (c, (b[c] - a[c]) / 1000))
PY
  exit 0
fi
URL=${4:-http://localhost:8080/}
HOLD=${5:-20}
BIN="$SCRATCH/p17-sleep-probe"
swiftc -O -swift-version 5 -o "$BIN" "$HERE/p17-sleep-probe.swift" 2>/dev/null || { echo "swiftc failed" >&2; exit 1; }
LOG="$RAW/sleep-$(date +%Y%m%d-%H%M%S).jsonl"
"$BIN" "$LOG" --hold "$HOLD" --url "$URL" &
echo "probe pid $! logging to $LOG (hold ${HOLD}s, request to $URL)"
echo "Now sleep the Mac (lid, or idle), wake it, then: kill $!; $0 $1 $2 $3 report $LOG"
