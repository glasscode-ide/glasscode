# Vendored GPUI

Apache-2.0 crates copied from https://github.com/zed-industries/zed at commit
`c403bc6af644cd0ab60c90045ac6dc3436218809`.

Glasscode does not track upstream Zed. Changes here are ours; note them in
`vendor/CHANGES.md` so a later re-sync (if ever wanted) stays possible.

Crates: bench_metrics, collections, derive_refineable, gpui, gpui_apple, gpui_macos, gpui_macros, gpui_platform, gpui_shared_string, gpui_util, gpui_windows, http_client, path, perf, refineable, scheduler, sum_tree, util, util_macros, zlog, ztracing, ztracing_macro

`gpui_windows` (the Windows platform of GPUI) came later from the same commit. On Windows,
`gpui`'s build script embeds `resources/windows/gpui.manifest.xml` with `rc.exe`, and a release
build of `gpui_windows` compiles its shaders with `fxc.exe` (or the one `GPUI_FXC_PATH` names);
both come with the Windows SDK. A debug build compiles the shaders when it starts.
