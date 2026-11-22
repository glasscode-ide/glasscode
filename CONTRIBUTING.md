# Contributing to Glasscode

Thanks for helping build Glasscode. This guide covers how to get set up and the house style
the codebase follows.

## Getting set up

See the [README](README.md#requirements) for requirements, then:

```sh
cargo run                                           # run the app
cargo test -p glasscode_core -- --test-threads=1    # core tests
cargo test                                          # the app's tests (the default member)
```

On Windows, build the console program too, as `cargo run` builds only the app:

```powershell
cargo build -p glasscode -p glasscode_cli           # the app and the `glasscode` command
cargo run                                           # run the app
cargo test -p glasscode_core -- --test-threads=1    # core tests
cargo test -p glasscode_cli                         # the console program
```

[`docs/windows.md`](docs/windows.md) has the Windows details.

The Rust toolchain is pinned in `rust-toolchain.toml` and installed by rustup automatically.
Please run `cargo fmt` and `cargo clippy` before sending changes — both components are pinned
with the toolchain. Neither is a CI gate yet. In a development build `cargo clippy` stops on an
older error in `glasscode_core` (`update.rs`, a comparison with the build number, which is 0 there);
`GLASSCODE_BUILD=1 cargo clippy` gets past it.

To try a change without real agents or accounts, `script/test-agents/` has stand-ins for
`claude`, `codex` and `gh`, and `script/journeys.sh` drives a debug build through whole journeys
on a scratch home.

## Making changes

1. Branch off `main`.
2. Keep each change focused; a pull request should do one thing.
3. Add or update tests for behaviour you change — most logic lives in `glasscode_core`, which
   is UI-free and fast to test.
4. Make sure `cargo test` passes, and run `cargo clippy`.
5. Add a line under "Unreleased" in [`CHANGELOG.md`](CHANGELOG.md) when a person sees the
   change.
6. Open a pull request describing what changed and why.

## Licence

Glasscode is licensed under the Apache License 2.0. What you contribute is under that licence
too (its section 5); you need the right to give it.

- **No new dependency without the owners' agreement.** Its licence must be one of `about.toml`:
  the release build stops at another one. Nothing under the GPL, the LGPL or the AGPL.
- **Code from elsewhere** comes with a note of its source, its commit and its licence, as
  `vendor/README.md` has for GPUI, and a line in `NOTICE`. Do not copy code from the parts of
  Zed that are under the GPL (its editor, terminal and most of its application): only the
  Apache-2.0 crates in `vendor/` come from there.
- **Icons and fonts** need a licence that lets them be distributed, and its text in the
  repository.

## Code for one operating system

Glasscode builds for macOS and Windows from one source tree.

- Operating system code goes into `glasscode_core::sys` (`crates/glasscode_core/src/sys/`):
  `unix.rs` and `windows.rs` give the same functions, and the rest of the code calls them
  instead of adding its own `cfg` conditions.
- Start external programs with `sys::command`, not `std::process::Command::new`: on Windows it
  finds `.cmd` and `.bat` programs and opens no console window.
- Words that differ between the platforms ("this Mac", "File Explorer") come from
  `crates/glasscode/src/platform.rs`; key bindings and their labels from
  `crates/glasscode/src/keymap.rs`.
- A change for Windows keeps the macOS behavior the same. CI has a macOS job (it checks the app,
  the daemon and the console program, and runs the core tests) and two Windows jobs: "Windows"
  checks the app and runs the core, console program and app tests (`cargo test --locked -p
  glasscode`), and "Windows installer" builds the installer. A Linux job tests the core and the
  server daemon. From Windows, the macOS job is the only macOS test.

## House style

- **Write code that reads like the code around it.** Match the surrounding naming, comment
  density, and idioms rather than introducing a new style.
- Prefer small, well-named functions; keep UI code in `glasscode`, logic in `glasscode_core`.
- Comments explain *why*, not *what*. The existing code favours short, plain-language comments
  on the non-obvious parts.
- Keep user-facing strings clear and free of internal jargon.

## Please do not commit

- **Secrets or credentials** — tokens, API keys, signing keys and certificates (`.pfx`), `.env`
  files.
- **Personal or company-specific data** — real names, emails, internal ticket IDs, private
  repository names, or hostnames. Use neutral placeholders in tests and examples
  (`alex@example.com`, `acme/app`, `ENG-123`).
- **Build output** — `target/`, Xcode `build/` folders, SwiftPM `.build/`, installers
  (`Glasscode-*-setup.exe`) (these are gitignored).

If you spot any of the above already in the tree, please open an issue or a fix.

## Questions

Open an issue in this repository.
