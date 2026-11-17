# Changes to vendored crates

- 2026-10-14: copied from zed-industries/zed @ c403bc6af644cd0ab60c90045ac6dc3436218809.
- Cargo manifests rewritten for a macOS-only build: dev-dependencies, examples, benches and
  non-macOS target sections removed; features that pointed at removed dependencies pruned.
- Source changes since:
  - `gpui_macos/src/window.rs`: `GPUI_NO_KEY_WINDOW` keeps every window from becoming the key
    window (test copies of Glasscode never take the keyboard).
  - `gpui/src/elements/text.rs`: `TextLayout::try_bounds`, the bounds without panicking before
    the text is painted (the Chat view hit-tests selectable text with it).
- 2026-11-15, for Windows: `gpui_windows` copied from the same commit. Its source is unchanged;
  its manifest is rewritten like the others.
- Windows target sections put back, as Zed has them: `gpui` (`windows`, and `windows-manifest`
  turns on `embed-resource` again), `gpui_platform` (`gpui_windows`, and `gpui` with
  `windows-manifest`; `test-support` and `screen-capture` reach `gpui_windows` too), `gpui_util`
  (`which`), `path` (`tendril`) and `util` (`which`, `windows`). Linux and wasm sections stay out.
- `scap` stays out of `gpui` and `gpui_windows`: it is a git crate that brings a second git patch
  and its own macOS and Linux crates, only for the `screen-capture` feature, which Glasscode does
  not use. So `screen-capture` does not build on Windows.
- `gpui/resources/windows/gpui.manifest.xml`: `longPathAware`, as worktree paths can be longer
  than 260 characters.
- `gpui_windows/src/window.rs`, `events.rs`: a `WindowKind::PopUp` window opened without focus
  answers `WM_MOUSEACTIVATE` with `MA_NOACTIVATE`, so a click on a notification does its action
  without taking the keyboard from the program in front (on macOS such a window is a
  non-activating panel). Every other window still activates on a click.
- `gpui_windows/src/platform.rs`: when `run_foreground_task` has used its 10 ms and drains the
  input messages, it first takes the text messages `TranslateMessage` posted for the keys before
  (`WM_CHAR` to `WM_IME_COMPOSITION`, posted ones only, oldest first), before each input message
  and after the last (`drain_in_key_order`, `take_posted_text`). Taking only input let a key that
  acts at once (Enter in the terminal) overtake the characters of the letters typed before it.
  No other posted work runs there. Upstream (main, 2026-11) still takes input only.
