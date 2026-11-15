# Windows acceptance record, release 1

| | |
|---|---|
| Date | 2026-11-15 |
| Branch and commit | `windows-port` at `c2cfdeb` ("fix: text input in the terminal and npm installs on Windows (#18)") |
| Release 1 | All slices except the server slices S14, S15 and S16 (decision P5 of the slice plan) |
| Tests | The 16 acceptance tests of [GC-WIN-001, section 9](windows-port-plan.html#accept) |
| System | Windows 11, 64-bit x86 |

This record says what was checked on Windows for release 1, how, and what was not. **It does not
accept release 1.** Four of the 16 tests passed on the screen; eleven are not verified, mostly
because they need a person with signed-in Claude and Codex accounts, the screen, or an installer
run. The owner decides on the merge of `windows-port` into `main` after the checklist below.

How to build and run on Windows, and the differences from macOS: [`windows.md`](windows.md).

## How it was checked

- **On the screen.** An agent operated a debug build on Windows 11 through synthetic input
  (`SendInput`) and screenshots. These results are real, and are marked "Pass, checked on screen".
- **Automatic tests.** The CI jobs of each pull request and push (macOS, Windows, Windows
  installer) and the local test runs listed under [Automatic tests](#automatic-tests).
- **Experiments in the slice pull requests** (for example the hooks with the real `claude.exe`
  against a local stand-in for the API, in PR 16). They are recorded in the pull requests, but are
  not repeatable tests, and no acceptance test passes on them alone.

Windows 10 was not used. The plan asks for a clean Windows 11 computer and a Windows 10 computer;
the screen checks ran on a development computer.

## Results of the 16 acceptance tests

| Result | Tests |
|---|---|
| Pass, checked on screen | 4 (tests 2, 5, 10, 11) |
| Pass, automatic test | 0 |
| Not verified | 11 (tests 1, 3, 4, 6, 7, 8, 9, 12, 13, 14, 15) |
| Not in release 1 | 1 (test 16) |

| # | Test | Result | Evidence or reason |
|---|---|---|---|
| 1 | Install Glasscode. Start it from the Start menu. | Not verified | No installer was run. The CI job "Windows installer" builds the unsigned installer (artifact `glasscode-windows-installer`, last for the push of `c2cfdeb`), and `script\bundle-windows.ps1 -NoInstaller` ran in S18 (PR 21). Steps: [Check the installer](#check-the-installer). |
| 2 | Add a project that is a git repository. | Pass, checked on screen | A project added, with spaces in its paths. |
| 3 | Start a Claude session on a new worktree. Send a prompt. The activity changes to "working", then "idle". | Not verified | No real Claude conversation in the app. The hooks and the status line were tried with the real `claude.exe` against a local stand-in for the API only (PR 16). Automatic tests cover the start (`crates/glasscode_core/tests/claude_launch.rs`) and `glasscode hook` (`crates/glasscode_cli/tests/command.rs`). Needs a signed-in Claude account. |
| 4 | Start a Codex session. Send a prompt. The chat view shows the reply. | Not verified | No real Codex conversation, and the chat view was not checked on screen. `crates/glasscode_core/tests/codex_discovery.rs` covers finding the conversation and the start from npm. Needs a signed-in Codex account. |
| 5 | Start a shell session. Start `git status`. | Pass, checked on screen | PowerShell in a new worktree with spaces in the paths: `git status`, Tab completion, history, Ctrl+C stops `ping -t`. |
| 6 | Change a session between the chat view and the terminal view. | Not verified | The chat view was not checked on screen. |
| 7 | Paste an image into a Claude session. Claude reads the file. | Not verified | Needs a real Claude conversation. Only the paste of text (Ctrl+Shift+V) was checked on screen. |
| 8 | Stop Glasscode. Start it again. Each session resumes its conversation. | Not verified | Checked on screen: a restart restores the shell sessions, and `exit` then Resume starts the shell again. The resume of a Claude or Codex conversation was not checked. `crates/glasscode_core/tests/history.rs` covers the parser of earlier conversations. |
| 9 | Hand a Claude session to Codex, and a Codex session to Claude. | Not verified | Needs real conversations. `codex_discovery.rs` (`handoff_notes_have_the_diff_and_the_codex_transcript`) covers the notes. |
| 10 | Start `glasscode board add "Test"` in a session. The Today board shows the task. | Pass, checked on screen | `glasscode board add`, `glasscode session list` and `glasscode help` worked from PowerShell, cmd.exe and Git Bash. `command.rs` (`a_command_reaches_the_app_and_prints_its_answer`) covers the command and the pipe. |
| 11 | Start a command that needs approval. Approve it in the notification. | Pass, checked on screen | An approval request from a program shows a card and a notification at the bottom right; Allow works from both. |
| 12 | Open a pull request in the Git tab. The checks and the comments show. | Not verified | Not checked on screen. Needs a repository with a pull request and a signed-in `gh`. |
| 13 | Connect Linear with a key. Stop and start Glasscode. The key is still there. | Not verified | Linear and Credential Manager were not used in the app. The test `sys::secrets::tests::write_read_delete_and_read_again` (Windows CI job) writes, reads and deletes a credential. |
| 14 | Remove a worktree while a program operates in it. Glasscode shows a warning. | Not verified | Not checked on screen. `sys::tests::processes_has_this_test_and_its_parent` covers the process list that the warning uses. On Windows a process of another user, or an elevated one, shows no work folder, so it gives no warning. |
| 15 | Use two accounts of the same agent. Each stays signed in. | Not verified | Needs two signed-in accounts. `crates/glasscode_core/tests/git_and_accounts.rs` covers the copy of a conversation between accounts without credentials. |
| 16 | After phase 6: move a session to the server and back. | Not in release 1 | The server slices S14 and S15 come after release 1. |

No test failed. The defects that are known are under [Known defects and limits](#known-defects-and-limits).

## Checked on screen

On Windows 11, a debug build, through synthetic input:

- Add a project. Start a shell session on a new worktree with spaces in the paths. PowerShell runs;
  `git status`, Tab completion and history work; Ctrl+C stops `ping -t`; `exit`, then Resume; a
  restart of the app restores the sessions.
- Typing: letters, Space, Enter, Shift, Backspace, digits, and Unicode text with an emoji through
  `SendInput`. 78 fast lines with Enter right after a change of session and during heavy output of
  another session: 0 out of order.
- `glasscode board add`, `glasscode session list` and `glasscode help` from PowerShell, cmd.exe
  and Git Bash. An approval request from a program shows a card and a notification at the bottom
  right; Allow works from both.
- Shortcuts: Ctrl+Shift+N, Ctrl+Shift+K, Ctrl+Tab in the terminal, Ctrl+Shift+V paste,
  Ctrl+Shift+L, the Ctrl labels in the palette and in Settings, Alt+F4 with the terminal in focus.
- Title bar: minimize, maximize, restore, drag, double click, the search box and New session
  clickable, the close button.

## Automatic tests

- **CI**, the push of `c2cfdeb` to `windows-port` (run 43768410919): all three jobs passed.
  - macOS: `cargo check --locked` of the app (`glasscode`), the daemon (`glasscoded`) and the
    console program (`glasscode_cli`), and `cargo test --locked -p glasscode_core -- --test-threads=1`.
  - Windows: `cargo check --locked -p glasscode`, the core tests, and
    `cargo test --locked -p glasscode_cli`.
  - Windows installer: `script/bundle-windows.ps1` (release build, unsigned) and the artifact
    `glasscode-windows-installer`.
- **Local, Windows 11**, on this branch (`c2cfdeb` and the one-word fix of this pull request):
  - `cargo check --locked -p glasscode`: passes (the 12 warnings are on the base branch already).
  - `cargo test --locked -p glasscode --bin glasscode`: 53 passed, 1 ignored. No CI job runs the
    app's tests, on either platform.
  - `cargo test --locked -p glasscode_core -- --test-threads=1`: 146 passed and 7 ignored in the
    library; the integration tests 1, 4, 5, 1, 1 and 1 passed.
  - `cargo test --locked -p glasscode_cli`: 5 passed (`crates/glasscode_cli/tests/command.rs`).

The macOS job is the only macOS test: it compiles the app and runs the core tests. It does not run
the app's tests, and nothing opens a window on macOS.

## Not verified: checklist for a person

Each item needs the screen, a signed-in account, or a system that the port did not have. Tick an
item when it works; write a defect with the steps when it does not.

From the acceptance tests and the screen checks:

- [ ] A real Claude conversation in the app: a prompt, "working" then "idle", a question that
      needs you, the permission buttons. (The hooks and the status line were tested only against
      the real `claude.exe` with a stand-in API.)
- [ ] A real Codex conversation in the app, with the trust question of a new worktree.
- [ ] The chat view of both agents, and the change between the chat view and the terminal view.
- [ ] The resume of an agent conversation after a restart (Claude and Codex), and "Resume an
      earlier conversation".
- [ ] Handoff from Claude to Codex and from Codex to Claude.
- [ ] Paste of an image (and of copied files, and a drop of files) into a Claude session.
- [ ] The agent browser with a visible window: start it, sign in to one site, an agent opens a page,
      Show, and the browser stays open when Glasscode stops.
- [ ] File Explorer: reveal a file. The editor opening (VS Code, Cursor or Zed), and a file that
      would run opened with no editor set (it is only shown in File Explorer).
- [ ] The two sounds (a session needs you, a session finished).
- [ ] Notification stacking: several at once, at the bottom right.
- [ ] The notification focus fix (PR 13): with Notepad in front, a click on Allow, Open or Dismiss
      does its action and Notepad keeps the keyboard.
- [ ] IME (for example Japanese), dead keys (US-International), AltGr (German layout), the emoji
      panel (Win+.) and dictation (Win+H) in the terminal.
- [ ] The Themes page and the fonts (Segoe UI, Cascadia Mono or Consolas, Georgia), and the Auto
      theme when Windows changes between light and dark.
- [ ] The words on every page after S11: no "Mac", "Finder", "Keychain" or ⌘ on Windows.
- [ ] Linear in the app, and the key in Credential Manager after a restart.
- [ ] Settings › Updates & CLIs: a version update of an agent and a real npm install, also with the
      execution policy Restricted.
- [ ] A pull request in the Git tab: checks, comments, review threads.
- [ ] Remove a worktree while a program runs in it: the warning.
- [ ] Two accounts of the same agent stay signed in.
- [ ] The installer: install, Start menu, "Installed apps", uninstall, upgrade (see
      [Check the installer](#check-the-installer)).
- [ ] A signed build (no certificate exists).
- [ ] PowerShell 7 as the shell of the sessions.
- [ ] Windows 10.
- [ ] A second display, and other scale factors (100%, 200%).
- [ ] macOS: every behavior beyond what the macOS CI job compiles and tests.

Also not verified, from the slice pull requests:

- [ ] A release build opens no console window when it starts `git` or `gh` (PR 21; the subsystem
      of the program is checked).
- [ ] The request files when a Codex sandbox may not open the control pipe (PR 10, PR 17).
- [ ] The control pipe of another account, and an app that runs as administrator (PR 10).
- [ ] Claude Code without Git Bash, where PowerShell runs `glasscode hook` (PR 16).
- [ ] An agent whose `node.exe` or `pwsh.exe` is below `C:\Program Files` (a program path with a
      space, PR 18).
- [ ] Win+Arrow, snap layouts and the minimum window size (PR 8).
- [ ] Touch and pen input on a notification (PR 13).
- [ ] `update::windows_app::relaunch_after_exit` and a real update: there is no release server
      (PR 21).

## Known defects and limits

No acceptance test failed, so no defect comes from a test. These are known from the port:

| Defect or limit | Where | Owner |
|---|---|---|
| Output of the `glasscode` command shows wrong characters for curly quotes when it is piped in Windows PowerShell 5.1: PowerShell reads the UTF-8 output with the OEM code page. | The console program | S05 |
| Settings › General said a notice shows "at the top right"; on Windows it is at the bottom right. **Corrected in this pull request** (`platform::notice_corner`). | `settings_ui.rs` | S11 |
| "Could not fetch origin (offline?)" shows for a repository with no remote. The same code runs on the Mac. | `git::start_base` and its callers | Not a port slice |
| Errors are logged at each window close ("window not found", "Invalid window handle"). Nothing crashes. | The window code and `vendor/gpui_windows` | S09 |
| A command that the person types in a Glasscode shell session is not asked for approval: it counts as the person. This is the design, the same as on the Mac. | `control_ui.rs` | None (design) |
| Pasted file paths are quoted for PowerShell; in cmd.exe a typed path with `&` or `%NAME%` is not safe. | `terminal/files.rs` | S10 |
| macOS behavior change of the port: since S10, only http, https and mailto links from a conversation open (mailto without a subject or body); file paths and other schemes no longer open. Made on purpose, for safety. | `sys::open_url` | S10 |
| With no editor set, a file that would run when opened is only shown in File Explorer. | `sys::open_path` | S10 (design) |
| Codex Update installs the npm package even when another `codex.exe` is first on the PATH, so the version shown can stay the same. | `versions.rs` | S13 |
| Under `cargo run` the agent browser ends when cargo ends; start `target\debug\glasscode.exe` directly. | `sys::spawn_detached` | S17 |
| `cargo run` builds only the window program; run `cargo build -p glasscode -p glasscode_cli` first. | The build | S05 |
| `cargo clippy -p glasscode` stops on an old error in `server_ui.rs` (line 141). Clippy is not a CI gate. | `server_ui.rs` | Not a port slice |
| The vendor changes since the copy was taken are in `vendor/CHANGES.md`. Upstream Zed has the same key-order loop in `run_foreground_task`, so that change must be carried along at a vendor update. | `vendor/gpui_windows` | PR 18 |

Not ported in release 1 (see [`windows.md`](windows.md#what-is-not-ported)):

- The server connection, sync and move, and phone pairing (S14, S15, S16). Open notes for them:
  `server_ui::binaries_dir` does not look in `{app}\glasscoded` on Windows, the installer from CI
  has no daemon builds, and two errors in `crates/glasscode_core/src/sync.rs` still say "this Mac".
- The voice panel: hidden on Windows (`glasscode_ui::VOICE`).
- The self-hosted relay: not configured in this build (`relay-connect`), as on the Mac.
- The installer is unsigned; Windows SmartScreen warns. No release is published.

## Windows stubs

The places where Windows code compiles but does nothing, or refuses, in release 1:

| Place | On Windows |
|---|---|
| `crates/glasscode/src/glasscode_ui.rs`, `VOICE` | `false`: no Glasscode tab, no voice settings rows, no voice helper. |
| `crates/glasscode/src/server_ui.rs`, `binaries_dir` | No Windows place: it does not find the daemon builds of an installed Glasscode. |
| `crates/glasscoded/src/main.rs` | Only a `main` that prints "glasscoded operates on Linux". |
| `crates/glasscode_core/src/update.rs`, `windows_app::stage` | "auto-update is not configured in this build", as `mac_app::stage` on the Mac; `update::newer` has no server. |
| `crates/glasscode_core/src/cli.rs`, `run_mode("relay-connect")` | "a relay is not configured in this build", exit code 255, as on the Mac. |
| `crates/glasscode_core/src/sys/windows.rs`, `control::prepare_socket_dir` | Nothing to do: a pipe has no folder; its own security keeps others out. |
| `crates/glasscode_core/src/paths.rs`, `ensure_dirs`, and `crates/glasscode/src/terminal/files.rs`, `save_image` | The Unix file modes (0700) for the accounts folder and pasted images are not set; the folders keep the permissions of the user profile. |

## Check the installer

From the demonstration of S18 (PR 21). Use Windows Sandbox or a clean user account, best a
computer without Rust.

1. Download the artifact `glasscode-windows-installer` of the newest "Windows installer" job (a
   push to `windows-port`, or Actions › CI › Run workflow) and unzip it:
   `Glasscode-0.1.0-setup.exe`.
2. Start it. SmartScreen warns, because it is unsigned: "More info", "Run anyway". No
   administrator prompt appears. Finish with "Launch Glasscode".
3. Close Glasscode, then start it from the Start menu entry "Glasscode". The window and the taskbar
   show the glasses icon. No console window opens, also not when a session runs git or gh.
4. Right-click `%LOCALAPPDATA%\Programs\Glasscode\Glasscode.exe` › Properties › Details: the icon,
   "Glasscode", file version 0.1.0.0, product version "0.1.0 (<build>)".
5. Start a shell session and type `glasscode help`.
6. Settings › Apps › Installed apps: "Glasscode" with version 0.1.0. Uninstall it.
   `%LOCALAPPDATA%\Programs\Glasscode` is gone; `%USERPROFILE%\.glasscode` stays.
7. Upgrade: install, start Glasscode, then run the same (or a newer) installer again. It asks to
   close Glasscode, replaces the files, and the sessions and settings stay.
