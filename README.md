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

> **Status:** in active development. Contributions are welcome — see
> [CONTRIBUTING.md](CONTRIBUTING.md).

> **This is the `windows-port` branch.** It adds Windows to the macOS app. Release 1 for Windows
> is a test build of the local work (projects, worktrees, shell, Claude and Codex sessions, the
> `glasscode` command, the agent browser). The server, phone pairing and the voice panel are
> **not ported** to Windows, and most of release 1 is not yet verified with real agent
> conversations — see [Windows port](#windows-port-release-1-in-test).

## Highlights

- **Projects → worktrees → sessions** in one sidebar. Start a session on the latest `main`
  (fetched fresh), on a pull request (found by number, title, branch, or author and checked out
  on its own branch), or on an existing worktree.
- **Each session is the real CLI** — `claude`, `codex`, or a plain shell — running in an actual
  terminal, so nothing is simulated or wrapped away.
- **Chat view or Terminal view**, per session. Chat surfaces the agent's questions
  (permissions, trust prompts, menus) as inline buttons; the terminal is always one click away.
- **Today board** — a lightweight planner for "what needs me?": tasks, recurring items, and
  outside items (Linear / GitHub) in one place.
- **Review a pull request** in three steps — Explain → Plan → private review — checked out as
  `review/<n>` and never pushed from.
- **Team-approved CLI versions.** Settings › Updates & CLIs shows each CLI's installed version,
  the newest published to npm, and a one-click **Update** — per person, without changing the
  team's pinned version for everyone else.
- **Run anywhere.** Hand a session to a Linux server daemon (`glasscoded`) and follow or decide
  on it from your iPhone; bring it back to the Mac when you want. (Not on Windows yet.)
- **Self-updating.** Signed releases are verified against a built-in key and installed in place.
  This build has no release server yet, so it does not update itself.

See [`docs/architecture.md`](docs/architecture.md) for the full design.

## Screenshots

_Screenshots live in [`docs/screenshots/`](docs/screenshots/)._

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
| `docs/` | Architecture notes, the Windows guide and the Windows port plans |

## Contributing

Contributions are welcome. Please read [CONTRIBUTING.md](CONTRIBUTING.md) for how to build,
test, and send changes, and the house style the codebase follows.

## Use it freely

Glasscode is free for anyone to use, build on, and share — do whatever you like with it.
