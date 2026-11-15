# Contributing to Glasscode

Thanks for helping build Glasscode. This guide covers how to get set up and the house style
the codebase follows.

## Getting set up

See the [README](README.md#requirements) for requirements, then:

```sh
cargo run                                           # run the app
cargo test -p glasscode_core -- --test-threads=1    # core tests
cargo test                                          # everything
```

The Rust toolchain is pinned in `rust-toolchain.toml` and installed by rustup automatically.
Please run `cargo fmt` and `cargo clippy` before sending changes — both components are pinned
with the toolchain.

## Making changes

1. Branch off `main`.
2. Keep each change focused; a pull request should do one thing.
3. Add or update tests for behaviour you change — most logic lives in `glasscode_core`, which
   is UI-free and fast to test.
4. Make sure `cargo test` and `cargo clippy` pass.
5. Open a pull request describing what changed and why.

## House style

- **Write code that reads like the code around it.** Match the surrounding naming, comment
  density, and idioms rather than introducing a new style.
- Prefer small, well-named functions; keep UI code in `glasscode`, logic in `glasscode_core`.
- Comments explain *why*, not *what*. The existing code favours short, plain-language comments
  on the non-obvious parts.
- Keep user-facing strings clear and free of internal jargon.

## Please do not commit

- **Secrets or credentials** — tokens, API keys, signing keys, `.env` files.
- **Personal or company-specific data** — real names, emails, internal ticket IDs, private
  repository names, or hostnames. Use neutral placeholders in tests and examples
  (`alex@example.com`, `acme/app`, `ENG-123`).
- **Build output** — `target/`, `ios/build/`, SwiftPM `.build/` (these are gitignored).

If you spot any of the above already in the tree, please open an issue or a fix.

## Questions

Open an issue in this repository, or reach out to the maintainers.
