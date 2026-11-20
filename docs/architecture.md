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
| The manager: page, sidebar row, bubble, Settings › Manager, its rounds | `manager_ui.rs`, `manager_settings.rs`, `manager_tick.rs`, `manager_home.rs`, `manager_voice.rs`, `scenes_ui.rs` | from `main` |

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
- **The first stores** are `authority/machine.json` (`MachineId::this()`), `authority/engine.json`, the engine switch (v1 until M56, and each v2 feature off until it is switched on), and `authority/projects.json`, the project keys (M11, below).
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
  - every message expires after a day.
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

### Memory (M20.1)

`glasscode_core::memstore` is Glasscode's trusted memory: one git repository of Markdown under `~/.glasscode/memory/`, written only by this module (D19, D20). Nothing in v1 calls it; the memory ops and the brief come in M20.2.

- **Layout** (`memstore::layout`): `you/profile.md`; `org/<employer>/people.md` and `glossary.md`, one folder per employer; `projects/<key>/` with `PROJECT.md`, `POLICY.md`, `decisions.md`, `knowhow/<topic>.md`, `work/<work file>.md` and `archive/<yyyy-mm>/`; `inbox/proposals.md`; and `INDEX.md`, which M20.2 generates. `inbox/rejected.md` keeps text a write could not save; it is never committed and never read for a brief.
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
  (`HELP` went in M33.1; `past_tense` goes with M14's Activity, the manager's `VIEWS` and
  `SECTIONS` with M33.2's `ui.open`).
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
  profile and nothing weaker. Claude is one long-lived `claude -p` with stream-json in and out:
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
- **Signatures**: `sha256(secret ‖ session id)` with a secret made at each launch; sessions get it
  as `GLASSCODE_SESSION_SIG` (not `…TOKEN`: Codex drops variables named like secrets). It is
  attribution, not a security boundary: any program of the same user can open the socket, so
  callers without a signature are "outside" and may only read unless allowed.
- **Callers** (`glasscode_core::caller`, design D2) are decided by the channel, never by what a
  request says: a valid signature is its session; a command from a Glasscode shell tab is the
  person's when that tab's terminal view saw them press Enter on a `glasscode` line in the last
  15 s (only the time is kept, never the line); a process under the manager's brain is the
  manager (v1's rule, attribution); anything else is a `Terminal`, which may read and asks for
  the rest (owner decision O13). Only the app's own buttons are `Ui`. The `interactive` flag
  stays on the wire; with the engine at v1 the app still takes a terminal that sets it for the
  person, and `glasscode whoami` warns about it.
- **The words ledger** (`glasscode_core::words`, D1) records each text at the place it came in,
  with an origin Glasscode sets: the Today box, the Manager chat and the Chat view's box (only
  the typed spans count, never a paste or text the app put there), an agent session's terminal
  keys (a shell's keys are never kept), and everything Glasscode types into a session, recorded
  before it is typed (`session send`, CI fix, review comments, first prompts at launch,
  handoffs); glasscoded's gate records what a phone types. A hook's prompt only counts when one
  record is the whole prompt (`match_prompt`). It keeps text only in a Manager v2 home;
  `GLASSCODE_WORDS_MEASURE=1` counts origins in any home. `glasscode words recent` shows it, to
  the person only.
- **The gate** (`glasscode_core::gate`, Manager v2's M21.1) decides with the v2 engine; with v1,
  Settings › Permissions decides as before. `gate::decide(caller, op, targets, policy, grant)` is
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
- **Tests**: `script/test-server/run.sh` starts a throwaway Linux server in Docker (sshd, tmux,
  git, and stand-in `claude`, `codex` and `gh`) to try the daemon against. The end-to-end test
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
