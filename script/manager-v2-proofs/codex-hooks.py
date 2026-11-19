#!/usr/bin/env python3
"""Codex hooks that log every event with hook.py, in two forms.

Usage: codex-hooks.py json <log> <label>     a hooks.json body (for $CODEX_HOME/hooks.json)
       codex-hooks.py flags <log> <label>    -c arguments, one per line (session flags)
"""
import json
import os
import sys

form, log, label = sys.argv[1], os.path.abspath(sys.argv[2]), sys.argv[3]
here = os.path.dirname(os.path.abspath(__file__))
events = ["SessionStart", "UserPromptSubmit", "PreToolUse", "PermissionRequest", "PostToolUse",
          "Stop", "Interrupt", "SessionEnd", "PreCompact", "PostCompact"]


def cmd(e):
    return "%s %s %s %s-%s" % (sys.executable, os.path.join(here, "hook.py"), log, label, e)


if form == "json":
    print(json.dumps({"hooks": {e: [{"hooks": [{"type": "command", "command": cmd(e), "timeout": 10}]}] for e in events}}, indent=1))
else:
    for e in events:
        # TOML inline: hooks.<Event> = [{ hooks = [{ type = "command", command = "..." }] }]
        print('hooks.%s=[{hooks=[{type="command",command=%s,timeout=10}]}]' % (e, json.dumps(cmd(e))))
