#!/usr/bin/env python3
"""Print a Claude `--settings` JSON whose hooks log every event with hook.py.

Usage: claude-hooks.py <log-file> [--brief <file>] [--stop-once <flag>]
                       [--unknown] [--extra '<json merged into settings>']

Events: the ones Glasscode uses today, the new ones of P05, and with
--unknown one made-up event name, to see whether it breaks the block.
"""
import json
import os
import sys

here = os.path.dirname(os.path.abspath(__file__))
log = os.path.abspath(sys.argv[1])
py = sys.executable


def opt(name):
    return sys.argv[sys.argv.index(name) + 1] if name in sys.argv else None


events = ["SessionStart", "SessionEnd", "UserPromptSubmit", "Stop", "StopFailure",
          "PreCompact", "PostCompact", "SubagentStart", "SubagentStop",
          "PermissionRequest", "PermissionDenied", "CwdChanged", "Notification",
          "PreToolUse", "PostToolUse"]
if "--unknown" in sys.argv:
    events.append("GlasscodeMadeUpEvent")
extra_args = []
if opt("--brief"):
    extra_args += ["--brief", os.path.abspath(opt("--brief"))]
if opt("--stop-once"):
    extra_args += ["--stop-once", os.path.abspath(opt("--stop-once"))]

hooks = {}
for e in events:
    cmd = " ".join([py, os.path.join(here, "hook.py"), log, e] + extra_args)
    hooks[e] = [{"hooks": [{"type": "command", "command": cmd}]}]
settings = {"hooks": hooks}
if opt("--extra"):
    settings.update(json.loads(opt("--extra")))
print(json.dumps(settings))
