# Glasscode on Windows

The Windows port lives on the `windows-port` branch. Release 1 is a test build for local work:
projects, worktrees, shell, Claude and Codex sessions, the `glasscode` command and the agent
browser. The server daemon, session sync and move, phone pairing and voice are **not ported**
(see [What is not ported](#what-is-not-ported)). What was checked on Windows, and what was not,
is in [`windows-acceptance.md`](windows-acceptance.md).

The plan and the slices of the port: [`windows-port-plan.html`](windows-port-plan.html) and
[`windows-port-slices.html`](windows-port-slices.html). The new interface of the branch
`ui-redesign`, and how to check it on Windows: [`ui-redesign-acceptance.md`](ui-redesign-acceptance.md).

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

A check of the new interface needs more than `GLASSCODE_HOME`: the accounts of its own, the `gh`
login and the Linear key also come from elsewhere. In a debug build, `GLASSCODE_DEBUG_NO_AGENTS=1`
keeps Claude and Codex from starting for a session (shells start).
[`ui-redesign-acceptance.md`](ui-redesign-acceptance.md#start-a-safe-check) says how to start a
check, and lists the debug hooks of a debug build.

The checks and tests that the Windows CI job runs:

```powershell
cargo check --locked -p glasscode
cargo test --locked -p glasscode_core -- --test-threads=1
cargo test --locked -p glasscode_cli
cargo test --locked -p glasscode
```

The last line is the app's own tests (colours and their contrast, icons, key tables, the layout
functions of the screens); the Windows job runs them since slice U01 of the UI redesign. The
macOS job does not. `cargo clippy -p glasscode` stops on an old error in `server_ui.rs`
(line 141); clippy is not a CI gate.

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

## The application icon

`crates/glasscode/resources/Glasscode.ico` (which `build.rs` puts into `glasscode.exe`) and
`assets/glasscode-icon-1024.png` are made from `assets/glasscode-icon.svg`, with a transparent
background. After a change to the SVG, run
`powershell -NoProfile -ExecutionPolicy Bypass -File script\windows\make-icon.ps1` and commit the
three files. The script needs Windows PowerShell 5.1 (System.Drawing, part of Windows) and
Microsoft Edge or Google Chrome, which draws the SVG without a window; `ICON_BROWSER` names another
Chromium browser. It ends with a check that each corner pixel of the PNG and of each image in the
ICO is transparent and each centre pixel is opaque; `-Check` does only the check and needs no
browser. It does not make `Glasscode.icns`, the macOS icon.

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

Every hook is `glasscode hook --stamp` (each payload written in one go,
stamped, with a timeout) and `SessionEnd` reports too; Claude's `StopFailure` reports as well when
the Claude on the session's PATH is at least the manager's minimum version
(`versions::MIN_CLAUDE`). The memory brief's hook is `glasscode memory brief --hook`, the same
console program. None of this is checked on a Windows machine yet: the plan's runs of P03 and P05
on the owner's Windows machine are still to come.

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
| Sessions started and pull requests merged each day (Today's heat map) | `.glasscode\activity.json` |
| Usage limits that Claude's status line reported | `.glasscode\usage\limits` |
| Copies of unsaved editor text, when a quit could not ask | `.glasscode\unsaved` |
| The `glasscode` command | `.glasscode\bin\glasscode.exe` |
| The agent browser's profile and MCP config | `.glasscode\browser` |
| The Linear key | Windows Credential Manager, generic credential `Glasscode Linear/api-key` |
| The manager's feature switches | `.glasscode\authority\engine.json` |
| The journal, a file per UTC day (30 kept) | `.glasscode\journal` |
| The project memory (a git repository) | `.glasscode\memory` |
| The effect ledger | `.glasscode\effects` |
| The manager's brain's folder, the only one it runs in | `.glasscode\manager\work` |
| The installed programs | `%LOCALAPPDATA%\Programs\Glasscode` |

## Shortcuts

macOS keeps its ⌘ shortcuts. On Windows the app's own shortcuts use Ctrl+Shift, because Ctrl with
a letter (Ctrl+C, Ctrl+N, Ctrl+W...) belongs to the program in the terminal. Text fields keep the
usual Windows keys. The tables come from `crates/glasscode/src/keymap.rs`, which has every binding
of both platforms; the labels in the app (Jump, the title bar, the left sidebar, Settings ›
Keyboard) come from the same table.

Anywhere in the window:

| Action | macOS | Windows |
|---|---|---|
| New session | ⌘N, ⌘T | Ctrl+Shift+N, Ctrl+Shift+T |
| Close session | ⌘W | Ctrl+Shift+W |
| Next session | ⌃Tab, ⌘⇧] | Ctrl+Tab, Ctrl+PageDown |
| Previous session | ⌃⇧Tab, ⌘⇧[ | Ctrl+Shift+Tab, Ctrl+PageUp |
| Settings | ⌘⇧S | Ctrl+Shift+S |
| Settings at Agents and accounts | ⌘, | Ctrl+, |
| Back | ⌘[ | Ctrl+< (Ctrl+Shift+, on a US keyboard) |
| Forward | ⌘] | Ctrl+> (Ctrl+Shift+. on a US keyboard) |
| Add a project | ⌘O | Ctrl+Shift+O |
| Handoff | ⌘⇧H | Ctrl+Alt+H |
| Show or hide the left sidebar | ⌘⇧E | Ctrl+Shift+E |
| Show or hide the side panel (the right panel) | ⌘B | Ctrl+Shift+B |
| Jump (search and commands) | ⌘K | Ctrl+Shift+K |
| Quit | ⌘Q | Ctrl+Shift+Q, Alt+F4 |

Alt+F4 is not a binding: Windows closes the window, and closing the main window quits.

Back and Forward on Windows are the keys `<` and `>` with Ctrl. Windows reports Shift with a
punctuation key as the character it types, so on a US keyboard Ctrl+Shift+, and Ctrl+Shift+. are
Ctrl+< and Ctrl+>. On a keyboard with a `<` key of its own (German, French), it is Ctrl+< and
Ctrl+Shift+<. Alt+Left and Alt+Right stay with the program in the terminal.

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

In the editor of a file tab (after Edit):

| Action | macOS | Windows |
|---|---|---|
| Save | ⌘S | Ctrl+S |
| Undo | ⌘Z | Ctrl+Z |
| Redo | ⌘⇧Z | Ctrl+Y, Ctrl+Shift+Z |
| Select all, copy, cut, paste | ⌘A, ⌘C, ⌘X, ⌘V | Ctrl+A, Ctrl+C, Ctrl+X, Ctrl+V |
| Word left, word right | ⌥Left, ⌥Right | Ctrl+Left, Ctrl+Right |
| Select a word left, right | ⌥⇧Left, ⌥⇧Right | Ctrl+Shift+Left, Ctrl+Shift+Right |
| Start, end of the line | Home, End, ⌘Left, ⌘Right | Home, End |
| Select to the start, end of the line | ⇧Home, ⇧End, ⌘⇧Left, ⌘⇧Right | Shift+Home, Shift+End |
| Start, end of the file | ⌘Up, ⌘Down | Ctrl+Home, Ctrl+End |
| Select to the start, end of the file | ⌘⇧Up, ⌘⇧Down | Ctrl+Shift+Home, Ctrl+Shift+End |
| A page up, a page down (with Shift: select) | PageUp, PageDown | PageUp, PageDown |
| Delete the word to the left, to the right | ⌥Backspace, ⌥Delete | Ctrl+Backspace, Ctrl+Delete |
| Indent (4 spaces), outdent | Tab, ⇧Tab | Tab, Shift+Tab |

The editor binds no key of the window, so Jump, Back, Forward, the panels, the sessions and Quit
keep working while it has the keyboard. On Windows, Ctrl+Tab and Ctrl+PageUp/PageDown stay the
next and the previous session.

Backspace, Delete, the arrows, Shift with an arrow, Enter and Escape are the same on both.

## Other differences from macOS

- **Shell.** Shell sessions and one-off commands use PowerShell 7 (`pwsh.exe`) when it is on the
  PATH, else Windows PowerShell, by its full path. The screen checks used Windows PowerShell 5.1;
  PowerShell 7 ran only in the tests of the Windows CI job, whose runner has it.
- **Title bar.** Glasscode draws its own title bar: the mark, the left panel button, Back and
  Forward, the place, the Jump field, the pills and the right panel button, then the minimize,
  maximize and close buttons at the right; there is no menu bar. The bar is the drag area of the
  window. On the Mac the system draws the traffic lights at the left instead.
- **Ports and programs** (the status bar). The ports come from the TCP listener table of Windows
  with the program of each (`GetExtendedTcpTable`); on the Mac from `lsof`. Ending a program uses
  `TerminateProcess`, the programs under it first; on the Mac `SIGKILL`. A program of another user
  or an elevated one was not tried.
- **Voice.** There is no voice helper on Windows, so there are no microphone buttons in Today's
  start box and the status bar. The voice screen opens
  only through its debug hook (`GLASSCODE_DEBUG_OPEN=voice`, a debug build).
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
- **Voice** (its settings card, the voice helper, and the two microphone
  buttons of the redesign that open the voice screen) is hidden on Windows
  (`glasscode_ui::VOICE` is false): the helper is a macOS program. A Windows helper needs its own
  plan (decision P4 of the UI redesign, open). The microphone button of the Chat view stays: it is
  Claude's own `/voice`.
- **The self-hosted relay** is not configured in this build (the same on the Mac).
- **The manager** is client only on Windows: its server parts come with the server (slices S14 to
  S16 above). Session profiles have no sandbox on Windows; a profile carries only file-tool
  deny rules and hooks, and Settings says so. The voice of the manager is not on Windows (no
  voice helper).
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
- The version and install commands of Settings › Updates run in PowerShell without the
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
