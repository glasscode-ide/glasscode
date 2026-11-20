# Manager v2: acceptance list

| | |
|---|---|
| Document | Acceptance list of Manager v2 (GC-MGR-002, slice M55) |
| Date | 2026-11-20 (M55: the automated gates and the test-only list filled in) |
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
- [ ] **CLI versions.** With Claude Code older than 2.1.322 (or Codex older than 0.167.0) on the
  PATH, the chat says which version it needs and the brain does not start. (M55)

## Before release 1, outside the tests

- [ ] **Three security reviews**, each as its own review pull request by a reviewer who built none
  of the slices, with their findings fixed (exec-24): the gate and callers; session profiles and
  hooks; memory and words. The checklist for each: who can call each new op; what an agent's
  shell can change that the gate trusts; what is stored in plain text; spoofed prompt boxes for
  `session.answer`; argument injection in each external command.
- [ ] **The headless items of the CLI checklist** ([`architecture.md`](architecture.md), items 10
  to 16) on the real CLIs at the minimum versions, with scratch logins.
- [ ] **The live evals** with the real CLIs (exec-4's third part, trust-17's second suite, ops-23's
  selection run), in a scratch home and within the weekly budget.

## Never checked on screen

Each of these is checked only by a test. The automated gates of release 1 (slice plan M55 step
2) are marked "gate".

| Behaviour | Test | CI job |
|---|---|---|
| Gate: seven simulated days of the reference load, with sleeps, restarts, crashes and partitions, give exactly one report per turn and no duplicate effect | `testkit::soak::seven_day_soak` | soak |
| Gate: over the soak, routine wakes stay within six an hour, the journal keeps 30 days, and open files and threads do not grow | `testkit::soak::seven_day_soak` (a day of it: `one_day_soak`) | soak, linux |
| Gate: every store survives a damaged file (with and without its previous generation), a newer schema, and a save that fails at each step | `store::tests::fault_injection_for_every_store` | soak, linux |
| Gate: memory lost from the disk comes back from an export, and search finds it again | `portability::tests::restore_after_damage` | soak, linux |
| Gate: forgetting a person leaves nothing that names them | `portability::tests::forget_person_removes_everywhere` | soak, linux |
| Gate: the adversarial suite: a planted instruction in every channel gives no effect and no fact; forged callers get nothing | `tests/evals/adversarial.rs` | evals |
| Gate: no eval case waits but the one that needs the Docker test server | `only_the_listed_cases_wait` | evals |
| The effect ledger does an effect once with a failure at each step | `effects::tests::fault_injection_at_each_step` | linux |
| The memory poisoning set: planted facts are not stored and not returned | `tests/evals/memory_poisoning.rs` | evals |
| No object of the server's memory copy holds a local-only project's text, its index lines or its inbox proposals | `a_local_only_project_never_reaches_the_server`, `memory_copy::tests` | evals, linux |
| A session on another account reads nothing of the project | `another_account_does_not_read_the_project` | evals |
| An unpaired phone decides nothing, not even an approval | `an_unpaired_phone_decides_nothing` | evals |
| An unpaired phone's key is refused before any gate runs | `an_unsigned_phone_gets_no_caller` (ignored: needs the Docker test server) | none |
| Retrieval recall at 5 is above its floor | `tests/evals/retrieval.rs` | evals |
| Natural requests select the right op, and some select none | `tests/evals/op_selection.rs` | evals |
| The brain runs only in its work folder; a repository as its folder is refused before anything starts | `the_brain_spawns_only_in_its_work_folder`, `the_codex_brain_spawns_only_in_its_work_folder` | linux |
| Below the minimum CLI version the brain does not start, and says once which version it needs | `runtime::tests::a_cli_below_the_minimum_does_not_start` | linux |
