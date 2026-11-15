<div align="center">

<img src="docs/logo.png" alt="Glasscode" width="120" height="120" />

# Glasscode

**A terminal-first macOS workspace for running Claude Code and Codex agents side by side —
each in its own git worktree, on the subscription account you choose.**

</div>

---

Glasscode is a native macOS app (built on [GPUI](https://www.gpui.rs/)) for people who run
several coding agents at once. Every piece of work is a git worktree with its own agent
session; you can watch them as a clean **chat** or drop into the **raw terminal** at any time.
It keeps your approved CLI versions in sync, and can hand a long-running session off to a Linux
server (and your iPhone) and bring it back.

> **Status:** in active development. Contributions are welcome — see
> [CONTRIBUTING.md](CONTRIBUTING.md).

> **This is the `windows-port` branch.** Glasscode runs on macOS only today. This branch adds
> Windows support slice by slice and is **not usable on Windows yet** — see
> [Windows port](#windows-port-in-progress) for what works so far.

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
  on it from your iPhone; bring it back to the Mac when you want.
- **Self-updating.** Signed releases are verified against a built-in key and installed in place.

See [`docs/architecture.md`](docs/architecture.md) for the full design.

## Screenshots

_Screenshots live in [`docs/screenshots/`](docs/screenshots/)._

## Requirements

- macOS 12 or newer (Apple Silicon or Intel)
- [Rust](https://rustup.rs/) — the exact toolchain is pinned in `rust-toolchain.toml` and
  installed by rustup automatically
- Xcode **Command Line Tools** (`xcode-select --install`) — full Xcode is not required for the
  Mac app
- `git`
- A `claude` and/or `codex` CLI on your `PATH` (Glasscode can install and keep these current)

## Build and run

```sh
# Development build — runs the app
cargo run

# Run the test suites
cargo test -p glasscode_core -- --test-threads=1   # core logic
cargo test                                          # everything

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

## Windows port (in progress)

The port from macOS to Windows happens on this branch, `windows-port`. The `main` branch does
not change until the port is complete. The work is divided into 19 slices; each one adds one
function that a person or a test can see on Windows, and lands as one pull request into this
branch.

- [`docs/windows-port-plan.html`](docs/windows-port-plan.html) — the findings and the design
  decisions (GC-WIN-001).
- [`docs/windows-port-slices.html`](docs/windows-port-slices.html) — the 19 slices, their
  sequence, and the rules for each pull request (GC-WIN-002).

Both are HTML pages: open them in a browser from a local checkout.

### Status

| Slice | Result on Windows | Status |
|-------|-------------------|--------|
| S01 | The core library builds and its tests pass. CI runs on macOS and Windows. | Merged |
| S02 | `cargo run` shows the main window. | In work |
| S03 | The core finds and starts programs correctly. | In work |
| S04 – S19 | Sessions, the `glasscode` command, keyboard, title bar, agents, server connection, installer, documents. | Not started |

### What works on Windows today

Only the core library. The app itself does not build on Windows until slice S02 merges, so
`cargo run` and a plain `cargo test` fail there.

```sh
cargo test -p glasscode_core -- --test-threads=1
```

You need Windows 10 version 1809 or newer (64-bit x86), [Rust](https://rustup.rs/) with the
MSVC toolchain (the Visual Studio Build Tools with the "Desktop development with C++"
workload), and `git`.

### Rules for a change on this branch

- macOS behavior stays the same. Windows code goes behind `#[cfg(windows)]` or into
  `crates/glasscode_core/src/sys/windows.rs`.
- Pull requests target `windows-port`, not `main`, and merge only when both CI jobs (macOS and
  Windows) pass.

## Project layout

| Path | What it is |
|------|------------|
| `crates/glasscode` | The macOS app (GPUI): UI, sessions, terminals, settings |
| `crates/glasscode_core` | All non-UI logic: agents, git/GitHub, board, sync, CLI |
| `crates/glasscoded` | The Linux server daemon that hosts handed-off sessions |
| `relay/` | Optional self-hosted relay, to reach the Mac or a server from any network |
| `glasses/` | The iPhone app that puts your sessions on Meta Ray-Ban Display glasses |
| `voice/` | The voice copilot's control server and Mac helper |
| `vendor/` | Vendored [Zed](https://github.com/zed-industries/zed) GPUI crates (Apache-2.0) |
| `script/` | Build, bundle, and release scripts |
| `docs/` | Architecture notes |

## Contributing

Contributions are welcome. Please read [CONTRIBUTING.md](CONTRIBUTING.md) for how to build,
test, and send changes, and the house style the codebase follows.

## Use it freely

Glasscode is free for anyone to use, build on, and share — do whatever you like with it.
