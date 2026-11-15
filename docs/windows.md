# Glasscode on Windows

The Windows port lives on the `windows-port` branch. Release 1 is a test build for local work:
projects, worktrees, shell, Claude and Codex sessions, the `glasscode` command and the agent
browser. The server daemon, session sync and move, phone pairing and the voice panel are **not
ported** (see [What is not ported](#what-is-not-ported)). What was checked on Windows, and what
was not, is in [`windows-acceptance.md`](windows-acceptance.md).

The plan and the slices of the port: [`windows-port-plan.html`](windows-port-plan.html) and
[`windows-port-slices.html`](windows-port-slices.html).

## Requirements

- Windows 10 version 1809 or newer, or Windows 11, 64-bit x86. Only Windows 11 was tested.
- [Rust](https://rustup.rs/) with the MSVC toolchain. The exact version is pinned in
  `rust-toolchain.toml` and installed by rustup automatically.
- The Visual Studio Build Tools with the "Desktop development with C++" workload and a Windows
  SDK. The build embeds resources with `rc.exe`; a release build also compiles the GPUI shaders
  with `fxc.exe` (see [`vendor/README.md`](../vendor/README.md)).
- Git for Windows.
- A `claude` and/or `codex` CLI (npm or their own installers) for agent sessions, and `gh` for
  pull requests.
- For the installer only: [Inno Setup](https://jrsoftware.org/isinfo.php) 6.3 or later.

## Build and run

```powershell
# Build both programs: the app and the console program behind the `glasscode` command
cargo build -p glasscode -p glasscode_cli

# Run the app (a debug build keeps a console window for its log)
cargo run
```

`cargo run` builds only the app. Without `glasscode-cli.exe` next to it, the app logs
`could not install the glasscode command` and sessions have no `glasscode` command, so build both
programs first. To try the agent browser, start `target\debug\glasscode.exe` directly: under
`cargo run` the browser ends when cargo ends.

`GLASSCODE_HOME` moves all of Glasscode's data to another folder, which is the easy way to try a
build without touching your own sessions:

```powershell
$env:GLASSCODE_HOME = "$env:TEMP\glasscode-try"; .\target\debug\glasscode.exe
```

The checks and tests that the Windows CI job runs:

```powershell
cargo check --locked -p glasscode
cargo test --locked -p glasscode_core -- --test-threads=1
cargo test --locked -p glasscode_cli
```

The app's own tests (`cargo test --locked -p glasscode --bin glasscode`) pass on Windows, but no
CI job runs them. `cargo clippy -p glasscode` stops on an old error in `server_ui.rs` (line 141);
clippy is not a CI gate.

## The installer

```powershell
# The release programs, the folder target\release\Glasscode and target\release\Glasscode-<version>-setup.exe
powershell -NoProfile -ExecutionPolicy Bypass -File script\bundle-windows.ps1

# The programs and the folder only (no Inno Setup needed)
powershell -NoProfile -ExecutionPolicy Bypass -File script\bundle-windows.ps1 -NoInstaller
```

The installer is made with Inno Setup (`script/windows/installer.iss`). It installs for the
current user, without administrator rights, into `%LOCALAPPDATA%\Programs\Glasscode`, with a Start
menu entry and an entry in "Installed apps". Uninstall removes the program folder and leaves
`%USERPROFILE%\.glasscode`; an upgrade keeps it. The installer does not change the PATH. This is
how `installer.iss` is written: no installer has been run on a real computer yet, and the steps to
check it are in [`windows-acceptance.md`](windows-acceptance.md#check-the-installer).

**The installer is unsigned.** No code signing certificate exists yet, so Windows SmartScreen
warns ("Windows protected your PC") before it runs: choose "More info", then "Run anyway". The
script signs the programs and the installer when `SIGN_CERT` is set (see the top of
`script/bundle-windows.ps1`); that path has not been used.

The CI job "Windows installer" builds the unsigned installer on each push to `windows-port` and
keeps it as the workflow artifact `glasscode-windows-installer` for 30 days. No release is
published, and Glasscode does not update itself: as on the Mac, there is no release server
(`update::windows_app::stage` says "auto-update is not configured in this build"). A newer build
goes in by running its installer over the old one.

## Two programs

| Program | Crate | What it is |
|---|---|---|
| `glasscode.exe` (`Glasscode.exe` when installed) | `crates/glasscode` | The app, a window program. A release build opens no console window. |
| `glasscode-cli.exe` | `crates/glasscode_cli` | The console program. A window program cannot write to a console, so the `glasscode` command is a program of its own. |

At each start the app copies `glasscode-cli.exe` from its own folder to
`%USERPROFILE%\.glasscode\bin\glasscode.exe` and puts that folder first on the PATH of each
session, so `glasscode` works in PowerShell, cmd.exe and Git Bash inside a session. Outside
Glasscode's sessions it is not on the PATH. On the Mac the app's own binary does this work
(`Glasscode cli`, through `~/.local/bin/glasscode`).

Besides the commands of `glasscode help`, the console program has the words that other programs
run: `glasscode hook` and `glasscode statusline` for Claude Code (below), and `browser-mcp`, the
agent browser's MCP server. `relay-connect` exits with "a relay is not configured in this build".

### The control channel

The `glasscode` command reaches the app through a named pipe,
`\\.\pipe\glasscode-control-<id>`, where `<id>` is made from the user's security identifier (SID)
and the Glasscode folder, so a test copy with its own `GLASSCODE_HOME` has its own pipe. The pipe
lets in only this user and only from this computer (`PIPE_REJECT_REMOTE_CLIENTS`), and the app
learns the process of each command (`GetNamedPipeClientProcessId`). Before it sends a byte, the
command checks that the pipe belongs to its own account: another account could make a pipe of the
same name first. When the system denies the pipe (a sandbox), the command uses the request files
in `<worktree>\.glasscode\cli\`, as on the Mac; on Windows this fallback has not been tried with a
real sandbox. The code is `sys::control` in `crates/glasscode_core/src/sys/windows.rs`.

### Claude hooks and the status line

On the Mac, Claude's hooks and status line are shell commands. On Windows each Claude session
gets its settings in a file, `%USERPROFILE%\.glasscode\settings\<session>.json`, passed with
`claude --settings <file>`, and the commands are `glasscode hook` and `glasscode statusline`:

- `glasscode hook` appends what the hook gets on stdin, and a line end, to the session's events
  file (`GLASSCODE_EVENTS`). It prints nothing.
- `glasscode statusline` keeps the input for Usage (`GLASSCODE_LIMITS`), then runs the person's
  own status line command (`GLASSCODE_STATUSLINE`) with the same input.

They are plain commands, the same for Git Bash, which Claude Code prefers, and PowerShell, which it
uses when there is no Git Bash. They were tried with the real `claude.exe` (against a stand-in for
its API) and Git Bash; the PowerShell case and a real conversation are not yet checked.

## Where things are

| What | Where |
|---|---|
| Glasscode's data (`GLASSCODE_HOME` replaces it) | `%USERPROFILE%\.glasscode` |
| State, board, settings files | `state.json`, `board.json`, `cli-versions.json`, `clis.json`, ... in that folder |
| New worktrees | `.glasscode\worktrees\<project>\<branch>` |
| Agent accounts | `.glasscode\accounts` |
| Claude hook events of each session | `.glasscode\events\<session>.jsonl` |
| Claude settings of each session (Windows only) | `.glasscode\settings\<session>.json` |
| Handoff notes | `.glasscode\handoff` |
| The `glasscode` command | `.glasscode\bin\glasscode.exe` |
| The agent browser's profile and MCP config | `.glasscode\browser` |
| The Linear key | Windows Credential Manager, generic credential `Glasscode Linear/api-key` |
| The installed programs | `%LOCALAPPDATA%\Programs\Glasscode` |

## Shortcuts

macOS keeps its ⌘ shortcuts. On Windows the app's own shortcuts use Ctrl+Shift, because Ctrl with
a letter (Ctrl+C, Ctrl+N, Ctrl+W...) belongs to the program in the terminal. Text fields keep the
usual Windows keys. The tables come from `crates/glasscode/src/keymap.rs`, which has every binding
of both platforms; the labels in the app (the palette, Settings) come from the same table.

Anywhere in the window:

| Action | macOS | Windows |
|---|---|---|
| New session | ⌘N, ⌘T | Ctrl+Shift+N, Ctrl+Shift+T |
| Close session | ⌘W | Ctrl+Shift+W |
| Next session | ⌃Tab, ⌘⇧] | Ctrl+Tab, Ctrl+PageDown |
| Previous session | ⌃⇧Tab, ⌘⇧[ | Ctrl+Shift+Tab, Ctrl+PageUp |
| Settings (at Accounts) | ⌘, | Ctrl+, |
| Add a project | ⌘O | Ctrl+Shift+O |
| Handoff | ⌘⇧H | Ctrl+Alt+H |
| Show or hide the side panel | ⌘B | Ctrl+Shift+B |
| Command palette | ⌘K | Ctrl+Shift+K |
| Quit | ⌘Q | Ctrl+Shift+Q, Alt+F4 |

Alt+F4 is not a binding: Windows closes the window, and closing the main window quits.

In the terminal:

| Action | macOS | Windows |
|---|---|---|
| Copy | ⌘C | Ctrl+Shift+C |
| Paste | ⌘V | Ctrl+Shift+V |
| Clear the scrollback | ⌘L | Ctrl+Shift+L |

Every other Ctrl key goes to the program in the terminal (Ctrl+C stops it). On Windows a
Ctrl+Shift+letter that has no binding does nothing in the terminal.

In the Chat view and the message box:

| Action | macOS | Windows |
|---|---|---|
| Copy the selection (Chat view) | ⌘C | Ctrl+C |
| Change the mode | ⇧Tab | Shift+Tab |
| Send | Enter | Enter |
| Send now | ⌘Enter | Ctrl+Enter |
| New line | ⇧Enter, ⌥Enter | Shift+Enter, Alt+Enter |
| Word left, word right | ⌥Left, ⌥Right | Ctrl+Left, Ctrl+Right |
| Delete the word to the left | ⌥Backspace | Ctrl+Backspace |
| Delete to the start | ⌘Backspace | Ctrl+Shift+Backspace |
| Home, End | Home, End, ⌘Left, ⌘Right, ⌘Up, ⌘Down | Home, End, Ctrl+Home, Ctrl+End |
| Select all, copy, cut, paste | ⌘A, ⌘C, ⌘X, ⌘V | Ctrl+A, Ctrl+C, Ctrl+X, Ctrl+V |

In one-line text fields and text areas:

| Action | macOS | Windows |
|---|---|---|
| Word left, word right (one line) | ⌥Left, ⌥Right | Ctrl+Left, Ctrl+Right |
| Delete the word to the left (one line) | ⌥Backspace | Ctrl+Backspace |
| Home, End (one line) | Home, End, ⌘Left, ⌘Right | Home, End |
| Home, End (text area) | ⌘Left, ⌘Right | Home, End |
| Send (text area; Enter is a new line there) | ⌘Enter | Ctrl+Enter |
| Select all, copy, cut, paste | ⌘A, ⌘C, ⌘X, ⌘V | Ctrl+A, Ctrl+C, Ctrl+X, Ctrl+V |

Backspace, Delete, the arrows, Shift with an arrow, Enter and Escape are the same on both.

## Other differences from macOS

- **Shell.** Shell sessions and one-off commands use PowerShell 7 (`pwsh.exe`) when it is on the
  PATH, else Windows PowerShell, by its full path. The screen checks used Windows PowerShell 5.1;
  PowerShell 7 ran only in the tests of the Windows CI job, whose runner has it.
- **Title bar.** Glasscode draws the minimize, maximize and close buttons at the right of its own
  title bar; there is no menu bar.
- **Notifications** stand at the bottom right, above the taskbar. They open without the keyboard
  focus, and a click on one is meant not to take the keyboard from the program in front
  (`vendor/CHANGES.md`; not yet checked on screen). The sounds are the Windows sound events
  "Notification.Reminder" (a session needs you) and "Notification.Default" (a session finished).
- **Files and editors.** Folders open in File Explorer, and "reveal" selects the file there. With
  no editor set, a file that Windows would run when opened (a program, a script, a shortcut) is
  only shown in File Explorer, never started.
- **Pasted and dropped file paths** are typed as PowerShell reads them: a path with a space or
  another sign gets single quotation marks.
- **Words and fonts.** The app says "this PC", "File Explorer" and "Windows Credential Manager"
  (`crates/glasscode/src/platform.rs`). The system fonts are Segoe UI, Cascadia Mono (or Consolas
  where Windows has no Cascadia Mono) and Georgia.
- **Agent browser.** It is Google Chrome, else Microsoft Edge, then Brave or Chromium, started as a
  separate instance with its own profile in `.glasscode\browser`.

## Operating system code

Operating system code goes into `glasscode_core::sys` (`crates/glasscode_core/src/sys/`):
`mod.rs` has what both platforms share, `unix.rs` and `windows.rs` give the same names for each
platform. The rest of the core calls `sys` and keeps no platform conditions of its own. External
programs start with `sys::command`, which on Windows also finds `.cmd` and `.bat` files on the PATH
and opens no console window. The words people read that differ between the platforms come from
`crates/glasscode/src/platform.rs`, the key bindings from `crates/glasscode/src/keymap.rs`. A change
for Windows keeps macOS behavior the same; Windows-only code is behind `#[cfg(windows)]` or in
`sys/windows.rs`.

## What is not ported

- **The server** (`glasscoded`), and with it moving and syncing sessions and phone pairing: slices
  S14, S15 and S16 come after release 1. The server code builds on Windows, but it is not ported or
  tested there. Known gaps: `server_ui::binaries_dir` does not look in the installed
  `glasscoded` folder (`%LOCALAPPDATA%\Programs\Glasscode\glasscoded`), the installer that CI builds
  has no daemon builds (they need Docker), and two errors in `crates/glasscode_core/src/sync.rs`
  still say "this Mac".
- **The voice panel** (the Glasscode tab, its settings rows and the voice helper) is hidden on
  Windows: the helper is a macOS program. The microphone button of the Chat view stays: it is
  Claude's own `/voice`.
- **The self-hosted relay** is not configured in this build (the same on the Mac).
- **Signed builds, a release server and an ARM64 build.**

## Known limits

- Output of the `glasscode` command shows wrong characters for curly quotes when it is piped in
  Windows PowerShell 5.1: PowerShell reads the UTF-8 output with the OEM code page.
- "Could not fetch origin (offline?)" shows for a repository with no remote.
- Errors are logged at each window close ("window not found", "Invalid window handle"). Nothing
  crashes.
- A command that the person types in a Glasscode shell session is not asked for approval: it
  counts as the person (the same design as on the Mac).
- Pasted file paths are quoted for PowerShell. In cmd.exe a typed path with `&` or `%NAME%` is not
  safe.
- Only http, https and mailto links open from a conversation, and a mailto link keeps only its
  addresses (no subject or body). File paths and other schemes do not open. This is Windows only,
  made for safety (slice S10): the Mac opens every link with `open` as before the port, file paths
  and other schemes too.
- With no editor set, a file that would run when opened is only shown in File Explorer.
- Codex Update installs the npm package even when another `codex.exe` (for example OpenAI's own
  installer) is first on the PATH; then the version shown can stay the same.
- The version and install commands of Settings › Updates & CLIs run in PowerShell without the
  person's profile (`-NoProfile`), so PATH additions from a PowerShell profile are not there.
- Under `cargo run` the agent browser ends when cargo ends: start `target\debug\glasscode.exe`
  directly.
- `cargo run` builds only the window program: run `cargo build -p glasscode -p glasscode_cli`
  first.
- `cargo clippy -p glasscode` stops on an old error in `server_ui.rs` (line 141). Clippy is not a
  CI gate.
- The changes to the vendored GPUI for Windows are in [`vendor/CHANGES.md`](../vendor/CHANGES.md).
  Upstream Zed has the same key-order loop in `run_foreground_task`, so that change must be
  carried along at a vendor update.
