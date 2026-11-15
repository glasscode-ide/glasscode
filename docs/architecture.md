# Glasscode architecture

## Decisions

| Decision | Choice | Why |
|---|---|---|
| Base | Own app on a frozen copy of GPUI, not a Zed fork | No weekly Zed rebases; we own ~150k lines of UI toolkit instead of ~1.7M lines of editor |
| Code editing | View + diff in Glasscode, "Open in editor" for edits | Agents write the code; developers review it |
| Terminal | `alacritty_terminal` + our own GPUI view | Apache-2.0, so the app stays free of GPL code |
| Agents | Official `claude` / `codex` binaries in a PTY | The only subscription use both vendors clearly allow; no token handling |
| Accounts | One config folder per account | Documented by both CLIs (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`) |
| CLI updates | Team-approved versions, self-update off | CLIs change daily; session formats are internal |
| Platform | macOS only (client); Linux for the future server daemon | Built for the Mac first |

## Processes

```
Glasscode.app (GPUI)
 ├─ one PTY per open session ── claude / codex / zsh   (cwd = the session's worktree)
 │      └─ Claude hooks append JSON lines to ~/.glasscode/events/<session>.jsonl
 ├─ background tick (700 ms): read hook events, find Codex conversation ids,
 │      refresh git status (3 s) and worktree lists (15 s)
 ├─ gh ── pull request of each session's branch; for the PR in the open Git tab (30 s):
 │      pr view (checks, reviews, comments, labels), GraphQL review threads,
 │      deployments of preview/pr-<n>; actions: pr edit --add/remove-label, pr comment,
 │      run rerun --failed, thread reply/resolve, pr create --draft. Each call times out at 30 s.
 └─ state.json written after every change
```

## The glasscode command

```
agent's shell ── glasscode <words> ──▶ ~/.local/bin/glasscode (exec Glasscode.app/…/Glasscode cli)
      │                                     │ one JSON line: words, GLASSCODE_SESSION_ID + _SIG, tty?
      │                                     ▼
      │                         ~/.glasscode/control.sock (0600) ──▶ the app's main thread
      │                                     ▲          who: signed session / you at a tty / outside
      └─ (sandbox: socket denied) ─ <worktree>/.glasscode/cli/<id>.req ─┘   rule: allow / ask / never
                                                     answers: waiting (approval #n) … done {code, text, data}
```

- **Protocol** (`glasscode_core::control`): the command sends `Request { args, session, sig,
  interactive, cwd }` and reads `Reply` lines: `waiting { approval, summary }` while the person
  decides, then `done { code, text, data }`. The command itself only knows its options (`--json`,
  `--no-wait`, `--timeout`) and the help text (`glasscode_core::cli`); the app parses the words
  (`commands.rs`, `commands2.rs`) into a plan: its kind (Settings › Automation's rows), a
  sentence for the approval and the log, and what it does — the same functions the buttons call.
- **Signatures**: `sha256(secret ‖ session id)` with a secret made at each launch; sessions get it
  as `GLASSCODE_SESSION_SIG` (not `…TOKEN`: Codex drops variables named like secrets). It is
  attribution, not a security boundary: any program of the same user can open the socket, so
  callers without a signature are "outside" and may only read unless allowed.
- **Approvals** live in memory (`control_ui.rs`): the request's connection stays open; a decision
  runs the plan and answers; the answer is kept for `glasscode wait <id>` until the app quits.
- **Agents learn it**: Claude gets `--plugin-dir ~/.glasscode/agent/claude-plugin` (a plugin with
  the `glasscode` skill) and `--allowedTools Bash(glasscode:*)`; Codex
  `-c developer_instructions=…`. Both only when the plugin folder exists (Settings can turn it
  off, which removes it).
- **Server sessions**: `glasscoded` writes the same launcher (`glasscoded cli`) and listens on its
  own `control.sock`; it checks the session's signature, queues the request as a `RemoteCall`,
  and the Mac's next `mirror.sync` takes it, runs it as "Claude in <title> (on <server>)" and
  sends the answers back with the sync after (`cli_replies`). No Mac synced for 45 s: exit 3.

## Today

`glasscode_core::board` is the model: tasks (list todo / asked / waiting / later, due day, person,
links, notes, who added it), recurring tasks (`Every`), and `marks` for items from elsewhere
(Linear tickets, review requests: done today, snoozed, moved), plus Focus. `Board::view` places
everything in sections for a day (`day.rs`: local calendar days); `prepare` makes the day's
recurring copies once a day. The app (`today_ui.rs`) adds the items from Linear and GitHub it
already fetches (`board::outside_items`), what needs the person, the approvals and the log. With
every `mirror.sync` the Mac sends a `TodaySnapshot`; phones read it with `today.get` and send
`TodayOp`s with `today.op`, which reach the Mac with its next sync.

## Session lifecycle

**New session.** Optional `git worktree add -b <branch> ~/.glasscode/worktrees/<project>/<branch>`,
then:

- Claude: `CLAUDE_CONFIG_DIR=<account> claude --session-id <uuid> --settings '<hooks>'`.
  The id is chosen up front, so the session is resumable immediately.
- Codex: `CODEX_HOME=<account> codex`. Codex has no `--session-id`; Glasscode finds the new
  conversation by reading the header of the newest `sessions/YYYY/MM/DD/rollout-*.jsonl`
  whose `cwd` is the worktree.

**Hooks (Claude).** `--settings` adds `SessionStart`, `UserPromptSubmit`, `Stop` and
`Notification` hooks without touching the user's own settings. They append the hook input
to the session's events file. Glasscode reads `session_id` (it changes after `/clear`),
`transcript_path` and the activity (working / idle / needs input).

**Restore.** On launch the active session starts again with `claude --resume <id>` or
`codex resume <id>`; other sessions show "Resume session" and start when opened. If a Claude
conversation has no transcript yet (no prompt was sent), it is started again with the same
`--session-id`.

**Switch account (same agent).** The transcript file is copied into the other account's
folder at the same relative path (never credentials), the account is changed and the
session resumes.

**Handoff (Claude ⇄ Codex).** `handoff/<id>-<time>.md` gets the previous transcript path,
the uncommitted diff (up to 80 KB), the last screen and the developer's note. The other agent
starts with a prompt that tells it to read the notes, summarize and continue. The prompt is
kept until the new agent has started its conversation, so a quit in between does not lose it.

## Updating the CLIs (compatibility checklist)

Before adding a new version to `cli-versions.json`:

1. `claude --help` still lists `--session-id`, `--resume`, `--settings`; `codex resume --help`
   still takes a session id.
2. A new Claude session writes `SessionStart` to its events file with `session_id` and
   `transcript_path`; a prompt produces `UserPromptSubmit` and `Stop`.
3. Quit and reopen Glasscode: the Claude and Codex sessions resume their conversations.
4. Codex rollout headers still have `type: session_meta` with `payload.id` and `payload.cwd`
   (covered by `crates/glasscode_core/tests/codex_discovery.rs` for the parser).
5. Handoff in both directions starts the other agent with the notes prompt.
6. Two accounts of the same agent stay separately signed in.
7. "Resume an earlier conversation" still finds them (`crates/glasscode_core/tests/history.rs`
   covers the parser): Claude files under `projects/<folder with non-alphanumerics as ->/` with
   `cwd`, `entrypoint`, `ai-title` and `last-prompt` lines; Codex `session_meta` with `cwd`,
   `source`, `originator`, `thread_source`, and `history.jsonl` with `session_id` and `text`.

## Server: glasscoded

```
Glasscode.app ──ssh (ControlMaster)──▶ server: ~/.glasscode/bin/glasscoded rpc ─unix socket─▶ glasscoded serve
      │                                                                                          │ state.json (same model)
      └──ssh -tt──▶ tmux -L glasscode attach-session -t <session>   ◀── sessions run in tmux ──────┘ glasscoded.json (running set)
```

- **Transport**: the developer's SSH (Tailscale name or `~/.ssh/config` alias), `BatchMode`,
  one shared master connection. `glasscoded rpc` bridges stdio to the daemon's socket
  (`~/.glasscode/glasscoded.sock`, mode 0600) and starts the daemon when it is not running.
  Messages are JSON lines; the protocol lives in `crates/glasscode_core/src/remote.rs`
  (`PROTOCOL_VERSION`; a mismatch asks to update the daemon).
- **Sessions** run in tmux on the socket `glasscode` with Glasscode's own options (no status
  bar, prefix Ctrl-] because Claude Code uses Ctrl-B, no Escape delay, true color, extended
  keys). The daemon launches the CLIs exactly like the app (`agents::launch`: same hooks,
  `--session-id`/`--resume`, `CLAUDE_CONFIG_DIR`/`CODEX_HOME`) with the user's login-shell
  environment. Hook events give working/idle/needs-you; Codex ids come from rollout files.
- **Persistence**: tmux keeps agents running through SSH drops and daemon restarts (the
  systemd unit uses `KillMode=process`); `glasscoded.json` lists the sessions that should run,
  and after a reboot the daemon starts them again with `--resume`.
- **Accounts** sign in on the server through the CLI's own login in a server terminal. Nothing
  copies credentials; the dialog shows only whether a credential file exists.
- **Tests**: `crates/glasscode_core/tests/remote_e2e.rs` drives a real Linux server
  (`script/test-server/`) over SSH: install, sessions, hooks, typing, resume, a simulated
  reboot, Codex discovery, account sign-in and cleanup.

## Moving sessions, and keeping them in sync

**Sync** (`crates/glasscode_core/src/mirror.rs` on the Mac, `crates/glasscoded/src/mirror.rs` on
the server): every 4 s while connected, on its own RPC connection, the app fingerprints each
local session's worktree (HEAD + `git status` + the listed files' sizes and times); when it
changed, `snapshot()` (seeded from the worktree's own index, so only changed files are hashed)
is pushed to `refs/glasscode/mirror/<token>/{head,snap}`. One `mirror.sync` carries every
session's state (running, activity, the open question, `.env` files when they changed, the
account) and returns what phones queued (`MirrorInput`: text, answer, keys), the sessions
continued on the server, and how much of each conversation the server has; the rest goes with
`mirror.conversation` (appended in 512 KB pieces). The server keeps mirrors in
`~/.glasscode/mirrors/<id>/`, lists them in `state.mirrors`, answers `sessions.transcript` from
the copy and queues `sessions.send/answer/keys` for the Mac. `mirror.take_over` does what
`move_to_server` does, from the synced refs and copy, and refuses while the agent is working
unless the Mac has not synced for 90 s. Sessions the Mac stops listing are forgotten, refs too.


`crates/glasscode_core/src/sync.rs` moves a session in either direction; the app runs it for
each session in turn (`crates/glasscode/src/switch_ui.rs`), waiting until the agent is idle
(Claude hooks; for Codex, its rollout file not changing for a few seconds).

1. **Worktree**: `snapshot()` builds a commit of the uncommitted state in a temporary index
   (`GIT_INDEX_FILE`), so the user's index and stashes are untouched. To the server: `git push`
   over the same SSH to `refs/glasscode/in/<token>/{head,snap}`, then `worktrees.receive`.
   Back: `worktrees.snapshot` leaves `refs/glasscode/out/<token>/…`, the Mac fetches them.
   `receive_worktree()` (used on both sides) checks the branch out at `head`, then
   `read-tree -u --reset <snap>` + `reset` so the work arrives uncommitted. It never loses work:
   uncommitted changes that differ from what last left that folder (`refs/glasscode/sent/…`)
   are stashed, and an overwritten branch tip is kept under `refs/glasscode/backup/`.
2. **Ignored config**: `.env*` files outside ignored folders travel in the request (≤256 KB
   each); `node_modules`/build output are rebuilt, never copied.
3. **Conversation**: the Claude transcript or Codex rollout is copied to the matching account
   (same id, else same name, else the default) under the new folder's project directory, with
   `cwd` fields rewritten (JSON-aware). The session is created there with `resume`.
4. **One runner**: the source session closes after the target one started.

Paths map by their place under each machine's home folder. Tested end to end in
`remote_e2e.rs` (`moves_a_session_to_the_server_and_back`), including the stash case.

## New sessions and reviews

- **Where work starts** (`glasscode_core::git`): `fresh_base` fetches the default branch and
  returns `origin/main` (or the local branch when offline); new branches are made from it with
  `--no-track`, so their own `git push -u` sets the upstream (tracking `origin/main` made a
  plain `git push` fail). `checkout_pr_branch` puts a PR's own branch in a worktree at its
  latest commit (same repository) or a local `pr/<n>` from `refs/pull/<n>/head` (forks);
  `checkout_review` uses `review/<n>` with push disabled (`branch.<b>.pushRemote` points at a
  remote that does not exist). Both reuse a worktree already on that branch and bring it up to
  date without losing anything: fast-forward when clean, a jump to the new head when it was
  force-pushed and the checkout is exactly what was fetched before, otherwise they leave it and
  say so. `crates/glasscode_core/tests/pr_checkouts.rs` covers each case.
- **PR search** (`github::search_prs`, `review_requests`): a number is looked up directly;
  words match the newest 100 open PRs on title, branch and author, with GitHub's text search
  added when few match. The New session dialog searches as you type (350 ms).
- **Review sessions**: `Session.review` (`ReviewRef`: repo, number, title, author, head,
  base) marks a session that reviews someone else's pull request. Its branch is `review/<n>`
  with push disabled. The local auto-reviewer (`crates/glasscode/src/auto_review.rs`) can
  post an overview comment on pull requests of repositories the person opted in; it never
  approves or merges.
- **On the server**: `sessions.create` takes `start` (`main`, `pull_request`, `review`),
  `review` and `review_files`; `reviews.files` / `reviews.write` read and write only those
  six files; `sessions.set_review` stores the private review's id; `ServerState.repos` gives
  each project's GitHub repository so the Mac can search PRs for it. The daemon's PR loop uses
  a review session's PR number instead of looking one up by branch. Server mode carries a
  review's notes with the session (they are not in git).

## iPhone

```
iPhone ──SSH (Secure Enclave key)──▶ server: command="glasscoded gate" ──▶ glasscoded rpc / attach <id>
   ▲                                                        │
   └──────────── APNs (openssl JWT + curl) ◀────────────────┘ needs: permission prompts, questions
Mac ◀─one-time HTTP POST {token, name, key}── iPhone      (pairing only; the Mac runs phones.add)
```

- **Access**: the phone has its own SSH key (P-256, in the Secure Enclave on a device). It is
  added to `authorized_keys` with a forced command, `glasscoded gate`, that allows only `rpc`
  and `attach <session id>`, and with forwarding and `~/.ssh/rc` off. The phone pins the
  server's host keys it received while pairing. `crates/glasscoded/src/phones.rs` validates
  keys and names before writing that file.
- **Pairing** (`crates/glasscode/src/pair_ui.rs`): the Mac opens a listener for 10 minutes and
  shows `glasscode://pair?v=1&t=<token>&a=<ip:port,…>&n=<server>` as a QR code (Tailscale
  addresses first, then the LAN). The phone tries every address at once and POSTs its public
  key with the token; the Mac calls `phones.add` on the server and answers with the SSH
  endpoint (host, port, user, host keys, binary path). Nothing else crosses.
- **Protocol**: still version 2; `Hello.features` says what is new (`phone`, and `push` when
  `~/.glasscode/push.json` exists), so older Macs keep working. The daemon answers requests
  concurrently (a thread per request, one writer) so a slow `gh` never holds up the list.
- **What the server works out for the phone**, in `RemoteSession`:
  - `prompt`: the menu the agent's terminal shows now (`glasscode_core::prompt`), read from the
    visible tmux screen every 2 s. Only a numbered list with exactly one highlighted entry and
    nothing but footers below it counts, so a list in a reply or an answered menu does not.
    `sessions.answer` replays the keys (arrows to the option, Enter; text for "No, and tell
    Claude…").
  - `last_reply`, `updated_at`: from the Claude transcript or the Codex rollout
    (`glasscode_core::transcript`), which `sessions.transcript` also pages through by byte
    offset.
  - `pr`: a `PrSummary` (approved, CI, preview)
    refreshed every minute by `pr_loop` with the server's `gh`, and right away after
    `github.action`.
- **Push**: `announce_needs` sends each new need once per session (`push.rs`); the first pass
  after a restart only records what is already waiting.
- **The app** (`ios/`): SwiftUI, iOS 18+. `GlasscodeKit` is the transport and the model
  (swift-nio-ssh, the same JSON lines as the Mac) plus `Present.swift`, which turns a session
  into words ("Claude wants to push to GitHub", "CI failing · 2 checks"). The terminal is
  SwiftTerm's headless emulator drawn by our own SwiftUI view. Widgets and the Live Activity
  read a snapshot the app writes to the shared app group.
- **Tests**: `GlasscodeKit` unit tests, `EndToEndTests` (the phone's path against the Docker
  server: pair, gate, prompt, answer, chat, terminal) and UI tests in the simulator.

## What is not built yet

- Moving a session automatically when the lid closes (sync already keeps the server current;
  a session can be continued there from the iPhone).
- Git panel and diff view for server worktrees.
- The Ask Glasscode harness on the server (the phone has the interface, marked BETA).
- Usage meters per account (Claude status line / Codex rate-limit events).
- Terminal extras: search, clickable links, IME composition for CJK input.
- Auto-update of Glasscode itself (for now: install the new DMG or a private Homebrew tap).
