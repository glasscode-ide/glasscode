#!/usr/bin/env bash
# Start a throwaway Linux "server" for testing glasscoded: Ubuntu 24.04 with sshd, tmux, git
# and stand-in claude/codex programs, reachable as `glasscode-test` through
# target/test-server/ssh_config (port 2222 on 127.0.0.1, a fresh key, user `dev`).
#
#   script/test-server/run.sh          # (re)start it
#   script/test-server/run.sh stop
#
# Sessions that test at once each take a server of their own (one session's tests reset what
# another's expect): GLASSCODE_TEST_SERVER_NAME and GLASSCODE_TEST_SERVER_PORT name it and its
# port, and its ssh_config is then in target/<name>/.
set -euo pipefail
cd "$(dirname "$0")/../.."
NAME="${GLASSCODE_TEST_SERVER_NAME:-glasscode-test-server}"
PORT="${GLASSCODE_TEST_SERVER_PORT:-2222}"
if [ "$NAME" = glasscode-test-server ]; then OUT="$PWD/target/test-server"; else OUT="$PWD/target/$NAME"; fi

docker rm -f "$NAME" >/dev/null 2>&1 || true
[ "${1:-}" = "stop" ] && exit 0

mkdir -p "$OUT"
cp script/test-server/Dockerfile script/test-server/fake-claude script/test-server/fake-codex script/test-server/fake-gh "$OUT/"
[ -f "$OUT/key" ] || ssh-keygen -q -t ed25519 -N "" -C glasscode-test -f "$OUT/key"
cp "$OUT/key.pub" "$OUT/authorized_keys"
docker build -q -t "$NAME" "$OUT" >/dev/null
docker run -d --name "$NAME" -p "127.0.0.1:$PORT:22" "$NAME" >/dev/null
cat > "$OUT/ssh_config" <<CFG
Host glasscode-test
  HostName 127.0.0.1
  Port $PORT
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
