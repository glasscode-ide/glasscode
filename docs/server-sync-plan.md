# Server sync: the switch, worktree moves and server sessions

The plan for working on a Mac and continuing on the server, as of 20 November 2026. It sits
beside the Manager v2 plan ([`manager-v2-design.md`](manager-v2-design.md),
[`manager-v2-slices.md`](manager-v2-slices.md)). That plan owns where the manager acts. This
plan owns where sessions and worktrees run, and the moves between the two machines.

## Decisions

| Decision | Made | What it means |
|---|---|---|
| The title bar's "Server mode" / "Laptop mode" switch is the planned hand-over | The owner, 19 Nov (manager plan PR #81) | You press it before you leave; the lid-close sleep hold and the lease are only the fallback |
| Every open session moves, working or idle | The owner, 19 Nov | Any session can be continued on the other side |
| A worktree moves as one unit with all its sessions, never split | The owner, 19 Nov | No two agents edit one worktree in two places |
| One owner per worktree, with a counter (the session runner epoch) | The owner, 19 Nov | A copy that sees a newer counter never runs |
| Worktrees with no open session stay, unless ticked; those with uncommitted work leave a copy on the server | Manager plan PR #82 | They can be opened on the server while the Mac sleeps |
| The phone is parked | The owner, 19 Nov | The phone app becomes the Meta glasses app, with its own plan |
| Caller identity per glasscoded connection ("phase 0a") is the manager plan's M13.2 | Both plans, 20 Nov | One definition of `Caller` |
| New sessions start where the last one ran | This plan, 20 Nov | Phase 1 is opt-in by memory, not server-first by default |

## The moves (phase 2)

Built in PR #87. `docs/architecture.md`, "Moving sessions, and keeping them in sync", describes
the mechanism. In short:

- A **unit** is one worktree with every open session in it. Its key is its path under the home
  folder (`WorktreeKey`), the same on both machines.
- glasscoded keeps an **owner record** per unit in `authority/owners.json` and is its only
  writer. A move runs `owners.begin` → one push and one `worktrees.receive` → per session its
  conversation and a held `sessions.create` → `owners.commit`, which starts them. A failure
  before the commit runs `owners.abort`, and the unit stays where it was. A move that makes no
  progress for 2 minutes is given up, timed by the server's clock.
- Both directions run on the Mac, because they need its disk. In "Laptop mode" the manager's
  home is the server when the moves start. M58 asks it to pause the sessions, calls the moves on
  the Mac, then claims home.

### Op names (cited by the manager plan's M58)

Until M12's registry lands, they are functions in `glasscode_core::switch`. M12 then registers
them with `OpHost::SessionHost`, implemented by `AppHost` only. `DaemonHost` refuses each of
them with `Unavailable { host: SessionHost, queued: false }`.

| Op | Function | Group / effect / retry | What it does |
|---|---|---|---|
| `switch.units` | `units_to_server`, `units_to_mac`, `idle_worktrees` | Read / Read / Idempotent | The units, each with its sessions; worktrees with no session |
| `switch.check` | `check_to_server`, `check_to_mac` | Read / Read / Idempotent | What stops each unit: an old server, a missing project, an account not signed in, a working agent, a shell that restarts |
| `switch.move` | the switch's loop (`switch_ui.rs`), four at a time | Worktree / Create / Verifiable | Every unit with an open session, plus ticked ones; parks copies of the unticked ones |
| `worktree.move` | `move_unit_to_server`, `move_unit_to_mac`, then `close_on_server` | Worktree / Create / Verifiable | One unit, under its owner record |
| `worktree.owner` | `owner` (RPC `owners.get`) | Read / Read / Idempotent | `{ owner, epoch, moving }`; the probe M31 uses for a `Verifiable` move |
| `session.move` | `move_one` | as `worktree.move` | Moves the session's whole worktree and names the sessions that come with it |

Behind them, `owners.begin`, `owners.commit` and `owners.abort` take the Mac's `MachineId` in
their parameters until M13.2 restricts them to `Caller::Machine`.

### Rules M58 relies on

- Before `owners.begin`, no session in the unit may be working. With the manager on, M58 has
  paused them. Otherwise the unit waits for its turn to end, or "Move now" interrupts it.
- Every move to the server stops the unit's agents on the Mac before the snapshot. Until the
  move ends, nothing starts in the unit (`ensure_terminal`), and messages wait in the queue.
  Leaving the server, glasscoded stops its sessions at `owners.begin`.
- A unit whose move failed is never left paused. Sessions stopped on the Mac resume when opened;
  the server starts its own again when a move away is given up. M58 resumes the ones it paused,
  with the same message it sends after a move.
- A copy left behind never runs. The Mac stops a session whose worktree the server owns, but
  keeps it listed; opening it brings the worktree back first. "Laptop mode" also lists the
  worktrees the server owns with no session there. A take-over from the mirror continues every
  Mac session of the worktree.
- A Shell session moves as a fresh shell in the same folder, and what ran in it stops.
  `switch.check` lists such sessions. An older server is not refused: its sessions move one at a
  time, without an owner record, and the check says so.

## Phases

| Phase | State | Next step |
|---|---|---|
| 0a. Caller identity per connection | The manager plan's M13.2 | It cites this plan's design notes: both Mac connections declare `Machine`; the declaration is handled in order; existing `authorized_keys` lines are never rewritten |
| 0b. The phone gate's allow-list | Parked with the phone | Needs the iOS app's real calls |
| 1. Server sessions | PR #88: new sessions start where the last one ran | Project setup scripts on the server; review notes carried to server sessions |
| 2. The switch moves worktrees | PR #87 | The conversation's unsent tail instead of the whole file (the mirror already holds most of it); the Mac's final sync and "asleep" flag inside M58's sleep hold, as the fallback |
| 3. Files and terminal on the phone | Parked with the phone | — |
| 4. Reach from anywhere | Parked with the phone | — |
| 5. The Today board on the daemon | Not started; needs M11's `Hlc` | Agreed with the manager plan: `board.json` becomes an M10 `Store` (in `ALL_STORES`, with a `DATA_MAP` row; refuse on damage, fallback allowed); each entry carries an `Hlc` stamp, so the Mac and the server merge per entry, never by wall clock; core gets plain functions (`apply_op(TodayOp)`, `view(day)`, `prepare(day)`) that M12's board ops call on either host; while the server is unreachable the Mac keeps working on its own copy and merges on reconnect, so the server holds the board without the Mac depending on it |
| 6. State and actions on the manager's foundations | Waits for M10–M13, M50, M57 | An event stream instead of polling; `MachineId` and `ProjectKey` instead of matching by path; the `glasscode` command's executor on the daemon |
| 7. Automatic manager home | The manager plan's M58 | Calls the ops above |

## Testing

- Unit tests: `owners` and `switch` in `glasscode_core`.
- End to end: `crates/glasscode_core/tests/switch_e2e.rs` (`--ignored`), against the Docker
  test server (`script/test-server/run.sh`) with glasscoded built from the branch
  (`ARCHS=aarch64 script/build-glasscoded.sh` on Apple silicon), installed at
  `~/.glasscode/bin/glasscoded` there. It moves a worktree with two sessions to the server and
  back, aborts a move, parks a worktree and opens it on the server.
- Still to measure: 30 worktrees in two projects (10 and 20), two with several sessions. That is
  M58's acceptance test, and it times the moves.
