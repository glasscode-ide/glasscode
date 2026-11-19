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
- **Result: pass** (2 prompts, haiku). Script: `p01-claude-brain.sh <claude-dir> <codex-home> <scratch> [none|toolsearch]`. The stub is `stub-mcp.py` (tools `status`, `note`). The user line is the 4,000-character situation of `situation.py` followed by "Call the glasscode status tool once, then answer with one line: the marker it returned."
  ```
  claude -p --input-format stream-json --output-format stream-json --verbose --include-partial-messages \
    --restricted --tools "" --strict-mcp-config --mcp-config <scratch>/mcp.json \
    --allowedTools "mcp__glasscode__*" --permission-mode dontAsk --permission-prompts none \
    --disable-slash-commands --model haiku
  ```
  | | `--tools ""` | `--tools ToolSearch` |
  |---|---|---|
  | `system/init` `tools` | `["mcp__glasscode__note","mcp__glasscode__status"]` | `["ToolSearch","mcp__glasscode__note","mcp__glasscode__status"]` |
  | `slash_commands`, `skills` | `[]`, `[]` | `[]`, `[]` |
  | `apiKeySource` | `"none"` (subscription; `claude auth status`: `authMethod: claude.ai`, plan `max`) | `"none"` |
  | Calls reaching the stub | 1 (`tools/call status`) | 1 (after one ToolSearch call) |
  | Answer | `STUB-STATUS-OK-7731` | `STUB-STATUS-OK-7731` |
  | Spawn to `init` | 651 ms | 457 ms |
  | First token / result after the user line | 2,275 ms / 3,789 ms | 1,874 ms / 4,770 ms |
  | API requests (`num_turns`) | 2 | 3 |

  `--restricted` is accepted with all these flags; no warning on stderr. Notes for M50:
  - The environment was `env -i` with only `HOME`, `PATH`, `TERM`, `CLAUDE_CONFIG_DIR` and `DISABLE_AUTOUPDATER=1`, so no API key could be used.
  - `init` still lists `agents` (`claude`, `Explore`, `general-purpose`, `Plan`, `statusline-setup`, `web-fetch`) and four built-in plugins (`cc-plugin-agents-md`, `cc-plugin-telemetry`, `cc-plugin-privacy`, `cc-plugin-plugin-authoring`, `source: builtin`). There is no Agent tool to start them. M50's `brain_tool_list_is_exactly_glasscode` should check `tools` only, and record the other lists.
  - The stream has one `rate_limit_event` per turn. Its `unifiedWindows` held only `seven_day` (`utilization`, `resetsAt`, `status`). There was no five-hour window on this plan (exec-30's "which windows").
  - Claude 2.1.328 first sends the MCP `server/discover` probe of protocol `2026-07-28`. When that gets an empty result it falls back to `initialize` with `2025-11-25`. M34's `glasscode mcp` must answer or reject `server/discover` cleanly.

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
- **Result: pass with the fallback, plus one more flag** (2 prompts, `gpt-5.6-luna`, reasoning `low`). Script: `p02-codex-brain.sh <claude-dir> <codex-home> <scratch> probe|live`.
  - **Probe (no prompt).** Codex 0.167 runs in *code mode*: the model sees `functions.exec` (JavaScript), `functions.wait` and `functions.request_user_input`. The other tools are nested in `exec` and documented in its description (`codex-tools.py` lists them).

    | Flag set (`codex debug prompt-input -c model="gpt-5.6-luna" … hello`) | Nested in `exec` |
    |---|---|
    | none | `apply_patch`, `exec_command`, `write_stdin`, `list_mcp_resource_templates`, `list_mcp_resources`, `read_mcp_resource`, `request_plugin_install`, `view_image` |
    | `--disable shell_tool --disable apps --disable plugins --disable multi_agent --disable memories` | `apply_patch`, `list_mcp_resource_templates`, `list_mcp_resources`, `read_mcp_resource`, `view_image` |
    | the same with `-c sandbox_mode="read-only" -c sandbox_workspace_write.network_access=false` and the three `mcp_servers.glasscode.*` keys | the same |

    - No `collaboration.*` or agent-spawning tool is visible with any set.
    - The MCP server's tools are *deferred*. They do not appear in prompt-input ("Some deferred nested tools may be omitted from this description. They are still available on the global `tools` object and listed in `ALL_TOOLS`"), so only the live run shows them.
    - `apply_patch` stays. Neither `-c include_apply_patch_tool=false`, `-c tools.apply_patch=false` nor `--disable code_mode_host` removes it.
    - **New finding.** Even with a scratch `CODEX_HOME`, the prompt lists the person's own skills from `~/.agents/skills` (skill root `r0`, 7 skills on this Mac). `-c skills.include_instructions=false -c skills.bundled.enabled=false` removes the list (42,837 to 38,353 characters). The brain profile needs both keys.
  - **Live.** The command was `codex exec -m gpt-5.6-luna -c model_reasoning_effort="low" <the flags above> -c skills.include_instructions=false -c skills.bundled.enabled=false -s read-only --ignore-user-config --ignore-rules --skip-git-repo-check --color never --json -c mcp_servers.glasscode.…` with the situation of P01. Stdin must be `/dev/null`: with a pipe, `exec` waits on "Reading additional input from stdin...". The results:
    - `item.completed mcp_tool_call status completed` → `STUB-STATUS-OK-7731`. The call ran with no approval prompt, and the stub logged 1 `tools/call`.
    - `exec resume <thread>` kept the thread (the same `thread_id`) and answered the earlier marker.
    - Asked to create `probe.txt` with `apply_patch`, it answered "writing is blocked by the read-only sandbox". stderr showed `patch rejected: writing is blocked by read-only sandbox; rejected by user approval settings`, and no file was written.
    - The login was ChatGPT (`codex login status`: "Logged in using ChatGPT"; no API key in the `env -i` environment).
  - **Fallback applied.** The invariant is "read-only sandbox, network off; `apply_patch` and `view_image` are visible but cannot write, and no tool reaches the network". Design D4 and section 7 now say so, and D4 adds the two skills keys.

## P03 Delivering the memory brief to interactive sessions
- **Method.**
  - Claude: start a session with `--append-system-prompt "<brief v1>"`, quit, then resume it with `<brief v2>`, and look at what the model sees (ask it to repeat the brief's marker).
  - Claude, second variant: a `SessionStart` hook that prints `{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"<brief>"}}`, on start, on resume and after `/compact`.
  - Codex: `-c developer_instructions=<brief>` on start and on `codex resume`.
- **Measure.** Which brief the model sees in each case, the size limits, and whether the hook output appears on resume and after compaction.
- **Pass.** At least one Claude path gives the current brief on start and on resume.
- **Fallback.** Give the brief at start only, and give changes as the first lines of the next prompt Glasscode sends (marked as from Glasscode). Say in the plan that a resumed session may see an older brief until it searches.
- **Waits.** M20.2.
- **Result, Claude: pass, by the SessionStart hook** (5 prompts, haiku). Script: `p03-brief.sh <claude-dir> <codex-home> <scratch> start resume snapoff`. These are interactive sessions in a pseudo-terminal (`pty-drive.py`), launched as `agents::launch` launches them. Each one has the hook of `claude-hooks.py` that logs every event, and a SessionStart hook (`hook.py --brief`) that prints `{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"Glasscode hook brief marker: HOOK-Vn"}}`. Every phase asked the same question: "which APPEND-V marker is in your system prompt now, and which HOOK-V markers were given to you as SessionStart hook context?"

  | Phase | Launch | SessionStart `source` | Answer (from the transcript) |
  |---|---|---|---|
  | start | `--session-id S --append-system-prompt "…APPEND-V1"`, hook gives HOOK-V1 | `startup` | `system=APPEND-V1; hook=HOOK-V1` |
  | resume | `--resume S --append-system-prompt "…APPEND-V2"`, hook gives HOOK-V2 | `resume` | `system=APPEND-V1; hook=HOOK-V1, HOOK-V2` |
  | resume, snapshot off | `--resume S --system-prompt-snapshot off --append-system-prompt "…APPEND-V3"`, HOOK-V3 | `resume` | `system=APPEND-V3; hook=HOOK-V1, HOOK-V2, HOOK-V3` |
  | after `/compact` in that session (brief file changed to HOOK-V4 first) | — | `compact` (fired 25 ms before `PostCompact`) | `system=APPEND-V3; hook=HOOK-V4` |

  What it shows:
  - **The SessionStart hook delivers the current brief** on start, on resume and after compaction. The transcript keeps each one as an `attachment` of type `hook_additional_context`.
  - **Old briefs stay** in the conversation after a resume, until a compaction drops them. A brief must therefore say that it replaces every earlier one, and name its memory commit.
  - **`--append-system-prompt` is stale on resume** by default. 2.1.328 records the system prompt in the transcript (an `attachment` of type `prompt_snapshot`, 127 KB here) and sends that record on every resume ("even when a later launch passes different text, until the conversation is compacted", `claude --help`). With `--system-prompt-snapshot off` the new text is used. A session can therefore get a current brief from either path. The hook is the one M20.2 uses, because it also runs after `/compact`, and the flag would change prompt caching for the person's own sessions.
  - **Size.** The docs (code.claude.com/docs/en/hooks) cap `additionalContext` at 10,000 characters per string. Over the cap, the text is saved to a file and replaced by its path and a 2,000-character preview, and Claude is not told to read it. M20.2 keeps a brief under 10,000 characters. This proof did not test past the cap.
  - The fallback is not needed for Claude.
  - **Privacy.** A 2.1.328 transcript also holds `credential_org` and `session_context` attachments. Transcripts are never committed (rule 13), and M24's digest must leave them out.

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
- **Result, trust part (no prompt).** Script: `p06-codex-turn-end.sh … trust`. It writes the hooks of `codex-hooks.py`: ten events, each logged by `hook.py`, once as `$CODEX_HOME/hooks.json` and once as `-c hooks.<Event>=[…]` session flags. It then asks `codex app-server` for `hooks/list` (`appserver-client.py`):

  | Case | `trustStatus` of the `stop` hook: `hooks.json` / session flags |
  |---|---|
  | as written | `untrusted` / `untrusted` |
  | `-c 'hooks.state."<key>".trusted_hash="<currentHash>"'` as a session flag | `untrusted` / `untrusted` (a flag cannot grant trust) |
  | `[hooks.state."<key>"] trusted_hash = "<currentHash>"` in `$CODEX_HOME/config.toml` | `trusted` / `untrusted` |
  | the same entry after the hook's command changed | `modified` / `untrusted` |
  | the entry also for the session-flag key `/<session-flags>/config.toml:stop:0:0` | `trusted` / `trusted` |

  - Keys are `<file>:<event>:<group>:<handler>`, and the hash is `sha256:…` of the hook's definition, as `hooks/list` reports it in `currentHash`.
  - The TUI's `/hooks` review writes the same `hooks.state` table, through `config/batchWrite` (strings in the 0.167 binary: `HookStateToml`, `trusted_hash`, `trusted_content_hash`, "config/batchWrite failed while updating hook trust in TUI").
  - **So Glasscode can trust its own hooks without the bypass.** At launch, as `ensure_codex_trusts` does for the folder, it adds one `hooks.state` entry per Glasscode hook, with the hash that `hooks/list` gives. A repository's hooks stay untrusted until the person reviews them, and a changed Glasscode hook becomes `modified` (skipped) until Glasscode writes the new hash.
  - `SessionEnd` and `Interrupt` timeouts are clamped to 3 s (warning from `hooks/list`).

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
- **Result: fail as specified; pass with one more key in `--settings`** (no prompt). Script: `p08-hostile-repo.sh <claude-dir> <codex-home> <scratch>`.
  - **Setup.**
    - The scratch repository commits a `.claude/settings.json` with `"disableAllHooks": true`, its own `SessionStart` hook, and `"env": {"PATH": "<repo>/evilbin:/usr/bin:/bin"}`. `evilbin/glasscode` logs `evil-glasscode-on-PATH`.
    - Glasscode's `--settings` has two `SessionStart` hooks: `/…/python3 /…/hook.py … glasscode-absolute`, and the bare `glasscode hook`. A "good" `glasscode` is first on the session's own `PATH`.
    - Each variant runs `claude -p --input-format stream-json …` with no user line; SessionStart fires anyway.

  | Variant | Folder | Hooks that ran |
  |---|---|---|
  | A: Glasscode's `--settings` as today (no `disableAllHooks` key) | trusted | **none** |
  | B: `--settings` adds `"disableAllHooks": false` | trusted | `glasscode-absolute`, `repo-hook`, `evil-glasscode-on-PATH` |
  | C: as A | not trusted | **none** |
  | D: as B | not trusted | `glasscode-absolute`, `repo-hook`, `evil-glasscode-on-PATH` |
  | E: B plus `"allowManagedHooksOnly": true` in `--settings` | trusted | the same as B (the key acts only from managed settings) |

  What it shows:
  - A project file **does** turn off Glasscode's hooks when `--settings` leaves `disableAllHooks` unset. A key that `--settings` does not set keeps its value from the project file, as the settings docs say.
  - With `"disableAllHooks": false` in `--settings`, the flag outranks the project and Glasscode's hooks run.
  - The repository's own hooks still run beside them. Hook lists merge, and only managed settings can stop that.
  - A hook named by its bare name runs the **repository's** program. The project's `env.PATH` applies to hook commands, and in `-p` mode even for a folder whose trust dialog was not accepted.
  - **Applied.** Glasscode's session settings (`claude_hook_settings`) always set `"disableAllHooks": false`, and every hook command is an absolute path, as D11 requires. The design's D11 row and section 7 now say both. The fallback (mark "unreported" and tail) remains for the case where a future CLI changes the precedence. M22's test `project_settings_cannot_disable_glasscode_hooks` repeats variant A.

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
- **Result so far: stop condition hit on Codex.** The wake is the P01 and P02 runs: the 4,000-character situation of `situation.py` and one stub tool call. Tokens are as reported by the CLI and include cached input.

  | Wake | Model requests | Input tokens (cached) | Output | Total tokens | Time to answer |
  |---|---|---|---|---|---|
  | Claude, haiku (`claude-haiku-5-5`), P01 `--tools ""` | 2 | 10,678 (5,299 read, 5,375 written) | 49 | **10,727** | 3.8 s (first token 2.3 s) |
  | Claude, haiku, P01 `--tools ToolSearch` | 3 | 17,827 (13,252 read) | 133 | 17,960 | 4.8 s |
  | Codex, `gpt-5.6-luna` low, P02 | 3: 11,126 + 12,576 + 12,633 | 36,221 (32,768 cached) | 114 | **36,335** | 7.5 s |

  The Codex wake is above the 20,000-token limit; Claude's default model has not been run yet. Each Codex request carries about 11,000 tokens of Codex's own prompt (the code-mode `exec` description, the base instructions and the permission text), and a wake with one tool call takes three requests. By the rule above this stops wave 5 until the owner decides. It was reported to the main agent at once.

  **Costs without a model** (no prompt; `p11-costs.sh <claude-dir> <codex-home> <scratch> [app binary]`). The machine was an Apple M4 Pro with an APFS SSD, on macOS 27.0.1. Commands are timed from one Python process (`p11-time.py`), so the numbers include the process start (`/usr/bin/true`: p50 2.1 ms, p95 4.0 ms).

  | Path | p50 | p95 | Budget (design section 9) |
  |---|---|---|---|
  | Today's Mac hook, `sh -c 'cat >> "$GLASSCODE_EVENTS"; printf …'`, 1.6 KB payload | 8.1 ms | 12.3 ms | hook to journal ≤ 300 ms |
  | `glasscode hook` (the app binary as `Glasscode cli hook`) | 4.9 ms | 6.1 ms | same |
  | Git commit of one edit, memory repository of 10,174,211 bytes of Markdown in 2,000 files | 18.2 ms | 19.6 ms | — |
  | The same with `git add` in one shell command | 27.4 ms | 29.5 ms | — |
  | `git grep -i -n` for one word over those 10 MB | 29.8 ms | 35.1 ms | memory search ≤ 30 ms p95 |
  | `git grep -i -l --all-match` for two words | 29.8 ms | 35.8 ms | same |
  | `grep -r -i -n` for one word | 161.7 ms | 173.7 ms | same |
  | Journal append, `write` only (Rust, 500 lines of about 400 bytes, `p11-journal.rs`) | 2 µs | 4 µs | ≤ 1 ms p95 |
  | Append plus `fsync(2)` | 29 µs | 65 µs | same |
  | Append plus `File::sync_data`, which on Apple is `fcntl(F_FULLFSYNC)` | 4,026 µs | 4,566 µs | same |

  What follows for the plan:
  - **The journal budget holds** with `write`, and with `fsync(2)`. It does not hold with a full flush on each append, which takes about 4 ms. M14 keeps `write` on the hot path and flushes in the writer thread. D9 already wants the effect ledger's claim made durable before the action, so each claim will cost about 4 ms with `sync_data`. That is within "op dispatch ≤ 5 ms" only if no second flush follows. M31 must count it.
  - **`git grep` misses the 30 ms p95 by 5 ms.** M20.1 builds an in-process index or a filter by project key, and stays within budget on the reference load.
  - The hook start is well inside its budget. `glasscode hook` by absolute path (D11) is faster than today's shell line.

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
