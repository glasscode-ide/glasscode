#!/bin/bash
# P07 The session profile on the real path layout.
#
#   p07-profile.sh <claude-config-dir> <codex-home> <scratch-dir> codex [person|automation]
#   p07-profile.sh <claude-config-dir> <codex-home> <scratch-dir> claude person|automation
#
# Builds a scratch Glasscode home with the path layout of design section 5
# (authority/, state.json, memory/, agent-notes/, worktrees/<project>/<branch>
# as linked git worktrees, control.sock as a listening Unix socket), a .env in
# the worktree, then runs the probe commands inside the profile:
#   codex  (no prompt): `codex sandbox -C <worktree>` with a [permissions.*]
#          profile built from the layout, one command at a time;
#   claude (1 prompt per profile): an interactive session whose --settings has
#          the sandbox and permission rules; the model runs the same probes.
# Probes never print a credential: a read is `head -c1 <file> >/dev/null`.
# The person's real ~/.ssh, ~/.config/gh and Keychain are probed that way only.
. "$(dirname "$0")/lib.sh"
proof_args "$@"
AGENT=${4:-codex}
PROFILE=${5:-person}
GC="$SCRATCH/gchome"
REPO="$SCRATCH/repos/proj"
WT="$GC/worktrees/proj/feature-a"
OTHER="$GC/worktrees/proj/feature-b"

setup() {
  rm -rf "$GC" "$SCRATCH/repos"
  mkdir -p "$GC/authority" "$GC/memory" "$GC/agent-notes/proj/$AGENT" "$GC/worktrees/proj" "$GC/bin"
  echo '{"canary":"P07-AUTHORITY"}' > "$GC/authority/policy.json"
  echo '{"canary":"P07-STATE"}' > "$GC/state.json"
  echo "# memory P07-MEMORY" > "$GC/memory/MEMORY.md"
  make_repo "$REPO"
  git -C "$REPO" worktree add -q -b feature-a "$WT"
  git -C "$REPO" worktree add -q -b feature-b "$OTHER"
  echo "P07_SECRET=canary" > "$WT/.env"
  mkdir -p "$WT/crate/src" "$WT/pkg"
  printf '[package]\nname = "p07"\nversion = "0.1.0"\nedition = "2021"\n' > "$WT/crate/Cargo.toml"
  echo 'fn main() {}' > "$WT/crate/src/main.rs"
  echo '{"name":"p07","version":"1.0.0","private":true}' > "$WT/pkg/package.json"
  # The Glasscode socket: a listener that answers "ok".
  python3 - "$GC/control.sock" <<'PY' &
import os, socket, sys
p = sys.argv[1]
if os.path.exists(p): os.remove(p)
s = socket.socket(socket.AF_UNIX); s.bind(p); s.listen(5)
s.settimeout(900)
while True:
    try:
        c, _ = s.accept()
    except OSError:
        break
    c.sendall(b"ok\n"); c.close()
PY
  SOCK_PID=$!
  sleep 0.5
}

# Every probe: <name> <shell command>. Prints ALLOWED or DENIED with the
# first line of the error, never file content.
PROBES=$(cat <<EOF
edit own worktree|echo x >> "$WT/README.md"
write the other worktree|echo x >> "$OTHER/README.md"
read authority/policy.json|head -c1 "$GC/authority/policy.json" >/dev/null
write authority/policy.json|echo x >> "$GC/authority/policy.json"
read state.json|head -c1 "$GC/state.json" >/dev/null
read memory/MEMORY.md|head -c1 "$GC/memory/MEMORY.md" >/dev/null
write memory/MEMORY.md|echo x >> "$GC/memory/MEMORY.md"
write agent-notes MEMORY.md|echo x >> "$GC/agent-notes/proj/$AGENT/MEMORY.md"
read .env in the worktree|head -c1 "$WT/.env" >/dev/null
read ~/.ssh (list)|ls "$HOME/.ssh" >/dev/null
read a key in ~/.ssh|f=\$(ls "$HOME"/.ssh/id_* 2>/dev/null | grep -v '\.pub$' | head -1); [ -n "\$f" ] && head -c1 "\$f" >/dev/null
read ~/.config/gh/hosts.yml|head -c1 "$HOME/.config/gh/hosts.yml" >/dev/null
Keychain reachable|security find-generic-password -s glasscode-proof-no-such-item 2>&1 | grep -q 'could not be found'
cd into the own worktree|cd "$WT"
git status in the linked worktree|git -C "$WT" status --short >/dev/null
git commit in the linked worktree|git -C "$WT" -c user.email=p@example.invalid -c user.name=p commit -q --allow-empty -m p07
gh api rate_limit|gh api rate_limit --jq .rate.limit >/dev/null
git ls-remote github.com|git ls-remote https://github.com/git/git HEAD >/dev/null
cargo build --offline (no deps)|cargo build --offline -q --manifest-path "$WT/crate/Cargo.toml"
npm install (no deps, offline)|npm install --prefix "$WT/pkg" --offline --no-audit --no-fund -q
connect to control.sock|python3 -c 'import socket,sys; s=socket.socket(socket.AF_UNIX); s.connect(sys.argv[1]); print(s.recv(3))' "$GC/control.sock"
EOF
)

codex_profile() { # -c flags for a [permissions.glasscode] profile from the layout
  # A -c dotted path keeps quoted segments as written, so paths go in one
  # inline table per key.
  python3 - "$HOME" "$GC" "$WT" "$AGENT" "$PROFILE" "$OTHER" <<'PY2'
import json, sys
home, gc, wt, agent, profile, other = sys.argv[1:7]
fs = {}
for d in [home + "/.ssh", home + "/.aws", home + "/.config/gh", home + "/.netrc", home + "/.gnupg",
          home + "/Library/Keychains", gc + "/authority", gc + "/state.json"]:
    fs[d] = "deny"
if profile != "person":
    # Other worktrees one by one: denying the parent breaks path lookups into
    # the session's own worktree (cd: Not a directory).
    fs[other] = "deny"
fs[gc + "/memory"] = "read"
fs[wt] = "write"
fs[gc + "/agent-notes/proj/" + agent] = "write"
fs[":workspace_roots"] = {"**/.env*": "deny"}
def toml(v):
    if isinstance(v, dict):
        return "{" + ", ".join("%s=%s" % (json.dumps(k), toml(x)) for k, x in v.items()) + "}"
    if isinstance(v, bool):
        return "true" if v else "false"
    return json.dumps(v)
net = {"enabled": False, "unix_sockets": {gc + "/control.sock": "allow"}}
for flag in ['default_permissions="glasscode"', 'permissions.glasscode.extends=":workspace"',
             "permissions.glasscode.filesystem=" + toml(fs), "permissions.glasscode.network=" + toml(net)]:
    print("-c"); print(flag)
PY2
}

if [ "$AGENT" = codex ]; then
  setup
  FLAGS=(); while IFS= read -r l; do FLAGS+=("$l"); done < <(codex_profile)
  echo "== codex, $PROFILE profile"
  echo "profile: $(printf "%s " "${FLAGS[@]}" | sed "s#$HOME#~#g; s#$SCRATCH#<scratch>#g")"
  while IFS='|' read -r name cmd; do
    [ -z "$name" ] && continue
    out=$(cd "$WT" && codex_env codex sandbox -C "$WT" -P glasscode "${FLAGS[@]}" --allow-unix-socket "$GC/control.sock" -- /bin/sh -c "$cmd" 2>&1)
    rc=$?
    first=$(echo "$out" | grep -v '^$' | head -1 | cut -c1-110 | sed "s#$HOME#~#g; s#$SCRATCH#<scratch>#g")
    printf '%-38s %s %s\n' "$name" "$([ $rc -eq 0 ] && echo ALLOWED || echo "DENIED($rc)")" "$([ $rc -ne 0 ] && echo "$first")"
  done <<< "$PROBES"
  kill $SOCK_PID 2>/dev/null
  exit 0
fi
echo "claude mode: see p07-profile-claude in this script's next revision" >&2
exit 2
