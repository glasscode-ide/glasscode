#!/usr/bin/env bash
# Run the worker directly on this machine, not in Docker. Needed for MANAGER_BRAIN=server,
# where the worker calls `glasscoded` (which lives on the machine, not in the container).
# Uses ../.env and a virtualenv in this folder; installs nothing anywhere else.
#
#   docker compose up -d livekit      # the room server still runs in Docker
#   ./worker/run-host.sh
set -euo pipefail
cd "$(dirname "$0")"

if [[ ! -f ../.env ]]; then
  echo "No ../.env yet: copy .env.example to .env and fill it in." >&2
  exit 1
fi
set -a
# shellcheck disable=SC1091
source ../.env
set +a
export LIVEKIT_URL="${LIVEKIT_URL:-ws://127.0.0.1:7880}"

PYTHON="${PYTHON:-python3}"
if [[ ! -x .venv/bin/python ]]; then
  "$PYTHON" -m venv .venv
  .venv/bin/pip install --disable-pip-version-check -r requirements.txt
fi
exec .venv/bin/python worker.py start
