#!/usr/bin/env python3
"""List the model-visible tools in `codex debug prompt-input` JSON (stdin).

Prints one name per line: top-level tools, namespaced tools as ns.name, and
tools that the `exec` (code mode) description says are reachable through it.
"""
import json
import re
import sys

items = json.load(sys.stdin)
names = []
for it in items:
    for t in it.get("tools", []) or []:
        if t.get("type") == "namespace":
            for sub in t.get("tools", []):
                names.append("%s.%s" % (t.get("name"), sub.get("name")))
                desc = sub.get("description", "") or ""
                # Code mode: nested tools are documented as "### `name`" sections.
                for m in re.findall(r"^### `([A-Za-z0-9_.]+)`", desc, re.M):
                    names.append("  nested in %s.%s: %s" % (t.get("name"), sub.get("name"), m))
        else:
            names.append(t.get("name") or t.get("type"))
seen = set()
for n in names:
    if n not in seen:
        seen.add(n)
        print(n)
