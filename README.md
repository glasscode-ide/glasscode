<div align="center">

<img src="docs/logo.png" alt="Glasscode" width="120" height="120" />

# Glasscode

**A terminal-first workspace on macOS and Windows for running Claude Code and Codex agents side
by side — each in its own git worktree, on the subscription account you choose.**

</div>

---

Glasscode is a native app for macOS and Windows (built on [GPUI](https://www.gpui.rs/)) for
people who run several coding agents at once. Every piece of work is a git worktree with its own
agent session; you can watch them as a clean **chat** or drop into the **raw terminal** at any
time. It keeps your approved CLI versions in sync, and on the Mac can hand a long-running session
off to a Linux server (and your iPhone) and bring it back.

The window has a tree of your projects, worktrees and sessions on the left, a **Today** page for
your day, session tabs with file tabs and a small editor in the centre, the session's files,
changes, checks and pull request on the right, and a status bar with your usage limits, the
memory of your sessions and their ports. A slow moving backdrop with glass cards stands behind
Today, Settings and the sessions; it can be switched off.

> **Status:** in active development. Contributions are welcome — see
> [CONTRIBUTING.md](CONTRIBUTING.md).

> **This is the `windows-port` branch.** It adds Windows to the macOS app. Release 1 for Windows
> is a test build of the local work (projects, worktrees, shell, Claude and Codex sessions, the
> `glasscode` command, the agent browser). The server, phone pairing and the voice panel are
> **not ported** to Windows, and most of release 1 is not yet verified with real agent
> conversations — see [Windows port](#windows-port-release-1-in-test).

## Highlights

- **Projects → worktrees → sessions** in one tree in the left sidebar. Each project and worktree
  folds, and a folded worktree shows a dot when one of its sessions needs you. Start a session on
  the latest `main` (fetched fresh), on a pull request (found by number, title, branch, or author
  and checked out on its own branch), or on an existing worktree.
- **Each session is the real CLI** — `claude`, `codex`, or a plain shell — running in an actual
  terminal, so nothing is simulated or wrapped away.
- **Chat view or Terminal view**, per session. Chat surfaces the agent's questions
  (permissions, trust prompts, menus) as inline buttons; the terminal is always one click away.
- **Session tabs, file tabs and an editor.** The sessions of a worktree are tabs. A file from the
  Files or Changes tab, or from Jump, opens in a tab of its own, as text or as its diff, with
  syntax colours; Edit makes small changes and saves without losing text.
- **Today** — the start of your day: a box that starts a session from a few words, cards for what
  needs you, what comes up, your usage, your activity, recent sessions, your pull requests and
  this month, and your list of tasks, recurring items and outside items (Linear / GitHub).
- **Board and Pull requests.** Your Linear tickets in columns, each with its sessions and a button
  to start one; your open pull requests and the ones that ask for your review, with their checks.
- **Jump** — one search for sessions, worktrees, projects, files, commands and tickets.
- **Review a pull request** in three steps — Explain → Plan → private review — checked out as
  `review/<n>` and never pushed from.
- **Status bar** with a usage meter for each account (5 hours and week), the memory of your
  sessions' programs, their terminals and their ports. Each opens a popover: limits and reset
  times; each program with its processor and memory, and a button to end it; each port with Open
  and Kill.
- **First run.** With no project yet, Glasscode lists the folders you opened in Cursor or VS Code
  or worked in with Claude Code or Codex, and the logins it found; you tick what to bring in, then
  choose the look.
- **One Settings page** with twelve sections and a search. Appearance has the themes (Glasscode
  Light and Dark, Classic, High contrast, Auto), the accent, the backdrop (Prism, Aurora, Dusk or
  none, smooth or dithered) and the scale of the whole interface (90 to 130 %).
- **Project settings in the repository.** `.glasscode/settings.json` can name a setup script for
  each new worktree (it runs only after you approve that exact command), a branch name template and
  a worktree folder template.
- **Team-approved CLI versions.** Settings › Updates shows each CLI's installed version, the
  newest published to npm, and a one-click **Update** — per person, without changing the team's
  pinned version for everyone else.
- **Run anywhere.** Hand a session to a Linux server daemon (`glasscoded`) and follow or decide
  on it from your iPhone; bring it back to the Mac when you want. (Not on Windows yet.)
- **Self-updating.** Signed releases are verified against a built-in key and installed in place.
  This build has no release server yet, so it does not update itself.

### Shortcuts

| Action | macOS | Windows |
|---|---|---|
| Jump (search and commands) | ⌘K | Ctrl+Shift+K |
| New session | ⌘N | Ctrl+Shift+N |
| Settings | ⌘⇧S | Ctrl+Shift+S |
| Show or hide the left sidebar | ⌘⇧E | Ctrl+Shift+E |
| Show or hide the side panel | ⌘B | Ctrl+Shift+B |
| Back, Forward | ⌘[, ⌘] | Ctrl+<, Ctrl+> (Ctrl+Shift+, and Ctrl+Shift+. on a US keyboard) |
| Next, previous session | ⌃Tab, ⌃⇧Tab | Ctrl+Tab, Ctrl+Shift+Tab |

Settings › Keyboard lists them all; [`docs/windows.md`](docs/windows.md#shortcuts) has every key
of both platforms.

See [`docs/architecture.md`](docs/architecture.md) for the full design.

## Screenshots

_Screenshots live in [`docs/screenshots/`](docs/screenshots/). They are not yet updated for the
new interface of the UI redesign._

## Requirements

### macOS

- macOS 12 or newer (Apple Silicon or Intel)
- [Rust](https://rustup.rs/) — the exact toolchain is pinned in `rust-toolchain.toml` and
  installed by rustup automatically
- Xcode **Command Line Tools** (`xcode-select --install`) — full Xcode is not required for the
  Mac app
- `git`
- A `claude` and/or `codex` CLI on your `PATH` (Glasscode can install and keep these current)

### Windows

- Windows 10 version 1809 or newer, or Windows 11 (64-bit x86); only Windows 11 was tested
- [Rust](https://rustup.rs/) with the MSVC toolchain (pinned in `rust-toolchain.toml`, as above)
- The Visual Studio Build Tools with the "Desktop development with C++" workload and a Windows
  SDK (the build uses `rc.exe`, a release build also `fxc.exe`)
- Git for Windows
- A `claude` and/or `codex` CLI, and `gh` for pull requests
- For the installer only: [Inno Setup](https://jrsoftware.org/isinfo.php) 6.3 or later

## Build and run

### macOS

```sh
# Development build — runs the app
cargo run

# Run the test suites
cargo test -p glasscode_core -- --test-threads=1   # core logic
cargo test                                          # the app (crates/glasscode, the default member)

# Build a signed, bundled Glasscode.app (and a .dmg) in target/release/
script/bundle-mac.sh
```

`bundle-mac.sh` produces an **ad-hoc signed** build by default (macOS asks you to right-click →
Open the first time). To produce a signed + notarized build for distribution, set a signing
identity and notary profile:

```sh
SIGN_IDENTITY="Developer ID Application: Your Co (TEAMID)" \
NOTARY_PROFILE=glasscode \
BUNDLE_ID=com.yourco.glasscode \
script/bundle-mac.sh
```

The optional Linux server daemon is built separately with Docker via
`script/build-glasscoded.sh`.

### Windows

```powershell
# Build both programs: the app and the console program behind the `glasscode` command
cargo build -p glasscode -p glasscode_cli

# Development build — runs the app (it builds only the app, so build both programs first)
cargo run

# Run the test suites (what the Windows CI job runs)
cargo check --locked -p glasscode
cargo test --locked -p glasscode_core -- --test-threads=1
cargo test --locked -p glasscode_cli
cargo test --locked -p glasscode

# Build the release programs and an unsigned installer in target\release\
powershell -NoProfile -ExecutionPolicy Bypass -File script\bundle-windows.ps1
```

On Windows the app is a window program, and the `glasscode` command is a second, console
program (`glasscode-cli.exe`) that the app copies to `%USERPROFILE%\.glasscode\bin\glasscode.exe`
and puts on the PATH of each session. **The installer is unsigned**, so Windows SmartScreen warns
before it runs. See [`docs/windows.md`](docs/windows.md) for the details: the two programs, where
data lives, the shortcuts, what is not ported, and the known limits.

## Windows port (release 1, in test)

The port from macOS to Windows happens on this branch, `windows-port`. The `main` branch does
not change until the owner decides to merge it. The work is divided into 19 slices; each one adds
one function that a person or a test can see on Windows, and lands as one pull request into this
branch.

- [`docs/windows.md`](docs/windows.md) — Glasscode on Windows: build, run, data, shortcuts,
  limits.
- [`docs/windows-acceptance.md`](docs/windows-acceptance.md) — what was checked on Windows for
  release 1, and the checklist of what was not.
- [`docs/windows-port-plan.html`](docs/windows-port-plan.html) — the findings and the design
  decisions (GC-WIN-001).
- [`docs/windows-port-slices.html`](docs/windows-port-slices.html) — the 19 slices, their
  sequence, and the rules for each pull request (GC-WIN-002).

The two plans are HTML pages: open them in a browser from a local checkout.

### Status

| Slice | Result on Windows | Status |
|-------|-------------------|--------|
| S01 – S13, S17, S18 | The core and the app build; shell, Claude and Codex sessions; the `glasscode` command; keyboard; title bar; files, links and notifications; fonts and words; secrets; agent versions; the agent browser; the installer. | Merged (15 of 19) |
| S14 – S16 | Server connection, sync and move, phone pairing. | Not started, after release 1 |
| S19 | Documents and acceptance. | In work |

Merged means the code is in this branch and its CI jobs pass. It does not mean that every
function was checked on Windows: of the 16 acceptance tests, 4 passed on the screen, 11 are not
verified (they need signed-in agents, the screen or an installer run), and 1 is not in release 1.
See [`docs/windows-acceptance.md`](docs/windows-acceptance.md).

### Rules for a change on this branch

- macOS behavior stays the same. Windows code goes behind `#[cfg(windows)]` or into
  `crates/glasscode_core/src/sys/windows.rs`; operating system code goes into
  `glasscode_core::sys`, and other modules call it.
- Pull requests target `windows-port`, not `main`, and merge only when both CI jobs (macOS and
  Windows) pass.

## UI redesign

The new interface is built on the branch `ui-redesign`, made from `windows-port`, in 25 slices.
Each slice is one pull request into that branch. The interface code is shared, so macOS gets the
new interface too; it has not yet been run on macOS.

- [`docs/ui-redesign-slices.md`](docs/ui-redesign-slices.md) — the plan (GC-UI-001): the slices,
  the rules, the owner's decisions, and a "Result" note for each merged slice.
- [`docs/ui-redesign-mock.html`](docs/ui-redesign-mock.html) — the interactive mock of the
  screens; open it in a browser.
- [`docs/ui-redesign-proofs.md`](docs/ui-redesign-proofs.md) — the measurements that chose the
  method of the backdrop, the glass and the scale (GC-UI-002).
- [`docs/ui-redesign-acceptance.md`](docs/ui-redesign-acceptance.md) — the list to check each
  screen on the screen, what was never checked there, and the known limits.

The code of the 25 slices is merged; the last one, U23, converted the remaining sizes to the
scale, removed old code and updated these documents. The owner's check on the screen, with the
acceptance list, comes next. A pull request into `ui-redesign` merges when the Windows jobs pass;
since 2026-11-17 the macOS job is not a gate, because no macOS runner is available to the owner
and another person builds the Mac version.

## Project layout

| Path | What it is |
|------|------------|
| `crates/glasscode` | The app for macOS and Windows (GPUI): UI, sessions, terminals, settings |
| `crates/glasscode_cli` | The `glasscode` command on Windows (`glasscode-cli.exe`), a console program |
| `crates/glasscode_core` | All non-UI logic: agents, git/GitHub, board, sync, CLI; operating system code in `sys/` |
| `crates/glasscoded` | The Linux server daemon that hosts handed-off sessions |
| `relay/` | Optional self-hosted relay, to reach the Mac or a server from any network |
| `glasses/` | The iPhone app that puts your sessions on Meta Ray-Ban Display glasses |
| `voice/` | The voice copilot's control server and Mac helper |
| `vendor/` | Vendored [Zed](https://github.com/zed-industries/zed) GPUI crates (Apache-2.0) |
| `script/` | Build, bundle, and release scripts |
| `docs/` | Architecture notes, the Windows guide, the Windows port plans, and the UI redesign plan, mock and acceptance list |

## Contributing

Contributions are welcome. Please read [CONTRIBUTING.md](CONTRIBUTING.md) for how to build,
test, and send changes, and the house style the codebase follows.

## Use it freely

Glasscode is free for anyone to use, build on, and share — do whatever you like with it.

## License

Glasscode is licensed under the [Apache License 2.0](LICENSE). Third-party components and their licences are listed in [NOTICE](NOTICE).
