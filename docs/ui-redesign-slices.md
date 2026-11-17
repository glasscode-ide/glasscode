# UI Redesign: Slice Plan for Agents

| | |
|---|---|
| Document | GC-UI-001, issue 1 (draft, not approved) |
| Date | 2026-11-16 |
| Visual specification | `docs/ui-redesign-mock.html` (open it in a browser; it is interactive) |
| Process parent | `docs/windows-port-slices.html` (GC-WIN-002). This plan uses the same roles and procedure. |
| Repository | glasscode-ide/glasscode, remote `origin` |
| Progress | 14 of 25 slices merged (U01 to U11, U15, U24, U25). In work: U12. Next: U13, U14, U16 (wave 6). |

> **For the main agent:** you do not write slice code. One Opus subagent does one slice in one worktree
> and opens one pull request. Use `docs/ui-redesign-orchestrator-prompt.md` to start.

## Contents

1. About this document
2. What exists today and where it goes
3. Mock items that have no behaviour today
4. Rules for all slices
5. Procedure, branches, and the brief for a subagent
6. Slice map and sequence
7. Shared interfaces
8. The slices
9. Files with a conflict risk
10. Conditions to stop
11. Review focus
12. Decisions for the owner

## 1. About this document

**Goal.** Rebuild the Glasscode user interface to match the mock: one tree sidebar, a Today dashboard,
session tabs with a file tree and an editor, a settings page, a status bar with usage, resources and
ports, a first-run flow, a voice screen, an animated backdrop with glass cards, light and dark themes,
and a global scale.

**Approach.** The application keeps its structure (one `Workspace` entity, `impl Workspace` blocks in
many files, polling ticks). Three serial foundation slices make the shared parts. After them, each
screen is one slice that owns its own files. Core-only slices operate in parallel from the first day.

**What the research found** (2026-11-16, branch `windows-port` at `6c6a889`):

- Much of the mock exists already in a different form: the kanban board with drag and drop, the
  project list with worktrees, resizable panels, the file tree, the settings page, many Claude and
  Codex accounts, the Today task board, the command palette, a status bar.
- New work: the backdrop and glass, the global scale, the Today dashboard cards, file tabs and the
  editor, the first-run flow, the resource manager, the ports list, the voice orb.
- The vendored GPUI has no backdrop blur, no custom shaders, no scroll bar element, no editor, and
  only bilinear image scaling. `overflow_hidden` clips to a rectangle, not to rounded corners.
- The source has 1090 `px(` literals and no `rems(`. `set_rem_size` scales none of them.
- The automatic build does not run `cargo test -p glasscode`.

## 2. What exists today and where it goes

A subagent must not remove a function of this table unless its slice tells it to.

| Today | New home | Slice |
|---|---|---|
| 64 px rail (Sessions, Today, Tickets, Usage, Settings, Themes) | Removed. Its items become the navigation rows at the top of the left sidebar. | U03 |
| `tickets_ui::View` in `Workspace.tickets.view` | `shell::View` in `Workspace.view` | U03 |
| Sidebar: one row for each worktree; sessions show only for the selected worktree | Tree: project, worktree, session. All levels can fold. Pins and "N more worktrees without a session" stay. | U10 |
| Server sessions in the sidebar (`render_server_sidebar`) | A "Server" group at the bottom of the tree. Behaviour is the same. | U10 |
| Today task board: sections, add box, filters, approvals, "What agents did", phone sync | Stays, below the dashboard cards, under the heading "Your list". Approvals and "What agents did" move to the right panel tabs Inbox and Activity. | U11 |
| Session header with many buttons | One tab strip and a short header. Browser, Hand off, account menu and "⋯" stay. | U12 |
| Right panel tabs: Pull request, Changes, Files, Glasscode (voice, macOS) | Files, Changes, Checks, Pull request. Checks is the checks part of the Pull request tab. The Glasscode tab stays on macOS until U19 is done. | U12 |
| File preview: fixed 300 px pane below the file tree, read only | A tab in the centre. The pane below the tree is removed. | U13 |
| Tickets board and list, reader panel | Board page. The reader panel becomes the right panel of the Board page. | U14 |
| Review requests (board column, Today section, palette) | Stay. Also shown on the new Pull requests page. | U14 |
| Command palette | Jump. It also finds projects, worktrees and files. | U15 |
| Settings: 8 sections, plus a separate Themes page | One Settings page. Themes becomes the section Appearance. Each of the 8 sections keeps all its settings. | U16 |
| Themes Classic and High contrast, accents, chat font, cursor style | Stay in Appearance. They get the new glass colours. | U01, U16 |
| Usage page | Stays as a page. It opens from the usage popover of the status bar. | U17 |
| Status bar: branch, changes, GitHub account, CLI versions, saved sessions, agent and account | Stays on the left side of the new status bar, after the usage meters. | U17 |
| Welcome empty state, automatic first Claude session | Replaced by the first-run flow when no project exists. | U18 |
| Notifications, agent browser, handoff, server and phone pairing, auto-review, CI fix | No change. | none |
| Debug hooks `GLASSCODE_DEBUG_*` | Stay. They are the screen test harness. U03 keeps them in operation. | U03 |

## 3. Mock items that have no behaviour today

**Rule: no control without behaviour.** A slice must not draw a switch, list or button that does
nothing. The mock shows some items only as ideas.

| Mock item | Decision in this plan |
|---|---|
| Gemini CLI and Cursor agent sessions | Not in this plan. `AgentKind` has Claude, Codex and Shell only. The first-run flow lists Cursor and VS Code only as sources of projects. |
| "Protected paths", "commands that never ask", network access | Not in this plan. The real permission model is the 13 `control::Kind` rules (Allow, Ask, Never). The Permissions section shows those. |
| Settings scope "For me / Project" and `.glasscode/settings.json` | Slice U22. Until U22 merges, Settings has no scope switch. |
| Worktree folder template, branch name template, setup script | Slice U22. |
| Activity heat map and "This month" | Slice U07 adds a store. History starts on the day of the first run with the new build, so the map is almost empty at first. The card says "Since <date>". |
| "My pull requests" list | Slice U07 adds the query, U14 shows it. |
| Usage in the status bar | Slice U07 makes the usage scan run without the Usage page. |
| Back and forward buttons | Slice U03 (views and sessions only). |
| Resource manager, ports with Kill | Slices U08 (core) and U17 (screen). |
| Environment variables in the Windows credential store, quiet hours, queue of agents, rebind of shortcuts, "Read questions aloud" | Not in this plan. Do not draw them. |
| First-run step "Skills, plugins and tools" | Not in this plan. Sessions use them already, and Glasscode does not manage them. The flow has four steps. |
| Voice mode on Windows | The voice helper is macOS only. See decision P4. Without a Windows helper, the microphone button does not show on Windows. |
| Keyboard labels in the mock (`Ctrl K`, `Ctrl B`) | Examples only. Take each label from `keymap::label(action)`. The Windows table uses `Ctrl+Shift`. |
| "Coming up" card | Uses real data only: tasks and tickets with a due date in the next 7 days, and review requests. No scheduled agents. |

## 4. Rules for all slices

1. **Stay in the slice.** Change only the files in the "Files" list. If a different file must change,
   write the reason in the pull request.
2. **Keep the functions of section 2.** Move them; do not remove them.
3. **No control without behaviour** (section 3).
4. **The macOS job must pass.** The code is shared. Put Windows-only calls behind `#[cfg(windows)]`
   with a stub for the other platform, in `glasscode_core::sys`. Decision P2 says how much macOS
   behaviour can change.
5. **Do not change `vendor/`** unless the slice says so and the owner approved the decision.
6. **Do not add a dependency** unless the slice names it.
7. **Colours come from `theme.rs`.** No colour literal in a screen file. Each new colour has a value
   for all four palettes.
8. **Sizes go through the scale helper** of slice U05 in each file that the slice owns. Do not start a
   conversion of files that the slice does not own.
9. **Shortcut labels come from `keymap::label`.** A new action goes into both key tables. The tests
   of `keymap.rs` must pass.
10. **Add a debug hook** for each new screen (`GLASSCODE_DEBUG_OPEN=<name>`), so an agent can open it
    without clicks. Keep the existing hooks in operation.
11. **Pure logic gets tests.** Put logic in functions that do not need a window, and test them in the
    file. Screen behaviour goes into the "Demonstration" list.
12. **Text on glass must be readable.** Text colours on glass are `text`, `text_soft` and
    `card_soft_text` only. Use `glass_strong` under long text (chat, settings, the start box) and
    `glass` for cards. The test of U04 checks both over the brightest part of each backdrop.
13. **No secrets and no personal data** in code, tests, sample data or pull requests. Glasscode does
    not read login tokens (`agents.rs:5`).
14. **Verify before the report, and tell the truth in the report.** Put the real output of each
    Verify command in the pull request.
15. **A scratch home never uses a real login.** With no account in its `state.json`, Glasscode takes the system login (`~/.claude`, `~/.codex`) as "My login", and a Claude or Codex session then starts the real program with the real login. Each scratch home for a screen check has a `state.json` with its own accounts in the scratch folder, and uses Shell sessions unless the check needs an agent. If `claude.exe` or `codex.exe` starts from a check, end it and write that in the report.
16. **One agent at a time uses the screen.** Before it starts a Glasscode window, an agent makes the file `W:\personal-code\glasscode\wt\gcui\screen.lock` with its slice number, and removes it when its window is closed. If the file exists, wait and look again.
17. Obey `CONTRIBUTING.md`. Comments tell the reason. Do `cargo fmt` only on lines that you changed.
    Do not merge `Cargo.lock` by hand.

### Gates of the automatic build

| After slice | macOS job | Windows job |
|---|---|---|
| Now (the build also runs for pull requests into `ui-redesign`) | `cargo check --locked` for `glasscode`, `glasscoded`, `glasscode_cli`; `cargo test --locked -p glasscode_core -- --test-threads=1` | `cargo check --locked -p glasscode`; core tests; `cargo test --locked -p glasscode_cli` |
| U01 | The same | Add `cargo test --locked -p glasscode` |

## 5. Procedure, branches, and the brief for a subagent

The roles and the procedure are sections 2, 3, 5 and 6 of GC-WIN-002, with these values.

| Item | Value |
|---|---|
| Integration branch | `ui-redesign`, made from the branch of decision P1 |
| Slice branch | `ui/uNN-<name>`, made from the newest `origin/ui-redesign` |
| Worktree folder | `W:\personal-code\glasscode\wt\uNN-<name>` |
| Pull request | Base `ui-redesign`. Title `UNN: <slice name>`. Squash merge. |
| At the same time | A maximum of three slices |
| Screen checks | One agent at a time, through `W:\personal-code\glasscode\wt\gcui\gc-ui.ps1` |

### Brief for a subagent

The main agent replaces the text in angle brackets.

```text
You do one slice of the Glasscode UI redesign (a Rust desktop application built on GPUI).

Worktree:  <absolute path>      (work only in this folder)
Branch:    <ui/uNN-name>        (it exists and is checked out)
Base:      ui-redesign

Read first, in this sequence:
1. docs/ui-redesign-slices.md  sections 2, 3, 4 and 7, and the slice <UNN> in section 8.
   The slice text is your specification.
2. docs/ui-redesign-mock.html  Open it in a browser. It shows how the screen must look and behave.
   Its JavaScript is the reference for layout and for the backdrop mathematics. It is not Rust code.
   Where the mock and the plan are different, the plan is correct (see section 3).
3. CONTRIBUTING.md.
4. Each file in the "Files" list of the slice, before you change it.

Do the work:
- Do the steps of the slice in sequence. Add the tests in its "Tests" list.
- Do each command in the "Verify" list. Correct the code until each one passes.
- Do the "Demonstration" steps yourself when a screen helper is available. Put screenshots in the
  pull request for the light theme and the dark theme.
- Commit with conventional commit messages. Do not use --no-verify.
- Push the branch. Open one pull request with base ui-redesign and the title "<UNN>: <slice name>".
  Use the pull request template of docs/windows-port-slices.html section 6.

Limits:
- Change only the files in the "Files" list. Do not change vendor/ unless the slice says so.
- Do not merge the pull request. Do not push to ui-redesign, windows-port or main.
- If a condition of section 10 occurs, stop and give a report.

Your last message is the report: the pull request URL, the output of each Verify command, each step
that is not complete, each file changed outside the list with the reason, and each item that you
did not verify.
```

## 6. Slice map and sequence

| Wave | Slices | Notes |
|---|---|---|
| 1 | U01, U02, U06, U07 | U06 is very small. Start U07 when one of the other three has a pull request. |
| 2 | U03, U08, U09 | U03 is the only slice in the application crate in this wave. |
| 3 | U04 | |
| 4 | U05, U15 | U05 converts shared files, so no screen slice is in work. The foundation is complete after this wave. |
| 5 | U10, U11, U24, U25, U12 | U24 and U25 were added after the measurement of U11. U12 starts after U25. One screen slice at a time has the input helper. |
| 6 | U13, U14, U16 | |
| 7 | U17, U20, U22 | |
| 8 | U18, U19, U21 | |
| 9 | U23 | Last. No other slice is in work. |

| ID | Name | Size | Depends on | Status |
|---|---|---|---|---|
| U01 | Colours, icons and the UI kit | M | none | **Merged** 2026-11-16, pull request 28 |
| U02 | Proofs: backdrop, glass, dither, scale | S | none | **Merged** 2026-11-16, pull request 33 |
| U03 | Shell | L | U01 | **Merged** 2026-11-17, pull request 35 |
| U04 | Backdrop and glass | M | U01, U02 | **Merged** 2026-11-17, pull request 40 |
| U05 | Global scale | M | U01, U02, U03, U04 | **Merged** 2026-11-17, pull request 42 |
| U06 | Application icon | S | none | **Merged** 2026-11-16, pull request 27 |
| U07 | Core data for Today | M | none | **Merged** 2026-11-16, pull request 30 |
| U08 | Core: processes and ports | M | none | **Merged** 2026-11-16, pull request 32 |
| U09 | Core: find projects and logins | M | none | **Merged** 2026-11-17, pull request 37 |
| U10 | Tree sidebar | M | U03, U05 | **Merged** 2026-11-17, pull request 45 |
| U11 | Today dashboard | L | U03, U04, U05, U07 | **Merged** 2026-11-17, pull request 46 |
| U12 | Session view and tabs | M | U03, U04, U05 | In work |
| U13 | File tabs and preview | M | U12 | Not started |
| U14 | Board and pull requests page | M | U03, U04, U07 | Not started |
| U15 | Jump | S | U03, U05 | **Merged** 2026-11-17, pull request 44 |
| U16 | Settings page | L | U03, U04, U05 | Not started |
| U17 | Status bar: usage, resources, ports | M | U03, U07, U08 | Not started |
| U18 | First run | L | U04, U09, U16 | Not started |
| U19 | Voice screen and orb | M | U04, U11, U17 | Not started |
| U20 | Editor | L | U13 | Not started |
| U21 | Syntax colours | M | U20 | Not started |
| U22 | Project settings and templates | M | U16 | Not started |
| U24 | Cost of a frame (added 2026-11-17) | M | U11 | **Merged** 2026-11-17, pull request 48 |
| U25 | Still working dot (added 2026-11-17; the window structure was dropped) | S | U24 | **Merged** 2026-11-17, pull request 51 |
| U23 | Scale sweep, removal of old code, documents | M | all | Not started |

## 7. Shared interfaces

A slice that makes an interface uses these names exactly. A slice that uses one does not change it.

### From U01 (`theme.rs`, `ui.rs`, `icons.rs`)

```rust
// theme.rs: new colour functions. Values for Glasscode Dark / Glasscode Light are below.
pub fn glass() -> Hsla;            // rgba(22,22,26,.64)   / rgba(255,255,255,.70)
pub fn glass_strong() -> Hsla;     // rgba(20,20,24,.78)   / rgba(255,255,255,.84)
pub fn glass_border() -> Hsla;     // rgba(255,255,255,.14) / rgba(255,255,255,.95)
pub fn glass_highlight() -> Hsla;  // rgba(255,255,255,.22) / rgba(255,255,255,1)
pub fn card_soft_text() -> Hsla;   // #c6c6cf / #3f3f46   (second line of text on glass)
pub fn tint() -> Hsla;             // rgba(255,255,255,.07) / rgba(9,9,11,.05)  (hover on glass)
pub fn action() -> Hsla;           // #fb923c / #c2410c   ("Answer", "Fix" pills)
pub fn on_action() -> Hsla;        // #1c0d02 / #ffffff
pub fn contrast(text: Hsla, background: Hsla) -> f32;   // WCAG ratio, for tests

// icons.rs
pub enum Brand { Claude, Codex, Shell, Github, Linear, Cursor, VsCode }
pub fn brand(brand: Brand, size: f32) -> AnyElement;    // full colour, through img(); Codex, Github, Cursor take theme::text()
pub fn agent_brand(agent: AgentKind) -> Brand;
// Each Unicode glyph that is used as an icon gets an SVG constant, drawn with icons::icon().

// ui.rs
pub fn scroll_area(id: impl Into<ElementId>, handle: &ScrollHandle, content: impl IntoElement) -> AnyElement;
// No permanent scroll bar. A 5 px thumb shows while the pointer is in the area or the area scrolls,
// and goes after 900 ms. The thumb can be dragged.
pub fn kbd(label: impl Into<SharedString>) -> Div;
pub fn pill(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<Div>;
pub fn action_pill(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<Div>;
pub fn meter(fraction: f32, hot: bool) -> Div;
pub fn status_dot(state: sidebar::RowState, done: bool) -> AnyElement;
```

### From U03 (`shell.rs`, `model.rs`)

```rust
// crates/glasscode/src/shell.rs
pub enum View { Today, Sessions, Board, PullRequests, Usage, Settings, FirstRun }   // replaces tickets_ui::View
pub enum CenterTab { Session(String), File(std::path::PathBuf) }
pub enum Popover { Usage, Resources, Ports }
pub struct Place { pub view: View, pub session: Option<String> }

impl Workspace {
    pub fn go(&mut self, view: View, window: &mut Window, cx: &mut Context<Self>);   // records history, saves
    pub fn go_back(&mut self, window: &mut Window, cx: &mut Context<Self>);
    pub fn go_forward(&mut self, window: &mut Window, cx: &mut Context<Self>);
    pub fn toggle_left(&mut self, cx: &mut Context<Self>);
    pub fn toggle_right(&mut self, cx: &mut Context<Self>);
    pub fn toggle_popover(&mut self, popover: Popover, cx: &mut Context<Self>);
    pub fn render_shell(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement;
    // In U03 these give what the application shows today (see U03 step 3). The screen slices replace the bodies.
    pub fn render_left(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement;        // U10
    pub fn render_right(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement>; // per view
    pub fn status_left(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement>;                        // U17
    pub fn status_right(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement>;                       // U17
    pub fn render_popover(&mut self, popover: Popover, window: &mut Window, cx: &mut Context<Self>) -> AnyElement; // U17
}
// Workspace fields: view: View, tabs: Vec<CenterTab>, tab: usize, history: (Vec<Place>, Vec<Place>), popover: Option<Popover>

// glasscode_core::model
pub struct Layout { pub sidebar: Option<f32>, pub right: Option<f32>, pub tickets: Option<f32>,
                    pub left_hidden: bool, pub right_hidden: bool, pub right_tab: Option<String>,
                    pub folded: Vec<String> }      // new fields have #[serde(default)]
```

Actions added in U03, in both key tables: `ToggleLeftPanel`, `GoBack`, `GoForward`, `OpenSettings`.

### From U04 (`glasscode_core::backdrop`, `backdrop.rs`)

```rust
// glasscode_core/src/backdrop.rs: no window, no clock. The reference is draw() and drawOrb() in the mock.
pub enum Preset { Prism, Aurora, Dusk }
pub enum Finish { Smooth, Dither }
pub struct Frame { pub width: u32, pub height: u32, pub bgra: Vec<u8> }     // straight alpha
pub fn render(preset: Preset, finish: Finish, light: bool, strength: f32, seconds: f32, width: u32, height: u32) -> Frame;
pub fn blur(frame: &Frame, radius: u32) -> Frame;
pub fn orb(preset: Preset, finish: Finish, light: bool, level: f32, seconds: f32, size: u32) -> Frame;

// glasscode_core::model::Appearance: new fields with #[serde(default)]
pub backdrop: BackdropChoice,      // Prism (default), Aurora, Dusk, None
pub finish: Finish,                // Smooth (default)
pub backdrop_in_sessions: bool,    // default true
pub scale: UiScale,                // P90, P100 (default), P115, P130

// crates/glasscode/src/backdrop.rs
impl Workspace {
    pub fn backdrop_layer(&mut self, strength: f32, window: &mut Window, cx: &mut Context<Self>) -> AnyElement;
    // An absolute layer for the back of a screen. Strength: Today 0.85, sessions 0.6, Settings 0.8, first run 1.0.
}
pub fn glass(radius: Pixels) -> Div;          // blurred crop of the backdrop + theme::glass() + border + highlight
pub fn glass_strong(radius: Pixels) -> Div;
```

### From U05 (`theme.rs`)

```rust
pub fn sz(value: f32) -> Pixels;   // value * the scale of Appearance.scale. Replaces px(value) for sizes and text.
pub fn scale() -> f32;             // 0.9, 1.0, 1.15 or 1.3
```
Decision P5: `sz` is the helper; there is no zoom patch.

### From U07, U08, U09 (`glasscode_core`)

```rust
// activity.rs: file ~/.glasscode/activity.json
pub struct Day { pub sessions: u32, pub prs_merged: u32, pub lines: u32 }
pub struct Activity { pub since: String, pub days: BTreeMap<String, Day> }   // ISO dates
impl Activity {
    pub fn load() -> Activity;  pub fn save(&self) -> io::Result<()>;
    pub fn add_session(&mut self, day: &str);
    pub fn add_merge(&mut self, day: &str, lines: u32, key: &str);            // key stops a second count
    pub fn weeks(&self, today: &str, weeks: usize) -> Vec<[u32; 7]>;          // sessions for each day
    pub fn month(&self, today: &str) -> (Day, Day);                           // this month, the month before
}
// github.rs
pub struct MyPr { pub repo: String, pub number: u64, pub title: String, pub url: String, pub updated_at: String,
                  pub draft: bool, pub ci: Option<CiStatus>, pub review: String }      // CiStatus is github.rs:820
pub fn my_open_prs(env: &HashMap<String, String>) -> anyhow::Result<Vec<MyPr>>;

// sys/mod.rs
pub struct ProcessUse { pub pid: u32, pub parent: u32, pub name: String, pub cpu_percent: f32, pub memory_bytes: u64 }
pub fn process_use() -> Vec<ProcessUse>;
pub struct Listener { pub port: u16, pub pid: u32, pub process: String }
pub fn listeners() -> Vec<Listener>;
pub fn end_tree(pid: u32) -> io::Result<()>;
// resources.rs (pure)
pub struct Root { pub session: String, pub pid: u32 }
pub struct SessionUse { pub session: String, pub cpu_percent: f32, pub memory_bytes: u64, pub processes: Vec<ProcessUse> }
pub fn group(all: &[ProcessUse], roots: &[Root]) -> Vec<SessionUse>;
pub fn owner(pid: u32, all: &[ProcessUse], roots: &[Root]) -> Option<String>;   // session of a process
// terminal/mod.rs (application crate)
impl TerminalView { pub fn child_pid(&self) -> Option<u32>; }

// discover.rs
pub enum Source { Cursor, VsCode, ClaudeCode, Codex }
pub struct FoundProject { pub root: PathBuf, pub name: String, pub sources: Vec<Source>, pub last_used: Option<SystemTime> }
pub fn projects(home: &Path, app_data: &Path) -> Vec<FoundProject>;
pub enum LoginState { SignedIn, NeedsSignIn }
pub struct FoundLogin { pub agent: AgentKind, pub label: String, pub dir: Option<PathBuf>, pub state: LoginState, pub from: String }
pub fn logins(home: &Path) -> Vec<FoundLogin>;
pub fn signed_in(account: &Account) -> bool;      // looks only for the login file; does not read it
```

## 8. The slices

Each slice also has the items "Branch `ui/uNN-<name>`" and "Worktree `..\wt\uNN-<name>`".

### U01 Colours, icons and the UI kit (wave 1, M) — merged, pull request 28

The shared parts that each screen uses.

- **Files:** `theme.rs`, `ui.rs`, `icons.rs`, `sidebar.rs` (only `agent_icon`), `crates/glasscode/assets/icons/*`, `.github/workflows/ci.yml`
- **Steps:**
  1. Add the colour functions of section 7 to `theme.rs` with values for all four palettes. Classic and High contrast get values with the same contrast as Dark.
  2. Add `theme::contrast`.
  3. Add the SVG files for each Unicode glyph that is an icon (chevrons, branch, folder, file, plus, close, search, bell, calendar, gauge, chart, grid, gear, plug, cpu, terminal, refresh, mic, pull request, panel left, panel right, back, forward, play, shield, key). The mock has the paths.
  4. Add the brand files (Claude, Codex, Cursor, GitHub, Linear, VS Code) from svgl.app and `icons::brand`. Draw them with `img()`, because `svg()` keeps only one colour.
  5. Make `sidebar::agent_icon` call `icons::brand`.
  6. Add the helpers of section 7 to `ui.rs`. `scroll_area` paints its own thumb (see `vendor/gpui/examples/data_table.rs:299`).
  7. Add `cargo test --locked -p glasscode` to the Windows job.
- **Tests:** for each palette, with the glass colour composited over `bg`: `contrast(text, glass) >= 7.0`, `contrast(card_soft_text, glass) >= 4.5`, `contrast(on_action, action) >= 4.5`. (U04 adds the test over the backdrop.) Each icon constant parses as SVG. The thumb size and position function gives the correct value at top, middle and end.
- **Verify:** `cargo test -p glasscode`; `cargo check -p glasscode`.
- **Demonstration:** debug hook `GLASSCODE_DEBUG_OPEN=kit` shows a page with each helper and each icon, in the light theme and the dark theme.
- **Not in this slice:** the use of the helpers in screens.
- **Result:** as section 7, with these additions. The kit page is `kit.rs`; the hook also takes `kit:light`, `kit:dark`, `kit:classic` and `kit:contrast`. `icons::ALL` lists each icon. There is one chevron (`CHEVRON_DOWN`); turn it for the other directions. The keyboard icon is `KEYBOARD`. `ui::thumb` and `ui::scrolled_for_thumb` are the pure functions of the scroll thumb. The Windows job now runs `cargo test --locked -p glasscode`.
- **Follow-up, pull request 36:** a press on the scroll thumb that came before the frame that shows it was lost; the handlers now read the place of the thumb when the event comes. After it, the full kit page was seen on screen in the four themes, with drag, wheel, hover and fade.
- **Open items:** the glyphs ⋯, ⇄, ↗, 📌, ☑ and ☐ have no icon yet; the slice that draws one adds its SVG (U10 for the pin and "⋯", U11 for the check boxes, U12 for "⇄" and "↗").

### U02 Proofs: backdrop, glass, dither, scale (wave 1, S) — merged, pull request 33

Four short experiments. Their results set the method of U04 and U05. The code of this slice is a debug page only.

- **Files:** new `crates/glasscode/src/proofs.rs`, `main.rs` and `workspace.rs` (one debug hook each), new `docs/ui-redesign-proofs.md`
- **Steps:**
  1. **Backdrop.** Paint a new `RenderImage` of 320 x 200 from a byte buffer 15 times each second for 10 minutes. Call `window.drop_image` for the old one. Record the memory of the process at the start and the end, and the processor use.
  2. **Glass.** Call `paint_image` with the bounds of a card, `image_bounds` of the full backdrop, and corner radii. Make sure that the card shows the correct part of the image, not stretched, with rounded corners, and no colour bleed at the edges.
  3. **Dither.** The sampler is bilinear, so a small dither image becomes soft. Measure a dither image at the full pixel size of the window, 8 times each second: memory and processor use at 1920 x 1080 and at 3840 x 2160.
  4. **Scale.** Count how the work divides for the helper `theme::sz`. Then try a zoom in `vendor/gpui_windows`: multiply the scale factor of the window by a zoom value. Do this in a branch that is not merged. Record if text, hit tests, the terminal and popup windows stay correct.
  5. Write the numbers and one recommendation for each proof in `docs/ui-redesign-proofs.md`.
- **Tests:** none.
- **Verify:** `cargo check -p glasscode`. The document has a number for each measurement.
- **Demonstration:** `GLASSCODE_DEBUG_OPEN=proofs`.
- **Stop:** memory increases in proof 1 after `drop_image`; or proof 2 cannot crop with rounded corners. Tell the owner (decisions P5 and P6).
- **Result:** `docs/ui-redesign-proofs.md` has the numbers. Neither stop condition occurred. The backdrop at 15 frames each second costs 3.5 % of one core more than a still page, which is above the limit of U04; the owner decided on 10 frames each second (see U04).

### U03 Shell (wave 2, L) — merged, pull request 35

The frame of the window: title bar, left sidebar, centre, right panel, status bar, popover host.

- **Files:** new `shell.rs`; `workspace.rs`, `tickets_ui.rs`, `tickets_view.rs` (remove `render_rail`), `sessions_ui.rs`, `center.rs`, `keymap.rs`, `palette_ui.rs`, `usage_ui.rs`, `settings_ui.rs`, `themes_ui.rs`, `today_ui.rs` (only the lines that read the view), new `session_view.rs`, `crates/glasscode_core/src/model.rs`
- **Steps:**
  1. Add `shell::View` and move each read of `tickets.view` to `Workspace.view`. `Themes` becomes `Settings`.
  2. Move without change: `render_sidebar` into `sessions_ui.rs`; `render_center`, `render_session`, `render_session_header` into `session_view.rs`; `render_status_bar` into `shell.rs`. This makes file ownership possible for the later slices.
  3. Add `render_shell`: title bar, then a row of left sidebar, handle, centre, handle, right panel, then the status bar, then the popover and the overlays in their current order. In this slice `render_left` gives plain rows for Today, Board, Usage and Settings above the existing sidebar; `status_left` gives the existing status bar items; `status_right` and `render_popover` give nothing; `render_right` gives the existing right panel for sessions and the reader panel for the board. The application must be fully usable after this slice.
  4. Title bar, from left: mark, left panel button, back, forward, the Jump field in the middle, right panel button, window buttons on Windows. Keep the breadcrumb, the update pill, the server pill and the move button.
  5. The left sidebar shows in each view except Settings and FirstRun. It can hide (`Layout.left_hidden`). A drag of the handle below 140 px hides it. Keep double-click and the arrow keys.
  6. The right panel shows when `render_right` gives a value and `Layout.right_hidden` is false. Save `right_hidden` and `right_tab`.
  7. Add history: `go` puts the old place on the back list. A session change through `select_session` also does this.
  8. Add the actions of section 7 to both key tables.
  9. Keep each debug hook in operation.
- **Tests:** history: go, back, forward, and a new `go` clears the forward list. `Layout` reads an old `state.json` without the new fields. The key table tests pass.
- **Verify:** `cargo test -p glasscode`; `cargo test -p glasscode_core -- --test-threads=1`; each existing `GLASSCODE_DEBUG_OPEN` value still opens its screen.
- **Demonstration:** hide and show the two panels with the buttons and the keys; drag each handle; go to Today, Board, Settings and back with the back button; restart and see the same panel state.
- **Not in this slice:** the new look of any screen. Screens look as before inside the new frame.
- **Result, for the screen slices:**
  - **Sessions.** `select_session(id)` makes a session active and keeps the view (use it for background events: an agent's command, the glasses). `open_session(id)` and `add_and_open_session(session)` show the Sessions view and record the place (use them when the person asks). A background event must never take the person out of Today or Settings.
  - **Views with no page yet.** `View::PullRequests` shows the Board (U14 replaces this). `View::FirstRun` shows the Sessions view with no sidebars (U18 replaces this). Nothing leads to them yet.
  - **Themes** is `View::Settings` with `Section::Appearance`. Settings has a "Back to work" row, because the left sidebar does not show there. "New session" moved from the title bar to the top of the left sidebar.
  - **Keys.** Left sidebar: Ctrl+Shift+E / ⌘⇧E. Settings: Ctrl+Shift+S / ⌘⇧S. Back and forward: `ctrl-<` and `ctrl->` on Windows (Ctrl+Shift+, and Ctrl+Shift+. on a US keyboard, because gpui_windows folds Shift into the key), ⌘[ and ⌘] on macOS. There is no key for the right panel yet.
  - **Panels.** `shell::fit_panels` narrows the sidebars in a narrow window. A drag below 140 px hides a panel (both sides); its button brings it back at the old width. `Layout` has `left_hidden`, `right_hidden`, `right_tab`, `folded`; a field at its default is not written, so an old build reads the file.
  - **Screen checks.** `GLASSCODE_DEBUG_SIZE=900x560` opens the window at that size in a debug build (review focus 3). The view is not saved in a file.
  - **Seen on screen by the subagent** (Windows, dark; light only at 900 px): each step of the demonstration. Not seen: a background `select_session` that leaves Today alone (reasoned from the code), the update pill, the move button, the Browser button, macOS.
  - **For U10:** at 220 px the sidebar header ("PROJECTS, N need you, N working") is tight.

### U04 Backdrop and glass (wave 3, M) — merged, pull request 40

- **Files:** new `crates/glasscode_core/src/backdrop.rs`, `lib.rs`, `model.rs`; new `crates/glasscode/src/backdrop.rs`; `shell.rs` (one call), `themes_ui.rs` (tick), `theme.rs` (glass alpha values only)
- **Steps:**
  1. Port `draw()` of the mock to `backdrop::render`, with the three presets and their colour tables. Dither uses the 4 x 4 Bayer table with 3 levels. In the light theme the colour goes on white.
  2. Add `backdrop::blur` (box blur, three passes).
  3. In the application, make one frame for the window. Keep one smooth image and one blurred image. Call `drop_image` for each old image. **Method from U02 and the owner (2026-11-16):**
     - The image is the logical size of the window divided by 6, at most 320 x 200, with a border of one texel (without it the edge takes colour from the next image in the atlas).
     - **10 frames each second**, timed against a clock (Windows timers end on a 15.6 ms tick), and a late frame is skipped. Drop the old image one frame later.
     - The backdrop is an entity of its own, so a new frame does not draw the full `Workspace` again.
     - The blur must be much cheaper than the simple one of the proof (4.6 ms for each frame): half size and a lower rate.
     - **Dither (P6): a full-size image, no vendor change.** Above 4 million device pixels, lower the dither rate. Smooth stays the default.
  4. Stop the frames when the window is not active, when it is hidden, when `cx.reduce_motion()` is true, and when the choice is None. Draw one still frame in those cases.
  5. Add `glass` and `glass_strong` with the method that U02 proved: one `canvas` for each card that calls `paint_image(card bounds, image bounds of the backdrop, radius, blurred image)`.
  6. Add the `Appearance` fields.
- **Tests:** `render` gives the same bytes for the same input. All alpha bytes are 255. With `light`, a pixel far from the ribbon is white. Dither output has no more than 4 values in each channel. `blur` of one flat colour gives that colour. `render` with width or height 0 gives an empty frame and does not panic. **Readability:** for each preset, each palette and each strength (0.6, 0.8, 0.85), take the brightest pixel of `render`, composite `glass` and `glass_strong` on it, and require `contrast(text) >= 7.0` and `contrast(card_soft_text) >= 4.5` on both. If a pair fails, increase the alpha of that glass colour in `theme.rs` until it passes; do not change the text colour. (A first calculation gives about 4.7 for soft text on dark `glass` over Prism, so the margin is small.)
- **Verify:** core tests; `cargo check -p glasscode`.
- **Demonstration:** `GLASSCODE_DEBUG_OPEN=backdrop` shows each preset, each finish, light and dark, with three glass cards on it. Put the window behind a different window: the processor use goes to almost zero.
- **Stop:** memory increases with time. (The processor limit, first 3 % and then 6 %, no longer applies: the owner accepted about 18 % of one core on Today on 2026-11-17; see U25.)
- **Result:**
  - **Cost.** The backdrop at 10 frames each second costs about 4.9 % of one core above a still page in the real window (4.2 % with three glass cards; the cards add nothing that can be measured). Making a frame is about 1 %; the remainder is the draw, because GPUI draws the root view on each window draw, so each backdrop frame runs `Workspace::render`. An entity of its own does not prevent this. The home was empty, so real screens cost more.
  - **Owner decision (2026-11-17):** accept this, limit 6 %. **U11 measures again on the real Today screen. If the cost is above 8 % there, a new slice puts a thin root view in front of a cached `Workspace`** (not tried; the risk is a screen that does not update where `Workspace::render` reads an entity with no `notify`).
  - **Memory.** Flat in a 10-minute run (private bytes 144 MB, graphics memory 52.2 MB).
  - **Stops.** Behind a different window and minimized, the use is the same as with the choice None. A window that is fully covered but stays active keeps the animation (Windows reports no "covered" state); this occurs only below an always-on-top window.
  - **For the screen slices.** `self.backdrop_layer(strength, window, cx)` at the back of a screen, `backdrop::glass(radius)` and `backdrop::glass_strong(radius)` for cards. The cards get the blurred image in the same draw; they need no `notify`. The debug hook is `GLASSCODE_DEBUG_OPEN=backdrop[:prism|aurora|dusk|none][:smooth|dither][:light|dark][:still][:bare]`.
  - **Dither** uses a full-size image at 8 frames each second, 4 above 4 million device pixels. Its processor use was not measured in this slice.
  - **Margin.** The readability test passes with the glass values of U01; the lowest results are 7.26 for text and 4.53 for the soft text, so the margin is thin. A slice that changes a glass or text colour must run the test.
  - **Light theme.** A glass card on a white ground has almost no edge that shows, because `glass_border` is white, as in the mock. U11 decides if cards need a faint dark edge in the light theme.
  - **Not verified:** `reduce_motion` (nothing sets it), Classic and High contrast on screen, macOS apart from its build job. `orb` has tests and no use yet (U19).

### U05 Global scale (wave 4, M) — merged, pull request 42

- **Files:** `theme.rs`, `ui.rs`, `shell.rs`, `window_controls.rs`, `text_input.rs`, `text_area.rs`, `composer.rs`, `terminal/mod.rs` (font size only), `themes_ui.rs`
- **Steps:**
  1. Add `Appearance.scale` use and `theme::sz` and `theme::scale`. **P5 (owner, 2026-11-16): the helper, no vendor change.** Also call `window.set_rem_size` with the scale, because 1058 sizes are GPUI short forms in rems (`p_2`, `gap_3`, `text_sm`) that `sz` does not reach. U02 did not try this combination: make sure first that a size is not scaled two times.
  2. Convert the files of this slice from `px(` to `theme::sz(`. Text sizes, the terminal font and the chat font follow the scale. `TextSize` stays as a second, smaller control.
  3. Line widths of 1 px stay 1 px.
- **Tests:** `sz(10.)` is 9, 10, 11.5 and 13 px for the four scales.
- **Verify:** `cargo test -p glasscode`.
- **Demonstration:** `GLASSCODE_DEBUG_OPEN=kit` at 90 % and 130 %: text, icons and gaps change together; nothing is cut.
- **Not in this slice:** files of other slices. Each screen slice converts its own files. U23 converts the remainder.
- **Result, rules for each screen slice:**
  - `theme::sz(value)` replaces `px(value)` for each size and text size. Lines of 1 px stay `px(1.)`. GPUI short forms (`p_2`, `gap_3`, `text_sm`, `rounded_lg`) follow the scale through the rem size, which `render_shell` sets on each frame. **A size goes through one of the two, never both:** no `sz` of a value that came from a rem or from the bounds of an element.
  - `theme::ui_text_size()`, `chat_text_size()` and `terminal_font_size()` are scaled already (text size times scale): use them with `px`, never `sz`.
  - Saved panel widths (`Layout.sidebar`, `right`, `tickets`) and their limits are logical values. A drag is in screen pixels, so divide by `theme::scale()` before you save (see `set_edge`).
  - The scale is set on the Themes page (U16 moves it to Appearance), on the kit page, and with the hook `GLASSCODE_DEBUG_OPEN=kit:dark:130`. `theme::SCALES` has the choices.
  - Measured on screen: a `sz` size and its rem short form agree within 1 pixel at 90, 100 and 130 %. Inputs keep caret and selection; the terminal lays out again when the scale changes.
  - `backdrop::begin_frame()` runs first in `render_shell`, and a glass card uses only the layer of its own window and frame (a correction to U04).
- **Screens that look mixed at 115 and 130 % until their slice converts them:** sidebar tree (U10), Today (U11), session view, chat and right panel (U12; at 900 px and 130 % the third button of the session header is cut), Files (U13), Board (U14), Jump (U15), Settings and dialogs (U16), voice (U19). For U23: Usage, the worktree dialog, session start, move, pairing, notification popups, the brand marks and agent icons (`icons::brand`, `sidebar::agent_icon`), `THUMB_MIN`, and the backdrop debug page (it starts at `top(px(44.))`).
- **Not verified:** 115 % and the light theme on screen; notification popups at 130 %; the title bar with an update pill or a move button; macOS apart from its build job.

### U06 Application icon (wave 1, S) — merged, pull request 27

The icon has opaque white corners: `assets/glasscode-icon-1024.png` has alpha 255 everywhere, and each entry of `Glasscode.ico` copies this. On the taskbar the dark tile sits on a white square.

- **Files:** `assets/glasscode-icon.svg`, `assets/glasscode-icon-1024.png`, `crates/glasscode/resources/Glasscode.ico`, new `script/windows/make-icon.*`, `docs/windows.md` (one paragraph)
- **Steps:**
  1. Remove the inner hairline rectangle from the SVG. Keep the tile and the gold mark.
  2. Add a script that makes the PNG and the ICO (16, 24, 32, 48, 64 as bitmaps, 256 as PNG) from the SVG with a transparent background. Write in `docs/windows.md` how to run it.
  3. Add a check mode to the script: for each ICO entry and the PNG, each corner pixel has alpha 0 and the centre pixel has alpha 255.
- **Verify:** the check mode passes; `cargo build -p glasscode` on Windows.
- **Demonstration:** pin the built program to the taskbar on a light and a dark taskbar. No border and no white corner shows.
- **Not in this slice:** `Glasscode.icns` for macOS (not examined).
- **Result:** `script/windows/make-icon.ps1` draws each size from the SVG with Edge or Chrome and checks the corners. Open items: the owner looks at the pinned icon on a real taskbar one time; the 16 px mark is soft, because its lines are thinner than one pixel (a separate small drawing can correct this later).

### U07 Core data for Today (wave 1, M) — merged, pull request 30

- **Files:** new `crates/glasscode_core/src/activity.rs`, `lib.rs`, `github.rs`, `usage.rs`; `crates/glasscode/src/usage_ui.rs`, `pr_status.rs`, `commands.rs` and `workspace.rs` (one call at each place that makes a new local session)
- **Steps:**
  1. Add `Activity` with atomic save, as `board.rs` does.
  2. Call `add_session` at each place that pushes a new local session into `state.sessions` (`commands.rs` has one, `workspace.rs` has two). Do not count restored sessions or sessions that move from the server (`switch_ui.rs`, `server_ui.rs`). Call `add_merge` when `pr_tick` sees that a pull request of a session became merged; the key is `repo#number`.
  3. Add `github::my_open_prs` with `gh search prs --author @me --state open`, for each GitHub account of the settings.
  4. Make the usage scan start 20 seconds after launch and then every 15 minutes, without a visit to the Usage page.
- **Tests:** `add_merge` with the same key two times counts one time. `weeks` gives 7 values for each week and zero for days with no data. `month` at a month change. A broken file gives an empty `Activity` and keeps a `.broken` copy. `my_open_prs` reads a recorded `gh` reply; an empty reply and a `gh` failure give an empty list and an error.
- **Verify:** `cargo test -p glasscode_core -- --test-threads=1`.
- **Demonstration:** the test output. No screen.
- **Result:** as section 7, with these additions for the slices that use it (U11, U14, U17):
  - `activity::record_session()` and `activity::record_merge(merged_at, lines, key)` read, change and save the file; `activity::today()` gives the local date. `Activity` has one more field, `merged` (the counted keys).
  - `since` is the first day that has a count, not the day of the first run. A resumed conversation, a handoff to a new tab and the Claude session made at launch count as sessions. Not counted: review sessions, merges while the application is closed, pull requests with no session.
  - `my_open_prs` asks each account that `gh` is signed in to, and gets checks and review state only for the newest 10 (`MY_PR_DETAILS`); the others have `ci: None`. It blocks, so call it in the background.
  - The usage scan starts 20 seconds after launch (`usage::scan_due`); `UsageUi.wanted` is gone.
  - Not verified in a running application: merge detection end to end, and `my_open_prs` against the real `gh` with more than one account.

### U08 Core: processes and ports (wave 2, M) — merged, pull request 32

- **Files:** `crates/glasscode_core/src/sys/*`, new `resources.rs`, `lib.rs`, `Cargo.toml` (features of `sysinfo` and `windows` only), `crates/glasscode/src/terminal/mod.rs` (`child_pid`)
- **Steps:**
  1. `process_use` with `sysinfo` (processor and memory).
  2. `listeners`: on Windows `GetExtendedTcpTable` with the listener table that has the owner process; on Unix `lsof -iTCP -sTCP:LISTEN -P -n`.
  3. `end_tree`: end the children first, then the process. Refuse process 0, 4, and the process of Glasscode itself.
  4. `resources::group` and `resources::owner` follow the parent chain to a root.
  5. `TerminalView::child_pid`.
- **Tests:** `group` with a made-up process list: children count for their root; a process with no root is in no group; a parent loop does not hang. `listeners` contains a port that the test opens. `end_tree` ends a child process that the test starts, and refuses the protected numbers.
- **Verify:** core tests on Windows; the macOS job passes.
- **Demonstration:** the test output. No screen.
- **Result:** as section 7. For U17:
  - `process_use()` gives the processor use since the call before it (100 is one full core). The first call, and a call more than 10 seconds after the last one, blocks about 0.25 s to take two readings, so call it in the background.
  - `ProcessUse.parent` is 0 when the parent ended or its number belongs to a newer process.
  - `end_tree` ends at once (no time to save). It refuses 0, 4, Glasscode itself and each program that Glasscode runs under, and returns an error that names what did not end.
  - `listeners()` gives one entry for each (port, pid); the name is empty when the process cannot be read. On Linux without `lsof` the list is empty.
  - `TerminalView::child_pid()` is marked `#[allow(dead_code)]` until U17 uses it.
  - Not verified: Linux (no build job); a process of a different user or an elevated process.

### U09 Core: find projects and logins (wave 2, M) — merged, pull request 37

- **Files:** new `crates/glasscode_core/src/discover.rs`, `lib.rs`, `history.rs` (read only use)
- **Steps:**
  1. Cursor and VS Code: read `User/workspaceStorage/*/workspace.json` in their data folders and take the `folder` value. Confirm the format on this computer first; if it is different, write what you found in the pull request.
  2. Claude Code and Codex: take the work folders from the conversation files that `history.rs` reads.
  3. Keep only folders that exist. Join the same folder from more than one source. Sort by last use.
  4. `logins`: the system login folders, the account folders of Glasscode, and, if decision P3 allows it, the names of accounts that claude-swap saved. Do not read a token.
  5. `signed_in`: the login file of the account exists.
- **Tests:** temporary folders for each source: found, joined, sorted. A folder that does not exist is not in the result. A broken `workspace.json` is skipped. A path with spaces and a path on a different drive. `signed_in` is false for an empty folder.
- **Verify:** core tests.
- **Demonstration:** the test output, and the list that the function gives on this computer (folder names only) in the pull request.
- **Result:** as section 7. For U18:
  - Call `discover::projects(&paths::home(), &discover::app_data()?)` and `discover::logins(&paths::home())` in the background; both block (about 0.2 to 0.5 s here).
  - `FoundLogin.from` is `FROM_SYSTEM`, `FROM_GLASSCODE` or `FROM_CLAUDE_SWAP`. A claude-swap entry has `dir: None` and `NeedsSignIn`; only `sequence.json` is read, which holds names and no token.
  - The list has linked worktrees and folders without git; `add_project` maps a worktree to its main checkout. U18 decides how to show them (this computer gives 179 folders, so the step needs a search box or a short first list).
  - On macOS Claude keeps its login in the Keychain, so a Claude folder counts as signed in when it exists; the poll of U18 cannot see a Claude sign-in finish there. U18 needs a different end signal on macOS (for example the exit of the login program).
  - Not read: VS Code Insiders, VSCodium. Not verified: the claude-swap format on a real install; Linux.

### U10 Tree sidebar (wave 5, M) — merged, pull request 45

- **Files:** `sidebar.rs`, `sessions_ui.rs`, `server_ui.rs` (sidebar part), `shell.rs` (`render_left`)
- **Steps:**
  1. Top: "Workspace", the New session button, then the rows Today, Board, Pull requests with their counts.
  2. Tree: project row (folder icon, name in medium weight, count of sessions that need the person, count of worktrees), worktree row (branch icon, branch name in the code font, "+" to add a session), session row (brand icon, title, state dot).
  3. Each project and each worktree can fold. Save the folded keys in `Layout.folded`. A folded worktree shows a dot if one of its sessions needs the person.
  4. Keep pins, the pull request number with its check colour, the row menu, the close button, and "N more worktrees without a session".
  5. The Server group and "Add project…" stay at the end. The Settings row is at the bottom.
  6. Use `ui::scroll_area`. Convert the files to `theme::sz`.
- **Tests:** the row builder: order (pins first), folded keys, the state of a worktree is the most urgent state of its sessions, a project with 0 worktrees, a title of 200 characters.
- **Verify:** `cargo test -p glasscode`.
- **Demonstration:** three projects; fold and open each level; restart keeps the folds; select a session; the menu and the pin work.
- **Result:**
  - `sidebar::tree` builds the rows from plain data; render does no git and no file work. Fold keys: the project id, and `<project id>|<path>` for a worktree. `sidebar::reveal` opens the folds of a session that the person goes to (`open_session`); a background `select_session` leaves the folds.
  - **A click on a worktree row folds it.** Before, it opened the last session of the worktree; now each session has its own row.
  - Rows at the top: New session, Today, Board, Usage. There is no Pull requests row; a comment in `render_left` marks its place for U14. U17 decides about the Usage row.
  - New icons: `PIN`, `MORE`.
  - Closing a session draws the change about 0.75 s after the click, in the old build too (the window is busy while the terminal ends). Not changed.
  - Not seen on screen: the need-you and working counts, a pull request number with its colour, connected server rows, Classic and High contrast, 15 projects with 60 worktrees (unit test only).

### U11 Today dashboard (wave 5, L) — merged, pull request 46

- **Files:** `today_ui.rs`, new `today_cards.rs`, `shell.rs` (`render_right` for Today)
- **Steps:**
  1. Back layer: `backdrop_layer(0.85)`. Content in a centred column with `scroll_area`.
  2. Start box (glass, strong): a `Composer`, pills for agent and account, project, and start point, and Send. Send calls `start_session` with a `NewSpec`, as `today_start` does.
  3. Cards (glass), in a grid that goes from 3 columns to 2 to 1 as the centre becomes narrow: Needs you (`today_needs`, with an action pill), Coming up (due in 7 days, review requests), Usage (limits of each account from the usage report), Activity (`Activity::weeks`, 26 weeks, "Since <date>"), Recent sessions (5), Pull requests (`my_open_prs`, 3 and "N more"), This month (`Activity::month`).
  4. Below the cards: "Your list", the existing task board with its filters, add box and sections.
  5. Right panel: tabs Inbox (approvals, "Needs you") and Activity ("What agents did").
  6. Each card has an empty state with one sentence.
- **Tests:** the column count for widths 300, 700, 1100. "Coming up" picks and sorts items, and leaves out items of today and of more than 7 days. Empty input for each card builder.
- **Verify:** `cargo test -p glasscode`.
- **Result:**
  - The seven cards, the start box, "Your list" with the complete task board, and the right panel tabs Inbox and Activity. Each function of the old Today is kept (the table is in the pull request). New hook: `GLASSCODE_DEBUG_TODAY=still|inbox|activity`. New icons: `CHECKBOX`, `CHECKBOX_CHECKED`.
  - In the light theme the cards get a soft shadow instead of an edge.
  - **Cost, a stop:** with a realistic home (3 projects, 8 sessions, 7 tasks) the moving backdrop costs 9.04 % of one core more than a still one (16.23 % against 7.19 %), above the threshold of 8 %. Only 5.4 frames each second were made of the 10 wanted, so one frame costs about 17 ms of processor time, against about 5 ms on the empty page of U04. Today with a still backdrop is 7.19 %, against 2.3 % for the empty page. Slice U24 follows.
  - Known defects: a click on an open pill of the start box opens its list again instead of closing it (U24). The message box of the start box draws its placeholder in `faint` and its text in `text_strong`, which rule 12 does not permit on glass; they come from `composer.rs` (U12).
  - Not seen on screen: the agent and start-point lists, a failed Send, the box with no project, Deny and "Allow for the session", rows of the Pull requests card with real data, the screen below its first part in light and at 900 x 560, Classic and High contrast.
- **Demonstration:** `GLASSCODE_DEBUG_OPEN=today` in light and dark; drag a sidebar and see the grid change; start a session from the box; answer an approval in Inbox; add a task in "Your list".

### U24 Cost of a frame (wave 5, M, added 2026-11-17) — merged, pull request 48; the gate failed

U11 measured 9 % for the backdrop on Today, and 7 % for Today with nothing moving. The owner decided on 2026-11-17 that a slice follows when the cost is above 8 %. The numbers show that a frame of Today is expensive and that something draws the window again when nothing changes, so this slice finds the causes first; a change of the window structure is the last choice.

- **Files:** `today_ui.rs`, `today_cards.rs`, `sidebar.rs`, `sessions_ui.rs`, `shell.rs`, `backdrop.rs`, `workspace.rs` (ticks only); others only with the reason in the pull request.
- **Steps:**
  1. Measure on Today with a realistic home, for the backdrop None, still and moving: `Workspace` renders each second, and milliseconds for each render.
  2. Name the causes: what calls `notify` when nothing changes; where a render of Today uses its time (tree, cards, heat map, task list, a file or git read in the render path); why only 5.4 frames each second were made.
  3. Correct the largest causes. Measure again.
  4. Correct the pill of the start box that opens again instead of closing.
- **Gate:** moving minus None is no more than 6 % of one core at 10 real frames each second, and Today with a still backdrop is near the empty page (about 3 %).
- **Stop:** the gate fails after the corrections. Then write the proposal for the window structure (a thin root view, a cached `Workspace`, the layer paints the crops of the cards) with the numbers, and stop. The owner decides.
- **Tests:** each pure function that the corrections add.
- **Verify:** `cargo test -p glasscode`; the measurements in the pull request.

- **Result:**
  - **Corrected:** the 700 ms tick ran `git` for each project on the main thread (`tickets_connected`, 583 ms of each 1.29 s, on each screen). The repositories are now read in the background and kept (`TicketsUi.repos`). Today with a still backdrop went from 6.6 % to about 3 % of one core, and the backdrop makes its 10 frames each second.
  - **The gate failed:** at 10 real frames each second the backdrop costs 18.3 % of one core on Today (21.3 % against 3.0 % for None). One render of Today is about 17 ms, and about 13.5 ms of it is the layout pass of GPUI (the rows of "Your list" 7.7 ms, the cards 6.1 ms, the left sidebar 1.1 ms). Building the elements is about 1 ms.
  - **A working session costs about 100 % of one core** with no backdrop: the pulse of the green dot (`sidebar::status_icon`, `ui::status_dot`) draws the window on each display frame (64 each second). The old build had this too.
  - Renders with no change are 0.14 each second (about 0.2 %); not changed, because the greeting, the date and "started N min ago" update only through them.
  - New: `GLASSCODE_DEBUG_FRAMES` logs renders, their phases and the tick times. The pill of the start box closes on a second click.
  - **Owner decisions (2026-11-17):** build the window structure of the proposal (slice U25); the working dot is a still green dot.
  - Other `git` calls on the main thread, not changed: `settings_ui.rs` (`repo_slug` in the render of Settings, Projects; U16), `tickets_ui.rs` `git_projects` (U14), `workspace.rs` `project_worktrees` and `configure_github` (U23).
  - Not measured: a release build with no debug assertions.

### U25 Still working dot (wave 5, S, added 2026-11-17)

**Owner decision (2026-11-17, later the same day): the moving backdrop at about 18 % of one core on Today is accepted. The window structure (a thin root view with a cached `Workspace`) is not built.** The reasons: processors have many cores, so the share of the machine is small (about 1.5 % here), and the structure has a risk of screens that do not update. The limit of 6 % of U04 and the gate of U24 no longer apply. Known cost that stays: each backdrop frame runs the layout of the window (about 17 ms on Today) on the main thread, 10 times each second, while the window is active; a screen with more content costs more. A later slice can come back to the structure if typing or scrolling shows a delay.

- **Files:** `sidebar.rs`, `ui.rs`, `kit.rs`, `backdrop.rs` (comment only).
- **Steps:**
  1. The working dot is a still green dot (`sidebar::status_icon`, `ui::status_dot`). Before, its pulse drew the window on each display frame: about 100 % of one core for one working session.
  2. List the other animations (spinner, caret, voice level, toasts) with their cost and say if one runs when nothing happens.
- **Verify:** `cargo test -p glasscode`; the measurement of one working session in the pull request.
- **Result:** one working session on Today with no backdrop: 2.95 % of one core and 0.14 draws each second (before: about 103 % and 64 draws each second). With the moving backdrop: 21.6 %, the same as with no working session. No code of the window structure was written.
  - **Other animations:** the chat spinner (`chat_ui.rs` `spinner`) uses the same mechanism as the old dot and draws the window on each display frame while the agent of the chat on screen works; its cost was not measured (it needs a real agent session). There is no caret blink. The voice level, glow and rings are macOS only and run only while voice is live. Toasts, the scroll thumb and notification popups have no animation.

### U12 Session view and tabs (wave 5, M)

- **Files:** `session_view.rs`, `chat_ui.rs` (frame of the column and the message box only), `right_panel.rs`, `github_ui.rs` (split of checks), `shell.rs` (`render_right` for sessions)
- **Steps:**
  1. Tab strip: one tab for each `CenterTab`. Session tabs are the sessions of the worktree. "+" adds a session. The Chat and Terminal control is at the right.
  2. Short header in the chat column: brand, project and branch, account, state. Keep the account menu, Browser, Hand off and "⋯".
  3. With `backdrop_in_sessions`, draw `backdrop_layer(0.6)` and put the chat column and the message box on `glass_strong`. The terminal stays opaque with a margin.
  4. Right panel tabs: Files, Changes, Checks, Pull request. Save the tab.
- **Tests:** the tab list for a worktree with 1 and with 4 sessions; closing the active tab selects the neighbour.
- **Verify:** `cargo test -p glasscode`; the hooks `_CHAT` and `_RIGHT_TAB` work.
- **Demonstration:** a worktree with three sessions; change tabs; Chat and Terminal; each right tab; an approval request in Chat; light and dark.

### U13 File tabs and preview (wave 6, M)

- **Files:** `files_ui.rs`, new `file_view.rs`, `session_view.rs` (tab content)
- **Steps:**
  1. A click on a file in Files or Changes opens `CenterTab::File` in the centre. A second click on the same file selects its tab.
  2. The file view shows the path, then the text with line numbers, or the diff when the file changed (Diff and File control). Keep the limits of 512 KB and 4000 lines and the binary check; show a sentence when a limit applies.
  3. Draw lines with `uniform_list`.
  4. Keep "Open in <editor>", "Copy path", "Mention in session". Remove the 300 px pane.
- **Tests:** open, select again, close. A file of 0 bytes, a binary file, a file above the limit, a file that was deleted after the tab opened, a file with very long lines.
- **Verify:** `cargo test -p glasscode`; hook `_FILES`.
- **Demonstration:** open four files and a changed file; switch Diff and File; close tabs; delete an open file on disk.

### U14 Board and pull requests page (wave 6, M)

- **Files:** `tickets_view.rs`, `tickets_ui.rs`, new `prs_view.rs`, `shell.rs` (`render_right` for Board)
- **Steps:**
  1. Board: columns and cards with the new look; source brand on each card; the brands of the sessions on the card; a "Start workspace" button on a card with no session, which calls `open_start_ticket`.
  2. The reader panel becomes the right panel of the Board view.
  3. New Pull requests page: "Mine" (`my_open_prs`) and "Asked of me" (review requests), each row with checks state and an action ("Fix with agent" opens the session; "Review" starts a review).
- **Tests:** the row builder of the page: order, an empty list, a pull request with no checks.
- **Verify:** `cargo test -p glasscode`.
- **Demonstration:** drag a card to a different column; start a workspace from a card; open the Pull requests page with and without a GitHub login.

### U15 Jump (wave 4, S) — merged, pull request 44

- **Files:** `palette_ui.rs`
- **Steps:** add projects, worktrees and, when a session is active, the files of its worktree to the items. Group the results: Sessions, Worktrees, Projects, Files, Commands, Tickets. Show the brand and the state dot on session rows. New look.
- **Tests:** the match function with the new groups; an empty query; a query with no match.
- **Verify:** `cargo test -p glasscode`.
- **Demonstration:** open Jump, type part of a branch name, a project name and a file name, press Enter on each.
- **Result:**
  - Groups: Sessions, Worktrees, Projects, Files, Commands, Tickets, each with a cap of rows; a query of one word that names a group ("commands", "files") shows that group in full. The 18 commands keep their labels and actions.
  - Enter: a session calls `open_session`; a worktree opens its most recent session, or the New session dialog when it has none; a project folds or opens in the tree; **a file opens in the Files tab of the right panel (`files_select`), with a comment for U13 to change it to a centre tab**; commands, tickets and review requests as before.
  - Files: only with an active session, the first 20 000 files of its worktree, read in the background when Jump opens.
  - With no backdrop painted, the tint of `glass_strong` alone lets text from behind show, so Jump paints `theme::dialog()` under the glass. Other overlays on glass need the same.
  - Hook: `GLASSCODE_DEBUG_OPEN=palette[:theme][:scale][:query]`.
  - Seen on screen by the subagent: each of the seven steps of the interactive check. Not seen: ticket and review rows, the Claude and Codex marks and the state dots, Classic and High contrast, macOS.

### U16 Settings page (wave 6, L)

- **Files:** `settings_ui.rs`, `themes_ui.rs`, `dialogs.rs` (account groups: frame only), `github_accounts_ui.rs`, `browser_ui.rs`, `update_ui.rs`, `server_ui.rs` (settings rows only)
- **Steps:**
  1. Back layer `backdrop_layer(0.8)`; the section menu and each card on `glass_strong`. A change of theme or backdrop shows at once.
  2. Sections: General, Appearance, Agents and accounts, Permissions, Git and projects, Integrations, Automation, Browser, Server and devices, Updates, Keyboard, Advanced. Put each existing setting in one section (the pull request has the table: old section, new section). No setting is lost.
  3. Appearance: theme (Auto, Light, Dark, Classic, High contrast), accent, backdrop, finish, scale, density, fonts, cursor, chat font, default session view, backdrop in sessions.
  4. Permissions: the 13 `control::Kind` rules.
  5. Card form: title, one sentence, the control, and a footer line when it helps.
  6. Search: filters the cards of all sections by title and sentence.
  7. Advanced: "Open state.json folder", "Open log folder", "Run first run again" (after U18; hide until then).
  8. Remove each `coming(...)` row.
- **Tests:** the search function. A test that lists each field of `Prefs`, `Appearance`, `NotifySettings` and the other settings structures and makes sure that a card exists for it.
- **Verify:** `cargo test -p glasscode`; hook `_THEMES` opens Appearance.
- **Demonstration:** change each Appearance control and see the page change; search "notify"; sign in to a second account; light and dark.

### U17 Status bar: usage, resources, ports (wave 7, M)

- **Files:** new `status_bar.rs`, `shell.rs` (the status functions), `usage_ui.rs` (read only)
- **Steps:**
  1. Left: a usage meter for each account that has limits (5 hour and week), a refresh button, then the existing items (branch, changes, GitHub, CLI versions).
  2. Right: memory of all sessions, count of terminals with the count of orphans, count of ports.
  3. Usage popover: each account with its limits and reset times; "Open usage".
  4. Resource popover: project, worktree, process, with processor and memory; "×" calls `end_tree` after one confirm line; "End N orphan terminals". Refresh every 2 seconds only while the popover is open.
  5. Ports popover: ports of sessions with Open (default browser) and Kill; other ports folded.
  6. A click outside or Esc closes the popover.
- **Tests:** the format of bytes and time. The model of the two popovers from made-up input: no sessions, one orphan, a port with no session.
- **Verify:** `cargo test -p glasscode`.
- **Demonstration:** start `npm run dev` in a shell session; see the port; Open; Kill; see it go. End an orphan.

### U18 First run (wave 8, L)

- **Files:** new `first_run.rs`, `workspace.rs` (start condition), `shell.rs`, `crates/glasscode_core/src/model.rs` (`State.first_run_done`, with `#[serde(default)]`), `settings_ui.rs` (show "Run first run again")
- **Steps:**
  1. Show the flow when `state.projects` is empty and the flow was not completed or skipped (`State.first_run_done`).
  2. Left: the backdrop at strength 1.0 in a rounded panel with the mark and one sentence. It hides when the window is narrow.
  3. Step 1 Projects: the result of `discover::projects`, with check boxes; "Add a folder…"; "Clone from GitHub…" (`project import`).
  4. Step 2 Agents: `discover::logins`. A tick on an entry that is not signed in starts `agents::login_launch` in the embedded terminal at once. Poll `signed_in` every second. Continue waits while a login is in work; Cancel stops it. "Sign in to another account" adds an account and starts its login. GitHub uses `gh auth status` and `gh auth login --web`. Linear uses the existing connect form.
  5. Step 3 Appearance: the controls of U16.
  6. Step 4 Summary: counts; "Open Today".
  7. "Skip for now" at each step.
  8. Show "Run first run again" in Settings, Advanced.
- **Tests:** the step model: next, back, skip; Continue is blocked while a login is pending; a cancelled login clears the tick; no project and no login found gives usable empty states.
- **Verify:** `cargo test -p glasscode`.
- **Demonstration:** `GLASSCODE_HOME=<empty folder>` and `GLASSCODE_DEBUG_OPEN=firstrun`: do the four steps; add two projects; sign in to one more account; restart and see that the flow does not show again.

### U19 Voice screen and orb (wave 8, M)

Do this slice only after decision P4.

- **Files:** `glasscode_ui.rs`, new `voice_view.rs`, `today_cards.rs` (the button), `status_bar.rs` (the button), `crates/glasscode_core/src/backdrop.rs` (`orb`)
- **Steps:**
  1. Port `drawOrb()` of the mock to `backdrop::orb`.
  2. The voice screen covers Today: the orb, the state (Listening, Thinking, Speaking, Muted), the last words, the last action, Mute, End. The level and the state come from the existing voice state.
  3. The microphone buttons show only when `glasscode_ui::VOICE` is true.
  4. A debug hook plays made-up states so the screen can be checked on Windows.
- **Tests:** `orb` gives the same bytes for the same input; pixels outside the circle have alpha 0; `level` 0 and 1 do not panic.
- **Verify:** core tests; `cargo check -p glasscode`.
- **Demonstration:** `GLASSCODE_DEBUG_OPEN=voice` in each finish and theme.

### U20 Editor (wave 7, L)

- **Files:** new `editor.rs`, `file_view.rs`, `keymap.rs`
- **Steps:**
  1. A new entity with lines in a `Vec<String>`, a cursor, a selection, and input method support (the pattern of `composer.rs`). Do not build on `TextArea`.
  2. Shape only the lines that show.
  3. Undo and redo with joined typing runs. Clipboard. Tab inserts 4 spaces. Word movement. Home, End, Page keys.
  4. Save writes atomically and keeps the line ending style of the file. A dot on the tab shows a change that is not saved. Closing a changed tab asks.
  5. When the file changes on disk: reload if the tab has no change; if it has, show a bar with "Reload" and "Keep mine".
  6. Edit mode is off above 2 MB; the view stays read only.
  7. Edit and Preview control in the tab strip.
- **Tests:** insert, delete and selection at line starts and ends; undo and redo sequences; CRLF stays CRLF; a file with no last line end; characters of more than one byte and emoji; a 2 MB file opens in under 1 second in the test; save to a read-only file gives an error and keeps the text.
- **Verify:** `cargo test -p glasscode`.
- **Demonstration:** edit and save a Rust file; undo 20 steps; let an agent change the same file while it is open, with and without your own change.

### U21 Syntax colours (wave 8, M)

- **Files:** new `crates/glasscode_core/src/syntax.rs`, `editor.rs`, `file_view.rs`, `theme.rs` (colours)
- **Steps:** a small scanner with a word table for each language (Rust, TypeScript and JavaScript, JSON, TOML, Markdown, Python, shell) that gives ranges for comment, string, number, keyword, type. No new dependency (decision P6). Colour only the lines that show.
- **Tests:** for each language one sample with the expected ranges; a string that is not closed; a file in an unknown language gives no ranges.
- **Verify:** core tests.
- **Demonstration:** open one file of each language in light and dark.

### U22 Project settings and templates (wave 7, M)

- **Files:** new `crates/glasscode_core/src/project_settings.rs`, `git.rs`, `naming.rs`, `settings_ui.rs`, `commands.rs`
- **Steps:**
  1. Read `.glasscode/settings.json` of a project: `setup`, `branch_template`, `worktree_folder`, `default_agent`.
  2. Run `setup` in the terminal of a new worktree before the agent starts. A failure shows in the session and does not stop it.
  3. Use the templates in `add_worktree` and the branch name. Variables: `{repo}`, `{branch}`, `{issue}`, `{type}`, `{slug}`.
  4. Settings: the switch "For me / Project · <name>" on the sections that have project values, with a line that says where the file is.
- **Tests:** the template function with each variable, an unknown variable, characters that a path cannot have; a broken settings file gives the defaults and one warning; a project value wins over the personal value.
- **Verify:** core tests; `cargo test -p glasscode`.
- **Demonstration:** set a setup script and a branch template for one project; start a session from a ticket; see the folder, the branch and the script output.

### U23 Scale sweep, removal of old code, documents (wave 9, M)

- **Files:** each file of `crates/glasscode/src` that still has `px(`; `README.md`, `docs/architecture.md`, `docs/screenshots/`, new `docs/ui-redesign-acceptance.md`
- **Steps:** convert the remaining sizes; remove code that nothing calls (the rail, the Themes page, `proofs.rs`); update the documents and add screenshots; write the acceptance list (each screen, light and dark, 90 % and 130 %, window at the minimum size 900 x 560).
- **Verify:** a search for `px(` finds only 1 px lines and `theme.rs`; all gates pass.
- **Demonstration:** the acceptance list, done by the owner.

## 9. Files with a conflict risk

| File | Slices | Rule |
|---|---|---|
| `shell.rs` | U03, then U04, U05, U10, U11, U12, U14, U17, U18 | Each later slice changes only its own function (`render_left`, `render_right` arm, status functions). U05 is the exception: it converts the sizes of the full file, and no screen slice is in work then. |
| `workspace.rs` | U01, U02, U03, U04, U07, U18 | U01, U02 and U07 add a few lines each (debug hooks, `record_session`). After U03, screen slices do not change it. |
| `usage_ui.rs` | U03, U07, U17 | U07 changed the scan timer. U17 only reads. |
| `theme.rs` | U01, U04, U05, U21 | Serial by wave. |
| `session_view.rs` | U12, U13 | U13 changes only the tab content function. |
| `settings_ui.rs` | U03, U16, U22 | Serial by wave. |
| `model.rs` | U03, U04, U18 | New fields only, each with `#[serde(default)]`. |
| `keymap.rs` | U03, U20 | Both tables, same actions. |
| `Cargo.lock` | U08 | Take the version of `ui-redesign`, then `cargo check`. |

### Open findings that have no slice yet

| Finding | From | Owner |
|---|---|---|
| With no backdrop, the application uses 2.4 % of one core with the window hidden and about 6.5 % with it shown and one shell session (its own ticks). Review focus 4 expects almost zero. | U02 | U23 finds the ticks that cause it and reports; a correction can need its own slice. |
| A window that never had the real foreground (for example one that a terminal started in the back) keeps the backdrop animation behind other windows, because it gets no deactivation. | U04 | U11 looks at it when it measures Today; a correction is in `backdrop.rs`. |
| Screen checks started Glasscode with scratch homes that had no account, so it took the real system login as "My login" (a name and a folder path; no token is copied). In a run of U11, and possibly earlier runs, a Claude session started `claude.exe` with the real login in a throw-away repository; it stayed at its prompt and no message was sent. | U11 | Rule 15 of section 4. U23 looks at a debug switch that stops agent programs in a check. |
| The chat spinner draws the window on each display frame while the agent of the chat on screen works (the same mechanism as the old working dot; probably about one core for that time, not measured). | U25 | The owner decides. U12 owns `chat_ui.rs`. |
| The folder `accounts` in the real `~/.glasscode` got a new time during the work, possibly from `cargo test -p glasscode`, which runs with no scratch home. | U02 | U23 finds the test and gives it a temporary home. |
| `end_tree` fails now and then on the build runner for a process that is already ending. | plan update, pull request 38 | Corrected in pull request 39: a process that is gone after the wait has ended, whatever TerminateProcess said. |

## 10. Conditions to stop

Stop and tell the main agent (who tells the owner) when:

1. A step needs a change in `vendor/` that the owner did not approve.
2. A step needs a new dependency that the slice does not name.
3. A function of section 2 cannot be kept.
4. The macOS job fails and the cause is not in your slice.
5. The work needs a file that a slice in work owns.
6. A measured cost is above the limit in the slice (U02, U04).
7. A step needs to read a login token or a secret.
8. A debug hook or a test of a different area fails after your change and you cannot find the cause in one hour.

## 11. Review focus

The main agent checks these in each pull request, because no single slice test covers them.

1. **A person can read all text** on the backdrop at its brightest, in each of the four palettes.
2. **Nothing is lost:** each function of section 2 is still reachable, and each old setting has a card.
3. **The window at 900 x 560 and at 130 % scale:** nothing is cut and nothing overlaps.
4. **Idle cost:** with the window in the back, Glasscode uses almost no processor time; memory does not increase in one hour.
5. **State from the old build:** an old `state.json` and `board.json` load without loss.

## 12. Decisions for the owner

The main agent gets the answers P1, P2 and P7 before it starts a slice. The other answers can come later, at the time the table gives.

| No. | Question | Recommendation | Answer |
|---|---|---|---|
| P1 | Which branch is the base of `ui-redesign`? | `windows-port`. It has all Windows work and is not in `main` yet. | **Yes**, `windows-port` (owner, 2026-11-16). |
| P2 | The recorded rule is "macOS must behave exactly as before the port". The interface code is shared, so the redesign changes macOS too. Is that accepted? | Yes: macOS gets the new interface, must compile, and is checked on screen later. | **Yes**, accepted (owner, 2026-11-16). |
| P3 | Accounts that claude-swap saved: Glasscode does not read tokens. Can the first-run flow list those accounts by name and ask you to sign in to each one again in its own Glasscode folder? | Yes. No copy of login files. | **Yes**, names only (owner, 2026-11-16). |
| P4 | The voice helper is macOS only. Do you want a Windows voice helper now? | Not in this plan. U19 makes the screen and the orb; the button shows on Windows only after a helper exists. A Windows helper needs its own plan. | Open. The plan uses the recommendation until the owner answers. |
| P5 | Global scale: a small patch in `vendor/gpui_windows` (a zoom on the scale factor) or the helper `theme::sz` in all files? | Decide after U02. The helper is the default; it needs no vendor change. | **The helper `theme::sz`** with `set_rem_size`; no vendor change (owner, 2026-11-16). |
| P6 | Dither finish: GPUI scales images with a bilinear filter. Dither needs a full-size image (more memory, see U02) or a small vendor patch for a nearest filter. Editor colours: a small scanner of our own (no dependency) or a library. | Decide the dither method after U02. For colours: our own scanner. | **Full-size image** for dither, no vendor change; our own scanner for colours (owner, 2026-11-16). |
| P7 | Does the agreement of 2026-11-15 (the agents merge after review and green builds, and do the screen checks) apply to `ui-redesign`? | Yes, for pull requests into `ui-redesign` only. | **Yes** (owner, 2026-11-16). |
| P8 | The mock has items that this plan leaves out (section 3). Is one of them necessary now? | No. | Open. The plan uses the recommendation until the owner answers. |
| P9 | Which Wayglim looks do you want as the three presets? | The plan ports the three of the mock (Prism, Aurora, Dusk). Wayglim shows no licence, so its code is not copied. | Open. The plan uses the recommendation until the owner answers. |
