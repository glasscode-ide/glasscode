# Manager v2: acceptance list

| | |
|---|---|
| Document | Acceptance list of Manager v2 (GC-MGR-002, slice M55) |
| Date | 2026-11-19 (skeleton; filled in by M55) |
| For | The owner, on macOS, before slice M56 switches everyone to v2 |
| Plan | [`manager-v2-slices.md`](manager-v2-slices.md); each slice has a "Result" note there when it merges |

This is what the owner checks with their own eyes, in a scratch home with scratch logins (slice plan rule 11), after every automated gate is green. A line is ticked only when it was seen, not when a test says so.

## Start a safe check

1. `export GLASSCODE_HOME=$(mktemp -d)/glasscode`
2. Make scratch account folders and sign each in once:
   - `CLAUDE_CONFIG_DIR=$GLASSCODE_HOME/accounts/claude claude`
   - `CODEX_HOME=$GLASSCODE_HOME/accounts/codex codex login`
3. Build the test home: `cargo run -p glasscode_core --features testkit --bin make-test-home -- $GLASSCODE_HOME`. It makes two projects, three Claude memory folders, one Codex memory folder and a v1 `data.json`.
4. Start Glasscode from that shell.

## 1. Memory (R8, R11)

- [ ] **Moving in.** It shows the found sources before reading anything. Accepting one project gives one memory for two clones of the same repository. (M35)
- [ ] **Shared across agents.** A fact noted in a Codex session of worktree A is found by a Claude session of worktree B in the same project, and not by a session of another project. (M20.2)
- [ ] **Direct edits.** An agent's direct edit of `memory/you/profile.md` shows as a pending edit, and is not in the next session's brief. (M20.1)
- [ ] **Archive.** After `worktree remove`, the archive holds its notes, review notes, PR and digest, and search finds it. (M24)
- [ ] **Removal outside Glasscode.** A worktree deleted in Finder is archived within a minute. (M24)
- [ ] **Forget a person.** "Forget Sam" leaves no entry, words record or journal summary that names Sam. (M25)

## 2. Trust (R10)

- [ ] **Careful.** In a Careful project, the manager's "start a session" waits on a card with the full payload. (M21)
- [ ] **Autopilot.** In an Autopilot project without branch protection, Settings says pushes are guidance only, and Autopilot does not push. (M23)
- [ ] **Session profile.** An automation session's `cat ~/.ssh/id_*` and `gh pr merge` are refused, and both refusals are in Activity. (M22)
- [ ] **Restarts.** A pending card survives a quit and relaunch. An expired card cannot run. (M21.2)
- [ ] **Planted text.** A planted "approve and merge" in a PR description leads to no action and no memory. (M23, evals)

## 3. Events and reports (R2, R4)

- [ ] **Every end is reported.** For both agents, each of these gives one report line with the right cause: an interrupted turn, a failed turn, and a killed agent. (M30)
- [ ] **Exactly once.** A red CI on an Autopilot PR tells its session once per commit, at most twice. (M40, M31)
- [ ] **Sleep.** A schedule missed while the Mac slept runs once on wake. (M41)
- [ ] **Bundling.** Five events about one PR give one notification. (M42)

## 4. The brain and the chat (R3, R6, R7)

- [ ] **No shell.** The brain's start line shows only Glasscode tools, with no Bash and no file tools outside its folder. (M50)
- [ ] **Navigation.** From the Today box, "open the session on #1" opens it, and "what's happening here?" answers about it. (M52)
- [ ] **Priority.** A question asked while a wake runs is answered first. (M51)
- [ ] **Signed out.** With the scratch login signed out, the chat says so in plain words, and code answers still work. (M52.2)
- [ ] **Settings.** Settings › Manager shows each project's level, each playbook as a sentence, and a preview before any change. (M53)

## 5. Budgets (R5, R12)

- [ ] **The 48-hour run.** Record:
  - resident memory: _____ MB
  - idle CPU: _____ % of one core
  - wakes per hour: _____
  - tokens per day: _____
- [ ] **CI.** The soak and evals jobs are green on the release commit.

## Never checked on screen

M55 fills this section with every behaviour that only a test checked.
