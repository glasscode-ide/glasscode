#!/usr/bin/env python3
"""Print a synthetic wake situation of exactly N characters (default 4000).

The text has the shape of what the wake lane would send (sessions, PRs, CI,
board items), with made-up names only.
"""
import sys

n = int(sys.argv[1]) if len(sys.argv) > 1 else 4000
lines = ["Situation at 2026-11-19 14:02 (wake: ci.failed on pr 412)."]
i = 0
while sum(len(l) + 1 for l in lines) < n:
    i += 1
    lines.append(
        f"- session s{i:03d} (claude, project demo-{i % 7}, branch feature/item-{i}): "
        f"turn {i % 5 + 1} ended {i % 9 + 1} min ago, cause Done; last report: "
        f"'updated parser tests, 3 files changed'; PR #{400 + i} checks: "
        f"{'failing (lint)' if i % 4 == 0 else 'passing'}; board item B-{i}: in review."
    )
text = "\n".join(lines)
sys.stdout.write(text[:n])
