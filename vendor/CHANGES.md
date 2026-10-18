# Changes to vendored crates

- 2026-10-14: copied from zed-industries/zed @ c403bc6af644cd0ab60c90045ac6dc3436218809.
- Cargo manifests rewritten for a macOS-only build: dev-dependencies, examples, benches and
  non-macOS target sections removed; features that pointed at removed dependencies pruned.
- Source changes since:
  - `gpui_macos/src/window.rs`: `GPUI_NO_KEY_WINDOW` keeps every window from becoming the key
    window (test copies of Glasscode never take the keyboard).
  - `gpui/src/elements/text.rs`: `TextLayout::try_bounds`, the bounds without panicking before
    the text is painted (the Chat view hit-tests selectable text with it).
