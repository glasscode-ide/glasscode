# Manager v2: Proofs

| | |
|---|---|
| Document | GC-MGR-002, proofs, issue 1 (not run yet) |
| Slice | M01 of [`manager-v2-slices.md`](manager-v2-slices.md) |
| CLIs | claude 2.1.322, codex-cli 0.167.0 (the approved versions in `cli-versions.json` when M01 runs) |

These are the facts the plan relies on but has not tried. Each proof gives:
- a **method**;
- what is **measured**;
- a **pass** rule;
- the **fallback** if it fails;
- the slices that **wait** for it.

M01 fills in "Result" with the real commands and outputs, then the owner answers the decisions that depend on it, before wave 1 starts.

**Rules for running proofs:**
- Every proof runs in a scratch home (`GLASSCODE_HOME=<scratch>`), with scratch account folders (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`) signed in once for the proofs.
- Never with the person's own login folders.
- Live model calls use the cheapest model.
- At most 40 prompts in total. Count them in the result.

## P01 The Claude brain profile
- **Method.** Start `claude -p --input-format stream-json --output-format stream-json --verbose --restricted --tools "" --strict-mcp-config --mcp-config <file> --allowedTools "mcp__glasscode__*" --permission-mode dontAsk --permission-prompts none --disable-slash-commands`, with a stub MCP server that lists two tools. Send one user line. Repeat with `--tools ToolSearch` added.
- **Measure.**
  - the `system/init` tool list;
  - whether the subscription login is used (no API key in the environment);
  - whether a tool call reaches the stub;
  - whether ToolSearch is accepted with `--restricted`;
  - the time to the first token.
- **Pass.** The init tool list holds only the two `mcp__glasscode__` tools (plus ToolSearch in the second run), a call reaches the stub, and the run used the plan login.
- **Fallback.** If `--restricted` is refused with these flags, use `--tools ""` alone and add a `PreToolUse` deny for every non-MCP tool (guarded). If the subscription is not used, stop: the brain cannot run on R6.
- **Waits.** M34, M50.

## P02 The Codex brain profile
- **Method.** Run `codex debug prompt-input` with each candidate flag set:
  - `--disable shell_tool --disable apps --disable plugins --disable multi_agent --disable memories`
  - `-s read-only -c sandbox_workspace_write.network_access=false --ignore-user-config --ignore-rules`
  - `-c mcp_servers.glasscode.command=<stub> -c mcp_servers.glasscode.default_tools_approval_mode="approve" -c mcp_servers.glasscode.required=true`

  Then one `codex exec` and one `codex exec resume <id>` with the same flags.
- **Measure.**
  - the model-visible tool list (look for `apply_patch` and `collaboration.*`);
  - whether the MCP call runs without a prompt;
  - whether resume keeps the thread;
  - whether the login used is the ChatGPT sign-in.
- **Pass.** No shell, patch or agent-spawning tool is visible, MCP calls run, and resume continues the conversation.
- **Fallback.** If a tool cannot be removed, the stated invariant is "read-only sandbox, network off". The tool may be visible, but it cannot write or reach the network. This is recorded in design section 7.
- **Waits.** M50.

## P03 Delivering the memory brief to interactive sessions
- **Method.**
  - Claude: start a session with `--append-system-prompt "<brief v1>"`, quit, then resume it with `<brief v2>`, and look at what the model sees (ask it to repeat the brief's marker).
  - Claude, second variant: a `SessionStart` hook that prints `{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"<brief>"}}`, on start, on resume and after `/compact`.
  - Codex: `-c developer_instructions=<brief>` on start and on `codex resume`.
- **Measure.** Which brief the model sees in each case, the size limits, and whether the hook output appears on resume and after compaction.
- **Pass.** At least one Claude path gives the current brief on start and on resume.
- **Fallback.** Give the brief at start only, and give changes as the first lines of the next prompt Glasscode sends (marked as from Glasscode). Say in the plan that a resumed session may see an older brief until it searches.
- **Waits.** M20.2.

## P04 The native memory switches
- **Method.**
  - Claude: pass `{"autoMemoryDirectory": "<scratch>/agent-notes/p/claude"}` and, separately, `{"autoMemoryEnabled": false}` in `--settings`. Ask the model to remember a fact, then check where the file was written and whether `/memory` shows the folder.
  - Codex: run `-c memories.use_memories=false -c memories.generate_memories=false` and, separately, `-c memories.disable_on_external_context=true`. Check with `codex debug prompt-input` whether memories are in the prompt.
- **Measure.** Whether each key is honoured, and whether Claude writes to the custom folder without asking.
- **Pass.** At least the redirect for Claude and the off switch for Codex work.
- **Fallback.** Leave native memories on. Label them in the brief as a second memory that Glasscode does not check. Harvest them as proposals (M35).
- **Waits.** M22 (D19 part), M35.

## P05 New Claude hook events
- **Method.** Add `StopFailure`, `PostCompact`, `SubagentStart`, `PermissionRequest`, `PermissionDenied`, `SessionEnd` and `CwdChanged` to the `--settings` hooks, plus one made-up event name. Then:
  - an interrupt (Esc during a turn);
  - a forced API error (an invalid model name);
  - a `kill -9` of the agent;
  - a stop-hook continuation.
- **Measure.**
  - which events arrive, with their payload fields (`prompt_id`, `notification_type`, `background_tasks`);
  - whether the unknown name breaks the whole `--settings` block;
  - the transcript's interrupt marker;
  - the lag between `Stop` and the transcript's last line.
- **Pass.** The turn state machine of M30 can close every case with one cause.
- **Fallback.** For each missing signal, use the watchdog of M30: no transcript growth plus a dead process means `Crashed`, and the terminal back at the prompt with a turn still open means `Interrupted`.
- **Waits.** M30.

## P06 Codex turn ends and hooks without the trust bypass
- **Method.** In a Codex session started by Glasscode, install a hook through `-c hooks…` and through a `hooks.json` in `CODEX_HOME`, and see whether it runs without `--dangerously-bypass-hook-trust`. Read the rollout for `task_complete`, `turn_aborted` and the user-message items. Check the `notify` setting: if a `notify` program is already configured, find out whether Glasscode can add its own without replacing it.
- **Measure.** Which path reports a turn end, and how long after the turn.
- **Pass.** A turn end is known within 2 seconds without the bypass flag.
- **Fallback.** Use the rollout only: tail it, with the reconciler as the safety net. Never use the bypass, because it also runs hooks the repository carries.
- **Waits.** M30.

## P07 The session profile on the real path layout
- **Method.** Generate the person, automation and quarantine profiles (M22 code, run early as a script) for Claude and Codex. In a scratch worktree under `<scratch>/worktrees/`:
  - edit a file of the worktree;
  - `cat` and Read `authority/policy.json`, `~/.ssh/id_*` and `.env`;
  - run `gh pr list`, `git commit` in a linked worktree, `cargo build`, `npm install`;
  - call the Glasscode socket;
  - write the redirected `MEMORY.md` in agent notes;
  - try a `!` negation for a home-relative path.
- **Measure.**
  - what is allowed and what is refused;
  - whether gh and git reach the Keychain from inside the macOS sandbox;
  - whether the login still works from an account folder;
  - whether hook events still arrive.
- **Pass.** Work in the own worktree runs, credentials and authority files are refused, and hooks arrive.
- **Fallback.** Wherever the sandbox breaks a tool, that tool runs outside the sandbox for the person profile only, and the row in design section 7 changes to "guarded".
- **Waits.** M22.

## P08 Hook integrity against a hostile repository
- **Method.** Put a `.claude/settings.json` in a scratch repository with its own hooks and `disableAllHooks`, plus a `glasscode` program on its own `PATH` entry. Start a session with Glasscode's absolute-path hooks.
- **Measure.** Whether Glasscode's hooks still run, and which program runs.
- **Pass.** Glasscode's hooks run from their absolute path, and a project setting cannot turn them off.
- **Fallback.** If a project file can turn them off, mark the session "unreported" in the UI and rely on the rollout or transcript tail and the reconciler for it.
- **Waits.** M22, M30.

## P09 Where the person's prompts come from
- **Method.** On the owner's own Mac, for 5 working days, M13's ledger runs in measure-only mode. It counts the origin of each prompt: composer, terminal keys or pasted. It stores counts only, never text.
- **Measure.** The share of each origin.
- **Pass.** Only informational. The owner sees the numbers before the tiers of D1 are fixed, because most prompts are typed in terminals today, and tier 2 cannot back confirmed facts.
- **Waits.** The owner's confirmation of D1.

## P10 What a quit does to running agents
- **Method.** With a Claude and a Codex session working, quit Glasscode normally, then force-quit it. Check whether the agent processes live on, and what their transcripts show.
- **Measure.** Process state, and the last transcript lines.
- **Pass.** The facts are written down for the lifetime rules of design section 10.
- **Waits.** M41.

## P11 Costs
- **Measure.**
  - the tokens and time of a fresh wake with a 4,000-character situation, on Claude (the cheapest model and the default model) and Codex;
  - the start time of `glasscode hook`;
  - a git commit in a memory repository of 10 MB;
  - a search over 10 MB of Markdown;
  - a journal append.
- **Pass.** The numbers go into the budgets of design section 9 and D6. A fresh wake that costs more than 20,000 tokens or 30 seconds stops wave 5 until the owner decides.
- **Waits.** M50, M51 (budgets).

## P12 How long Claude keeps transcripts
- **Method.** Pass `{"cleanupPeriodDays": 3650}` in `--settings` for a scratch session, and read the documentation of the cleanup. Find out which process deletes old transcripts, and when.
- **Measure.** Whether the setting per session protects that session's transcripts.
- **Pass, and fallback either way.** M24 always copies a bounded digest into the archive. This proof only decides whether the full transcript can also be kept.
- **Waits.** M24.

## P13 The Codex terminal UI attached to an app server
- **Method.** Start `codex app-server --listen unix://<path>` under one scratch `CODEX_HOME`. Run the terminal UI in a PTY with `--remote unix://<path>` and connect a second client.
- **Measure.** Whether the second client gets turn and item events for the UI's thread, can answer an approval, can steer and interrupt, and what the UI shows when it does.
- **Pass.** Record only. If it passes, a later slice can build the Codex session driver on it behind a switch (D14).
- **Waits.** Nothing in release 1.

## P14 Brokered push
- **Method.** Run an automation-profile Claude session whose sandbox network allowlist does not include github.com. Prototype `glasscode git push` as an op that pushes from the app's process with the branch rule.
- **Measure.**
  - whether the agent can finish a "fix and push" task through the op;
  - how many extra turns it needs;
  - whether any other path to GitHub stays open: the browser, or an MCP server.
- **Pass.** The task finishes, and no other route reaches the forge.
- **Fallback.** Use forge rules only (D29). Autopilot pushes only where branch protection holds.
- **Waits.** Owner decision O7, then M23.

## P15 Tool-call timeouts
- **Method.** A stub MCP tool that answers after 30 s, 2 min, 5 min and 10 min, on both CLIs.
- **Measure.** When each CLI gives up.
- **Pass.** The time is written down. It decides whether an op that waits for the person can keep the tool call open, or must return `Waiting` with a pending id.
- **Waits.** M34.

## P16 Claude sessions messaging each other
- **Method.** Two Claude sessions of the same account. Try the native cross-session message features (`SendMessage`, the inbox socket), and the setting `crossSessionInbound: "refuse"` in `--settings`.
- **Measure.** Whether one session can make another act, and whether the setting stops it.
- **Pass.** A way to refuse is confirmed for automation and quarantine profiles.
- **Fallback.** A `PreToolUse` deny on those tools (guarded), and a label in design section 7.
- **Waits.** M22.

## P17 Sleep timing and the lease clock
- **Method.** GPUI's `on_system_sleep` is asynchronous and does not hold sleep, so measure three things across lid close on battery, lid close on mains, and idle sleep:
  - when the GPUI notice arrives, against a monotonic and a wall clock;
  - how long an I/O Kit power registration (`IORegisterForSystemPower`, with `IOAllowPowerChange` delayed) can hold sleep;
  - whether the network still works in that window, by a request to the test server.

  Also check whether `std::time::Instant` advances during sleep on macOS.
- **Measure.** The time available before sleep, and the clock behaviour.
- **Pass.** At least 5 s, held, with network, to hand home over; and a lease rule that never uses a clock that stops during sleep.
- **Fallback.** If there is no time to hand over, the server takes home after the lease expires (D5, unplanned loss).
- **Waits.** M58.

## Stop conditions

Wave 1 does not start, and the owner decides first, when any of these happens:
- P01 shows the brain cannot use the subscription in the restricted profile;
- P03 has no path that delivers a current brief at start;
- P06 shows a Codex turn end cannot be seen without the bypass;
- P11 shows a fresh wake above the limit.
