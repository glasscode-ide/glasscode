# UI redesign: acceptance list

| | |
|---|---|
| Document | Acceptance list of the UI redesign (GC-UI-001, slice U23) |
| Date | 2026-11-17 |
| Checked against | `ui-redesign` at `c52a95b` ("U21: Syntax colours (#69)") |
| Not in this build yet | The code part of U23 (the scale sweep and the removal of old code). Steps that need it say "after the sweep". |
| For | The owner, on Windows 11, before the merge of `ui-redesign` |
| Plan | [`ui-redesign-slices.md`](ui-redesign-slices.md); each slice has a "Result" note there |

This list is what a person checks on the screen. The slices were checked by their agents with
made-up data, and many parts were not seen at all. Those parts are in
[Never checked on screen](#never-checked-on-screen). This list does not accept the redesign by
itself: the owner decides after it.

The form is the one of [`windows-acceptance.md`](windows-acceptance.md). How to build and run on
Windows: [`windows.md`](windows.md).

## Contents

1. [How to use this list](#how-to-use-this-list)
2. [Start a safe check](#start-a-safe-check)
3. The screens: [shell and title bar](#1-shell-and-title-bar), [tree sidebar](#2-tree-sidebar),
   [Today](#3-today), [session view and tabs](#4-session-view-and-tabs), [file tabs](#5-file-tabs),
   [editor](#6-editor), [Board](#7-board), [Pull requests](#8-pull-requests), [Jump](#9-jump),
   [Settings](#10-settings), [status bar and popovers](#11-status-bar-and-its-three-popovers),
   [first run](#12-first-run), [voice screen](#13-voice-screen-through-its-debug-hook),
   [project settings and the setup question](#14-project-settings-and-the-setup-question)
4. [Never checked on screen](#never-checked-on-screen)
5. [For the person who builds the Mac version](#for-the-person-who-builds-the-mac-version)
6. [Known limits and open items](#known-limits-and-open-items)

## How to use this list

Each screen has a table of numbered steps: what to do, and what must be seen. Write in the last
column:

- **P** when the step passed;
- **F** when it failed, and a note under the table with what you saw;
- **–** when you did not do it.

Each screen ends with a box for the conditions. In each condition, look again at the steps the
box names. The box passes when nothing is cut, nothing overlaps, and each text can be read.

| Condition | How to set it |
|---|---|
| Dark, Light | Settings › Appearance › Theme. Or `"theme": "dark"` or `"light"` in `appearance` of the check home's `state.json`. |
| 90 %, 130 % | Settings › Appearance › Scale. Or `GLASSCODE_DEBUG_SCALE=90` or `130` (debug build; it is saved). |
| 900 x 560 | `GLASSCODE_DEBUG_SIZE=900x560` (debug build). Or drag the window to its smallest: Glasscode does not get smaller than 900 x 560. |

Do the steps in Dark at 100 % first. The steps use the Windows keys; the macOS keys are in
[`windows.md`](windows.md#shortcuts).

### Results

| # | Screen | Steps | P | F | – | Conditions box |
|---|---|---|---|---|---|---|
| 1 | Shell and title bar | 10 | | | | |
| 2 | Tree sidebar | 10 | | | | |
| 3 | Today | 11 | | | | |
| 4 | Session view and tabs | 10 | | | | |
| 5 | File tabs | 9 | | | | |
| 6 | Editor | 12 | | | | |
| 7 | Board | 7 | | | | |
| 8 | Pull requests | 5 | | | | |
| 9 | Jump | 8 | | | | |
| 10 | Settings | 16 | | | | |
| 11 | Status bar and its three popovers | 8 | | | | |
| 12 | First run | 10 | | | | |
| 13 | Voice screen | 6 | | | | |
| 14 | Project settings and the setup question | 9 | | | | |

## Start a safe check

A check must not touch your own sessions, logins or keys. `GLASSCODE_HOME` moves Glasscode's
data to another folder, but it does not move everything:

| Risk | What happens | What prevents it |
|---|---|---|
| The system login | At launch, a home with projects and no account of its own for an agent adds the system login of that agent (`~/.claude`, `~/.codex`, when the folder exists) as "My login" (`State::bootstrap_accounts`). A Claude or Codex session then runs with your real login. | Put an account of its own for each agent in the check home's `state.json`, with `"system_default": false`. Its folder is then `<home>\accounts\<id>`. |
| A Claude session at launch | A home with projects and no session (and no server) starts a Claude session in the first project (`start_default_claude`), which starts `claude.exe`. | Put at least one Shell session in `state.json`. |
| `gh` | `gh` keeps its login in its own folder, not in `GLASSCODE_HOME`. Pull requests and GitHub accounts then show your real account. | `GH_CONFIG_DIR` set to an empty folder; `GH_TOKEN` and `GITHUB_TOKEN` removed. |
| The Linear key | The key is in Windows Credential Manager, not in `GLASSCODE_HOME`. | `GLASSCODE_LINEAR_KEY_FILE` set to a file in the check folder (a made-up key, or no file). |
| Agent programs | The start box of Today, a sign-in and a Claude or Codex session start the real `claude` or `codex`. | A folder with stand-in `claude.cmd` and `codex.cmd` first on the `PATH`, for the steps that need an agent. `GLASSCODE_DEBUG_NO_AGENTS` (added by the sweep; not in this build yet) is meant to stop agent programs in a check. |
| The first run | With no project, the first run lists the folders and logins of your computer. | `GLASSCODE_DEBUG_DISCOVER_HOME` set to a made-up home (debug build). With it, a system login cannot be ticked. |

A small check home (replace `<check>` with the check folder, with `\\` in JSON):

```json
{
  "projects": [{ "id": "p-demo", "name": "demo", "root": "<check>\\repos\\demo", "expanded": true }],
  "accounts": [
    { "id": "claude-test", "agent": "claude", "label": "Test", "system_default": false },
    { "id": "codex-test", "agent": "codex", "label": "Test", "system_default": false }
  ],
  "defaults": { "claude": "claude-test", "codex": "codex-test" },
  "sessions": [
    { "id": "s-one", "project_id": "p-demo", "worktree": "<check>\\repos\\demo", "branch": "main",
      "title": "Shell on main", "agent": "shell", "created_at": 0 }
  ],
  "active_session": "s-one",
  "startup": "restore",
  "appearance": { "theme": "dark", "scale": "p100", "backdrop": "prism" }
}
```

`<check>\repos\demo` is a git repository with one commit. Then, from the folder of the checkout:

```powershell
cargo build -p glasscode -p glasscode_cli          # a debug build: the debug hooks work only there
$check = "$env:TEMP\gc-check"
$env:GLASSCODE_HOME = "$check\home"                  # holds the state.json above
$env:GH_CONFIG_DIR = "$check\ghconfig"               # an empty folder
$env:GLASSCODE_LINEAR_KEY_FILE = "$check\linear-key"
Remove-Item env:GH_TOKEN, env:GITHUB_TOKEN -ErrorAction SilentlyContinue
$env:GLASSCODE_DEBUG_OPEN = "today"                  # optional: a screen at the start
.\target\debug\glasscode.exe
```

If `claude.exe` or `codex.exe` starts during a check, end it.

### Debug hooks that open a screen

Each hook works only in a debug build. A theme or scale word in a hook is saved in the check
home, as Settings would save it.

`GLASSCODE_DEBUG_OPEN=<value>`:

| Value | Opens |
|---|---|
| `today` | Today. |
| `usage` | The Usage page. |
| `themes` | Settings at Appearance. |
| `settings[:<name>]` | Settings at a section. It knows the old names only: `accounts` (Agents and accounts), `projects` (Git and projects), `automation`, `browser`, `server` (Server and devices), `updates`, `shortcuts` (Keyboard). Each other word opens General, also `permissions`, `integrations`, `advanced` and `appearance`. The sweep may add the new names. |
| `accounts` | Settings at Agents and accounts. |
| `linear` | The Board, then Settings at Integrations (Linear). |
| `palette[:light\|dark\|classic\|contrast][:90\|100\|115\|130][:<words>]` | Jump, with the words typed. |
| `prs[:sample][:<theme>][:<scale>]` | The Pull requests page. `sample` puts made-up pull requests in both lists and links the first of "Mine" to the active session. |
| `status[:usage\|resources\|ports][:<theme>][:<scale>]` | A popover of the status bar. |
| `firstrun[:projects\|agents\|appearance\|summary]` | The first run at a step. |
| `voice[:prism\|aurora\|dusk\|none][:smooth\|dither][:light\|dark][:<scale>]` | The voice screen, with made-up states in a loop. |
| `kit[:<theme>][:<scale>]` | The UI kit page: each helper, icon and brand mark, and the glass colours with their contrast. |
| `backdrop[:prism\|aurora\|dusk\|none][:smooth\|dither][:light\|dark][:still][:bare]` | The backdrop page with three glass cards (`bare`: none). `still`: one frame. |
| `tickets`, `tickets-list`, `tickets-expand`, `tickets-done`, `tickets-review`, each with `:<ticket>` | The Board in a layout, and with a ticket (`ENG-1` or `owner/name#12`) its reader. |
| `new` | The New session dialog. |
| `new-in:<end of a worktree path>:<agent>[:pick]` | The New session dialog for that worktree. |
| `build-pr:<words>` | The New session dialog, searching pull requests for the words (it asks `gh`). |
| `review-new` | The New session dialog in Review mode. |
| `handoff` | The Handoff dialog. |
| `browser` | The Browser dialog. |
| `menu` | The account menu of the session header. |
| `project-menu` | The row menu of the first project. |
| `wt-menu:<end of a path>`, `wt-project-menu`, `wt-remove:<end of a path>`, `wt-cleanup` | A worktree's menu, or the remove or clean-up dialog (they open only). |
| `gh-expand[:<section>]` | Opens the sections of the Pull request tab. |
| `code` | The Files tab with the first changed file (from before the redesign; not checked since U13). |
| `proofs[:<what>]` | The measurement page of U02 in a window of its own. The sweep removes it. |

Other variables:

| Variable | Does |
|---|---|
| `GLASSCODE_DEBUG_SIZE=<w>x<h>` | The size of the window at the start (else 1440 x 900). |
| `GLASSCODE_DEBUG_SCALE=90\|100\|115\|130` | Sets the scale once at the start, and saves it. |
| `GLASSCODE_DEBUG_THEMES=1` | Opens Settings at Appearance. |
| `GLASSCODE_DEBUG_TODAY=still,inbox,activity` | `still`: the backdrop stands still, as with less motion; `inbox` or `activity`: that tab of Today's right panel. |
| `GLASSCODE_DEBUG_RIGHT_TAB=files\|changes\|checks\|pr` | That tab of the session's right panel. `files:changed` shows only changed files, `files:<path>` opens that file; `checks:sample` and `pr:sample` give the active session a made-up pull request. |
| `GLASSCODE_DEBUG_EDIT=<path>` | Opens that file of the active session's worktree in a tab, in Edit. |
| `GLASSCODE_DEBUG_USAGE=1\|table\|claude\|codex\|30\|hover:<day>` | Opens the Usage page so. |
| `GLASSCODE_DEBUG_ACTIVITY=<id start>:needs\|working,…` | Shows those sessions as needing you or working. |
| `GLASSCODE_DEBUG_DISCOVER_HOME=<folder>` | The first run looks for projects and logins in this made-up home (the editors' data in `<folder>\AppData\Roaming`). |
| `GLASSCODE_DEBUG_FRAMES=1` | Writes to the log, once each second, the draws of the window, the time of their parts, and the time of the three tick loops (slice U24). |
| `GLASSCODE_DEBUG_BACKGROUND=1\|hidden` | A window that never takes the keyboard, or no window; notifications are written to the log, not shown. `GLASSCODE_DEBUG_ORIGIN=<x>,<y>` places that window. |
| `GLASSCODE_DEBUG_NO_AGENTS` | Added by the sweep; not in this build yet. Stops agent programs in a check. |

### Debug hooks that act

Do not use these in an acceptance check unless a step names them. They change data, start
programs or need a server.

| Hook | Does |
|---|---|
| `GLASSCODE_DEBUG_OPEN=handoff-go` | Hands the active session to the other agent. |
| `GLASSCODE_DEBUG_OPEN=tickets-start:<ticket>`, `tickets-go:<ticket>` | Opens the start dialog of a ticket; `go` also starts the session. |
| `GLASSCODE_DEBUG_OPEN=tickets-comment:<ticket>` | Posts a comment on the ticket in Linear. |
| `GLASSCODE_DEBUG_OPEN=review-new:<number>` | Starts a review session of that pull request. |
| `GLASSCODE_DEBUG_OPEN=wt-remove-go:<path end>`, `wt-cleanup-go` | Removes worktrees (the clean-up also deletes their branches). |
| `GLASSCODE_DEBUG_OPEN=server`, `server-new`, `server-session`, `pair`, `switch-to-server`, `switch-to-laptop`, `bring-first`, `move-first` | Need a server; open its dialogs, start a session there, pair a phone, or move sessions. |
| `GLASSCODE_DEBUG_DECIDE=<answer>` | Answers each approval by itself after 1.5 s. |
| `GLASSCODE_DEBUG_TYPE`, `GLASSCODE_DEBUG_FILES`, `GLASSCODE_DEBUG_CHAT`, `GLASSCODE_DEBUG_NOTIFY` | Type into the terminal, paste or drop files, drive the Chat view, show notifications. |

## 1. Shell and title bar

Slices U03 and U05. Use a home with two projects and three sessions.

| # | Do | Must see | P / F / – |
|---|---|---|---|
| 1 | Start Glasscode. | The title bar, from the left: the mark (and the word Glasscode when there is room), the left panel button, Back, Forward, the place (project / branch, the branch in the code font), the field "Search sessions, tickets and commands" with Ctrl+Shift+K, the server pill, the right panel button, and minimize, maximize and close. | |
| 2 | Click the left panel button. Click it again. Press Ctrl+Shift+E two times. | The left sidebar hides and comes back at the same width, with the button and with the key. | |
| 3 | In a session, click the right panel button, then press Ctrl+Shift+B. | The right panel hides and comes back. | |
| 4 | Drag the edge of the left sidebar until it is narrower than 140 px. Click its button. Do the same with the right panel. | Each panel hides when the drag ends; its button brings it back at the width it had before the drag. | |
| 5 | Click Today, then Board, then Settings in the left sidebar (Settings is at the bottom). Click Back two times. Click Forward. | Back goes to Board, then Today. Forward goes to Board. In Settings the left sidebar does not show, and "Back to work" is in the section menu. | |
| 6 | Open a session, then Today. Press Ctrl+< (Ctrl+Shift+, on a US keyboard), then Ctrl+> (Ctrl+Shift+.). | Back shows the same session; Forward shows Today. | |
| 7 | Click Back two times, then click Board in the sidebar. | Forward is faint: a new place clears the forward list. | |
| 8 | Double-click the title bar. Double-click again. Drag the title bar. Use the three window buttons. | Maximize, restore, move, minimize and close work. Close quits. | |
| 9 | Hide the right panel, choose the Checks tab in a session, make the left sidebar wider. Quit with Ctrl+Shift+Q and start again. | The panels, their widths and the right panel's tab are as you left them. | |
| 10 | Make the window 900 x 560. | The title bar gives way in this order: the word Glasscode, then the place, then the words of the search field (its icon and key stay). The window buttons stay. | |

| Look again at | Dark | Light | 90 % | 130 % | 900 x 560 at 130 % |
|---|---|---|---|---|---|
| Steps 1, 5, 10 | | | | | |

## 2. Tree sidebar

Slice U10. Use a home with three projects, one of them with two worktrees and three sessions.

| # | Do | Must see | P / F / – |
|---|---|---|---|
| 1 | Look at the top of the left sidebar. | "Workspace", with a dot and a number for the sessions that need you and for those at work when there are some; New session with Ctrl+Shift+N when the sidebar is 260 px or wider; the rows Today, Board, Pull requests and Usage, each with its count when it has one. | |
| 2 | Look at the tree. | Project rows (folder icon, name), worktree rows (branch icon, branch in the code font, "+"), session rows (agent mark, title, state dot). | |
| 3 | Click a project row; click it again. Click a worktree row. | The project folds and opens. A click on a worktree row folds it; it does not open a session. | |
| 4 | Start with `GLASSCODE_DEBUG_ACTIVITY=<start of a session id>:needs`. Fold that session's worktree. | The session shows that it needs you; the folded worktree shows a dot; the "Workspace" count shows it. | |
| 5 | Fold one project and one worktree. Quit and start again. | The same folds. | |
| 6 | Click a session row. | The Sessions view with that session. The row is marked. | |
| 7 | Open the "⋯" menu of a session row, and its right-click menu. Pin the session. | The row menu opens both ways. A pinned session shows the pin and comes first. | |
| 8 | Close a session with the × of its row. | The session goes. (The change shows about 0.75 s after the click; this is known.) | |
| 9 | Look at the end of the tree and the bottom of the sidebar. In a project with worktrees that have no session, open "N more worktrees without a session". | "Add project…" at the end of the tree (and the Server group when a server is set up); the Settings row at the bottom with Ctrl+Shift+S. The worktrees without a session are listed. | |
| 10 | Make the tree longer than the sidebar. Move the pointer into it, scroll, drag the thumb. | A thin thumb shows while the pointer is in the tree or it scrolls, and goes after about one second. The thumb can be dragged. | |

| Look again at | Dark | Light | 90 % | 130 % | 900 x 560 at 130 % |
|---|---|---|---|---|---|
| Steps 1, 2, 9 | | | | | |

## 3. Today

Slice U11. Use a home with tasks in `board.json` and days in `activity.json`, and one with none.
The start box starts Claude or Codex sessions only: do step 5 only with the stand-in agents on the
`PATH`.

| # | Do | Must see | P / F / – |
|---|---|---|---|
| 1 | Click Today in the sidebar (or `GLASSCODE_DEBUG_OPEN=today`). | The moving backdrop behind the page. The start box on glass: the message box, pills for agent and account, project and start point, and Send. No microphone button (Windows). | |
| 2 | Look at the cards. | Needs you, Coming up, Usage, Activity (26 weeks and "Since <date>"), Recent sessions (5 at most), Pull requests (3 at most and "N more"), This month. Each text can be read on the glass. | |
| 3 | Drag the left sidebar wider and narrower; hide it. | The cards change between 3, 2 and 1 columns. | |
| 4 | Start on a home with no tasks, no activity and no limits. | Each card with nothing to show has one sentence. | |
| 5 | Click a pill of the start box; click it again. Choose an account, type a few words, Send. | The list opens and closes. A session starts with the words as its first message and shows. | |
| 6 | Scroll to "Your list". Add a task in its add box. Use its filters. | The task is in its section; the filters change the list. | |
| 7 | Look at the right panel. Click Inbox, then Activity. | Inbox has what agents ask to do and what needs you now; Activity has what agents did. | |
| 8 | Make a request that needs approval (as in test 11 of [`windows-acceptance.md`](windows-acceptance.md)). Answer it in Inbox: Allow; next time Deny; next time "Allow for the session". | A card in Inbox and a notification; each answer does what it says. | |
| 9 | Click another window, so that Glasscode is in the back. Look at Task Manager. Click Glasscode again. | In the back the backdrop stands still and Glasscode uses almost no processor time; in front it moves again. | |
| 10 | Settings › Appearance › Backdrop: None. Open Today. | A plain ground; the cards are still cards and can be read. | |
| 11 | Open the agent list of the start box where the left sidebar is under it, and click a part of the list that is over the sidebar. | The click chooses the entry; it does not reach the sidebar row below (U12 saw this defect with another menu). | |

| Look again at | Dark | Light | 90 % | 130 % | 900 x 560 at 130 % |
|---|---|---|---|---|---|
| Steps 1, 2, 6, 7 | | | | | |

## 4. Session view and tabs

Slice U12. Use a worktree with three sessions: two Claude sessions on the stand-in, one Shell.

| # | Do | Must see | P / F / – |
|---|---|---|---|
| 1 | Open a session of the worktree. | One tab for each session of the worktree, "+" after them, and the Chat and Terminal control at the right of the strip. | |
| 2 | Click each tab. Close one with its ×. Click "+". | Each session shows. The × closes the session and its neighbour shows. "+" does what the sidebar's "+" does. | |
| 3 | Look at the header in the column. | One row that wraps: agent mark, project and branch, account, state; the account menu, Browser, Hand off, "⋯". | |
| 4 | In a Claude session, switch Chat and Terminal. | The Chat view and the terminal. A Shell session has the terminal only. | |
| 5 | Click Files, Changes, Checks and Pull request in the right panel. Start with `GLASSCODE_DEBUG_RIGHT_TAB=checks:sample`, then `pr:sample`. | Each tab. With the sample, a made-up pull request with a failed, a running and passed checks. | |
| 6 | With "Backdrop behind sessions" on, then off (Settings › Appearance). | On: the backdrop behind the column, the conversation and the message box on glass, the terminal opaque with a margin. Off: a plain ground. | |
| 7 | Make the centre narrower (wider sidebars, smaller window). | The Chat and Terminal control moves from the strip to the header, then into the "⋯" menu. | |
| 8 | Open the "⋯" menu and click each entry. | The menu stays inside the header; a click on it does what the entry says and does not reach the left sidebar. | |
| 9 | With a real Claude session (signed in, outside a scratch check): send a prompt that needs a permission. | The question shows as buttons in Chat; an answer reaches Claude. | |
| 10 | In a chat with many exchanges (a few hundred), with the moving backdrop behind sessions: type in the message box, then scroll the conversation. | Each letter shows at once; the scroll does not stutter. (The frame log of U12 showed a letter one or two frames after the key and 17 to 38 frames each second while scrolling; nobody looked.) | |

| Look again at | Dark | Light | 90 % | 130 % | 900 x 560 at 130 % |
|---|---|---|---|---|---|
| Steps 1, 3, 5, 6 | | | | | |

## 5. File tabs

Slices U13 and U21. Use a worktree with a changed file, and files of each language below.

| # | Do | Must see | P / F / – |
|---|---|---|---|
| 1 | Click a file in the Files tab. Click the same file again. | A tab in the centre with the path and the text with line numbers. The second click brings its tab to the front. | |
| 2 | Click a changed file in Changes. Use the Diff and File control. | The diff with HEAD, and the whole file; git's status tag. | |
| 3 | In the head of the tab: Copy path, Open in <editor>, Mention in session. | The path in the clipboard; the file in the editor of Settings › General; the path in the session's message box. | |
| 4 | Open four files and close them with their ×. | The tabs close; the session's tab shows. | |
| 5 | Delete an open file on the disk. | The tab says that the file is gone. | |
| 6 | Open a file above 512 KB, one above 4000 lines, a binary file and one with a line of more than 1000 characters. | Each shows a sentence that names its limit. | |
| 7 | In Jump, type part of a file name and press Enter. | The file opens in a tab of the centre. | |
| 8 | Open a `.rs`, `.ts`, `.js`, `.json`, `.toml`, `.md`, `.py`, `.sh` and `.ps1` file, in File and in Diff. | Comments, strings, numbers, keywords and types have their colours (keys and headings too in JSON, TOML and Markdown); other files are plain. There is no setting for this. | |
| 9 | Quit and start again. | The file tabs are gone (by design). | |

| Look again at | Dark | Light | 90 % | 130 % | 900 x 560 at 130 % |
|---|---|---|---|---|---|
| Steps 1, 2, 8 | | | | | |

## 6. Editor

Slice U20 (colours U21). Use a copy of a repository, not your own work.

| # | Do | Must see | P / F / – |
|---|---|---|---|
| 1 | In a file tab, click Edit (the Edit and Preview control). Type. | The editor; a dot on the tab for the change that is not saved. | |
| 2 | Ctrl+S. Look at the file on the disk. | The dot goes. The file has the text. A file with CRLF line ends keeps them; a file with no last line end keeps none. | |
| 3 | Make 20 changes. Ctrl+Z 20 times, then Ctrl+Y and Ctrl+Shift+Z. | Undo takes typing runs back in steps; redo brings them again. | |
| 4 | Tab, Shift+Tab. | Tab puts 4 spaces; Shift+Tab takes them back. | |
| 5 | Ctrl+Left, Ctrl+Right, Home, End, Ctrl+Home, Ctrl+End, PageUp, PageDown; the same with Shift; Ctrl+Shift+Left; Ctrl+A. | The caret moves and selects as the keys say. | |
| 6 | Ctrl+X, Ctrl+C, Ctrl+V; Ctrl+Backspace, Ctrl+Delete. | Cut, copy, paste; delete a word to the left and to the right. | |
| 7 | With no change in the tab, change the file in another program. Then make a change in the tab and change the file again outside. | The first time the tab reads the file again. The second time a bar with Reload and Keep mine. | |
| 8 | With a change not saved: close the tab with ×; then Quit (Ctrl+Shift+Q); then the window's close button. | Each asks "Save the changes…?" with Save, Discard and Cancel. | |
| 9 | Make the file read only. Save. | An error; the text stays in the editor. | |
| 10 | Open a file above 2 MB. | It shows read only; Edit is not offered. | |
| 11 | Type with an input method (for example Japanese), and the emoji panel (Win+.). | The text goes in where the caret is. | |
| 12 | While the editor has the keyboard: Ctrl+Shift+K, Ctrl+<, Ctrl+Tab, Ctrl+Shift+E. | Jump, Back, the next session and the left sidebar work. | |

| Look again at | Dark | Light | 90 % | 130 % | 900 x 560 at 130 % |
|---|---|---|---|---|---|
| Steps 1, 7, 8 | | | | | |

## 7. Board

Slice U14. Use made-up Linear tickets (a `linear.json` in the check home and a made-up key in the
key file), or a real Linear account.

| # | Do | Must see | P / F / – |
|---|---|---|---|
| 1 | Click Board in the sidebar. | The backdrop; the toolbar and each column on glass, each card on stronger glass. In the light theme the cards have a soft shadow. | |
| 2 | Look at the cards. | Each card has its source mark, the marks of its sessions' agents, and one button: Start workspace, Start in worktree, Start review or Open session. | |
| 3 | Click a card. | Its reader in the right panel, on an opaque ground. | |
| 4 | Drag a card to another column. With a real Linear account, look in Linear. | The card moves. In Linear the ticket has the new status. | |
| 5 | Click Start workspace on a card with no session. | The start dialog for the ticket. | |
| 6 | Use the list layout and the quick filters. | The list; the filters change it. | |
| 7 | At 900 x 560. | The toolbar takes at most about 42 % of the height and scrolls. | |

| Look again at | Dark | Light | 90 % | 130 % | 900 x 560 at 130 % |
|---|---|---|---|---|---|
| Steps 1, 2, 3 | | | | | |

## 8. Pull requests

Slice U14.

| # | Do | Must see | P / F / – |
|---|---|---|---|
| 1 | Click Pull requests in the sidebar with `gh` signed in to nothing. | The page with "Mine" and "Asked of me", each with a sentence. | |
| 2 | Start with `GLASSCODE_DEBUG_OPEN=prs:sample`. | Made-up rows in both lists, each with its checks state. | |
| 3 | Look at the actions of the rows. | "Fix with agent" when a session of that pull request exists and a check fails, else "Open session"; "Open review" or "Review" in "Asked of me". | |
| 4 | Click "Open session" or "Fix with agent". | The session of that pull request. | |
| 5 | With `gh` signed in to a real account (outside a scratch check). | Your open pull requests; the newest 10 have their checks. The sidebar count matches. | |

| Look again at | Dark | Light | 90 % | 130 % | 900 x 560 at 130 % |
|---|---|---|---|---|---|
| Step 2 | | | | | |

## 9. Jump

Slice U15 (files open in the centre since U13).

| # | Do | Must see | P / F / – |
|---|---|---|---|
| 1 | Click the search field of the title bar. Press Esc. Press Ctrl+Shift+K. | Jump opens on its own ground, with glass over it; Esc closes it. | |
| 2 | Type nothing. | Sessions, Worktrees, Projects and Commands in groups; session rows with the agent mark and the state dot. | |
| 3 | Type part of a branch name. Enter. | The worktree in Worktrees. Enter opens its most recent session, or the New session dialog when it has none. | |
| 4 | Type part of a project name. Enter. | The project folds or opens in the tree. | |
| 5 | With a session open, type part of a file name. Enter. | The file in Files; Enter opens it in a tab of the centre. | |
| 6 | Type `commands`. | All 18 commands. | |
| 7 | Use ↑ and ↓; click a row. | The mark moves; the click goes there. | |
| 8 | Type words that match nothing. | A sentence, no rows. | |

| Look again at | Dark | Light | 90 % | 130 % | 900 x 560 at 130 % |
|---|---|---|---|---|---|
| Steps 2, 3 | | | | | |

## 10. Settings

Slices U16 and U22. Settings opens with its row in the sidebar or Ctrl+Shift+S.

| # | Do | Must see | P / F / – |
|---|---|---|---|
| 1 | Open Settings. | The backdrop; the section menu and each card on glass; "Back to work" in the menu; no left sidebar. Each card: a title, one sentence, its control, a footer line where it helps. | |
| 2 | Type `notify` in "Search settings". | The cards of each section with that word. | |
| 3 | General. | When Glasscode opens; four notification cards (a session needs you, finishes its turn, CI fails, a ticket or a review asks for you); Play a sound; Name sessions and new worktrees by themselves; Ask before closing a session that is working; Open files in. | |
| 4 | Appearance: change each control. | Theme (Auto, Light, Dark, Classic, High contrast), Accent, Backdrop (Prism, Aurora, Dusk, None), Backdrop finish (Smooth, Dither), Backdrop behind sessions, Scale (90 to 130 %), Text size, Density, Interface font, Code font, Conversation font, Sessions open as, Cursor. Each change shows at once. A line says what the moving backdrop costs. | |
| 5 | Agents and accounts. | Claude and Codex accounts (sign in, add, default, remove) and GitHub accounts. | |
| 6 | Agents and accounts: add an account and sign in, with the stand-in on the `PATH`. | The sign-in runs in a terminal; the account shows signed in. | |
| 7 | Permissions. | One card for each of the 13 rules with Allow, Ask and Never, and three more cards about the `glasscode` command. | |
| 8 | Git and projects, "For me". | Projects; Start new work from the latest main; Also delete the local branch. | |
| 9 | Integrations. | Linear. | |
| 10 | Automation. | Eight cards: fix CI, resolve conflicts, the tries, the wait for checks, auto-review, Move a ticket, Show what agents did, Open Today each morning. | |
| 11 | Browser. | Glasscode Browser, Agents may use the browser, Start over. | |
| 12 | Server and devices. | Server. (iPhone shows only with a connected server; the network card of the voice server only on macOS.) | |
| 13 | Updates. | Glasscode updates ("Up to date"), Channel, Install updates when Glasscode quits, CLI versions. | |
| 14 | Keyboard. | The shortcuts with their Windows labels, the same as in [`windows.md`](windows.md#shortcuts). | |
| 15 | Advanced. | Settings file (Open state.json folder), Logs (Open log folder), Run first run again. The two folder buttons open the same folder (known, U16). | |
| 16 | At 900 x 560, set the scale to 115 % or 130 %. | Below 800 px of width at 100 % (the window's width divided by the scale), the section menu becomes rows of chips above the cards. | |

The project scope of Git and projects is in [section 14](#14-project-settings-and-the-setup-question).

| Look again at | Dark | Light | 90 % | 130 % | 900 x 560 at 130 % |
|---|---|---|---|---|---|
| Steps 1, 4, 7, 16 | | | | | |

## 11. Status bar and its three popovers

Slice U17. Use a home with made-up limits for one Claude and one Codex account, and Shell sessions.

| # | Do | Must see | P / F / – |
|---|---|---|---|
| 1 | Look at the status bar. | Left: a usage meter for each account with limits (5 hours and week; hot colour from 80 %), refresh, then branch, changes, the GitHub account, CLI versions, saved sessions, agent and account. Right: memory, terminals, ports. | |
| 2 | Make the window narrower. | Parts give way in this order: the meters' words, the week meters, the CLI versions, the saved sessions, the GitHub account, agent and account. The right side stays. | |
| 3 | Click a meter. Click "Open usage". | The Usage popover: each account with its limits and reset times. "Open usage" opens the Usage page. | |
| 4 | Click the memory. | The Resources popover: project, worktree and program, with processor and memory. | |
| 5 | Click × on a program; confirm. | One confirm line ("End <program> and N under it?"); the program ends. | |
| 6 | In a Shell session run `python -m http.server 8765` (or another server). Click the ports. | The port under its session with Open and Kill. Ports of other programs are folded and have no Kill. | |
| 7 | Click Open; then Kill and confirm. | The page opens in the default browser; the port goes. Glasscode does not end. | |
| 8 | With a popover open, click outside it; open it again and press Esc. | It closes; the click outside does not reach what is below. | |

| Look again at | Dark | Light | 90 % | 130 % | 900 x 560 at 130 % |
|---|---|---|---|---|---|
| Steps 1, 3, 4, 6 | | | | | |

## 12. First run

Slice U18. Use an empty `GLASSCODE_HOME`, `GLASSCODE_DEBUG_DISCOVER_HOME` with made-up projects
and logins, and the stand-in agents on the `PATH`.

| # | Do | Must see | P / F / – |
|---|---|---|---|
| 1 | Start Glasscode on the empty home. | The first run: no sidebars and no status bar; at the left the backdrop in a rounded panel with the mark and one sentence (it hides in a narrow window). Back and Forward are faint, and the title bar has no Jump field. | |
| 2 | Step Projects. | The folders found, last used first, with check boxes; a linked worktree folded into its main checkout; a search box; "Add a folder…" and "Clone from GitHub…". | |
| 3 | Search; tick two projects; "Add a folder…". | The search filters; the ticked projects and the added folder are ticked. | |
| 4 | Continue to Agents. Tick a login that is not signed in. | Its sign-in starts in a terminal in its row; Continue waits. | |
| 5 | Let the stand-in finish; then tick another and press Cancel. | The first shows signed in. Cancel stops the second and takes its tick off. | |
| 6 | "Sign in to another account". | A new account and its sign-in. | |
| 7 | Continue to Appearance; change the theme and the backdrop. Continue. | The cards of Settings › Appearance; the change shows at once. The summary with counts and "Open Today". | |
| 8 | "Open Today". Quit and start again. | Today with the projects and accounts. The first run does not show again. | |
| 9 | Settings › Advanced › Run first run again. | The flow again; the projects and accounts that exist are ticked and locked. | |
| 10 | On a new empty home with the made-up discovery home, press "Skip for now" at the first step. | The flow ends and Today shows. With the made-up discovery home no login is taken. (With a real home, Skip takes each system login whose folder exists as "My login", as the build before the first run did: that path was never checked.) | |

| Look again at | Dark | Light | 90 % | 130 % | 900 x 560 at 130 % |
|---|---|---|---|---|---|
| Steps 1, 2, 4, 7 | | | | | |

## 13. Voice screen, through its debug hook

Slice U19. On Windows only the hook opens the screen: there is no voice helper and no microphone
button.

| # | Do | Must see | P / F / – |
|---|---|---|---|
| 1 | Start with `GLASSCODE_DEBUG_OPEN=voice`. | The screen covers Today's content: the orb, the state (Listening, Thinking, Speaking, Muted in turn), the last words, the last action, Mute and End voice. | |
| 2 | Press Esc (click the screen first). | It does what End voice does. | |
| 3 | `voice:prism`, `voice:aurora`, `voice:dusk`, `voice:none`, each with `smooth` and `dither`. | The orb follows the look and the finish. With `none` it is a still image. | |
| 4 | Put the window in the back. | The orb stands still. | |
| 5 | Look at Today's start box and the right end of the status bar on Windows. | No microphone button. | |
| 6 | Look at the outer ring of the glow in the dark theme, smooth finish. | Note if a few pixels show as dots (known, cause not found). | |

| Look again at | Dark | Light | 90 % | 130 % | 900 x 560 at 130 % |
|---|---|---|---|---|---|
| Steps 1, 3 | | | | | |

## 14. Project settings and the setup question

Slice U22. In the check's repository, commit `.glasscode/settings.json`:

```json
{ "setup": "git status", "branch_template": "{type}/{slug}", "worktree_folder": "{repo}-{branch}" }
```

| # | Do | Must see | P / F / – |
|---|---|---|---|
| 1 | Settings › Git and projects: switch "For me" to "Project · <name>". | The four values of the file (Setup script, Branch names, Worktree folder, Default agent), the one in effect and where it comes from, read only. "Open the file" (and "Make the file" for a project with none). | |
| 2 | Click "Open the file"; on a project without one, "Make the file". | The file opens; the new file is made. | |
| 3 | Start a Shell session in a new worktree of the project. | The session shows the exact command with Run, Skip this time and Never for this project. Nothing runs before the answer. | |
| 4 | Run. | The command runs in its own terminal in the worktree, then the session starts, whatever the exit code. | |
| 5 | Start another session in a new worktree. Then change `setup` and start one more. | The same command runs without a question; the changed one asks again. | |
| 6 | Set `setup` to a command that does not end (`ping -t localhost`). | "Stop and start <agent>" stops it and starts the session. | |
| 7 | Set `setup` to a command with a tab or a character that is not ASCII. | Refused, with a warning that names the character. | |
| 8 | Look at the new worktree's branch and folder. | They follow the templates; the folder is below `~\.glasscode\worktrees`. | |
| 9 | Answer Never for this project. Look at Settings › Git and projects. | "Never for this project" with "Ask again"; Ask again brings the question back. | |

| Look again at | Dark | Light | 90 % | 130 % | 900 x 560 at 130 % |
|---|---|---|---|---|---|
| Steps 1, 3 | | | | | |

## Never checked on screen

From the Result notes of the slices: what an agent reasoned from the code, checked with a unit test
only, or did not see. The slice is in brackets. U21 has no Result note in the plan yet; its items
come from its pull request. The code part of U23 is not merged.

### All screens

- **macOS as a whole.** No window was opened on macOS for any slice (U03 to U22). Since
  2026-11-17 the macOS job is not a gate, and it was cancelled in several runs. See
  [For the person who builds the Mac version](#for-the-person-who-builds-the-mac-version).
- **Classic and High contrast** on screen in each screen but the UI kit page (U04, U10 to U22).
  The kit page was seen in the four themes (U01).
- **115 %** in most screens (U05, U16, U17, U19); **90 %** in Settings (U16) and the voice screen
  (U19); the light theme at 115 % (U05).
- **Less motion of the system.** Nothing in Windows was seen to set it; only the hook
  `GLASSCODE_DEBUG_TODAY=still` sets it (U04).
- **Costs not measured:** a release build (U24), the dither finish (U04), the backdrop on the Board
  (U14), a frame with a file tab in front (U13), the chat spinner (U25).
- **A deferred popup over the left sidebar.** A click on the "⋯" menu of the session header went to
  the sidebar row below it; the cause is not known. The lists of Today's start box were not tested
  (U12).
- **Linux:** no build job (U08, U09).
- **The application icon** pinned to a real taskbar, light and dark: left to the owner (U06).

### Shell and title bar

- A background `select_session` that leaves Today alone: reasoned from the code (U03).
- The update pill, the move button and the Browser button in the title bar (U03); the title bar with
  an update pill or a move button at 130 % in a narrow window (U05).
- Notification popups at 130 % (U05).

### Tree sidebar

- The need-you and working counts, a pull request number with its check colour, connected server
  rows (U10).
- 15 projects with 60 worktrees: unit test only (U10).

### Today

- The agent and start-point lists, a failed Send, the box with no project (U11).
- Deny and "Allow for the session" in Inbox (U11).
- Rows of the Pull requests card with real data (U11).
- The part below the first screen in the light theme and at 900 x 560 (U11).
- Today after the change of `center::open_url` (U17).
- The heat map with real history: merge detection end to end (U07).

### Session view and tabs

- Browser, the ticket and Review tags, the lineage line, Resume and the exited bar (U12).
- A working session, a server session (U12).
- Typing and scrolling in a long chat with the moving backdrop: read from the frame log only, not
  seen by eye (U12).
- Links of the chat and the GitHub panel after the change of `center::open_url` (U17).

### File tabs

- Copy path and Open in <editor> from the tab: they use the real clipboard and a real editor (U13).
- A Claude or Codex chat behind a file tab (U13).
- Syntax colours in Classic and High contrast, at 130 %, at 900 x 560, and in a 2 MB file (U21).

### Editor

- The input method, Tab and Shift+Tab, Cut and Copy, word moves, the page keys (U20).
- 900 x 560 at 130 % (U20).
- Project and worktree removal with unsaved text; the copies in `unsaved` (U20).

### Board

- A move of a card that is kept: the test address refused each move (U14).
- "Open review" and "Start in worktree" (U14).

### Pull requests

- Rows with real data (U11, U14).
- `my_open_prs` against the real `gh` with more than one account (U07).

### Jump

- Ticket and review rows; the Claude and Codex marks and the state dots (U15).

### Settings

- A real sign-in (U16).
- A connected server and phones; update states other than "Up to date" (U16).
- "Open the file", "Make the file" and "Ask again" (U22).

### Status bar and popovers

- An orphan terminal: none was made on screen (closing a session ends its programs within 1 s on
  Windows). The count and "End N orphan terminals" have unit tests only (U17).
- The reader after the change of `center::open_url` (U17).
- A process of another user or an elevated process (U08).

### First run

- Seen with stand-in programs only (U18).
- A signed-in system login that is adopted; the Skip path that takes the system logins;
  `gh auth login --web`; the Linear form from step 2 (U18).
- The claude-swap format on a real install (U09). VS Code Insiders and VSCodium are not read (U09).

### Voice screen

- All of macOS: the two microphone buttons and the helper (U19).
- 90 % and 115 % (U19).

### Project settings and the setup question

- "Skip this time" (U22).
- A Claude or Codex session with a setup (U22).

## For the person who builds the Mac version

The redesign changed shared code, so macOS has the new interface too (decision P2). Nobody opened
it on macOS. The macOS job of the automatic build only compiles the app and runs the core tests;
it does not run `cargo test -p glasscode`, and since 2026-11-17 it is not a gate. A test marked
`#[cfg(unix)]` may not have run for a change. On the Mac, run first:

```sh
cargo test -p glasscode_core -- --test-threads=1
cargo test -p glasscode
```

Then this list, and the screens above in the light and the dark theme.

| What changed and was never run | Where | Check first |
|---|---|---|
| **Voice: two microphone buttons and the voice screen.** A microphone next to Send in Today's start box and one at the right end of the status bar open the screen and start or stop the helper (`voice_open`, `glasscode_toggle`). The helper gives no microphone level and no mute: the orb follows the mode, and Mute shows only with the made-up states. The Glasscode tab of the right panel is unchanged. | `today_ui.rs`, `status_bar.rs`, `voice_view.rs` (U19) | Start voice from each button; the screen covers Today; End voice and Esc stop it; the Glasscode tab still works. |
| **The title bar and the traffic lights with the scale.** The title bar is `theme::sz(44)` high, so it follows the scale. The room for the traffic lights is 84 px at each scale, and the lights are placed at a fixed point (14, 15) by the window options. | `shell.rs`, `main.rs` (U03, U05) | At 90 % and 130 %: the lights sit inside the bar, and the mark and the buttons do not touch them. |
| **The sign-in wait of the first run.** Claude keeps its login in the Keychain, so a Claude folder counts as signed in when it exists, and the wait for a Claude sign-in ends when the sign-in program exits. An exit with no login takes the tick off. | `first_run.rs` (U18), `discover.rs` (U09) | Tick a Claude login that is not signed in; sign in; exit the program; Continue works. Exit without a sign-in: the tick goes. |
| **Finding projects.** Cursor and VS Code data are read from `dirs::config_dir()` (on macOS `~/Library/Application Support`). The format was confirmed on Windows only. | `discover.rs` (U09) | The first run lists the folders opened in Cursor and VS Code. |
| **Processes and ports (the Unix parts of `sys`).** Ports come from `lsof -iTCP -sTCP:LISTEN -P -n -Fpcn` (its parser is tested on Windows with a macOS sample). `end_tree` sends SIGKILL, children first, and also refuses process 1. Processor and memory come from `sysinfo` on both systems. | `glasscode_core/src/sys/mod.rs`, `sys/unix.rs` (U08) | The Ports popover lists a server started in a session; Kill ends it; Resources shows processor and memory; Glasscode never ends itself. |
| **File permissions of the editor's save.** The save writes a temporary file in the same folder with the file's own mode, sets the file's permissions on it, and renames it over the file. The copies of a quit that cannot ask go to `~/.glasscode/unsaved/` (folder 0700, copy 0600). The tests of these modes are `#[cfg(unix)]`. | `glasscode_core/src/save.rs` (U20) | Save a script with mode 0755: it stays 0755. Look at what a rename does not keep (owner, extended attributes, a hard link): not checked. |
| **Key bindings of the macOS table.** New: ⌘⇧E (left sidebar), ⌘⇧S (Settings), ⌘[ and ⌘] (Back, Forward), and the editor: ⌘S, ⌘Z, ⌘⇧Z, ⌘X, ⌘C, ⌘V, ⌘A, ⌥← and ⌥→ (words), ⌘← and ⌘→ (line), ⌘↑ and ⌘↓ (file), Tab and ⇧Tab. The labels come from the same table. | `keymap.rs` (U03, U20) | Each key in the terminal, the editor, a text field and the Chat view. ⌘[ and ⌘] in a terminal program. |
| **Everything else of the redesign:** the backdrop and glass on Metal, the rem size of the scale, the scroll thumb, the status bar, the first run, Auto following the macOS appearance. | all screens | The screens of this list, in Dark and Light. |

`Glasscode.icns`, the macOS icon, was not examined (U06).

## Known limits and open items

From the plan's Result notes and its table of open findings. The owner decided some of them; the
others have no slice yet.

### Cost

- **The moving backdrop costs processor time, and the owner accepted it (2026-11-17).** On Today at
  10 frames each second it costs about 18 % of one core (21.3 % against 3.0 % with the backdrop
  None) (U24). In a chat of 300 exchanges it costs about 29 % more than None (U12). Each frame runs
  the layout of the whole window on the main thread (about 17 ms on Today), so a screen with more
  content costs more (U25).
- Settings › Appearance › "Backdrop behind sessions" off takes it out of sessions; Backdrop None
  takes it out everywhere. It stops when the window is not active, is hidden or minimized, and when
  the system asks for less motion (U04).
- A window that is covered but still active keeps the animation (Windows reports no covered
  state). A window that never had the real foreground (one that a terminal started in the back)
  keeps it behind other windows (U04).
- With no backdrop, Glasscode used 2.4 % of one core with the window hidden and about 6.5 % with it
  shown and one Shell session (its own ticks) (U02). The sweep looks for the cause.
- The chat spinner draws the window on each display frame while the agent of the chat on screen
  works; not measured (U25). The owner decides.

### Missing on Windows or not built

- **No microphone button on Windows.** The voice helper is a macOS program; a Windows helper needs
  its own plan (decision P4, open). The microphone of the Chat view is Claude's own `/voice` and
  stays.
- **Project settings are read only in Settings.** `project_settings::write` exists and is tested;
  nothing calls it (U22).
- **The setup runs only locally,** not on the server (U22). `default_agent` does not reach Today's
  start box. A restart forgets a new worktree in which no session started.
- **File tabs are not saved over a restart,** and they are not places of Back and Forward (U13).
- **The heat map starts at the first run of the new build.** There is no history from before; the
  card says "Since <date>". Merges while Glasscode is closed and pull requests with no session are
  not counted (U07).
- Today's start box starts Claude or Codex sessions; Shell sessions come from New session (U11).
- Jump has no command "Go to Pull requests" (U14, U16).
- The editor has no scroll bar, the caret does not blink, and nothing removes old copies in
  `~/.glasscode/unsaved/`. A killed process loses the text. Only valid UTF-8 below 2 MB with no line
  above 20 000 bytes can be edited (U20).

### Wrong or rough on screen

- **Syntax colours that are wrong, each limited to its own line (U21):** a JavaScript regular
  expression is not recognised; `${…}` in a template literal is not treated specially; an
  apostrophe in JSX text starts a string to the end of the line; a shell string or here-document
  over several lines is coloured on its first line only; a TOML array element `[1, 2]` alone on its
  line is taken for a table head; Markdown has no emphasis, links or indented code; a diff hunk
  beyond the read limits starts as plain code.
- A few pixels at the outer ring of the voice orb's glow show as dots in the dark theme with the
  smooth finish (U19). Esc reaches the voice screen only while it has the keyboard.
- At 900 x 560 and 130 % the sign-in terminal of the first run shows only its top lines, with no
  scroll (U18).
- Closing a session draws the change about 0.75 s after the click (U10; the build before too).
- "Open log folder" and "Open state.json folder" open the same folder: the app has no log folder
  (U16).
- `GLASSCODE_DEBUG_OPEN=settings:<name>` knows only the old section names (U16).
- Texts in other files name old places: "Settings › Automation" for the rules (now Permissions),
  "Settings › Accounts" for Linear (now Integrations), "Settings › Server & devices", and the Jump
  labels "Projects & Git" and "Updates & CLIs" (U16).
- The Linear card (Settings › Integrations, and step 2 of the first run) shows "Signed in" with no
  key when a project has a GitHub remote, and repeats its caption (U16).
- "Asked of me" has checks only from the merge state; "Mine" has checks for its newest 10 (U14).
- The 16 px icon is soft, because its lines are thinner than one pixel (U06).

### In the code (for the sweep or later)

- `git` still runs on the main thread in two places: `configure_github` (the repository of each
  project, when a GitHub setting changes and at the start) and `project_worktrees` (the branch of a
  project whose worktree list is not read yet) (U16, U24).
- `sys::open_path`, `sys::reveal_path` and "Open in <editor>" start a program inside a click handler.
  On Windows this ended the application once for a link (`center::open_url`, corrected); for these
  three no crash was seen (U17).
- `cargo test -p glasscode` may touch the real `~/.glasscode`: its `accounts` folder got a new time
  during the work (U02).
- Sizes in `px(` that the screen slices did not own: Usage, the worktree dialog, session start,
  move, pairing, notification popups, the brand marks and agent icons, the scroll thumb's least
  size, the backdrop page, the other dialogs of `dialogs.rs` (U05, U16). The sweep converts them.
- Code that nothing calls: `proofs.rs`, the old Themes page, `DiffView.path` and `DiffView.lines`;
  the comment at `shell::CenterTab` is stale (U13). The sweep removes them.
