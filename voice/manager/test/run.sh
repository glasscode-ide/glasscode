#!/usr/bin/env bash
# Tests the worker and the Mac helper end to end on this Mac, with stand-in speech services
# (no keys, nothing is heard or played, no window). It starts its own LiveKit server in
# Docker on 127.0.0.1:17880 and removes it again.
#
#   voice/manager/test/run.sh
#
# Needs: Docker, python3.10+ (a virtualenv is made in the work folder), and for the helper
# test a built helper (cd voice/helper && swift build -c release --product manager-voice).
# The work folder is $MV_WORK (default: a new folder under $TMPDIR).
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
export MV_WORK="${MV_WORK:-$(mktemp -d "${TMPDIR:-/tmp}/manager-voice-test.XXXXXX")}"
PYTHON="${PYTHON:-python3}"
mkdir -p "$MV_WORK"
NAME=manager-voice-test-livekit
KEY=APImanagertest
SECRET="$(openssl rand -hex 24)"
export MANAGER_WINDOW_SECS=6

cleanup() {
  if [[ -n "${WORKER_PID:-}" ]]; then
    kill "$WORKER_PID" 2>/dev/null || true
    for _ in $(seq 1 10); do kill -0 "$WORKER_PID" 2>/dev/null || break; sleep 0.5; done
    # The worker and the processes it started for its jobs.
    pkill -9 -f "$HERE/harness_worker.py" 2>/dev/null || true
  fi
  docker rm -f "$NAME" >/dev/null 2>&1 || true
}
trap cleanup EXIT

if [[ ! -x "$MV_WORK/venv/bin/python" ]]; then
  "$PYTHON" -m venv "$MV_WORK/venv"
  "$MV_WORK/venv/bin/pip" install -q --disable-pip-version-check -r "$HERE/../worker/requirements.txt"
fi
PY="$MV_WORK/venv/bin/python"

# What is "said": recorded with the Mac's own speech, written to files, never played.
say -o "$MV_WORK/u1.aiff" "A I Mira, what is running right now?"
say -o "$MV_WORK/u2.aiff" "And what about the reviews from yesterday?"
say -o "$MV_WORK/u3.aiff" "Mira, please stop talking now."
for clip in u1 u2 u3; do afconvert -f WAVE -d LEI16@48000 -c 1 "$MV_WORK/$clip.aiff" "$MV_WORK/$clip.wav"; done

# The room server. TCP only: Docker Desktop on a Mac can stall when a UDP port is published.
docker rm -f "$NAME" >/dev/null 2>&1 || true
docker run -d --rm --name "$NAME" -p 127.0.0.1:17880:17880 -p 127.0.0.1:17881:17881 \
  -e LIVEKIT_KEYS="$KEY: $SECRET" \
  -e LIVEKIT_CONFIG="$(printf 'port: 17880\nrtc:\n  tcp_port: 17881\n  udp_port: 17882\n  use_external_ip: false\n  node_ip: 127.0.0.1\nroom:\n  empty_timeout: 30\n  departure_timeout: 5\n')" \
  livekit/livekit-server:v1.13.11 >/dev/null
for _ in $(seq 1 30); do [[ "$(curl -s -m 2 http://127.0.0.1:17880/ || true)" == "OK" ]] && break; sleep 1; done

# Tokens for the two test devices (the app makes its own with glasscode_core::manager::voice).
KEY="$KEY" SECRET="$SECRET" "$PY" -B - > "$MV_WORK/tokens.txt" <<'PYEOF'
import os
from livekit import api
for identity in ("mac-test1", "mac-helper"):
    grants = api.VideoGrants(room_join=True, room="manager", can_publish=True, can_subscribe=True, can_publish_data=True)
    print("TOKEN", identity, api.AccessToken(os.environ["KEY"], os.environ["SECRET"]).with_identity(identity).with_grants(grants).to_jwt())
PYEOF

MV_TRANSCRIPT="$MV_WORK/transcript.txt" LIVEKIT_URL=ws://127.0.0.1:17880 LIVEKIT_API_KEY="$KEY" LIVEKIT_API_SECRET="$SECRET" \
  MANAGER_STT=fake MANAGER_TTS=fake "$PY" -B "$HERE/harness_worker.py" start > "$MV_WORK/worker.log" 2>&1 &
WORKER_PID=$!
for _ in $(seq 1 40); do grep -q "registered worker" "$MV_WORK/worker.log" && break; sleep 1; done
sleep 5

echo "== the room and the worker =="
"$PY" -B "$HERE/room_test.py"

HELPER="${MV_HELPER:-$HERE/../../helper/.build/release/manager-voice}"
if [[ -x "$HELPER" ]]; then
  sleep 7 # the room of the first test closes
  echo "== the Mac helper =="
  MV_HELPER="$HELPER" "$PY" -B "$HERE/helper_test.py"
else
  echo "== the Mac helper: skipped, not built ($HELPER) =="
fi
echo "Logs: $MV_WORK/worker.log"
