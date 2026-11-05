#!/usr/bin/env bash
# Start a throwaway Linux "server" for testing glasscoded: Ubuntu 24.04 with sshd, tmux, git
# and stand-in claude/codex programs, reachable as `glasscode-test` through
# target/test-server/ssh_config (port 2222 on 127.0.0.1, a fresh key, user `dev`).
#
#   script/test-server/run.sh          # (re)start it
#   script/test-server/run.sh stop
set -euo pipefail
cd "$(dirname "$0")/../.."
NAME=glasscode-test-server
OUT="$PWD/target/test-server"

docker rm -f "$NAME" >/dev/null 2>&1 || true
[ "${1:-}" = "stop" ] && exit 0

mkdir -p "$OUT"
cp script/test-server/Dockerfile script/test-server/fake-claude script/test-server/fake-codex script/test-server/fake-gh "$OUT/"
[ -f "$OUT/key" ] || ssh-keygen -q -t ed25519 -N "" -C glasscode-test -f "$OUT/key"
cp "$OUT/key.pub" "$OUT/authorized_keys"
docker build -q -t "$NAME" "$OUT" >/dev/null
docker run -d --name "$NAME" -p 127.0.0.1:2222:22 "$NAME" >/dev/null
cat > "$OUT/ssh_config" <<CFG
Host glasscode-test
  HostName 127.0.0.1
  Port 2222
  User dev
  IdentityFile "$OUT/key"
  IdentitiesOnly yes
  StrictHostKeyChecking no
  UserKnownHostsFile /dev/null
  LogLevel ERROR
CFG
for _ in $(seq 1 30); do
  ssh -F "$OUT/ssh_config" -o BatchMode=yes -o ConnectTimeout=2 glasscode-test true 2>/dev/null && break
  sleep 0.5
done
echo "test server up: ssh -F $OUT/ssh_config glasscode-test"
