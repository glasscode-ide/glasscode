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
with the toolchain. On the `windows-port` branch, `cargo clippy -p glasscode` stops on an old
error in `server_ui.rs`; clippy is not a CI gate there, nor on `ui-redesign`.

## Making changes

1. Branch off `main`. Work for the Windows port branches off `windows-port` and goes back into
   it (see the README).
2. Keep each change focused; a pull request should do one thing.
3. Add or update tests for behaviour you change — most logic lives in `glasscode_core`, which
   is UI-free and fast to test.
4. Make sure `cargo test` passes, and run `cargo clippy` (see above for the branches where it
   stops on old errors).
5. Open a pull request describing what changed and why.

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
  glasscode`), and "Windows installer" builds the installer. From Windows, the macOS job is the
  only macOS test. On `ui-redesign` the macOS job is not a gate since 2026-11-17, and it can be
  cancelled before it runs: a change merges when the Windows jobs pass, so a `#[cfg(unix)]` test
  may not have run.

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

Open an issue in this repository, or reach out to the maintainers.
