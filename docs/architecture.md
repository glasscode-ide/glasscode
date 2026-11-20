# Glasscode architecture

Checked against the code of `ui-redesign` at `15eb78a`, after the UI redesign (the 25 slices of
[`ui-redesign-slices.md`](ui-redesign-slices.md), with the code sweep of U23).

## Decisions

| Decision | Choice | Why |
|---|---|---|
| Base | Own app on a frozen copy of GPUI, not a Zed fork | No weekly Zed rebases; we own ~150k lines of UI toolkit instead of ~1.7M lines of editor |
| Code editing | File tabs that show a file or its diff, with a small editor (U20); "Open in editor" for more | Agents write the code; developers review it and make small changes |
| Terminal | `alacritty_terminal` + our own GPUI view | Apache-2.0, so the app stays free of GPL code |
| Agents | Official `claude` / `codex` binaries in a PTY | The only subscription use both vendors clearly allow; no token handling |
| Accounts | One config folder per account | Documented by both CLIs (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`) |
| CLI updates | Team-approved versions, self-update off | CLIs change daily; session formats are internal |
| Platform | macOS and Windows (client; Windows on the `windows-port` branch, release 1 without the server features); Linux for the server daemon | Built for the Mac first; Windows through one source tree and `glasscode_core::sys` |
| Backdrop and glass | Our own frames, made as pixels on a background thread; a glass card paints a blurred crop of the same frame | The vendored GPUI has no backdrop blur and no custom shaders; nothing but our backdrop is behind a card (U02, U04) |
| Global scale | `theme::sz` for sizes and the window's rem size for GPUI's short forms; no vendor change | Decision P5 (U02, U05) |
| Dither finish | A full-size image; no vendor change | GPUI scales images bilinearly; decision P6 (U02, U04) |
| Syntax colours | A small scanner of our own; no dependency | Decision P6 (U21) |
| GitHub reads | Glasscode asks GitHub itself (`github_api`, `ureq` over kept-open connections), one GraphQL query per account and repository; `gh` for writes, for tokens and as the fallback | A `gh` process per question took about 0.5 s, one at a time: replayed over 22 real sessions, the old round made 37 calls in 23.7 s; the new one makes 5 queries in about 1.3 s ([GitHub reads](#github-reads)) |

## Processes

```
Glasscode.app (GPUI)
 ├─ one PTY per open session ── claude / codex / zsh   (cwd = the session's worktree)
 │      └─ Claude hooks append JSON lines to ~/.glasscode/events/<session>.jsonl
 ├─ background tick (700 ms): read hook events, find Codex conversation ids,
 │      refresh git status (3 s) and worktree lists (15 s) in the background
 ├─ tick (350 ms): requests of the glasscode command, the Chat view on screen
 ├─ tick (250 ms): the voice state (macOS)
 ├─ backdrop frames on a background thread (10 each second while a screen shows it)
 ├─ gh ── pull request of each session's branch; for the PR in the open Checks or Pull request
 │      tab (30 s): pr view (checks, reviews, comments, labels), GraphQL review threads,
 │      deployments of preview/pr-<n>; actions: pr edit --add/remove-label, pr comment,
 │      run rerun --failed, thread reply/resolve, pr create --draft. Each call times out at 30 s.
 │      The person's own open PRs (gh search prs) for Today and Pull requests.
 ├─ programs and ports of the sessions for the status bar (every 15 s while the window is
 │      active, every 2 s while Resources or Ports is open)
 ├─ usage of each account: 20 s after launch, then every 15 minutes
 └─ state.json written after every change
```

On Windows the app is `glasscode.exe` (`Glasscode.exe` when installed): each session's PTY is a
ConPTY, the shell is PowerShell, and the `glasscode` command is a second program (below).

## Two platforms

One source tree builds for macOS and Windows. Operating system code lives in
`glasscode_core::sys` (`crates/glasscode_core/src/sys/`): `mod.rs` has what both share, `unix.rs`
and `windows.rs` give the same names (the control channel, file locks, local time, random bytes,
`command`, `find_program`, `processes`, `default_shell`, secrets, `open_path` / `reveal_path` /
`open_url`, sounds, `spawn_detached`). Since slice U08, `mod.rs` also has `process_use` (processor
and memory of each program, with `sysinfo`), `listeners` (the TCP ports that programs listen on:
`GetExtendedTcpTable` on Windows, `lsof` on macOS and Linux) and `end_tree` (end a program and the
programs under it). The rest of the core calls `sys` and keeps no platform conditions of its own;
external programs start with `sys::command`, which on Windows finds `.cmd` and `.bat` programs and
opens no console window. In the app, the words that differ ("this Mac" / "this PC", "Finder" /
"File Explorer") come from `platform.rs` and every key binding from `keymap.rs` (⌘ on the Mac,
Ctrl and Ctrl+Shift on Windows). A change for Windows keeps the macOS behavior the same.
[`windows.md`](windows.md) has the Windows side in full.

The voice helper is a macOS program, so `glasscode_ui::VOICE` is false on Windows: no Glasscode
tab, no voice settings and no microphone buttons there.

## The window

The UI redesign (GC-UI-001) rebuilt the window to match
[`ui-redesign-mock.html`](ui-redesign-mock.html). The app keeps its structure: one `Workspace`
entity, with `impl Workspace` blocks in many files, and polling ticks. `Workspace::render` draws
the whole window through `shell::render_shell`.

### The shell

`crates/glasscode/src/shell.rs` (slice U03) is the frame of the window, from the top:

- **The title bar:** the mark, the left panel button, Back and Forward, the place (project and
  branch), the Jump field, the update, move and server pills, the right panel button, and on
  Windows the window buttons (`window_controls.rs`). In a narrow window `title_fit_at` drops the
  wordmark first, then the place, then the words of the Jump field.
- **A row** of the left sidebar, its edge, the centre, the right panel's edge and the right panel.
- **The status bar** (`status_bar.rs`, U17).
- **Over them, in this order:** the status bar's popover, the UI kit page and the backdrop page
  (debug), the row menu, a dialog, Jump, and a toast. `Workspace::render` puts the question
  "Save the changes?" of the editor over all of them.

**Views.** `shell::View` says what the centre shows: `Today`, `Sessions` (the default), `Board`,
`PullRequests`, `Usage`, `Settings` (its Appearance section was the Themes page), `FirstRun` and
`Manager` (the manager's page, [`manager.md`](manager.md); its row tops the left sidebar while it is
on, and its bubble sits above the status bar in the other views). The view is not saved. The left sidebar shows in each view but Settings (it has its own section
menu) and the first run. The right panel shows in Sessions (the session's tabs), Today (Inbox and
Activity) and the Board while a ticket is open in its reader.

**Tabs of the centre.** `shell::CenterTab` is a session (`Session(id)`) or a file (`File(path)`);
`Workspace.tabs` and `tab` hold the strip of the Sessions view (`session_view.rs`). The session
tabs are the sessions of the worktree on screen; the file tabs are the files open in that
worktree, kept for the run in `FileTabs` (`file_view.rs`) and not saved.

**History.** A `Place` is a view and, in Sessions, the session on screen. `go(view)` and a session
that the person opens (`open_session`) put the place left on the back list (at most 50) and clear
the forward list; `go_back` and `go_forward` walk the two lists. A session that becomes active in
the background (`select_session`: an agent's command, the glasses) is not a place, so it never
takes the person out of Today or Settings. While the first run shows, nothing leaves it but its own
buttons. File tabs are not places.

**Panels.** `Layout` in `state.json` keeps the sidebar widths (`sidebar`, `right`, `tickets`, in
logical pixels, as at 100 %), `left_hidden`, `right_hidden`, `right_tab` and the folded tree rows
(`folded`); each new field has `#[serde(default)]` and is not written at its default, so an old
build reads the file. A drag that leaves a sidebar narrower than 140 px hides it; its button brings
it back at the old width. `fit_panels_at` narrows the sidebars in a narrow window, the right one
first, so that the centre keeps 320 px where it can.

**Popovers.** `shell::Popover` is `Usage`, `Resources` or `Ports`; `status_bar.rs` draws them. A
click outside or Esc closes one, and the click does not reach what is below.

### Screens and their files

| Screen | Files (`crates/glasscode/src/`) | Slice |
|---|---|---|
| Shell, history, panels, title bar | `shell.rs`, `window_controls.rs` | U03 |
| Colours, icons, the UI kit | `theme.rs`, `icons.rs`, `ui.rs`, `kit.rs` (debug page) | U01 |
| Backdrop and glass | `backdrop.rs` | U04 |
| Global scale | `theme.rs` (`sz`, `scale`, `rem_size`) and the files of each screen | U05 |
| Tree sidebar | `sidebar.rs` (rows from plain data), `sessions_ui.rs`, `shell.rs` (`render_left`) | U10 |
| Today | `today_ui.rs` (state, start box, "Your list", right panel), `today_cards.rs` (the seven cards) | U11 |
| Session view and tabs | `session_view.rs`, `right_panel.rs`, `chat_ui.rs` (the column and the message box) | U12 |
| File tabs | `file_view.rs`, `files_ui.rs` | U13 |
| Board | `tickets_ui.rs` (state), `tickets_view.rs` (drawing) | U14 |
| Pull requests | `prs_view.rs` | U14 |
| Jump | `palette_ui.rs` | U15 |
| Settings | `settings_ui.rs` (13 sections from one table of cards), `themes_ui.rs` (applies the appearance) | U16 |
| Status bar, usage, resources, ports | `status_bar.rs` | U17 |
| First run | `first_run.rs` | U18 |
| Voice screen and orb | `voice_view.rs` | U19 |
| Editor | `editor.rs` | U20 |
| Syntax colours | `file_view.rs`, `editor.rs`, `theme.rs` (code colours) | U21 |
| Project settings, the setup question | `project_setup.rs`, `settings_ui.rs` | U22 |
| Programs outside Glasscode (folders, files, editors), started from a task | `center.rs` (`start_outside`, `open_url`) | U17, U23 |
| The manager: page, sidebar row, bubble, Settings › Manager, its round, its view (says, points, walks) | `manager_ui.rs`, `manager_settings.rs`, `manager_round.rs`, `manager_view.rs`, `manager_tick.rs` (v1's round, until M56's final removal), `manager_voice.rs`, `scenes_ui.rs` | from `main` |

### Core modules of the redesign

`glasscode_core` has no window. The redesign added these modules; each has tests in its file.

| Module | Slice | What it does |
|---|---|---|
| `activity` | U07 | Counts the sessions started and the pull requests merged (with their lines) on each local day, in `~/.glasscode/activity.json`, for Today's heat map and "This month". It starts on the first day that something is counted; there is no history from before. |
| `backdrop` | U04, U19 | The backdrop (`render`, three presets: Prism, Aurora, Dusk; finishes Smooth and Dither), its blur, and the voice orb (`orb`), as pixels. A port of `draw()` and `drawOrb()` of the mock. No window and no clock: the caller gives the size and the time. |
| `discover` | U09 | The projects and agent logins already on this computer, for the first run: the folders opened in Cursor or VS Code or worked in with Claude Code or Codex, the system logins, Glasscode's own account folders, and the names of the accounts that claude-swap saved. A login counts by whether its file exists; the file is never opened. |
| `project_settings` | U22 | A project's `.glasscode/settings.json`: `setup`, `branch_template`, `worktree_folder` and `default_agent`. The templates take `{repo}`, `{branch}`, `{issue}`, `{type}` and `{slug}`, and a folder always stays below Glasscode's worktree folder. The approval of a setup command is a SHA-256 in `State.setup_approvals`. |
| `resources` | U08 | Which session each running program belongs to (by its parent chain to a session's terminal), and what each session uses together. Pure. |
| `save` | U20 | Saves a file of a worktree: a temporary file in the same folder, renamed over the file, and only over the version the text is based on. Keeps the copies of a quit that cannot ask in `~/.glasscode/unsaved/`. |
| `syntax` | U21 | A scanner that gives the coloured parts of one line (comment, string, number, keyword, type, key, heading) for nine languages, chosen by the file name: Rust, TypeScript, JavaScript, JSON (and JSONC), TOML, Markdown, Python, shell and PowerShell. A block comment, a multi-line string or fenced code carries a state to the next line; `Starts` keeps the state of each line and, after an edit, scans again from the edited line until a line starts as before. |

Also new in the core: `github::my_open_prs` (U07, the person's open pull requests, through
`gh search prs`), the usage scan that runs without the Usage page (U07), and in `sys` the three
functions of U08 (above). In tests, `paths::root()` is a temporary folder of the test process
(`paths::use_test_root`, U23), so no test reads or writes the person's `~/.glasscode`.

### Core base of Manager v2 (M10)

[`manager-v2-design.md`](manager-v2-design.md) has the full rules in sections 5, 6 and 8.

**`store::Store`** is the base of every new store. Each store's file is an envelope, `{"store", "version", "data"}`, with these rules:
- **Saving** holds `<file>.lock` and goes into a temporary file, which is synced and then renamed over the file (`MoveFileExW` on Windows). The file before it is kept as `<file>.prev` when it loads, and as `<file>.broken` when it does not. A failed write, sync or rename is an error.
- **A damaged file** is copied to `<file>.broken` and left in place. Glasscode then reads `.prev`. Only when `.prev` cannot be read does the store's rule apply: `Refuse` stops the feature that needs it (it never starts empty), and `Empty` starts fresh. A refusing store whose file is gone beside its `.prev` refuses too.
- **Stores that fail closed** say `FALLS_BACK = false`, and then no way of reading them gives `.prev`. The engine switch is one: damaged, it is v1 with every feature off, never an older switch.
- **A newer schema** is never read and never written over. The version is read before anything else in the file.
- **The data map.** `store::DATA_MAP` is the data map of design section 6, with a row for every non-cache entry of the layout. `store::ALL_STORES` lists every store. A test reads every crate's source and fails when a store has no line or no row.
- **The first stores** are `authority/machine.json` (`MachineId::this()`), `authority/engine.json`, the engine switch (v1 until a home switches, M56; each v2 feature off until it is switched on), and `authority/projects.json`, the project keys (M11, below).
- **`state.json`** keeps its own `store::load()` and `store::save()`, unchanged.

**`clock`** is where v2 code gets the time: `now_ms`, and `local_minutes` for the local wall time. A test puts a clock of its own in place with `clock::use_test_clock`.

**`paths::class_of`** gives every path its class, by `paths::layout()`, the table of design section 5. The classes are Authority, Words, Ledger, Trusted memory, Agent notes, Handoff, Worktree, Accounts, Hook IO, Brain work, Session tools, Cache and Credentials.
- Session profiles (M22) build their rules from these classes, never from a literal path.
- A path inside `~/.glasscode` with no row is `Classified::InsideUnlisted`, which is denied to sessions.
- `class_of` matches `~/.glasscode` and the home folder both as named and with their links resolved.
- `every_folder_has_a_class` fails when `paths.rs` gains a path function without a class.
- `every_name_joined_to_the_glasscode_folder_is_in_the_layout` fails when any crate joins a name to `paths::root()` that has no row. It cannot see the folder held in a variable.
- `ProjectKey` and `MachineId` are path-safe newtypes in `paths`.

### Identity (M11)

`glasscode_core::identity` says which project a folder is, which piece of work a session is on, and in which order the records of several machines were written. Nothing calls it yet; memory (M20), the work ledger (M32) and sync (M57) will.

- **`ProjectKey`**, such as `glasscode-3f9a1c`, is a slug and six hex digits of the folder's first alias: its own remote, else its upstream, else its folder. It names the project's folder in memory and agent notes, and it is separate from the app's project id, which is a UUID per folder and machine. The same remote gives the same key on every machine.
- **`facts(dir)`** reads a folder with git:
  - the main checkout, which a worktree and a subfolder share;
  - `glasscode.repo`, then `origin`, normalized by `github::normalize_remote`;
  - `upstream`;
  - the root commits, skipped in a shallow clone.

  Git failing on a repository is an error, never a plain folder.
- **`matching`** checks, in order:
  1. a folder the person pinned;
  2. the folder's own remotes;
  3. `upstream` (a fork, so it asks);
  4. a root commit (known with the folder, else it asks);
  5. the folder, unless it now holds another repository.

  `KeyMatch::AskFork` is asked once, and `Projects::pin` keeps the answer.
- **`authority/projects.json`** (`Projects`) holds each key's remotes and roots, and each machine's folders, pinned folders and project ids. Each machine reads only its own (`known(machine)`). The store refuses on damage and never falls back.
  - A bound project's key comes from its binding (`key_for`), never from what its checkout says now.
  - `attach` only adds aliases, and never a remote another key has.
- **`WorkKey`** names a review, a PR a session was started from, a ticket, or a branch. Its `file_name()` is a lower-case slug and a hash of the exact id.
- **`Hlc`** stamps `(wall_ms, count, machine)`, with the wall time from `clock`.
  - `now` and `merge` are always later than what they saw. A clock that goes back does not take them back.
  - A stamp more than an hour ahead of the clock is `too_far_ahead`, and is quarantined rather than merged.

### Effects and delivery to sessions (M31)

These run in a Manager v2 home only. With the engine at v1 every send is typed as before.

- **The effect ledger** (`glasscode_core::effects`, `effects/<yyyy-mm>.jsonl`) is how an effect happens once. An agent, a rule or the manager claims the effect's key before it acts; the claim is synced to disk. The result is written after. The same key again gives the stored record. Claims of an earlier run with no result are settled at the first tick by the op's probe, with no model. An at-most-once effect with no probe is left uncertain for the person. `ops_run::claimed` wraps every effect op of a non-person caller. Each request has a subject of its own, so the ledger records it and repeats nothing.
- **Delivery to a session** (`glasscode_core::delivery_session`, run from `ci_fix.rs`) gives each session one queue that is the only writer of its terminal. It is used by `session send` from any caller, the CI fix and the manager's messages. A message waits until:
  - the session is idle;
  - nothing is asking on its screen;
  - nothing is typed in its box;
  - nobody has typed in it for 10 s.

  The message is typed with a marker such as `[gc:1a2b3c4d]`. Its Enter is held back when a prompt came up after the paste. It is delivered when the session's `UserPromptSubmit` hook event carries the marker. Other rules:
  - a newer head of the same PR drops a CI ask that is still waiting;
  - the same ask twice is one delivery;
  - every message expires after a day;
  - while a message is being typed, `session answer` and `session keys` answer `busy` (ops-m2), so no key lands inside it;
  - the session's header says how many messages wait and until when (`Queues::header`, from `Wait::words`).
- **Probes after a restart** (`ci_fix::reconcile`):
  - a `session send` is settled by its events file;
  - a board move by the board;
  - `pr open-draft` by GitHub: an open pull request with the session's branch as its head means it happened, none means it did not, and no answer is not known. Its claim keeps the repository and the branch (`effects::probe_open_draft`).
- **Activity** on Today lists the ledger's autonomous actions of the last 30 days, each with who set it off, why, and the result.

### Playbooks (M40)

`glasscode_core::playbooks` holds what Glasscode does by itself when something happens, without a model.

- **The format** is typed JSON tables in `authority/playbooks.json`:
  - an event kind, with a `where` table of field = value (or a list of allowed values);
  - typed conditions;
  - an op whose argument slots are the person's text or a typed field of the event (never a title or a comment);
  - an autonomy;
  - a limit per commit, PR, session, hour or day.

  `validate` checks each playbook against the journal's field schema. An invalid one is switched off with one alert, and the rest load.
- **Approval** works as the policy's does. Each machine keeps the hashes it approved in `authority/approved/playbooks.json`, which is never synced, so a synced or hand-edited playbook does not run. Anything other than the person can only propose. Any caller may pause; only the person releases.
- **The engine** is pure. When several playbooks would do the same thing about one event, they share one effect key and the strictest autonomy wins. The limits are:
  - depth 3;
  - 4 sends per session per hour and 12 per day;
  - one run per subject;
  - a breaker after 5 firings on one subject in a day or 3 failures in a row;
  - a global breaker.

  The counts are read from the effect ledger. `simulate::dry_run` replays events and acts on nothing.
- **Built-ins** are code. The CI and conflict fix (`builtins::ci_fix`, with `autofix` as its engine) is app behaviour. It runs with the manager off, needs no approval, and its switches stay in `state.json`. In a v2 home `ci_fix.rs` takes its subject, group and key from there.
- **The manager's own built-ins** (`native`) are needs-you, review-tell (only `trusted_author` reviews reach a session), stuck-session and usage-handoff, as state machines over journal events. `parity_with_harness_step` holds them to v1's harness. They run with the v2 manager (M51); until then v1's harness does it.
- **The runner.** In a v2 home, `ci_fix.rs` reads the journal's new events every 2 s off the main thread and runs the person's live playbooks through the engine.
  - **watch** records, and **tell** shows a toast.
  - **ask** and **act** claim the effect and run the op through the gate as the manager's wake.
  - The playbooks that gave way are recorded for their limits.
- **`glasscode playbook check`** says which playbooks run and why the others do not. **`glasscode playbook test <id> [--since 7d]`** is the dry run over the journal.

### Schedules and lifetime (M41)

- **`glasscode_core::schedule`** works out what is due from the wall clock on every round, never from sleeping timers.
  - A daily time is local (`clock`). A time the spring change skips runs at the next valid minute, and a time the autumn change repeats runs once.
  - Runs missed while asleep collapse into one, the last. It runs within its window (6 hours for a daily schedule, half its period up to 2 hours for a repeating one) and is skipped after, with one Activity line.
  - An occurrence that comes while the one before still runs is skipped.
  - Expiry is a UTC instant, checked against the latest wall time ever seen.
  - Only the home runs a schedule.
  - Each occurrence is marked started in `authority/schedules-state.json` before it acts, and claimed under the schedule's id and the occurrence's instant, so a quit in the middle leaves it uncertain, never repeated.
- **The app** (`lifetime.rs`, v2 only):
  - It runs a schedule round every 30 s off the main thread. A due op runs through the gate as the manager's wake.
  - After a start or a wake, schedules wait a minute, and late runs go one a minute. A wake is a gap between the wall clock and the monotonic one.
  - While an automation turn runs, it holds an idle-sleep assertion (GPUI's `prevent_idle_sleep`: IOKit on macOS, `SetThreadExecutionState` on Windows).
  - Quitting during automation turns asks: wait for them, or quit and leave them interrupted.
  - "Restart to update" waits for them, 10 minutes at most.

### Telling the person (M42)

In a v2 home every notification goes through `glasscode_core::delivery` first. The rules:

- Each notification belongs to a **matter**: its session, approval, PR or ticket.
- **The same words** are never said twice, answered or not. Only new evidence is news.
- **New evidence on a matter told under 10 minutes ago** updates the notification on screen, with no new popup and no sound.
- **Busy stretches.** Past three popups in a minute, or twelve in an hour, the rest are Inbox lines (journal lines in Today's Activity).
- **Quiet hours** (`state.notify.quiet`) hold all but urgent notices, and give one digest when they end.
- **Opening or dismissing** a notification acknowledges it.
- **The brief** (`manager brief`) ends with what was told and still waits, with its age.
- **Messages to teammates** still wait for the person by default (`SendPolicy::AskAlways`). `messages::send_intent` is the at-most-once claim for a send.

### v1's data, moved (M44)

`manager::migrate_v2::run` moves the rest of v1's `manager/data.json` into the v2 stores, on the home, in a v2 home. It copies the file to `data.v1.json` once and leaves `data.json` in place. v1's manager still writes that file in a v2 home, so the app runs the migration at each start, off the main thread. A run of an unchanged file does nothing; a run after v1 changed it takes the changes by v1's own merge rule. A file that does not parse stops it.

- **Follow-ups** become work items (`WorkItem::from_follow_up`; a repository with no checkout gets the key its checkout would, `identity::key_for_remote`). The manager's built-ins get their entries, switched as v1's manager was.
- **The outbox** moves to `manager/outbox.json` with the same ids and states. A sent message's send is a settled effect, so it is never sent again.
- **Checks** become schedules `v1-check-<id>`, proposed by the v1 manager: off until the person approves them.
- **The conversation** goes to M43's store, as "v1, origin unknown" lines.
- **`seen` keys** become settled effects (`seen_key`) and the built-ins' state (`manager/builtins.json`). A session v1 told about is not told again in that stop, and a review v1 had not told yet is still told.

`manager/migrated.json` marks the version and the hash of the file it read. Every step is idempotent besides. Records of v1's `seen` keys stay out of Activity.

### Memory (M20.1)

`glasscode_core::memstore` is Glasscode's trusted memory: one git repository of Markdown under `~/.glasscode/memory/`, written only by this module (D19, D20). Nothing in v1 calls it; the memory ops and the brief come in M20.2.

- **Layout** (`memstore::layout`): `you/profile.md`; `org/<employer>/people.md` and `glossary.md`, one folder per employer; `projects/<key>/` with `PROJECT.md`, `POLICY.md`, `decisions.md`, `knowhow/<topic>.md`, `work/<work file>.md` and `archive/<yyyy-mm>/`; `inbox/proposals.md`; and `INDEX.md`, which M20.2 generates. `inbox/rejected.md` keeps text a write could not save; it is never committed and never read for a brief. A Codex session gets its brief from a `SessionStart` hook too, once Codex trusts it (M22, P06; `codex_hooks`). At the start, in the background, the app does three things for each Codex account:
- adds memory's brief hook to the account's `hooks.json`, keeping the person's own hooks;
- asks `codex app-server`'s `hooks/list` for its key and hash;
- writes the trust of that hook only into the account's `config.toml`.

A repository's hooks stay untrusted. A Codex home whose hook is trusted gets no "ask for your brief" line in its instructions; one started before that asks, as before. Uninstall takes the hook and its trust out again (`MadeKind::CodexHook`). Before Claude compacts a session's conversation, its `PreCompact` hook lets the app write the session's last turns into its work file (land-18), with no model (`turns::compaction_digest`, `Memory::write_turns`). The turns go in a "Before compaction" section of a few lines: what each turn ran, what failed, what changed, what it committed, and what it asked or said. The brief after the compaction carries this section, with the work's Status, under "Where this work stands" (`work::generated_sections`). Each project's `POLICY.md` starts with a generated, read-only header (M21.1 step 6): the policy in force in words, from `gate::policy_header`. The app writes it when the policy changes (`Memory::write_policy`, one commit as you), and keeps the prose under it, which is guidance only.
- **Entries** (`memstore::entry`) are Markdown bullets with an HTML comment as header: `- Sam owns billing. <!-- id=… status=confirmed kind=fact source=words:W123 at=<hlc> -->`.
  - A fact about code cites its lines: `code=<path>@<commit>#L<first>-<last>:<sha256>`. It leaves the brief when those lines change.
  - Entries merge by stamp, and a forget leaves a tombstone with no text.
- **Facts and know-how.** A fact needs the person's own words: a quote of at least three words, typed by them, that names what the fact names.
  - Tier-one words in instruction form ("Remember that …") are confirmed at once.
  - Other tier-one facts wait as proposals for the person's tick.
  - Tier-two words are "mentioned once".
  - Know-how from a session is `learned` and agent-written. A know-how file the person edited takes no agent's change, only proposals for it.
- **Writes.** Each write is one commit, made with plumbing from the last commit Glasscode made, never from the working tree or `HEAD`.
  - Every git call names the repository itself, drops git's inherited variables, and reads no system or global configuration: no hooks, no signing, no filters.
  - The author is `Glasscode <glasscode@localhost>`, with a `Who:` trailer.
  - One writer at a time: `authority/memory.lock` and a thread lock.
- **The head** (`authority/memory-head.json`, `MemoryHead`) is that last commit. It refuses on damage and never falls back. Every read (`Snapshot`) comes from it.
  - A direct edit, or a commit made with plain git, is a pending edit (`Memory::pending`).
  - The person accepts it (`accept`: changed entries become `person-edit`, bare bullets are adopted) or discards it (`discard`).
  - A write to a file with a pending edit waits.
- **The scan** (`memstore::scan`) refuses, before any text reaches git:
  - instruction-like phrases (an advisory English list);
  - invisible Unicode;
  - URLs with credentials;
  - tokens;
  - values of the project's `.env` files (eight characters or more).
- **Caps** (`memstore::caps`, the table in design section 4) count the text of entries not let go. A write over the cap fails with "Merge or let go first" and lists the entries. `merge`, `let_go` and `rewrite` consolidate; from anyone but the person, new words may not add a name, number or identifier.
- **Forgetting a secret** rebuilds the repository into one fresh commit and deletes every other object.
- **Ages** count active days: UTC days on which the journal has an event for the project (`active_days`).
  - `mentioned` entries go after 30 active days, and `learned` ones after 90, unless they cite code that holds.
  - `let-go` entries are archived after 30 active days.
- **A project's key** is bound only to a folder that exists (`bind_project`).

### Memory: brief, search and ops (M20.2)

- **Search** (`memstore::search`) is BM25 over heading sections of Glasscode's last commit, with no model.
  - Words are folded (case, accents); identifiers and paths are split while kept whole, so `retryBackoffMs`, `retry_backoff_ms` and "retry backoff" find each other. The employer's `glossary.md` adds synonyms.
  - The index of a commit is built once (about 0.6 s for 10 MB in a release build) and kept; a query on 10 MB takes 1.6 ms p95.
  - A session reads its project, its employer's folder and the person's profile. The boosts run from its own work file, to the project, to the person and the org, to the archive. The manager reads every project.
  - Results are a data block (`<memory-results>`), not instructions.
  - Recall at 5 on the retrieval set is 0.977 (`floor.json`).
- **The brief** (`memstore::brief`) is built in one read of the commit (8 ms p95), within 8,000 characters. Its order:
  1. the policy header;
  2. the person's profile;
  3. `PROJECT.md`;
  4. the work file;
  5. the project's `INDEX.md` lines;
  6. a pointer to the repository's own `AGENTS.md` and `CLAUDE.md`.

  Only confirmed entries are given as what memory holds. Work notes, unconfirmed entries and index lines sit in one `<notes>` data block. Agent notes are never read. The manager's brief lists one line per project.
- **`INDEX.md`** is regenerated with every commit, from the files that changed.
- **Ops** (`ops/table_memory.rs`, `v2_only`): `memory.search`, `show`, `brief`, `note`, `propose`, `accept`, `reject`, `merge`, `let_go`, `rewrite`, `forget` and `pending`.
  - `accept`, `reject`, `forget` and `pending` are the person's only.
  - With v2, the v1 words `manager remember`, `forget` and `memory` reach the new memory (a proposal, a forget, a search).
  - Only the app's window is the person to memory; a session reaches only its own session and project; a change by anyone else to what the person confirmed is a proposal.
- **Delivery** happens with v2 and the engine switch `brief` on (rule 2: with v1 nothing changes).
  - A Claude session gets a second `SessionStart` hook, `"<glasscode>" memory brief --hook`. It prints the fixed memory instructions and the current brief as `additionalContext` at start, on resume and after a compaction (P03).
  - A Codex session gets the fixed instructions in `developer_instructions` and asks for its brief itself. Its hook waits for Glasscode to trust it (M22).
  - An auto-review run gets its project's brief, read-only, ahead of its task.

### Memory: the archive (M24)

`memstore::archive` keeps a piece of work in memory when it ends, however it ends (D23). The archive goes to `projects/<key>/archive/<yyyy-mm>/work/<work file>-<yyyymmdd>.md` in one commit, and the live work file leaves the project. Search still finds it, a little lower.

- **What it keeps:**
  - the work file's entries, with their ids;
  - the review notes (`<worktree>/.glasscode/review/`, read before the removal);
  - the pull request links with their last state;
  - the last turn report (M30's line) and the last reply;
  - the handoff notes that name the worktree;
  - for each conversation, its path and a digest (`transcript::digest`: the first prompt, the last 20 replies, the failed tools, at most 20 KB), because Claude deletes old transcripts.

  A line that holds a secret is left out and says so.
- **When it runs:**
  - `worktree remove`, `worktree clean` and the Worktrees dialog archive before `git worktree remove`. When the archive fails, nothing is removed unless forced; a clean skips that worktree and says why.
  - A worktree that goes outside Glasscode reaches the reconciler as `reconciled.worktree_gone`, now with its project. The app archives every work file that names that worktree (`Memory::archive_gone`). Work files record their worktree once a session notes in them (`Memory::note_in`).
  - A work whose pull request merged or closed a week ago, with none of its sessions open, is archived once from the work ledger's tick (`WorkLedger::due_for_archive`, `Ended::PrEnded`). When its pull request ended is the first `pr.merged` or `pr.closed`, or the first status round that found it so (`Facts::pr_ended_ms`).
  - glasscoded's own `worktrees.remove` archives in the server's memory before `git worktree remove` (`Ended::RemovedOnServer`). Memory is v2 there too, and M57's sync brings the archive to the Mac.
  - A session moved to the server is not archived. Since M57 its work file goes on there through the sync, and its archive comes when that worktree is removed on the server.
- **Idempotent:** an archive is named by its work and the UTC day the work ended, and is never written over. Archiving the same end again changes nothing. A branch reused on the same day gets the next name.
- **v2 only:** with the engine at v1, removals are as before.

### Moving in (M35)

`glasscode_core::movein` turns what is already on the computer into memory proposals, with no model and only after the person ticks it. `movein_ui.rs` is its dialog: Settings › Manager › Move in, and an offer at the end of the first run. Both exist only with the v2 engine.

- **Look** (names and paths only; nothing is opened). It finds:
  - Claude's memory folders, matched by the exact folder name Claude makes of each project folder and worktree;
  - Codex's memories;
  - the rules files of the editors and agents (`.cursor/rules`, `.cursorrules`, `.windsurfrules`, `.github/copilot-instructions.md`, `GEMINI.md`, `AGENTS.md`, `CLAUDE.md`);
  - `CODEOWNERS`;
  - 90 days of git history;
  - the board (who asks for work, and through what);
  - Linear's teams, from the Tickets view's last fetch (no network call);
  - the GitHub teams of each organisation that owns a project. This is the one online source: a read of GitHub's API as the account of that organisation's projects, unticked at first.

  An organisation's project (its remote owner is none of the person's GitHub logins) starts with its agents' memories unticked.
- **From an export.** "From an export…" moves a Glasscode export (M25) in whole: `portability::import` merges its memory entry by entry, and its policies wait to be approved again.
- **Harvest** (only what is ticked):
  - it takes bullets and lines of prose; front matter, headings, code and index lines are left out, and a `**Why:**` line joins the line before it;
  - CODEOWNERS lines become "Owners of …";
  - the committers become a "People" line;
  - the board gives a line a person ("Sam is on 2 of your board's tasks, mostly through Teams; they asked for …"), Linear a line a team with who works in it, and GitHub a line a team with at most 12 members. All three go to the person's profile;
  - lines are deduplicated across sources (one proposal, every source), and contradictions are flagged;
  - each source gives at most 40 lines.
- **Proposals** go to the inbox in one commit per project, with `source=import:<path>` and `run=<id>`.
  - A second run proposes only what memory does not keep yet.
  - Accepting (one line, or all from a source) confirms them; that is the person's click.
  - Undo takes a run back: what waits leaves the inbox, and what was accepted is forgotten.
- **Inventory** (`movein::inventory`) reads the languages, scripts, CI files and owners from file names and small files.
- **Estimates.** Once the review shows, `movein::estimate` works out what each project's study would send (what `study_reads` reads, at four characters a token, plus 2,000 tokens for the answer). The review then says the total and how it sits with what today's budget has left. Each waiting study shows its own estimate.

### Your data (M25)

`glasscode_core::portability` gives the person their data. The ops are `data.export`, `data.schedule`, `data.import`, `data.forget`, `data.move_out` and `data.uninstall`. They are the person's alone, and v2 only. Settings › Manager › Your data has Export to Downloads, Export by itself (Off, Daily, Weekly), Move out, and Uninstall…, which shows the list to confirm.

- **Export** writes one `tar` file with the `tar` program. It holds:
  - memory as of Glasscode's last commit;
  - the stores the data map marks for export;
  - the authority store, without this machine's own files and the device keys;
  - the words records that memory's facts cite;
  - a manifest.

  A project that stays on this Mac is left out unless named.
- **Import**:
  - merges memory entry by entry, by stamp and tombstone, as one commit by the person (caps do not stop a restore);
  - adds the project keys' remotes and roots, so this machine finds the keys by its own folders;
  - appends the cited words records;
  - sets the other authority files aside under `authority/imported/`, to approve again.
- **Forget**:
  - **a person:** every entry naming them, their words records, and the journal's and the effect ledger's lines naming them (the name becomes `[forgotten]`);
  - **a project:** its memory files, agent notes, key and journal lines;
  - **an employer:** every project that names it, and its folder.

  Memory's history is rebuilt each time, so the text is in no object. A forgotten secret also takes the words records it came from: the one it cites, and any that holds its text (`words::forget`).
- **The server forgets too.** After a forget that rebuilds the history, removes a project's or an employer's folder, or drops words records (a secret's, a person's), `manager/server-forget.json` asks the server (M57's sync) to do the same. It records that a restart is needed, which folders to drop and the dropped records' ids, never the forgotten text. At the next `memory.sync`, the folders go first, the records go by id from every machine's folder there, then the Mac's files with their tombstones are merged, then the server's history restarts (`sync::forget_here`). The server answers with what it did (`forgot`), and the Mac clears only that: an older glasscoded that ignores the request is asked again at each sync.
- **Scheduled export** (`ExportSchedule`, `manager/export-schedule.json`): every day, every week or every few days, to "Glasscode exports" in Downloads or a folder of the person's choosing, keeping the newest four (or as many as set). The app checks each hour. Only files named `glasscode-export-*.tar` are pruned.
- **Move out** gives Claude its notes back. Each project's agent notes go to that project's memory folder in each Claude account. A note there with other text is never written over; the new one goes beside it as `<name>-from-glasscode.md`.
- **Uninstall** lists everything Glasscode made. That covers worktrees (one with work that is nowhere else stops it), the Glasscode folder, the trust tables of its worktrees in Codex's `config.toml` (the person's own and each Codex account's), its keys in the system's keychain, and Time Machine copies, which it only names. Outside its folder it takes only those tables and keys; a test run never reaches the person's home or keychain (`paths::testing`).
- **Today's files** that are not stores are not in an export, and go with everything at uninstall.

### Memory in the app

With the v2 engine, the Manager page's Memory is memory's own page (`memstore::overview`, `memory_ui.rs`). It opens from Settings › Manager › Memory, from the Manager page's Memory tile, and with the manager off from the "manager is off" card too: memory is v2's, not the manager's.

- **Waiting for you:** the inbox's proposals, from moving in, sessions, the manager and learning. Each shows where it goes, where it came from and why it waits, with Accept and Reject, plus Accept all. Edits made by hand in the memory folder are held back and shown with Keep and Discard. They are read under memory's write lock (`Memory::pending_settled`), so a write under way never shows as an edit by hand.
- **What memory keeps,** file by file: the person first, then each project's PROJECT.md, POLICY.md, decisions and know-how. Each entry shows its standing ("Confirmed", "Learned", "Learned (v1)" for a migrated v1 note, "Mentioned once") and where it came from, with Forget. Entries let go are counted, not shown.
- **Freshness:** the page reads memory again every 10 s while it is shown. Every change runs in the background as the person's click.
- **v1's notes** come into memory at each start on the home, beside M44's migration (`memstore::migrate::run`, #196). Each becomes a "learned (v1)" entry of the profile.

### Memory's learning (M62)

`glasscode_core::learning` keeps memory in shape and learns, with v2 only. It runs only when the engine switch's `learning` feature is on: Settings › Manager › Learning, off at first. It runs only on the manager's home, so a Mac and a server that share memory never both learn. On the Mac, `learning_host.rs` runs it once an hour in the background, starting five minutes after launch, while the Mac is home (`home_here`). On the server, glasscoded's `learning.rs` runs it while the server is home: at most every five minutes it checks, and each part keeps its own hour or week. It reads every machine's words kept there, since the Mac's come with the sync (`words::since`). It uses the server's checkouts and the brain's account signed in there (the name the Mac chose), on Claude's cheapest model. Its state, `manager/learning.json`, holds times, cursors and the hashes of what it proposed, never text.

- **Repair, hourly, no model.** It applies memory's ages (`Memory::tidy_skipping`):
  - a mentioned or learned entry never confirmed is let go after its active days, and what was let go is archived;
  - a confirmed entry never ages;
  - a project whose folder is not on this machine is left alone, because its code anchors cannot be checked there;
  - a file with an edit waiting for the person is left out.
- **Tidy-up, weekly, no model:**
  - entries of one file that say the same are merged (`Memory::merge`; a merge of what the person confirmed is a proposal);
  - an entry whose cited code changed is let go, or, if the person confirmed it, named to them. Code cited from a commit not yet in the main checkout's history (an unmerged branch) is not judged;
  - two entries that say different things about the same subject are named to the person and never decided between;
  - a know-how file the person edited is left as it is.

  What it names goes into the conversation as one nudge per project, naming that project. A merge or a question is asked only once.
- **Reflect, Claude's cheapest model, within the budget.** It follows `Turn::Study`'s rules and is counted as "learning". It stops for the day once it has used a fifth of the daily cap, so the person's chat keeps the rest. Switched on, it reads three days back, never the whole history.
  - **Input:** the person's new typed words (tier one), with only the parts they typed. Pasted text is never sent. One batch per project, so one project's words never reach a run about another.
  - **Output:** each fact must name the record it came from. It must rest on at least four words the person typed (`admit_fact`), and name nothing they did not type. Anything else is dropped.
  - **Proposals only:** every remaining fact is a proposal that waits for the person's tick, never a confirmed entry.
  - **Cursor:** a run reads at most the 60 oldest new records. The cursor moves past them only when every batch of them ran, so a stop between projects loses nothing. A batch that runs again proposes nothing twice.
- **Know-how from fixed failures.**
  - **Detection, no model:** a pull request's `ci.failed`, then its `ci.passed`, with the commits and files of the turn reports between. The next read starts at the oldest failure that is not fixed yet, so a fix that passes an hour later is still paired. Each fix is drafted once, at most three a run.
  - **Stripping:** `strip_personal` takes out secrets, emails, home-folder user names, and the person's logins and git name.
  - **Draft:** the model writes up to two know-how lines, proposed into the project's know-how under "From fixed failures".
  - **Locked files:** a know-how file the person edited (an entry of theirs, or an edit waiting) is locked, and nothing is proposed into it.
- **Never twice.** Each proposal's hash is kept, because a rejection records nothing. A proposal the person rejected does not come back.
- **Codex.** With a Codex brain, only the parts that need no model run, as for the wakes (D6).

### Moving in, part 2 (M54)

After the review, "Study my projects" gets to know each project, so moving in feels like a migration. It goes one project at a time, the projects with the newest sessions first, within the manager's own token budget.

- **The study.** Glasscode reads the project itself (`movein::study_reads`): the README, `AGENTS.md`, `CLAUDE.md`, the scripts, three CI files and the last 50 commits, each through `reads::file_read`. It runs the manager's brain once with `headless_profile(Study)`: no tool at all, Codex read-only, the reads on stdin as data, and the answer bound to `runtime::study::schema`.
- **The draft** holds PROJECT.md lines, know-how and people, each naming the read it came from. A line that names no read it was given is dropped. The rest are proposals in memory's inbox under the run, so "Undo this move-in" takes them back too.
- **The budget.** `Turn::Study` is held back by today's cap and the plan's windows, but not by the wakes of the hour. Its tokens join `manager/tokens.json` under the store's lock (`add_since`), as the lanes' do.
- **Pause and resume.** Above the budget the study pauses, says why and when it tries again: the next UTC day for the daily cap, the window's reset for a vendor window, else a quarter of an hour. The app waits that long (at most half an hour between checks) and goes on by itself. A run that does not work (a CLI logged out, an answer that cannot be read) pauses ten minutes; after three the project is left out. `manager/movein.json` keeps where it is, and at start a study left paused goes on without the dialog.
- **One copy, many changes.** A study step and an answer each finish in the background. Each returns a `movein::Delta`, which the dialog applies to its copy and `movein::keep` applies to the stored one under its lock, so neither overwrites the other. A step of a run undone meanwhile changes nothing, and what it proposed is undone too.
- **The questions.** There are at most eight, in order of value:
  1. the level of the projects used most recently;
  2. who owns what;
  3. what is off limits.

  Each question has a default and is posted in the conversation topic "Moving in". The dialog's answer box is that topic's composer, so the person's typed answer is tier-one words. On the Manager page, with the topic "Moving in" shown and a question open, a message typed in the chat answers that question: it does not go to the brain, and it is posted beside the question with why when it is no answer. Questions left open are loaded at start, so the chat can answer them after a restart. An owner or "off limits" answer is a confirmed fact at once (`Memory::remember_answer`). Any other answer is kept as the person wrote it, never joined to the drafted question; a drafted owner area that is not a short name makes the question an "other" one.
  A level answer must name exactly one level. It opens the Settings preview of that policy change, and the question stays open until the person confirms it there. "Skip" is a click: it records no words.
- **Forget.** Forgetting a person drops the questions and answers that name them; forgetting a project or an employer drops their studies and questions.

### Backdrop and glass

`crates/glasscode/src/backdrop.rs` (U04). A screen puts `Workspace::backdrop_layer(strength)` at
its back: Today, the Board and Pull requests 0.85, Settings 0.8, sessions 0.6, the first run 1.0,
the voice screen 0.4.

- **Frames.** A background thread makes each frame with `glasscode_core::backdrop`. The smooth
  finish is an image of the screen area divided by 6 (at most 320 x 200 texels), 10 frames each
  second, timed against a clock; a late frame is skipped. The dither finish is a full-size image,
  8 frames each second, 4 above 4 million device pixels. Each image has a border that repeats its
  edge (one texel; one pixel for dither), so its edge takes no colour from the next image in
  GPUI's atlas. The old image is dropped one frame later.
- **Glass.** `backdrop::glass(radius)` and `glass_strong(radius)` are cards that paint a blurred
  copy of the same frame, cropped to the card with its rounded corners (`paint_image` with the
  card's bounds and the backdrop's image bounds), then the glass colour, an edge and a light along
  the top. The blur is made at half the texels, for every second frame (three box passes). Text on
  glass is `theme::text`, `text_soft` or `card_soft_text`; `glass_strong` goes under long text. A
  test of the app crate (`text_on_glass_is_readable_over_each_backdrop` in
  `crates/glasscode/src/backdrop.rs`) composites each glass colour over the brightest pixel of
  each preset, in each palette, and requires a contrast of 7 for text and 4.5 for the soft text;
  the lowest results were 7.26 and 4.53 (U04).
- **When it stops.** No frames while the window is not active or not shown, when the system asks
  for less motion, or with the choice None; the frame on screen stays. No frames while no screen
  paints a layer.
- **Cost.** Each new frame draws the window, and GPUI draws a window from its root view, the
  `Workspace`: each frame runs `Workspace::render` and the layout of the whole window on the main
  thread. On Today that is about 17 ms (U24), and the moving backdrop costs about 18 % of one core
  (21.3 % against 3.0 % with the choice None). In a chat of 300 exchanges it costs about 29 % more
  than None (U12). The owner accepted this on 2026-11-17 (U25); a thin root view in front of a
  cached `Workspace` was proposed and not built. "Backdrop behind sessions" off in Settings ›
  Appearance takes it out of sessions.
- **The voice orb** (U19) has no clock of its own: the next orb image is made in the background and
  painted with the next backdrop frame. When the backdrop stands still, the orb is a still image.

### The scale

`Appearance.scale` is 90, 100 (the default), 115 or 130 % (`theme::SCALES`). Decision P5:

- `theme::sz(value)` is `px(value * theme::scale())`. Each size and text size of a screen goes
  through it. Lines of 1 px stay `px(1.)`.
- GPUI's short forms (`p_2`, `gap_3`, `text_sm`, `rounded_lg`) are in rems. `render_shell` sets the
  window's rem size to `theme::rem_size()` (16 px times the scale) on each frame, so they follow.
- A size goes through one of the two, never both: no `sz` of a value that came from a rem or from
  the bounds of an element.
- `theme::ui_text_size()`, `chat_text_size()` and `terminal_font_size()` are scaled already (Text
  size times the scale): `px`, never `sz`. A terminal lays out its cells again when the scale
  changes.
- Saved panel widths are logical; a drag is in screen pixels, so it is divided by the scale before
  it is saved. The traffic lights of macOS keep their own size, so their room in the title bar is
  in screen pixels.

The screen slices converted their own files, and the sweep of U23 the rest (Usage, the dialogs,
notification popups and their windows, the brand marks, the scroll thumb's least length). A `px(`
that is left is a 1 px line or stroke, a zero, a value that is scaled already or comes from the
bounds of an element, a size the system sets, or debug and test code.

### Rules that keep the window cheap

A draw of the window costs about 17 ms on Today, and the backdrop asks for 10 each second, so:

- **No git, file or process work on the main thread** in a render or a tick. The work runs in the
  background and its result is kept: the repositories of the projects (`TicketsUi.repos`,
  `SettingsUi.repos`), the git status and the worktree lists, the person's pull requests and the
  activity file (Today), the programs and ports (status bar), the file of a tab (file tabs), the
  project files (`ProjectFiles`). U24 found a `git` call for each project on the main thread
  (583 ms of each 1.29 s) and moved it; the sweep of U23 moved `configure_github`,
  `project_worktrees` and the read for a draft pull request; Manager v2's M33.2 moved the last
  ones of the `glasscode` command's handlers. What a handler checks before anyone is asked (a
  branch that exists, a project not on GitHub, a scene's file) is its plan's `check`, run on the
  background executor before the rules or the gate ask (`Workspace::after_check`); a read that
  needs git or a file answers from a background task (`read_later`). A test scans the handlers
  for git, gh and file calls outside those (`blocking_list_empty`). Git still runs on the main
  thread in `control_ui`'s `is_tracked` and `exclude`, for request files.
- **A tick asks for a draw only when something it shows changed.** The 700 ms tick draws only on a
  change; the status bar, the file tabs, the Files tree (read every 4 s, only while the Sessions
  view shows the Files tab) and the backdrop do the same. With nothing on screen changing, the
  window draws about 0.14 times each second (the greeting, the date and "started N min ago" update
  through these draws). The sweep measured an optimized build with the backdrop None for 120 s:
  3.03 % of one core on the Sessions view, 2.83 % on Today.
- **No GPUI animation.** `with_animation` draws the whole window on each display frame (U25: one
  working session cost about 100 % of one core through the pulse of its state dot). None is left
  in the app: the working dot and the chat's working mark are still (a chat with a busy agent went
  from 37.5 to 0.13 draws each second, U23), the voice orb moves with the backdrop, and the scroll
  thumbs go with a timer of their own.
- **Programs outside Glasscode start from a task, never inside a click handler.** On Windows,
  `ShellExecuteW` inside a click handler runs window messages and ended the app once ("RefCell
  already borrowed", U17). `center::open_url` and `center::start_outside` (a folder, a file, an
  editor, `glasscode worktree open`) start them from a task and bring a failure back as a toast or
  as the reply to the command.

`GLASSCODE_DEBUG_FRAMES` (debug builds) writes to the log, once each second, the draws of the
window, the time of their parts (title, left sidebar, tree, centre, right panel, status bar) and of
their phases, and the time of the three tick loops.

### Debug hooks

Debug builds read `GLASSCODE_DEBUG_*` variables, so that an agent or a person can open a screen
without clicks: `GLASSCODE_DEBUG_OPEN=<screen>[:<words>]` (with a theme and a scale for several
screens), `GLASSCODE_DEBUG_SIZE=<w>x<h>`, `GLASSCODE_DEBUG_SCALE`, `GLASSCODE_DEBUG_TODAY`,
`GLASSCODE_DEBUG_RIGHT_TAB`, `GLASSCODE_DEBUG_EDIT`, `GLASSCODE_DEBUG_DISCOVER_HOME` (a made-up home
for the first run), `GLASSCODE_DEBUG_NO_AGENTS=1` (no Claude or Codex program starts for a session,
and the auto-review does not run: `workspace::no_agents`) and more. A release build ignores them.
The full list, and how to start a check that touches no real login, is in
[`ui-redesign-acceptance.md`](ui-redesign-acceptance.md#start-a-safe-check).

## The glasscode command

```
agent's shell ── glasscode <words> ──▶ ~/.local/bin/glasscode (exec Glasscode.app/…/Glasscode cli)
      │                                     │ one JSON line: words, GLASSCODE_SESSION_ID + _SIG, tty?
      │                                     ▼
      │                         ~/.glasscode/control.sock (0600) ──▶ the app's main thread
      │                                     ▲          who, by channel: session / shell tab / terminal
      └─ (sandbox: socket denied) ─ <worktree>/.glasscode/cli/<id>.req ─┘   rule: allow / ask / never
                                                     answers: waiting (approval #n) … done {code, text, data}
```

- **Protocol** (`glasscode_core::control`): the command sends `Request { args, session, sig,
  interactive, cwd }` and reads `Reply` lines: `waiting { approval, summary }` while the person
  decides, then `done { code, text, data }`. The command itself only knows its options (`--json`,
  `--no-wait`, `--timeout`) and the help text, which the registry makes (`ops::help`). A failure's `data` says
  which op it was and the class of the error (`op`, `error_code`: usage, not_found, ambiguous with
  its `candidates`, failed, denied, unavailable); a success's `data` stays what `--json` prints
  (with the v2 engine an object also gets `op`).
- **The registry** (`glasscode_core::ops`, Manager v2's M12) is the one list of what the command
  can do: an `OpSpec` for each of the 78 commands, with its words and their other words, its
  arguments and where each is on the line, its effect class, retry class, undo, host and the v1
  kind of Settings › Permissions. `ops::parse` reads the words with no IO, against a `ParseCtx`
  (the day, who calls, the sessions a word may name, the engine); `ops::json_schema` gives an
  op's arguments to typed callers (M34). In the app, `ops_run.rs` keys a handler by op name: it
  builds the plan (its kind, a sentence for the approval and the log, and what it does, the same
  functions the buttons call) from the `Op`. Projects, worktrees and tasks are found in an
  `ops::World` of what the app holds in memory, and `ops::targets` reads the same world to say
  what an op touches (projects, sessions, repositories, folders), from ids, never from a caller's
  `--project` alone. Each reply leaves an `ops::Action` (the op, its arguments and targets, who
  asked, what was decided, the exit code) for the journal of M14. Tests keep it honest: a golden
  corpus of command lines recorded from the app before the registry (`ops_golden`), a source scan
  that each `plan(Kind::…)` is its op's v1 kind, and the sentences of the approvals checked
  against what v1 said. `glasscode help` is made from the table (`ops/help.rs`: each command's
  line is its op's example and summary; the file keeps only the layout), with a golden of its
  text. GPUI actions stay for key bindings and text editing; a key bound to an op uses the one
  action `RunOp { name, args }`, and a Jump command that is an op runs it. Both go through
  `Workspace::run_op`, which runs an op for the person in the app (arguments checked by
  `ops::typed`) by the same rules as the command, so the person's own actions leave the
  journal's record too. Each list the registry absorbs is deleted by the slice that absorbs it
  (`HELP` went in M33.1). The manager's `VIEWS` and `SECTIONS` (in `ops_run.rs` since M56)
  stay: they are the screens and panel parts `ui.point` and `manager.show` accept, and `ui.open`
  has its own places.
  `past_tense` still makes an op's log line (`control::LogEntry`), which the journal's op event
  keeps (`data.log`): `journal query` and `log --json` show it, and Today's "What agents did"
  starts from the last ones at launch (`control::log_lines`). `agent-log.jsonl` is no longer
  written or read (M56 row 7).
- **The inventory** (`crates/glasscode/src/capabilities.rs`, M33.1) has one row per thing the
  person can do in the app, by screen, in one class: an op, an op planned for later, the
  person's alone (with what the manager says and where it takes them), or view-local. Tests
  with no window keep it whole: every op has a row, every Settings card a settings key (or only
  actions), every view, Settings section and side-panel tab a `Place`, every GPUI action and Jump
  command a row, and every row names a function of the app. A committed count of the raw click
  handlers of each file (`capabilities_baseline.txt`) may only go down unless the same change adds
  rows, and the manager's list of what it cannot do is made from the table (`cannot_do`).
- **Dry runs, undo and long ops** (M33.2, v2 engine). An op that changes something takes
  `--dry-run` (`dry_run` for a typed caller): its plan is made and checked, and the answer says
  what it would do, what it touches, whether it would ask the person, and what takes it back; it
  runs nothing, leaves no card or claim, and is no record of an effect (a gate "never" still
  refuses). A reply's `data.undo` is the inverse op and its arguments (`ops::undo_of`) where the
  spec declares one and the reply says what to take back; the Undo button and the journal call it
  as any op. A long op from a caller that does not wait (`Request.detach`: `glasscode mcp` for a
  clone, a worktree with its fetch, a push, a new session, a question to the manager) answers
  "started" with an id at once, and `wait <id>` gets its result. The control path aims at an op's
  round trip under 500 ms locally, request files too (ops-m5; not yet measured).
- **The brain runtime** (`glasscode_core::runtime`, M50) starts the manager's brain in its exact
  profile and nothing weaker: `profile::headless_profile(BrainChat | BrainWake)`, the wall of
  every headless run with Glasscode's MCP server as its one door (a wake keeps no session and
  runs no cron; no forge token reaches the brain). Claude is one long-lived `claude -p` with stream-json in and out:
  `--restricted`, only ToolSearch among its built-in tools, Glasscode's MCP server and no other,
  its tools allowed and the rest refused unasked, no slash commands and no hooks, an explicit
  model, and its instructions, MCP config and settings in files under `manager/run/` (mode
  0600), not argv; its conversation has an id of its own and is loaded again. Codex is `codex
  exec --json`, then `exec resume`, a process a turn, in the read-only sandbox with the network,
  shell, apps, plugins, agents, memories, browser and the person's skills and config off. Both
  share `run_turn` (`BrainDriver`, shaped like ACP): the tool list is checked on Claude's first
  init line and from Codex's `ALL_TOOLS` on its first turn, and a brain with other tools is
  stopped; only once it passed does its MCP server serve it (`Host::brain_checked`). Stop is
  the stream's interrupt, then SIGINT, then the end of the process (on Codex, the end). A turn
  quiet for 90 s, or longer than 15 minutes, is stopped. `BrainHealth` says in one sentence why
  a brain cannot think (signed out, out of usage, overloaded, the wrong version or tools…), with
  one journal event and one notification per change; then the no-model mode answers from code.
  The token ledger (`manager/tokens.json`) caps the day and the wakes of an hour; any vendor
  window that is not "allowed" stops background work. Outside text reaches it wrapped
  (`runtime::wrap`: invisible and control characters and escape sequences removed, a fresh
  delimiter). The app's host is `brain_host::AppHost` (calls go to the main thread); glasscoded's
  is `DaemonHost`. The manager's own conversations are never offered to resume.
- **The manager's lanes** (`runtime::lanes`, `queue`, `wake`, `situation`, M51) run at once: the
  chat lane is one brain kept across turns (`ChatLane`, rolled over at 150,000 tokens of context:
  a summary, then a fresh conversation linked in the conversation store), and the wake lane a
  fresh brain per wake on Claude's cheapest model, with no history, its own fixed instructions
  and no cron (no Codex wake lane in release 1, D6). Wakes come from the journal by rules
  (`wake_for_event`: CI that failed and a session that needs the person are their own, other
  warnings wake at once, notices join their project's digest, routine activity and the manager's
  own doings wake nobody), from playbooks whose action is `manager.wake`, and from that op (a
  schedule or a command). The queue runs
  urgent first, then playbook wakes, then digests; it coalesces by key, closes a digest after
  20 s of quiet (120 s or 50 events at most), raises a waiting wake a level each 10 minutes,
  holds 100 (the oldest digests fold), and before a wake runs reads what it is about again from
  the events since (a stale one is dropped, `wake.dropped`). The budget holds routine wakes back
  past the hour's cap, at a low cap without a fresh vendor reading, and everything past the day's
  tokens; they stay queued. A wake's situation holds the open work of the work ledger and only
  its digest's projects, a Careful
  project's only when it is alone, none the manager's account may not read (D17), and its tools
  reach no Careful project but that one. Its answer is
  one JSON shape (`wake::parse`, nothing else does anything); what it proposes is a card, and
  so is any op past a read its tools ask for, unless the playbook that fired it was approved to
  act (`under_playbook`). While the person's chat turn runs, the wake lane takes no wake and the
  gate refuses its actions; one lane at a time works on a session or a repository (claims per
  work key, "busy: who, what"). What the manager asks is a card in its conversation too. The
  app's worker is `manager_lanes.rs`, with the engine at v2 and the manager on.
- **The manager's chat** (`manager_chat.rs`, M52.1) with the engine at v2: every way to ask
  comes through `mtick_ask`; answers from code come at once; anything else goes to the chat lane
  on a thread of its own, which resumes the brain conversation of the newest rollover link. One
  writer thread posts to the conversation store and loads it again when another writer changed
  it; the answer being written stays in memory. A message sent while it answers is queued; Stop
  reaches the brain through its stop handle (`manager.stop`). One thread component shows it on
  the Manager page (with its topics), under the Today box, whose Session | Ask control says what
  Enter does (`today.ask`), and over any view (`ui.panel chat`, ⌘⇧J), the bubble being its
  collapsed state.
  Each message carries a snapshot of where the person was (names and ids from `ui.state`, M52.2);
  the gate lets the chat move the window only within 30 s of the person's message, and turns any
  other move into a "Show me" chip; what the brain did shows as action lines with Undo where the
  op has an inverse; approval cards are inline; a failure shows its sentence, what still works and
  one way out; each turn's first text and length are journalled (`chat.turn`).
- **`glasscode mcp`** (`glasscode_core::mcp`, M34) serves the registry's ops as MCP tools on
  stdio, for the manager's brain (`--profile manager`: every op but the person's own) or a
  session's agent (`--profile session`: no window, settings or other session's screen). Each tool
  is an op (`session_open` for `session.open`), with the op's description and schema and hints
  from its effect class; about a dozen carry Claude's `anthropic/alwaysLoad` and Claude's tool
  search finds the rest. A call's arguments are checked against the schema, then sent to the app
  as a typed op (`Request.op`), which the v2 gate decides as for the command; an op that asks the
  person answers "waiting" with its id at once. The JSON-RPC loop is `glasscode_core::mcp_stdio`,
  shared with the Glasscode Browser's server: `initialize` with a table of protocol revisions,
  `ping`, `tools/list`, `tools/call`, and -32601 for anything else (Claude's `server/discover`
  probe too). The app knows a brain's server by a token it signs when it starts it
  (`GLASSCODE_BRAIN_*`, `Request.brain`): `Manager(lane)`, attribution through the channel, and it
  refuses that brain's calls until it has checked the tools of its first turn
  (`mcp::init_tools_ok`, `control_brain_checked`). `claude_config` and `codex_flags` are what the
  brain's launch (M50) passes each CLI.
- **The v2 engine's ops** (M33.2; handlers in `crates/glasscode/src/ops_v2.rs`): `ui.state` names
  what the window shows by ids and counts (never screen text: a prompt is its `prompt_id`, a
  draft its length); `ui.open <place>` shows a place without taking the keyboard, clearing a
  "needs you" mark or dismissing a notice, and while the person is busy in the window it tells
  them instead (`--go`, the person's own going there, does all three). `session.answer` names the
  prompt by its id and the option by its label, reads the screen again before each key and stops
  with `stale_prompt` on any change; an option that allows something for good is the person's
  alone, as are Ctrl-C and Shift-Tab among `session.keys`, and `session.mode` never raises a mode.
  The typed reads (`glasscode_core::reads`) run git and gh on the background executor and return
  data marked as data; `file.read` reads only inside a managed project's folders, never a
  credential or Glasscode's own files, at most 200 KB. `settings.get` and `settings.set` go through
  one table (`glasscode_core::settings`) with a tier for each key: the gate, outward automation
  and programs to run are the person's alone, and secrets are never shown. Who is the person is
  read from the channel a request came in on, never from v1's trust. Only the person names a session, a project or a task by the
  start of its name or words from its title; with the v1 engine every caller still may, as
  before. The server-sync plan's switch ops are in the table for the v2 engine only, with no
  handler yet.
- **Op ids, progress and cancel** (M33.2, ops-10, v2 engine). The wire keeps its shape: `Request` gains an optional `op_id`, and `Reply` gains no kind.
  - **The op id is the caller's own id for a call** of an op that changes something. It is the idempotency key, so a call made again with the same id gets the first call's answer and never runs twice.
    - The command takes it as `--op-id <id>`. A typed tool takes it as `op_id`, which the MCP server moves out of the arguments into the request (`mcp::take_op_id`).
    - The app keeps each call by whose it is (`Caller::origin`) and the id, with a hash of the op and its arguments (`ops::job::Calls`, the newest 1,000).
    - A repeat gets one of: the first answer (`data.repeated`), the request that waits for the person (it waits along), or "still running" with the op's id. The same id with other arguments is refused (`busy`).
    - An op a caller other than the person sets off is claimed in the effect ledger under its op id. After a restart, a repeat is answered from the ledger: done, failed, or not known (the person decides).
  - **A long op that answered "started"** (`Request.detach`) has a job (`ops::job::Job`).
    - `wait <id>` answers `data.status: "running"` with how far it got (step, of, text), never "waiting", which callers read as waiting for the person. `glasscode wait` asks again every second.
    - `cancel <id>` (`ops.cancel`) stops it. Only its caller may, or the person.
    - The ops in `ops::job::CANCELLABLE` look at the job between their steps and kill the process they wait for (`job::output_unless`). These are `project import` from a URL (a stopped clone's folder is removed), `worktree new` (before the worktree is made) and `pr open-draft` (the push, then the draft).
    - The reply says what ran (`data.status: "cancelled"`). The effect ledger keeps a step that ran as done and one stopped midway as uncertain, never as a failure.
  - `cancel <id>` also takes back a request that waits for the person. Its card goes, and the caller hears that nothing was done.
- **`journal query --actions [--pr 42]`** (land-21) answers from the effect ledger, kept a year, not from the journal's 30 days. It lists what Glasscode did for a caller other than the person (who, why, what came of it), within the caller's scope (`core_reads::actions_query`). glasscoded answers it from its own copy.
- **Taking an op back** (M33.2, ops-18, v2 engine).
  - **The record.** An op whose answer carries `data.undo` (the inverse call, `ops::undo_of`) is kept with a number (`data.undo_id`), whose it was (`Caller::origin`), and a version of what it touched as it left it (`ops_v2::undo_version`). The app keeps the last 200 until it quits.
  - **`undo [<id>]`** (`ops.undo`; none means the caller's last) runs that inverse as the caller's own call, through the gate under the caller's own rule and never as the person's. The card, the claim and the journal therefore name the real op.
  - **When it is refused.** It refuses another caller's op (the person may take back any). It refuses an op taken back already, and one whose target changed since. The version is compared when `undo` is asked, and again when its card is approved.
  - **What has a version.** A version is read from what the app holds:
    - a board task, with its mark and Focus;
    - a session, which must be still there in the same worktree, or for a rename its title;
    - a project;
    - the sessions in a worktree;
    - a setting's value;
    - a work item's state.
    A place shown needs none.
  - **What is not taken back from here.** Glasscode cannot see whether a label on GitHub, a ticket's status in Linear or memory changed since, so the reply names the inverse, which the caller makes under its own rule.
  - **No redo.** The taking back has no undo of its own (`data.undid`), and marks the op it took back.
- **The work ledger's ops** (M33.2 once M32 is in; `ops/table_work.rs`):
  - `work list` and `work show <work>` read the app's ledger (`WorkLedger`). They show each piece of work's state, its done conditions with whether each holds, its pull request, ticket and sessions, and what its session claims (a claim, not evidence).
  - A caller sees its scope's work, as with the journal (a session its project's, the manager the projects it manages), and never a hidden project's.
  - `<work>` is the work's id (`acme/app#42`, `ENG-42` in any case, a branch, or an old name of a renamed branch), `<project>:<id>` where two projects share an id, or a session's id ("this": the caller's own).
  - `work done`, `work drop` and `work reopen` are the person's only (`human_only`), because done is code's to decide from the facts and the person's word overrides it.
  - `done` adds the person's condition and counts the work done. `drop` takes it off "Needs you". `reopen` takes either back, removes the condition that `done` added, and leaves the state to the facts again.
  - Each journals `work.changed` and writes the work file. The ledger is kept by the next tick, its one writer.
- **Signatures**: `sha256(secret ‖ session id)` with a secret made at each launch; sessions get it
  as `GLASSCODE_SESSION_SIG` (not `…TOKEN`: Codex drops variables named like secrets). It is
  attribution, not a security boundary: any program of the same user can open the socket, so
  callers without a signature are "outside" and may only read unless allowed.
- **Callers** (`glasscode_core::caller`, design D2) are decided by the channel, never by what a
  request says: a valid signature is its session; a command from a Glasscode shell tab is the
  person's when that tab's terminal view saw them press Enter on a `glasscode` line in the last
  15 s (only the time is kept, never the line); the manager's brain is the manager by the token
  its MCP server carries (M34; v1's rule, a process under v1's brain, went with it in M56);
  anything else is a `Terminal`, which may read and asks for the rest (owner decision O13). Only the app's own buttons are `Ui`. The `interactive` flag
  stays on the wire and only names a terminal ("A terminal outside Glasscode"); v1's rule that
  took such a terminal for the person went with M56 row 7, as did v1's `Trust`: the app's
  `control_ui::Caller` says who it is by its channel (`is_person`, `is_agent`, `journal_word`).
  A signed shell tab whose session went meanwhile is a program outside Glasscode, never the person.
- **The words ledger** (`glasscode_core::words`, D1) records each text at the place it came in,
  with an origin Glasscode sets: the Today box, the Manager chat and the Chat view's box (only
  the typed spans count, never a paste or text the app put there), an agent session's terminal
  keys (a shell's keys are never kept), and everything Glasscode types into a session, recorded
  before it is typed (`session send`, CI fix, review comments, first prompts at launch,
  handoffs); glasscoded's gate records what a phone types. A hook's prompt only counts when one
  record is the whole prompt (`match_prompt`). It keeps text only in a Manager v2 home;
  `GLASSCODE_WORDS_MEASURE=1` counts origins in any home. `glasscode words recent` shows it, to
  the person only.
- **The gate** (`glasscode_core::gate`, Manager v2's M21.1) decides every op, the command's and
  the app's own (`run_op`); v1's runner, which decided by Settings › Permissions' kind alone
  (`control_run`, `control_rule`), went with M56 row 7. `gate::decide(caller, op, targets, policy, grant)` is
  pure: a hard limit (a project that stays on this Mac never goes to the server, for the person
  too), then `never` (the project's list, its one model account (D17), a path it keeps out of
  reach, a pause), then the level's ceiling, then an explicit ask (the list, the branch, hours and
  person conditions), then a chat grant (M21.2), then the level's default in `gate::TABLE`
  (Careful, Standard, Autopilot, per op group, for the manager and for the sessions it drives).
  The strictest of the caller's own project (from the app's record of its session) and every
  target's project wins. Sessions the person started keep their Settings › Permissions rules in
  their own project; reaching into another, they get the stricter of those and that project's.
  Person-only ops (`human_only`) run only from the window; a shell tab confirms them there, and a
  phone only answers an approval, and only one this Mac paired itself. The app parses the words,
  resolves the targets, and decides before the op's handler is called (`control_handle_v2`); a
  refused op never reaches it. The authority store is `authority/`: the live policy the app
  writes, the approved snapshot the gate reads (`approved/policy.json`), the phones this Mac
  paired (`devices.json`), the pause, and who started or drives each session (`origins.json`).
  A live file that differs counts as the snapshot at most Careful, and the app shows the change
  in plain words with "Import this change" or "Keep what I approved". Each store fails closed and
  never reads its `.prev`. The server gets the approved copy inside `mirror.sync` (without the
  projects that stay on this Mac), keeps it as `authority/from-mac.json`, refuses there what it
  never allows, and never sends authority back. Levels, playbooks and approvals get their
  Settings screens in M53; until then a policy is set by writing the live file and importing it.
- **Session profiles** (`glasscode_core::profile`, M22) apply with the v2 engine and the
  "profiles" switch on; otherwise a session launches as before. A session the person started
  runs under the person profile. Claude runs in its sandbox with the credentials and Glasscode's
  own files denied (sandbox `denyRead` and `permissions.deny` with `//` paths), and `gh` runs
  outside the sandbox. Codex runs as today.
  - **Automation.** A session that a playbook, the manager or another session started, or sent
    text to (`authority/origins.json`, marked before its terminal starts) runs under it:
    - the sandbox cannot be left;
    - bypass is disabled;
    - cross-session messages are refused;
    - claude.ai connectors are off, with `--strict-mcp-config`;
    - WebFetch and WebSearch are denied;
    - the other worktrees are denied one by one;
    - a `PreToolUse` gate hook (`glasscode hook gate`) refuses `gh pr merge`, force or
      default-branch pushes, reads of credentials and of Glasscode's files, peer messages, other
      agents and outside MCP tools;
    - Codex gets the narrowed permission profile of O16 (its own git metadata only), no
      pre-trust, and the network off.
  - **Quarantine** is automation with the forge as the only network.
  - **Every origin** gets:
    - a `PermissionRequest` hook (`glasscode hook permission`) that answers from the approved
      allow-commands, with allow, deny or nothing;
    - `disableAllHooks: false`;
    - absolute hook commands, with the events file as an argument;
    - Claude's auto-memory redirected to the project's agent notes, after a one-time copy;
    - Codex's memories off in Careful and local-only projects;
    - no GitHub token in the environment.
  - **Other parts.**
    - The hooks' decisions go to the session's spool, and so to the journal.
    - The app's own git calls pass `-c core.hooksPath=/dev/null -c core.fsmonitor= -c
      core.sshCommand=ssh` (`sys::use_safe_git`).
    - A session the person started, which the manager then types into, keeps its person
      profile until it starts again, so that send asks.
    - `versions::argument_contract` runs every argument list against a candidate CLI.
    - glasscoded launches with the same profiles.
- **Headless runs, the browser and the forge wall** (M23).
  - **Every non-interactive run** that reads outside text uses `profile::headless_profile`: Claude `-p --restricted --tools "" --strict-mcp-config --permission-mode dontAsk --disable-slash-commands --no-session-persistence --output-format json --json-schema <schema>`, Codex `exec --sandbox read-only` (no network) with `--output-schema`. No GitHub token reaches it, and its input comes on stdin as labelled data.
  - **Auto-review**, in both engines (an owner-approved security fix):
    - Glasscode reads the pull request's title, description, author and diff itself (`review::PullRequest`), and the run answers with JSON.
    - The comment is Glasscode's fixed template with the model's text clipped and tamed (no mentions, no slash commands).
    - It is posted at once only when the author passes `gate::trusted_author` and the repository opted in. Otherwise it is `pr.comment` from auto-review, which waits on a card.
    - `review-prompt.md` counts only once its content is approved in Settings › Automation (`authority/approved/review-prompt.json`).
  - **The browser's MCP server** asks the gate before each tool call (`browser.read`, `browser.act`, `browser.eval`, v2 only), with the page's site and whether the browser holds cookies for it. Acting or running script on a signed-in site asks.
  - **Sessions the manager drives** get no browser tools, unless their project's approved policy names sites (`browser_hosts`). Such a session's browser server then works as follows:
    - its tab opens in a context of its own: no sign-ins and none of the person's cookies, disposed of when the tab goes;
    - it pauses every request (`Fetch.enable`) and lets through only those to the named hosts and their subdomains (`browser::host_allowed`, `cdp::request_answer`);
    - the gate allows its reads and acts there without a card, refuses every other site, and still asks on a site it became signed in on;
    - the gate hook lets its `mcp__glasscode-browser__*` tools through (`--browser`).

    Careful takes the list away, with the other permissions.
  - **The DevTools port** is a random one per launch, read from the profile's `DevToolsActivePort` with its secret path.
  - **The forge wall (D29).** With the v2 engine, the default branch's rules and protection are read at project setup into `authority/forge.json`. Settings › Projects says "Forge enforces: review before merge, no force-push, no bypass" or "Guidance only". The gate allows Autopilot's forge writes only where all three hold.
  - **`pr open-draft`** asked by anyone but the person pushes only commits the session's own turn reports list (the journal's `turn.ended`). Brokered push (D13) is not built, because P14 did not pass (O7).
- **Pending actions** (`gate::Pending`, M21.2) replace the closures with the v2 engine. Each one is
  data in `authority/pending.json`: a stable id and a number, the op with its arguments and their
  hash, what it touched, who asks and the rule that asks, and its expiry. It survives a restart.
  An expiry is a UTC instant that a clock set back never extends. Approving runs exactly the
  stored op, after the gate decides again and its targets are resolved again; when they no longer
  hold, the person reads why ("The pull request is closed now. Nothing was done.").
  - The card shows the full payload, which opens and is never clipped. Its buttons run the
    `approvals.decide` op, and "Always allow this in <project>" opens a policy preview that needs
    a second confirmation.
  - The popup shows Allow only for local, reversible ops; for anything else it shows Review.
  - "Allow for the rest of the session" allows this op on these targets only. It runs on the
    monotonic clock and is dropped on a restart and when the Mac wakes.
  - In the Manager chat, a typed "yes" approves the one card that waits, and nothing when two do.
    A pasted or spoken yes never binds.
  - Chat grants (`gate::ChatGrants`) take a quote only from the person's typed words and targets
    only from those Rust resolved from that message. They are used once and end with their turn.
  - With v1, approvals stay in memory, as below.
- **Approvals** live in memory (`control_ui.rs`): the request's connection stays open; a decision
  runs the plan and answers; the answer is kept for `glasscode wait <id>` until the app quits.
  Today's Inbox and a notification show each request that waits.
- **Agents learn it**: Claude gets `--plugin-dir ~/.glasscode/agent/claude-plugin` (a plugin with
  the `glasscode` skill) and `--allowedTools Bash(glasscode:*)`; Codex
  `-c developer_instructions=…`. Both only when the plugin folder exists (Settings can turn it
  off, which removes it).
- **Server sessions**: `glasscoded` writes the same launcher (`glasscoded cli`) and listens on its
  own `control.sock`; it checks the session's signature, queues the request as a `RemoteCall`
  (with the op its words name, `ops::name_of`), and the Mac's next `mirror.sync` takes it, runs it as "Claude in <title> (on <server>)" and
  sends the answers back with the sync after (`cli_replies`). No Mac synced for 45 s: exit 3.
- **On Windows** the app is a window program, which cannot write to a console, so the command is
  a console program of its own, `glasscode-cli.exe` (`crates/glasscode_cli`). At each start the
  app copies it from its own folder to `~/.glasscode/bin/glasscode.exe` and puts that folder first
  on each session's PATH (PowerShell, cmd.exe and Git Bash alike); no launcher script is written.
  The channel is a named pipe, `\\.\pipe\glasscode-control-<id>` (`<id>` from the user's SID and
  the Glasscode folder), in place of `control.sock`: only this user may open it, only from this
  computer, and the app learns the caller's process with `GetNamedPipeClientProcessId`. The
  command checks that the pipe belongs to its own account before it sends a byte, because
  another account could make a pipe of that name first. The request files stay the second path
  when the pipe is denied. The protocol, the signatures and the approvals are the same.

```
session (PowerShell / cmd.exe / Git Bash) ── glasscode <words> ──▶ ~/.glasscode/bin/glasscode.exe
      │                                               │ the same JSON line
      │                                               ▼
      │                          \\.\pipe\glasscode-control-<id> (this user only) ──▶ the app's main thread
      └─ (pipe denied) ─ <worktree>\.glasscode\cli\<id>.req ─┘
```

## Today

`glasscode_core::board` is the model of the person's list: tasks (list todo / asked / waiting /
later, due day, person, links, notes, who added it), recurring tasks (`Every`), and `marks` for
items from elsewhere (Linear tickets, review requests: done today, snoozed, moved), plus Focus.
`Board::view` places everything in sections for a day (`day.rs`: local calendar days); `prepare`
makes the day's recurring copies once a day.

**The board on the server** (server-sync phase 5). The Mac and the server each keep a copy of the
board (`board.json`, an M10 store) and work on their own while apart. Every change is stamped
per entry with an M11 clock stamp when it is saved (`Board::stamp_changes`; removals leave a
marker). With every `mirror.sync` the Mac sends its board, the server merges it into its copy
entry by entry (`Board::merge`: the latest change wins, a removal included), and sends the
merged board back for the Mac to merge in turn. Today's copy of a recurring task has the same
id on both machines, so it is one task. Phones read `today.get`: the Mac's `TodaySnapshot` while
it syncs, else one made from the server's board (`from: "server"`, without the tickets, reviews
and approvals only the Mac reads). `today.op` changes the server's board at once
(`board::apply_op`); answering an agent's request (`decide`) still goes to the Mac with its next
sync.

The Today screen (`today_ui.rs`, `today_cards.rs`, U11) has, on the backdrop: a start box that
starts a Claude or Codex session from a few words; seven cards (Needs you, Coming up, Usage,
Activity, Recent sessions, Pull requests, This month), in 3, 2 or 1 columns by the width; and "Your
list", the board above with its filters, add box and sections, including the items from Linear and
GitHub that the app already fetches (`board::outside_items`). Its right panel has Inbox (the
approvals and what needs the person now) and Activity (what agents did). Each card is a pure
function of data that `TodayUi` keeps; the pull requests and the activity file are read in the
background, when Today opens and every few minutes while it shows.

## GitHub reads

What the sidebar, the PR panel, Today, the board, CI auto-fix and the manager know about pull
requests comes from GitHub's API, asked by Glasscode itself. `gh` stays for what changes things
on GitHub (comments, labels, re-runs, replies), for the tokens (`gh auth token`), and as the
fallback when there is no token (`GLASSCODE_GITHUB_API=off` turns the direct reads off).

| Part | File | What it does |
|---|---|---|
| Client | `glasscode_core::github_api` | Long-lived HTTPS connections (`ureq`, through `HTTPS_PROXY`/`ALL_PROXY` when set); the token of the repository's account (the project's, else the default, else `GH_TOKEN`, else gh's active account), never logged or written; REST reads kept with their ETag and asked again with `If-None-Match` (a 304 is quick and not counted); an account GitHub limits is not asked again until `retry-after` or the reset; callers asking the same thing at once share one request (`shared`). Everything kept is per account and goes when an account route changes. Trust comes from the built-in roots, not the system's store: after two requests in a row with no answer at all (a proxy that inspects TLS, no network) `gh` is asked for five minutes. A limit or an HTTP error never falls back, as `gh` spends the same limit. |
| Questions | `glasscode_core::github_query` | `look_up`: the status of many checkouts in one query per account and repository (all their candidate branches, review PRs and watched merges, as aliases; 12 at most per query, and up to 6 queries at the same time, of one account or several; about 1 point each of the 5,000 an hour). The PR panel's details in one query, then the preview's REST reads. Today's own PRs, with checks and review, in one search per account. Answers come out in the shapes `gh --json` prints, so one parser reads both. |
| git | `glasscode_core::git_info` | A checkout's branch, repository and candidate branches, kept until its git files change (file times, no git process); the same file times say when it was switched, pushed or fetched. |
| On disk | `glasscode_core::github_cache` | The last statuses and PR details (`github-cache.json`, the person's only, bounded), shown at the start before GitHub answers. Display only: a status from it is unconfirmed, and CI auto-fix and the manager act only on confirmed ones. |
| When | `pr_status.rs`, `github_ui.rs` | Each checkout has its own next time: on screen every 15 s while its checks run (60 s once settled), others with running checks every minute, with an open PR every 3 minutes, else every 5; four times slower while Glasscode is not the active app; at once after a switch, push or fetch, when its session is opened, and when the person comes back after a minute; after a failure 30 s, doubling to 10 minutes. The PR panel reads its PR again every 15 s while checks run, else every minute, and at once when the status refresh saw other checks. |

A round is logged at debug level (`RUST_LOG=glasscode=debug`: "pull requests: N checkouts in … ms,
… queries"). In a check home with three checkouts of two accounts, the first round took 2 queries
(0.9 s), nothing was asked for the next 30 s, a branch switch was asked about a few seconds later
(1 query), and after a restart the kept statuses showed before GitHub answered. Two tests in
`github_query` are run by hand against GitHub: `parity_with_gh` asks the same questions through
`gh` and directly and compares the answers (only what changed on GitHub in between differs), and
`round_of_the_persons_sessions` times a round of a real `state.json`.

Not built yet: GitHub's webhooks through the relay (the server receives the events and the app
asks only when something happened); the polling above would then be the fallback.

## Session lifecycle

**New session.** Optional `git worktree add -b <branch> ~/.glasscode/worktrees/<project>/<branch>`
(a project's `worktree_folder` and `branch_template` can name the folder and the branch, U22),
then:

- Claude: `CLAUDE_CONFIG_DIR=<account> claude --session-id <uuid> --settings '<hooks>'`.
  The id is chosen up front, so the session is resumable immediately.
- Codex: `CODEX_HOME=<account> codex`. Codex has no `--session-id`; Glasscode finds the new
  conversation by reading the header of the newest `sessions/YYYY/MM/DD/rollout-*.jsonl`
  whose `cwd` is the worktree.

**Setup of a new worktree (U22).** When the project's `.glasscode/settings.json` has a `setup`
command, the first session in a worktree that Glasscode just made runs it first, in its own
terminal, as the person's shell's command line; the agent starts when it ends, whatever its exit
code. The file comes with the repository, so the command runs only after the person approved that
exact command on this computer (Run, Skip this time, Never for this project); a changed command,
or a changed file of the worktree that it names, asks again.

**Hooks (Claude).** `--settings` adds `SessionStart`, `UserPromptSubmit`, `Stop` and
`Notification` hooks without touching the user's own settings. They append the hook input
to the session's events file. Glasscode reads `session_id` (it changes after `/clear`),
`transcript_path` and the activity (working / idle / needs input).

**Windows.** The settings go into a file, `~/.glasscode/settings/<session>.json`, and
`--settings` gets its path (a file has no quotation marks for a command line to break). The hook
command is `glasscode hook`, which appends stdin and a line end to the events file
(`GLASSCODE_EVENTS`) and prints nothing; the status line command is `glasscode statusline`, which
keeps the input for Usage (`GLASSCODE_LIMITS`) and then runs the person's own status line
(`GLASSCODE_STATUSLINE`). Both are the console program, first on the session's PATH, so the same
command serves Git Bash (which Claude Code prefers, and which was tried) and PowerShell (which it
uses without Git Bash, not yet tried). An npm `claude.cmd` or `codex.cmd`
starts as the `node.exe` it names (`sys::script_target`), not through cmd.exe.

**Restore.** On launch the active session starts again with `claude --resume <id>` or
`codex resume <id>`; other sessions show "Resume session" and start when opened. If a Claude
conversation has no transcript yet (no prompt was sent), it is started again with the same
`--session-id`.

**First run (U18).** With no project, and when the first run was neither finished nor skipped
(`State.first_run_done`), the launch shows the first run instead: it lists the projects and logins
that `discover` found, and only what the person ticks becomes a project or an account. While it
shows, Glasscode takes no login by itself, starts no agent program and makes no session.

**Switch account (same agent).** The transcript file is copied into the other account's
folder at the same relative path (never credentials), the account is changed and the
session resumes.

**Handoff (Claude ⇄ Codex).** `handoff/<id>-<time>.md` gets the previous transcript path,
the uncommitted diff (up to 80 KB), the last screen and the developer's note. The other agent
starts with a prompt that tells it to read the notes, summarize and continue. The prompt is
kept until the new agent has started its conversation, so a quit in between does not lose it.

## Updating the CLIs (compatibility checklist)

With the v2 engine, each built-in CLI's installed version gets the argument contract (alt-m4, M22: `versions::argument_contract`) in the background, once per version. Every argument list Glasscode can start it with, ending in `--version` (Claude) or `--help` (Codex), must be taken. A version that refuses one is said once, and its row in Settings › Updates names the first refusal. Approving happens before the install, so the contract runs on the version installed, not on a candidate.

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
8. The first run still finds the work folders of the conversations (`discover` reads the files
   of `history.rs`), and the usage of each account still shows (the Claude status line and the
   Codex rate-limit events).
9. With the new version installed, `script/cli-probe.sh --record script/cli-help` and then
   `script/cli-probe.sh script/cli-help/claude-<version>.txt script/cli-help/codex-<version>.txt`
   pass: every flag of `cli-flags.json` is still in the real CLIs' help. Commit the recorded help;
   CI probes every file in `script/cli-help/`. Delete the recorded help of a version that is no
   longer approved, and re-check `known_failures` when approving a version.

The manager's brain runs the CLIs headless (Manager v2), so a new version also needs:

10. **Print mode on the subscription login.** With a signed-in scratch account, `claude -p
    --input-format stream-json --output-format stream-json --verbose` answers one prompt: its
    `system`/`init` line has `session_id`, `tools` (only `ToolSearch` and `mcp__glasscode__*`
    with the brain's arguments), `permissionMode` and `claude_code_version`, and its `result`
    line has `usage`. `codex exec --json` answers one prompt on the ChatGPT login.
11. **The brain's flags.** Every argument of `runtime::claude::args` and `runtime::codex::args`
    (their golden tests list them) is in the recorded help of item 9: `--restricted`, `--tools`,
    `--strict-mcp-config`, `--allowedTools`, `--permission-mode dontAsk`, `--permission-prompts`,
    `--disable-slash-commands`, `--mcp-config`, `--settings`, `--append-system-prompt`,
    `--include-partial-messages`, `--session-id` and `--resume`; for Codex `exec --json`, `exec
    resume`, `--skip-git-repo-check`, `-s read-only`, `--ignore-user-config`, `--ignore-rules`,
    `--disable <feature>` and the `-c` keys.
12. **The hook inputs the turn reports read** (`turns::TurnMachine`, with fixtures in its tests):
    `hook_event_name` and `prompt_id` on each; `prompt` on `UserPromptSubmit`;
    `last_assistant_message` and `background_tasks` on `Stop`; `error` on `StopFailure`;
    `notification_type` and `message` on `Notification`; and `SessionEnd`.
13. **Codex's turn ends.** The rollout's `event_msg` lines `task_started`, `task_complete` (with
    `turn_id` and `last_agent_message`), `turn_aborted` and `error` still mark where a turn
    starts and ends (`agents::codex_turn_line`).
14. **`codex exec --json` events.** `thread.started` (with `thread_id`), `turn.completed` (with
    `usage`), `turn.failed`, `item.started`, `item.completed` and `error` keep their names and
    fields (`runtime::codex::parse_line`).
15. **The rate limit event.** Claude's stream still sends `rate_limit_event` with
    `rate_limit_info.status`, `rateLimitType`, `resetsAt` and `utilization`
    (`runtime::budget::Ledger::record_window_at`). The wake budget reads it.
16. **The minimum versions.** The manager does not start below `versions::MIN_CLAUDE` and
    `versions::MIN_CODEX` (now Claude Code 2.1.322 and Codex 0.167.0, the versions on which items
    10 to 15 were checked). Instead it says which version it needs. Raise them only after items 10
    to 15 pass on the newer version, and raise `cli-versions.example.json` with them.

## Manager v2

Every home runs Manager v2 (M56). The engine switch (`store::EngineSwitch`,
`~/.glasscode/authority/engine.json`) holds its features, each off until switched on: `brief`,
`profiles`, `turn_hooks`, `learning`.

**The switch from the first manager (M56, `manager::migrate_v2`).**
- **At the start, before the window** and before anything of the run reads the switch (`switch_at_start`): a switch still written by a build before M56 gets the features `brief` and `turn_hooks`. A feature switched off before stays off. `profiles` (M22) stays off until the owner's live run: Settings › Manager switches it on.
- **The first ticks** then move v1's data into the v2 stores, off the main thread and on the home only: the notes into memory (M20.3), and the rest of `data.json` (M44, with `data.v1.json` kept). A move that fails runs again every minute; after three it is told, and v2 takes over on what moved (`lifetime::Migration`).
- **A switch file that does not read** has every feature off: it fails closed.
- **The server** takes the Mac's switch with the next sync (`Sync.engine`, `keep_engine`, #149).

The plan is
[`manager-v2-design.md`](manager-v2-design.md) and [`manager-v2-slices.md`](manager-v2-slices.md).
Each slice's "Result" note there says what it built.

- **Stores.** Every file of the manager is a `store::Store`: one writer at a time, atomic saves
  that keep the previous generation, a path class, and a row of the data map (`ALL_STORES`).
- **Commands are ops.** Everything a session, the brain, the phone or the person can do is an op
  of the registry (`ops`). One gate (`gate::decide`) decides each op from who calls (`caller`)
  and the project's level (Careful, Standard, Autopilot). An op beyond what the caller may do
  waits as a card (`gate::pending`). Every effect is done once, under its key (`effects`).
- **Journal and turns.** Hooks, rollouts, git, the forge and the clock become events of the
  journal (`journal`: one file per UTC day, 30 days kept). The turn machine (`turns`) gives each
  turn of each session exactly one report. The work ledger (`work`) keeps what each item waits
  for.
- **Memory.** One git repository of Markdown files (`memstore`). The words ledger (`words`) is
  the only source of the person's facts. The server gets a filtered copy that holds nothing of a
  project that stays on the Mac (`memory_copy`).
- **The brain.** `runtime` runs `claude -p` or `codex exec` with only Glasscode's tools. It runs
  in `~/.glasscode/manager/work` and nowhere else: a repository as its folder is refused before
  anything starts, because `claude -p` runs a folder's hooks and MCP servers without asking. It
  does not start on a CLI older than `MIN_CLAUDE` or `MIN_CODEX`. The study passes of moving in
  and the automatic reviews (`agents::headless_run`) run in the same folder. A chat lane answers the person.
  A wake lane runs queued wakes within the budget: six routine wakes an hour, a daily token cap,
  and the vendor's windows.
- **Tests.** No test calls a model; the stand-ins of `script/test-agents` play the CLIs. CI's
  evals job runs the eval suites in `crates/glasscode_core/tests/evals` (adversarial, memory
  poisoning, retrieval, op selection). Its soak job runs the seven-day soak (`testkit::soak`).

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
- **The setup of a project** (U22) runs only locally, not on the server.
- **The manager on the server (M57)**, with the engine at v2 (the Mac's engine switch is kept
  here, one way, like its approved authority). While the server is the manager's home (the Mac
  asleep, Server mode, or "Always on the server"), `glasscoded` runs the same wake worker as the
  app (`runtime::worker`) and a chat lane beside it, against `DaemonHost` (`brain_host.rs`,
  `brain.rs`). Its brains run as the server's own account the Mac named, and reach Glasscode
  through `glasscoded cli mcp`, served here only for a running, checked brain. It reads work,
  the journal and memory within a wake's projects (`ops::core_reads`), acts on the sessions here
  through the gate with the Mac's approved policy, makes cards the Mac shows (pending actions),
  runs the ones the person approved, and queues window ops for the Mac
  (`manager/window-queue.json`). What the Mac says of the manager (on, brain, model, account,
  the projects as a wake sees them) is in `manager/from-mac.json`. A second Mac is an observer:
  it reads and answers approvals, never becomes home, and takes back only its own worktrees.
  Settings › Manager › Test says what decides whether it runs here. v1's copy of the manager
  (its rounds, its brain, `manager.sync` of `data.json`) no longer runs on the server (M56 row 6).
  `glasscoded`'s `manager.rs` keeps the server test (`manager.test`, also `glasscoded manager
  status`) and the brain's account. The Mac stopped calling `manager.sync` with M56 row 4; an older
  Mac that still does is answered with no heartbeat, so its manager stays at home.
- **The follow-ups on the server (M56.2)**: while the server is home, `glasscoded`'s
  `followups.rs` follows them, with the `follow-ups` built-in on (the Mac sends the person's
  GitHub login and the teammates' names and logins, `ManagerFromMac::follow_ups`). They live in
  `manager/work.json` beside the work items, and the sync carries them both ways, each by the
  stamp of its last change (`WorkLedger::follow_edit`, `follow_changed`, `follow_merge`), with
  its "fixed" mark, so "ready for you to look again" is told once on either home. As with memory,
  nothing of a project that stays on the Mac goes: not its follow-ups (by session, or by their
  repository), nor a teammate whose follow-ups are all of such projects (`KeptBack::
  follow_ups_for_server`), nor a teammate named only in the people of an employer whose projects
  all stay (memory's `org/<employer>/people.md`, `KeptPeople`), with any follow-up of theirs;
  when that cannot be told (the projects' keys or memory cannot be read), no follow-up goes and
  they do not run there. Every 30 s the quiet ones are
  looked at (`followups::away`): one with nobody known is told, and a note to a teammate waits for
  the Mac, whose outbox drafts it. Every 5 min the reviews the person gave are read with `gh` there
  (`followups::given`), keeping only those it may know of (one the Mac's follow-ups have, or one in
  a repository checked out there). A change is stamped after the stamp it changes, so it wins on
  the other home even when that home's clock is ahead (`identity::next_stamps_after`). What they say is a line in the manager's conversation and a notice with the
  Mac's next sync. Reviews received on the person's own pull requests are the Mac's (the server
  does not read its sessions).
- **Tests**: `script/test-server/run.sh` starts a throwaway Linux server in Docker (sshd, tmux,
  git, and stand-in `claude`, `codex` and `gh`) to try the daemon against. Sessions that test at
  once each take their own (`GLASSCODE_TEST_SERVER_NAME`, `GLASSCODE_TEST_SERVER_PORT`). The end-to-end test
  that this document named before, `crates/glasscode_core/tests/remote_e2e.rs`, is not in this
  repository.
- **Windows**: the server features (connection, sync and move, phone pairing) are not ported to
  Windows; they come after release 1 (slices S14 to S16 of
  [`windows-port-slices.html`](windows-port-slices.html)). The SSH `ControlMaster` options and the
  server paths (`PathBuf` with `\` on Windows) are known work for them.

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
unless the Mac has not synced for 90 s, or its last sync said it was going to sleep (`asleep`:
the app sends one more sync when the system's sleep notice comes, best effort). Sessions the Mac
stops listing are forgotten, refs too.

**The switch** (the title bar's "Server mode" / "Laptop mode", `crates/glasscode/src/switch_ui.rs`)
moves every open session. A worktree moves as one unit with every session open in it
(`crates/glasscode_core/src/switch.rs`), never split between the two machines, up to four
worktrees at a time, each on its own connection. A unit waits until its agents are idle (Claude
hooks; for Codex, its rollout file not changing for a few seconds); "Move now" stops them
first. `glasscode session move` moves the session's whole worktree too.

**One owner per worktree.** glasscoded keeps an owner record per worktree that has moved
(`~/.glasscode/authority/owners.json`, `crates/glasscode_core/src/owners.rs`): the machine that
runs it and a counter, the session runner epoch. A damaged file is never read as empty or as its
older generation: moves stop until it is repaired, and the server's own sessions keep running. A move is `owners.begin` (the server refuses it if
the epoch changed or another move is under way), then one push and one `worktrees.receive` for
the worktree and, per session, its conversation and a held `sessions.create` (the agent waits;
the same move tag gives the same session, so a retry after a lost reply never starts a
second), then `owners.commit`, which starts them. A failure before the commit sends
`owners.abort`: the held sessions go, and the unit stays where it was. A move that makes no
progress for 2 minutes is given up by the server's clock. Every runner checks the record:
glasscoded refuses to start a session in a worktree it does not own. The Mac stops the agent of
a session whose worktree the server owns now, but keeps the session listed. Opening it brings
the worktree back first, with any server sessions in it. Each `mirror.sync` returns the records
of the Mac's worktrees, those with no open session too, and "Laptop mode" lists the ones the
server owns. `mirror.take_over` raises the epoch and continues every Mac session of that
worktree, not only the one asked for. During a move nothing starts in the unit: the Mac stops
its agents before the snapshot, and after a failed move they resume when opened. Leaving the
server, the move stops the server's sessions at `owners.begin` and starts them again if it is
given up. A record of a worktree that never moved is dropped when its move is given up. Older
daemons (no "owners" feature) get the session-by-session moves below; the check says so.

**Worktrees with no open session** stay where they are. The switch lists the ones with
uncommitted work, unticked: ticked, one moves as a unit with no session; unticked, its HEAD and
snapshot are pushed to `refs/glasscode/parked/<token>` and `worktrees.park` lists it in
`state.parked`, so it can be opened on the server while the Mac sleeps
(`worktrees.open_parked`, which rebuilds it there and gives the server its owner record).

`crates/glasscode_core/src/sync.rs` has the steps of a move in either direction:

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
   `cwd` fields rewritten (JSON-aware). The session is created there with `resume`. To the
   server, only what the mirror's copy lacks travels (`mirror.conversation_size`, then
   `mirror.conversation` from there), and `mirror.conversation_into` puts the mirrored copy in
   place; the whole file goes when the session has no mirror.
4. **One runner**: the source session closes after the target one started.

Paths map by their place under each machine's home folder, which also names a worktree in the
owner records. `crates/glasscode_core/tests/switch_e2e.rs` moves a worktree with two sessions
to the Docker test server and back, and 30 worktrees in two projects four at a time (run it with
`--ignored --test-threads=1`; the file says how). On the local Docker server the 30 take about
1.3 s; a real server adds the network and the size of each conversation. Moving and syncing are not ported to Windows (see above).

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
- **Worktree folders** (U22): before a worktree's folder is made, each part of its path is looked
  at, and a link or a Windows reparse point is refused (`git::make_parents_below`).
- **Clone** (the first run and `project import`): only `https://`, `ssh://`, `git@host:path` and
  `owner/name` on GitHub; `git clone -- <url> <target>` with `GIT_ALLOW_PROTOCOL=https:ssh`
  (`commands2::git_clone`).
- **PR search** (`github::search_prs`, `review_requests`): a number is looked up directly;
  words match the newest 100 open PRs on title, branch and author, with GitHub's text search
  added when few match. The New session dialog searches as you type (350 ms).
- **The person's own PRs** (`github::my_open_prs`, U07): `gh search prs --author @me --state open`
  for each account that `gh` is signed in to; checks and review state for the newest 10. Today's
  card and the Pull requests page ("Mine") show them; "Asked of me" shows the review requests.
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
iPhone ──SSH (Secure Enclave key)──▶ server: command="glasscoded gate --device <id>" ──▶ glasscoded rpc / attach <id>
   ▲                                                        │
   └──────────── APNs (openssl JWT + curl) ◀────────────────┘ needs: permission prompts, questions
Mac ◀─one-time HTTP POST {token, name, key}── iPhone      (pairing only; the Mac runs phones.add)
```

- **Access**: the phone has its own SSH key (P-256, in the Secure Enclave on a device). It is
  added to `authorized_keys` with a forced command, `glasscoded gate --device <id>`, that
  allows only `rpc` and `attach <session id>`, and with forwarding and `~/.ssh/rc` off. The
  phone pins the server's host keys it received while pairing. `crates/glasscoded/src/phones.rs`
  validates keys and names before writing that file, and never rewrites a line it wrote before:
  a line without `--device` runs as the device `unknown`.
- **Callers** (Manager v2's M13.2): each connection to the daemon may name its caller once,
  with `caller.declare` (`{"kind":"machine","id":…}`, `{"kind":"phone","device":…}` or
  `{"kind":"ui"}`), answered in the connection's read loop so it applies to every later line;
  `Hello.features` says `caller` when the daemon takes it. The gate declares the phone before
  it forwards anything, and each of the Mac's connections declares its `MachineId`
  (`authority/machine.json`). A connection that declares nothing is the app, as before. A phone
  may not call `restart`, `shutdown`, `phones.add` or `caller.declare`, and only a machine may
  call `owners.*` (`glasscode_core::caller::daemon_refusal`).
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
- **The app** (its source, `ios/`, is not in this repository; `glasses/` holds the glasses app, a
  different one): SwiftUI, iOS 18+. `GlasscodeKit` is the transport and the model
  (swift-nio-ssh, the same JSON lines as the Mac) plus `Present.swift`, which turns a session
  into words ("Claude wants to push to GitHub", "CI failing · 2 checks"). The terminal is
  SwiftTerm's headless emulator drawn by our own SwiftUI view. Widgets and the Live Activity
  read a snapshot the app writes to the shared app group.
- **Tests**: `GlasscodeKit` unit tests, `EndToEndTests` (the phone's path against the Docker
  server: pair, gate, prompt, answer, chat, terminal) and UI tests in the simulator, all with the
  app's source outside this repository.
- **Windows**: pairing is not ported (slice S16, after release 1).

## What is not built yet

- Moving a session automatically when the lid closes (sync already keeps the server current;
  a session can be continued there from the iPhone).
- Git panel and diff view for server worktrees.
- The Ask Glasscode harness on the server (the phone has the interface, marked BETA).
- Terminal extras: search, clickable links, IME composition for CJK input (on the Mac; on Windows
  the terminal takes IME, dead-key and emoji-panel text through an input handler, not yet checked
  on screen).
- Auto-update of Glasscode itself (for now: install the new DMG or a private Homebrew tap; on
  Windows, run the newer installer).
- On Windows: the server features, phone pairing, the voice panel and the voice buttons (no voice
  helper), and a signed installer. [`windows.md`](windows.md) lists them with the known limits.
- From the redesign: editing a project's settings in Settings (they are read only there; the file
  is edited by hand), the setup on the server, file tabs that are kept over a restart. [`ui-redesign-acceptance.md`](ui-redesign-acceptance.md#known-limits-and-open-items)
  has the full list of known limits and open items.
