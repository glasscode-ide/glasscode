# Manager v2: Proofs

| | |
|---|---|
| Document | GC-MGR-002, proofs, issue 3 (M01 run of 2026-11-19 and 2026-11-20; P11 stopped wave 5 and was decided, see "Stop conditions") |
| Slice | M01 of [`manager-v2-slices.md`](manager-v2-slices.md) |
| CLIs | claude **2.1.328** and codex-cli 0.167.0, as installed on the owner's Mac when M01 ran. There is no `cli-versions.json` yet. The recorded help is `script/cli-help/claude-2.1.322.txt`, and the diff of `claude --help` between 2.1.322 and 2.1.328 changes wording only, no flag. |
| Scripts | [`script/manager-v2-proofs/`](../script/manager-v2-proofs): one per proof, each `pNN-*.sh <claude-config-dir> <codex-home> <scratch-dir> [mode]`, plus the shared drivers (`lib.sh`, `pty-drive.py`, `stream-drive.py`, `appserver-client.py`, `stub-mcp.py`, `hook.py`). |

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

## Run notes (M01, 2026-11-19)

- **Accounts.** `CLAUDE_CONFIG_DIR` and `CODEX_HOME` were scratch account folders, signed in once for the proofs (Claude: the person's plan, `authMethod: claude.ai`; Codex: ChatGPT). Every agent started under `env -i` with only `HOME`, `USER`, `PATH`, `TERM`, `LANG`, `TMPDIR`, the account variable and `DISABLE_AUTOUPDATER=1` (`lib.sh`). So no API key, no `CLAUDECODE` or `CLAUDE_CODE_*` variable of the calling session, and no messaging socket reached them. The person's own `~/.claude`, `~/.codex` and `~/.glasscode` were never used. The running Glasscode app was not touched; its installed binary ran only as `Glasscode cli hook`, with a scratch `GLASSCODE_HOME` and `GLASSCODE_EVENTS` (P11).
- **A prompt** is one turn submitted to a model: a line typed in an interactive session, one `stream-json` user message, one `codex exec` or `exec resume`, a steer, and any turn that a message started (another session's message, a subagent's hand-back). A subagent's own run (P05's Explore agent) and a stop-hook continuation are part of the turn that started them, and do not count again. A `/compact` counts as one, because it makes a summary request. Starts that send no user line (SessionStart and SessionEnd hooks, `prompt-input`, `hooks/list`, `codex sandbox`, `claude doctor`, `claude mcp list`) count zero. The models were Claude `haiku` (`claude-haiku-5-5`) and Codex `gpt-5.6-luna` at reasoning `low`, listed by `codex debug models` ("Legacy fast model for simpler tasks").
- **"Docs only, not run"** marks a statement that rests on the vendor docs (code.claude.com, learn.chatgpt.com, read 2026-11-19) or on strings in the CLI binary, and that no run in this proof checked. Everything else was measured.
- **Raw captures** (transcripts, rollouts, hook logs, screens) stay on the owner's Mac under the scratch folders (`~/.glasscode-proofs/<proof>/raw/`). They are not committed (rule 13). M02's stand-ins can be recorded from them.

**Prompts used: 40 of 40** (2026-11-19 and 2026-11-20), counting every model turn that a message started. That includes P05's subagent hand-back turn, which no tally line has, and P16's receiver turn. Two more tally lines reached no model and count 0: a Codex start whose keys went into an update dialog, and a P05 run whose step list did not parse. Each has a correction line in the tally.

| Proof | Prompts | What they were |
|---|---|---|
| P01 | 2 | one wake per variant (`--tools ""`, `--tools ToolSearch`) |
| P02 | 2 | `codex exec` (the wake), `codex exec resume` |
| P03 | 6 | Claude: start, resume, resume with the snapshot off, `/compact`, the question after it; Codex: the resume with a SessionStart hook |
| P04 | 3 | Claude redirect; Codex memories on and off |
| P05 | 6 | interrupt, API error, two `kill -9` tries, the continuation, and the hand-back turn it caused |
| P06 | 2 | Codex TUI start and resume (also P03's Codex start and first resume) |
| P07 | 4 | Claude person profile twice (the first had two script faults), automation profile (with P14), and the person session resumed (Keychain control, git metadata, exec-30's stream-json resume) |
| P10 | 4 | proxy, normal and force, Claude and Codex |
| P11 | 2 | Claude default model; Codex with the trim keys |
| P13 | 3 | TUI turn with an approval; TUI turn; the second client's steer |
| P15 | 3 | Claude, Codex default, Codex with `tool_timeout_sec=900` |
| P16 | 3 | accept (sender, receiver's turn), refuse (sender) |

**Usage.** The proofs' prompts were small turns of the cheapest models. The account's usage readings are not recorded here (rule 13).

**The person's skill list went out in some runs.** The Codex runs of P04, P06, P10 and P13 did not pass `-c skills.include_instructions=false`, so their prompts carried the names and descriptions of the person's skills in `~/.agents/skills` (see P02). P02, P11 and P15 passed it.

**Owner steps** (to be batched by the main agent):
- **P17**: three sleep cases with `p17-sleep.sh`.
- **P10**: quit and force-quit the real app with `p10-quit.sh … watch` running, including a Claude turn cut in the middle.
- **P09**: five days of M13's measure mode.
- **M01's Windows line**: P03 and P05 on the owner's Windows machine.

**Could not verify in this run:**
- **Docker test server.** The Server line (P03 and P05 in a tmux session on `script/test-server` with stand-ins) did not run in M01; it moves to M02. The image does install tmux, and Docker runs here. The reasons it moved are these. The shared `glasscode-test-server` container was in use by another run, and `run.sh` removes any container of that name first. Today's `fake-claude` emits only `SessionStart`, `UserPromptSubmit`, `Notification` and `Stop`, and ignores hook output, so it cannot show a brief or a turn end beyond tmux plumbing. M02's stand-ins replay the events recorded here, and M02 now runs the Server line with them (slices, M01 and M02).
- **Windows.**
- **Brokered push.** P14's prototype op was not built.
- **Claude, cut mid-turn by a quit** (P10).
- **Codex compaction and the brief** (P03).
- **`PermissionDenied`** (P05: only auto mode's classifier emits it).
- **The `!` negation** (P07).
- **The live sleep window** (P17).
- **P02's pass rule at run time:** `ALL_TOOLS`, and whether any model-visible tool reaches the network.
- **P07, the Claude automation profile:** the Keychain control and the git metadata probes ran in the person profile only. The narrowed Codex profile ran under `codex sandbox`, not in a session.
- **P13's isolation:** the shared Codex daemon (a bare `--remote unix://`), and killing a server's process group during a turn.

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
  | `apiKeySource` | `"none"` (subscription; `claude auth status`: `authMethod: claude.ai`) | `"none"` |
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
- **Result: partial; the pass rule is not shown in full** (2 prompts, `gpt-5.6-luna`, reasoning `low`). **Shown:**
  - the write by `apply_patch` was refused by the read-only sandbox;
  - the MCP call ran without an approval prompt;
  - `exec resume` continued the thread;
  - the login was ChatGPT.

  **Not shown:** the full model-visible tool list (`ALL_TOOLS`, since prompt-input leaves out deferred tools), and whether any of those tools reaches the network outside the sandbox. The fallback's invariant covers the writes. A skills key is added. Script: `p02-codex-brain.sh <claude-dir> <codex-home> <scratch> probe|live`.
  - **Probe (no prompt).** Codex 0.167 runs in *code mode*: the model sees `functions.exec` (JavaScript), `functions.wait` and `functions.request_user_input`. The other tools are nested in `exec` and documented in its description (`codex-tools.py` lists them).

    | Flag set (`codex debug prompt-input -c model="gpt-5.6-luna" … hello`) | Nested in `exec` |
    |---|---|
    | none | `apply_patch`, `exec_command`, `write_stdin`, `list_mcp_resource_templates`, `list_mcp_resources`, `read_mcp_resource`, `request_plugin_install`, `view_image` |
    | `--disable shell_tool --disable apps --disable plugins --disable multi_agent --disable memories` | `apply_patch`, `list_mcp_resource_templates`, `list_mcp_resources`, `read_mcp_resource`, `view_image` |
    | the same with `-c sandbox_mode="read-only" -c sandbox_workspace_write.network_access=false` and the three `mcp_servers.glasscode.*` keys | the same |

    - No `collaboration.*` or agent-spawning tool is visible **in prompt-input** with any set. That does not show the pass rule: prompt-input leaves out the deferred tools (next line), and the live run did not record `ALL_TOOLS`. **Not verified.**
    - The MCP server's tools are *deferred*. They do not appear in prompt-input ("Some deferred nested tools may be omitted from this description. They are still available on the global `tools` object and listed in `ALL_TOOLS`"), so only the live run shows them.
    - `apply_patch` stays. Neither `-c include_apply_patch_tool=false`, `-c tools.apply_patch=false` nor `--disable code_mode_host` removes it.
    - **New finding.** Even with a scratch `CODEX_HOME`, the prompt lists the person's own skills from `~/.agents/skills` (skill root `r0`, 7 skills on this Mac). `-c skills.include_instructions=false -c skills.bundled.enabled=false` removes the list (42,837 to 38,353 characters). The brain profile needs both keys.
  - **Live.** The command was `codex exec -m gpt-5.6-luna -c model_reasoning_effort="low" <the flags above> -c skills.include_instructions=false -c skills.bundled.enabled=false -s read-only --ignore-user-config --ignore-rules --skip-git-repo-check --color never --json -c mcp_servers.glasscode.…` with the situation of P01. Stdin must be `/dev/null`: with a pipe, `exec` waits on "Reading additional input from stdin...". The results:
    - `item.completed mcp_tool_call status completed` → `STUB-STATUS-OK-7731`. The call ran with no approval prompt, and the stub logged 1 `tools/call`.
    - `exec resume <thread>` kept the thread (the same `thread_id`) and answered the earlier marker.
    - Asked to create `probe.txt` with `apply_patch`, it answered "writing is blocked by the read-only sandbox". stderr showed `patch rejected: writing is blocked by read-only sandbox; rejected by user approval settings`, and no file was written.
    - The login was ChatGPT (`codex login status`: "Logged in using ChatGPT"; no API key in the `env -i` environment).
  - **Fallback applied.** The invariant is "read-only sandbox, network off; `apply_patch` and `view_image` are visible but cannot write" (shown: the write was refused). Design D4 and section 7 now say so, and D4 adds the two skills keys. **Not verified:** that no model-visible tool reaches the network outside the sandbox. Codex's web search was not checked, and `codex features list` shows `browser_use` and `computer_use` as stable and on. M50 also passes `--disable browser_use --disable computer_use`, and checks the brain's tool list from `ALL_TOOLS` at its first start, not from prompt-input. The P02 method names `--ignore-user-config --ignore-rules`, which `prompt-input` does not accept; the probe used the `-c` forms of the sandbox keys, and the live run passed both flags.

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
  - **Size (docs only, not run).** The docs (code.claude.com/docs/en/hooks) cap `additionalContext` at 10,000 characters per string. Over the cap, the text is saved to a file and replaced by its path and a 2,000-character preview, and Claude is not told to read it. M20.2 keeps a brief under 10,000 characters. This proof did not test past the cap.
  - The fallback is not needed for Claude.
  - **Privacy.** A 2.1.328 transcript also holds `credential_org` and `session_context` attachments. Transcripts are never committed (rule 13), and M24's digest must leave them out.
- **Result, Codex: pass, by the SessionStart hook; `developer_instructions` is stale on resume** (3 prompts, counted under P06 and P03). These are P06's TUI sessions (below), asked "Which DEV-V marker is in your developer instructions right now?":

  | Launch | Answer | In the rollout |
  |---|---|---|
  | `codex -c developer_instructions="…DEV-V1" …` | `dev=DEV-V1` | `DEV-V1` |
  | `codex -c developer_instructions="…DEV-V2" … resume <thread>` | `dev=DEV-V1` | `DEV-V1` ×7, `DEV-V2` none |
  | `codex -c developer_instructions="…DEV-V3" … resume <thread>`, and a trusted `SessionStart` hook that prints `{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"Glasscode hook brief marker: HOOK-C2"}}` | `dev=DEV-V1; hook=HOOK-C2` | `HOOK-C2` ×4 |

  - **The flag is stale on resume.** Codex records the developer instructions of a thread's first start and keeps them; a resume with new `-c developer_instructions` is ignored. Glasscode's own instructions (`agents::cli_args`) are therefore fixed at a session's first start.
  - **The hook delivers the current brief on start and on resume,** as for Claude. It must be trusted through `hooks.state` (P06). So M20.2 uses the `SessionStart` hook on both CLIs, and keeps `developer_instructions` for Glasscode's fixed instructions only.
  - **Not run:** Codex compaction (`/compact`), and whether `SessionStart` fires there with `source: compact`, as the Codex docs list.

## P04 The native memory switches
- **Method.**
  - Claude: pass `{"autoMemoryDirectory": "<scratch>/agent-notes/p/claude"}` and, separately, `{"autoMemoryEnabled": false}` in `--settings`. Ask the model to remember a fact, then check where the file was written and whether `/memory` shows the folder.
  - Codex: run `-c memories.use_memories=false -c memories.generate_memories=false` and, separately, `-c memories.disable_on_external_context=true`. Check with `codex debug prompt-input` whether memories are in the prompt.
- **Measure.** Whether each key is honoured, and whether Claude writes to the custom folder without asking.
- **Pass.** At least the redirect for Claude and the off switch for Codex work.
- **Fallback.** Leave native memories on. Label them in the brief as a second memory that Glasscode does not check. Harvest them as proposals (M35).
- **Waits.** M22 (D19 part), M35.
- **Result, Codex: the switches cannot be checked without a model; off is the default** (no prompt). Script: `p04-memories.sh <claude-dir> <codex-home> <scratch> codex`.
  - **Method.** The script planted `$CODEX_HOME/memories/memory_summary.md` and `MEMORY.md` holding the marker `CODEX-MEM-MARKER-41` (the file names come from the 0.167 binary) and read `codex debug prompt-input` six ways: feature off, `--enable memories`, plus `use_memories=false`, plus `generate_memories=false`, plus `disable_on_external_context=true`, and `-c memories.version="v1"|"v2"`. The folder was removed afterwards.
  - **Result.** The marker, and the word "memory", appeared in none of them, so `prompt-input` does not render memory injection.
  - **What is known without a model.**
    - `codex features list` shows `memories  stable  false`: memories are off unless a person turns them on.
    - `--disable memories` (the same as `-c features.memories=false`) overrides the person's `config.toml` for that run. So D19's off switch for Careful and local-only projects is `--disable memories` on the session's command line.
- **Result, live: pass for both** (3 prompts, 2026-11-20).
  - **Codex** (`p04-memories.sh … codex-live`). `codex exec` was asked "What is the codename of this project, from anything you remember? Do not run commands or read files", with the marked memory planted:
    - with `--enable memories` it answered `CODEX-MEM-MARKER-41`, with no tool call;
    - with `--enable memories -c memories.use_memories=false` it answered `unknown`.

    So memories are injected in a live session although `prompt-input` does not show them, and the off switch works. The planted folder was removed afterwards.
  - **Claude** (`p04-memories.sh … claude`). An interactive session with `--settings {"autoMemoryDirectory": "<scratch>/agent-notes/p/claude", …}` was asked to remember the codename `CLAUDE-MEM-MARKER-17`.
    - It wrote `agent-notes/p/claude/MEMORY.md` (the index) and `agent-notes/p/claude/scratch-project-codename.md`, **without a permission prompt**: no `PermissionRequest` hook, and no dialog on screen.
    - Nothing was written to the account's own `projects/<project>/memory/`.
    - `/memory` shows "Auto-memory on" and "Open auto-memory folder". It does not print the folder's path.
    - In a session with `{"autoMemoryEnabled": false}`, `/memory` shows "Auto-memory off" (no prompt needed for that look).
  - **The pass rule holds:** the Claude redirect works, and so does the Codex off switch. D19 stands as written.
  - **Claude (docs only, not run).**
    - `autoMemoryDirectory` is honoured from `--settings`, among other scopes.
    - `autoMemoryEnabled: false` turns auto memory off, and `CLAUDE_CODE_DISABLE_AUTO_MEMORY` does so per session.
    - Whether Claude writes to the redirected folder without asking is not in the docs; the run above shows it does.

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
- **Result, from P03's sessions (no extra prompt).** What P03's three interactive sessions showed is below; the cases that need their own turn follow. They had every event of `claude-hooks.py` in `--settings`. `p03-brief.sh` and the zero-prompt starts of this proof produced them.
  - **An unknown event name does not void `--settings`.**
    - With `GlasscodeMadeUpEvent` in the block, SessionStart still ran.
    - An interactive start stops on a **Settings Warning** dialog: `hooks.GlasscodeMadeUpEvent: Unknown hook event "GlasscodeMadeUpEvent" was ignored.` It lists the valid events and says "The values listed above were skipped; the rest of the file is in effect", with the options 1. Continue, 2. Fix with Claude, 3. Exit and fix manually.
    - A `-p` start says nothing and goes on.
    - So a session that Glasscode starts with an event its CLI version does not know waits on that dialog. That is a case for the land-m2 probe (below).
    - The valid events of 2.1.328: `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `PostToolBatch`, `Notification`, `UserPromptSubmit`, `UserPromptExpansion`, `SessionStart`, `SessionEnd`, `Stop`, `StopFailure`, `SubagentStart`, `SubagentStop`, `PreCompact`, `PostCompact`, `PreModelSwitch`, `PostModelSwitch`, `PermissionRequest`, `PermissionDenied`, `Setup`, `TeammateIdle`, `TaskCreated`, `TaskCompleted`, `Elicitation`, `ElicitationResult`, `ConfigChange`, `WorktreeCreate`, `WorktreeRemove`, `InstructionsLoaded`, `CwdChanged`, `FileChanged`, `DirectoryAdded` and `MessageDisplay`. All seven events of this proof exist.
  - **Payload fields seen.**

    | Event | Fields beyond `session_id`, `transcript_path`, `cwd`, `hook_event_name` |
    |---|---|
    | `SessionStart` | `source` (`startup`, `resume`, `compact` seen), `model`, `scratchpad_dir` |
    | `UserPromptSubmit` | `prompt`, `prompt_id`, `permission_mode` |
    | `Stop` | `last_assistant_message`, `prompt_id` (the same id as the turn's `UserPromptSubmit`), `background_tasks` (`[]`), `session_crons`, `stop_hook_active`, `effort`, `permission_mode` |
    | `SubagentStop` | `agent_id`, `agent_type`, `agent_transcript_path`, `last_assistant_message`, `prompt_id`, `background_tasks`, `stop_hook_active` |
    | `PreCompact` / `PostCompact` | `trigger` (`manual`), `custom_instructions` / `compact_summary`, `prompt_id` |
    | `SessionEnd` | `reason`: `prompt_input_exit` for `/exit`, `other` on SIGHUP (exit status 129) |
  - **`SubagentStop` also fires for Claude's own helpers,** with an empty `agent_type`: once during `/compact` (its `last_assistant_message` is the compaction analysis), and once 1.7 s after a resumed turn's `Stop`. M30 treats an empty `agent_type` as internal and opens no subagent for it.
  - **The lag between `Stop` and the transcript.** The `Stop` hook ran before the turn's assistant lines reached the transcript. At hook time the last record was `ai-title`, and `transcript-watch.py` (50 ms polling) saw the assistant lines 111 ms later. The docs agree: hooks "should use `last_assistant_message` on Stop and SubagentStop instead of reading the transcript". M30 closes a turn from `Stop`'s own fields, and reads the transcript only for its digest, after it settles.
  - **The cases not run, by the docs (docs only, not run).**
    - `Stop` does not run on a user interrupt, and API errors fire `StopFailure` instead.
    - Fields: `StopFailure` has `error`, `error_details` and `last_assistant_message`; `PermissionRequest` has `tool_name`, `tool_input` and `permission_suggestions` (no `tool_use_id`); `PermissionDenied` has `tool_name`, `tool_input`, `tool_use_id` and `reason`; `CwdChanged` has `old_cwd` and `new_cwd`; `SubagentStart` has `agent_id` and `agent_type`.
    - `Notification.notification_type` takes values such as `permission_prompt`, `idle_prompt`, `auth_success`, `elicitation_dialog`, `agent_needs_input` and `agent_completed`.
    - `prompt_id` is a common field.
    - If the interrupt case confirms it, an interrupt shows no `Stop`, so M30 closes it from the watchdog (the terminal back at the prompt with the turn open) or from the transcript's interrupt marker.
- **Result, the cases that need a turn: pass with the watchdog for two of them** (6 prompts with the hand-back turn, haiku, 2026-11-20). Script: `p05-hooks.sh <claude-dir> <codex-home> <scratch> interrupt|apierror|kill9|continue`. Every run had all the events of `claude-hooks.py`:

  | Case | Events, in order (ms after `UserPromptSubmit`) | How M30 closes the turn |
  |---|---|---|
  | **Interrupt**: Esc 2 s into a turn that started `sleep 30` (haiku ran it in the background: `run_in_background: true`) | `PreToolUse` Bash +6,770, `PostToolUse` +6,895; **no `Stop`**; `SessionEnd` `other` when the session was closed | the transcript gets a user record `[Request interrupted by user]`; with no `Stop`, the watchdog sees the terminal back at the prompt with the turn open: `Interrupted` |
  | **API error**: `--model claude-glasscode-no-such-model` (`-p`; no model reached) | `StopFailure` +560 with `error: "model_not_found"` and `last_assistant_message` ("There's an issue with the selected model …"), then `SessionEnd` `other` | `StopFailure`: `Failed` with the error |
  | **`kill -9`** of `claude` 2 s into a 31 s foreground command (`sh wait31.sh`) | `PreToolUse` Bash +1,616, then **nothing**: no `PostToolUse`, `Stop` or `SessionEnd` | the watchdog: the process is dead and the transcript stops growing: `Crashed`. The command's `sh` and `python3` **lived on as orphans** until they finished |
  | **Stop-hook continuation**, with a `cd` and an Explore subagent | `PreToolUse`/`PostToolUse` Bash; `CwdChanged` +2,987 (`old_cwd`, `new_cwd`); `PreToolUse` Agent; `SubagentStart` +5,653 (`agent_id`, `agent_type: "Explore"`); `Stop` +7,018 (`stop_hook_active: false`, blocked by the hook); the subagent's own `PreToolUse`/`PostToolUse` (with `agent_id`, `agent_type`); `Stop` +10,963 (`stop_hook_active: true`, `last_assistant_message` "CONTINUED-58 …"); then `SubagentStop` after the turn | one turn with a continuation: the first `Stop` that the hook blocked does not close it; the `Stop` with `stop_hook_active: true` does |

  - **Background work outlives the turn.** The Explore subagent ran in the background: the Agent tool returned at once, and both `Stop` events listed it in `background_tasks` (`{"id", "type": "subagent", "status": "running", "description", "agent_type"}`). When it finished, Claude started a **new turn by itself**: `UserPromptSubmit` with a prompt that begins `<agent-message from="…">[Subagent hand-back] …`.
    - So M30 records `background_tasks` at `Stop`, and keeps the session "working in the background" until they end.
    - A `UserPromptSubmit` whose prompt is an `<agent-message>` hand-back is a turn that no person started (D1 tier "never").
  - **`kill -9` leaves the turn's children running.** M41 and the reconciler must look for the session's process group, not only for `claude`.
  - **A bare `sleep` is refused in the foreground**: the first kill try got "The Bash tool blocked `sleep 31` as a standalone …", and its turn ended with `Stop`. That run counted and taught nothing about the kill. It also showed that nothing fires on `kill -9` after a turn has ended, not even `SessionEnd`, and that `Notification` with `notification_type: "idle_prompt"` came 60 s after the turn.
  - **`PermissionRequest`** (seen in P07's `-p` runs, with no approval surface): `tool_name`, `tool_input`, `permission_suggestions` (Claude's own suggested rules, written as `//<absolute path>/**`), `permission_mode`, `prompt_id`, `effort`, and no `tool_use_id`, as the docs say.
  - **`PermissionDenied`** fires only for denials by auto mode's classifier (docs). No run produced one, so it is still docs only, not run.
  - **The pass rule holds:** every case closes with one cause, from a hook or from the watchdog of the fallback (interrupt, crash).
  - **Server and Windows lines of M01** (P03 and P05 in tmux on the Docker test server, and once on the owner's Windows machine): the Server line moves to M02, and the Windows run is an owner step (see "Run notes").

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
  - **A repository cannot trust its own hooks.** A scratch repository carries `.codex/hooks.json`:
    - while the project is not trusted, its hooks are not loaded at all;
    - once the account's `config.toml` marks it `trust_level = "trusted"`, as `ensure_codex_trusts` does for every worktree, they load but stay `untrusted` (`source: project`);
    - they still stay `untrusted` when the repository's own `.codex/config.toml` carries a `hooks.state` entry with the right hash.

    Only the account's `config.toml` (or managed config) grants trust.
- **Result, turn end: pass** (2 prompts, plus 1 for P03; `gpt-5.6-luna` low; 2026-11-20). Script: `p06-codex-turn-end.sh <claude-dir> <codex-home> <scratch> live start|resume|resume-hook`.
  - **The session.** An interactive TUI session in a pseudo-terminal, launched as `agents::launch` does: `codex -c developer_instructions=… -m gpt-5.6-luna -c model_reasoning_effort="low" -c check_for_update_on_startup=false -c notify=[…glasscode…] --sandbox workspace-write --ask-for-approval on-request`.
  - **Its hooks.** Glasscode's hooks are in `$CODEX_HOME/hooks.json` and trusted through `hooks.state` entries in the account's `config.toml`, as above. No bypass flag was passed.
  - **The person's `notify`.** The account's `config.toml` also had a `notify` program of its own (labelled "person").
  - **The update dialog.** Without `check_for_update_on_startup=false`, 0.167 first shows an "Update available 0.167.0 → 0.170.0" dialog that takes the first keys typed ("1. Update now (runs `npm install -g @openai/codex`)", "2. Skip", …). The first try lost its question to that dialog, reached no model, and counted 0; the installed version stayed 0.167.0. Glasscode's own Codex sessions need the same key, as `DISABLE_AUTOUPDATER` for Claude (the probe matrix lists it).

  | Signal | Turn of the start session | Turn of the resumed session |
  |---|---|---|
  | `SessionStart` hook | `source: startup`, 32 ms before `UserPromptSubmit` (Codex runs it with the first turn, not when the TUI opens) | `source: resume` |
  | `Stop` hook | `turn_id`, `last_assistant_message: "dev=DEV-V1"` | the same fields |
  | `notify` (Glasscode's, from `-c`) | +22 ms after `Stop`: `type: agent-turn-complete`, `turn-id`, `last-assistant-message` | +21 ms |
  | rollout `event_msg` `task_complete` (seen by 50 ms polling) | +33 ms after `Stop` | +41 ms |
  | the person's `notify` from `config.toml` | **not called** | not called |

  - **A turn end is known within 2 s without the bypass**, three ways: the trusted `Stop` hook, `notify`, and the rollout. The rollout needs no hook at all.
  - **A `-c notify` replaces the configured one.** It does not add to it. To keep the person's notifications, Glasscode's notify program must call the person's own program after its own work, reading the command from the account's `config.toml`.
  - **`notify` also fires for internal turns.** After the first turn a second `agent-turn-complete` came, with `last-assistant-message` `{"title":"Identify DEV-V marker"}` (the thread title). M30 matches `notify` to a `Stop` or a rollout turn by `turn-id`, and ignores the rest.

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
- **Result, Codex: partial, two tools break** (no prompt). Script: `p07-profile.sh <claude-dir> <codex-home> <scratch> codex person|automation`.
  - **Setup.**
    - The script builds a scratch Glasscode home with the layout of design section 5: `authority/policy.json`, `state.json`, `memory/MEMORY.md`, `agent-notes/proj/codex/`, and two linked git worktrees `worktrees/proj/feature-a` and `feature-b` of `repos/proj`.
    - `control.sock` is a listening Unix socket.
    - The worktree holds a `.env`, a dependency-free crate and an empty npm package.
    - Each probe runs as `codex sandbox -C <worktree> -P glasscode -c default_permissions="glasscode" -c permissions.glasscode.extends=":workspace" -c permissions.glasscode.filesystem={…} -c permissions.glasscode.network={enabled=false, unix_sockets={"<gc>/control.sock"="allow"}}`.
    - The filesystem table:
      - `deny` for `~/.ssh`, `~/.aws`, `~/.config/gh`, `~/.netrc`, `~/.gnupg`, `~/Library/Keychains`, `authority/` and `state.json`;
      - `read` for `memory/`;
      - `write` for the own worktree and the own agent-notes folder;
      - for automation only, `deny` for the other worktree.
    - Reads are probed with `head -c1 <file> >/dev/null`, so no content is printed; the real `~/.ssh` and `~/.config/gh` were only probed that way.
    - **Syntax.** A `-c` dotted path keeps a quoted segment as written (`permissions.g.filesystem."/x"="deny"` fails: "filesystem path `"/x"` must be absolute"). Paths must therefore go in one inline table per key.

  | Probe | person | automation |
  |---|---|---|
  | Edit a file of the own worktree | allowed | allowed |
  | Write the other worktree | refused (outside the workspace) | refused |
  | Read or write `authority/policy.json`; read `state.json` | refused | refused |
  | Read `memory/MEMORY.md` / write it | allowed / refused | allowed / refused |
  | Write `agent-notes/proj/codex/MEMORY.md` | allowed | allowed |
  | Read `.env` in the worktree (first by the deny glob `:workspace_roots."**/.env*"`; in the 2026-11-20 runs by its absolute path) | refused | refused |
  | List `~/.ssh`, read `~/.config/gh/hosts.yml` | refused | refused |
  | Read a key in `~/.ssh` by name (`id_ed25519`, `id_rsa`, `id_ecdsa`) | not tested: no file of those names on this Mac, and the folder listing is refused | the same |
  | Keychain: `security default-keychain` / `security list-keychains` (control: both exit 0 outside the sandbox) | **refused**: "SecKeychainCopyDefault: A default keychain could not be found" / exit 0, listing nothing | the same |
  | `git status` in the linked worktree | allowed | allowed |
  | `git commit` in the linked worktree | **refused**: `Unable to create '<repo>/.git/worktrees/feature-a/index.lock'` | refused |
  | `gh api rate_limit` | **refused**: `failed to read configuration: open ~/.config/gh/config.yml: operation not permitted` | refused |
  | `git ls-remote https://github.com/…` | refused (network off: "Could not resolve host") | refused |
  | `cargo build --offline` (no dependencies) | **refused** with the `.env` glob: `failed to create directory …/crate/target`; builds without the glob | the same |
  | `npm install --offline` (no dependencies) | allowed | allowed |
  | Connect to `control.sock` | allowed | allowed |

  What it shows:
  - **Path classes hold.** Credentials, `authority/` and `state.json` are refused for reading and writing. The worktree, the agent notes and the socket work.
  - **Denying a parent breaks lookups.** Denying `worktrees/` and allowing one worktree under it breaks `cd` into the own worktree ("Not a directory"). M22 denies sibling worktrees one by one, never their parent.
  - **A deny-read glob stops all new folders in the workspace.** `"**/.env*" = "deny"` under `:workspace_roots` makes Codex 0.167 refuse every new directory there, so `cargo build` fails. M22 cannot use the glob. It denies the `.env` files that exist at launch by absolute path, so a `.env` created later in the session is not covered. Section 7's row for credentials now says so.
  - **`.git` is read-only inside Codex's workspace sandbox** by default, in a linked worktree and in a plain clone alike ("Unable to create '<clone>/.git/index.lock'"). A first try with `write` on the gitdir's `worktrees/<name>`, `objects`, `refs` and `logs` printed `Unable to create '<repo>/.git/packed-refs.lock'`. That line was taken for a failure, but it is only git's warning: the 2026-11-20 re-run below shows the commit succeeds.
  - **Decision O16 (decided by the lead on 2026-11-19, under the owner's delegated authority, given 2026-11-20; narrowed by this proof, and the narrowed form accepted on 2026-11-20; tested 2026-11-20)** (no prompt; `p07-profile.sh … codex automation`, output in `raw/codex-automation.out`).
    - **The decision.** The automation profile makes the session's own repository's git metadata writable, and nothing else. Commits stay local, so this does not leave the Mac.
    - **What failed as decided.** With `write` for the gitdir, `objects`, `refs` and `logs` (and `packed-refs`), the commit works, but the session can also move **any** branch: `git update-ref refs/heads/main HEAD` and `refs/heads/feature-b` both succeeded (the reviewer found this first).
    - **The narrowed profile.** The committed script writes only these, as `write` in `[permissions.<name>.filesystem]`:
      - `<repo>/.git/worktrees/<name>`;
      - `<repo>/.git/objects`;
      - `<repo>/.git/refs/heads/<own branch>` and its `.lock`;
      - `<repo>/.git/logs/refs/heads/<own branch>`.

      No `packed-refs`: git then prints "Unable to create '…/packed-refs.lock'" and commits anyway. With `packed-refs` writable, a session could rewrite the refs that are packed.

      | Probe under the narrowed automation profile (`codex sandbox -P glasscode`) | Result |
      |---|---|
      | `git commit` in the linked worktree | allowed |
      | `git update-ref refs/heads/feature-b HEAD` (the other worktree's branch) / `refs/heads/main` | refused: "cannot lock ref … Unable to create '…/refs/heads/feature-b.lock': Operation not permitted" / the same for `main` |
      | `git branch p07-new-branch` | refused |
      | write `<repo>/.git/config`, `<repo>/.git/hooks/pre-commit`, the main checkout's `README.md` | refused ("Operation not permitted") |
      | write the other worktree, `authority/`, the home folder | refused |

    - **The grants in force** (the run's `profile:` line, re-run on 2026-11-20 with a plain redirect into `raw/codex-automation.out`; the first capture had filtered that line out):
      ```
      -c default_permissions="glasscode" -c permissions.glasscode.extends=":workspace"
      -c permissions.glasscode.filesystem={"~/.ssh"="deny", "~/.aws"="deny", "~/.config/gh"="deny", "~/.netrc"="deny", "~/.gnupg"="deny", "~/Library/Keychains"="deny", "<scratch>/gchome/authority"="deny", "<scratch>/gchome/state.json"="deny", "<scratch>/gchome/worktrees/proj/feature-b"="deny", "<scratch>/gchome/memory"="read", "<scratch>/gchome/worktrees/proj/feature-a"="write", "<scratch>/gchome/agent-notes/proj/codex"="write", "<scratch>/gchome/worktrees/proj/feature-a/.env"="deny", "<scratch>/repos/proj/.git/worktrees/feature-a"="write", "<scratch>/repos/proj/.git/objects"="write", "<scratch>/repos/proj/.git/refs/heads/feature-a"="write", "<scratch>/repos/proj/.git/refs/heads/feature-a.lock"="write", "<scratch>/repos/proj/.git/logs/refs/heads/feature-a"="write"}
      -c permissions.glasscode.network={"enabled"=false, "unix_sockets"={"<scratch>/gchome/control.sock"="allow"}}
      ```
      The re-run gave the same results as the table.
    - **Not tried:** a commit when the own branch's ref is packed (in `packed-refs`, with no loose file). The loose ref and its `.lock` are granted, so git should write a loose ref; M22's test `automation_commit_with_packed_own_ref` checks it.
    - **Limits of the test.** It ran with `codex sandbox -P`, never in a session. `codex sandbox` takes only `-P`, and `sandbox_workspace_write.writable_roots` was ignored under `-P :workspace`. A session's profile is the same permission profile, but that was not run, and neither was `--add-dir`.
    - Design section 7 and M22 now carry the narrowed list, and slices section 11 has the decision as O16.
  - **`gh` cannot start** when `~/.config/gh` is denied, because it reads `config.yml` first. By the docs, `gh`'s TLS can also fail under Seatbelt. **The fallback is applied, one way per profile:**
    - **person:** `gh` runs outside the sandbox. For Claude that is `sandbox.excludedCommands: ["gh"]`, which the docs call "a convenience, not a security boundary". For Codex it is the `on-request` approval to run it unsandboxed. It is guarded.
    - **automation and quarantine:** there is no `gh`. Forge reads and writes are Glasscode ops (D13).

    Design section 7 has the row, and M22 step 8 says so.
  - **The login keychain is not reachable inside Codex's sandbox.** `security default-keychain` exits 0 outside and fails inside: "A default keychain could not be found". The reviewer's control was a lookup of an existing item, exit codes only. It failed inside with "One or more parameters passed to a function were not valid", and failed the same way with the `~/Library/Keychains` deny removed, so the sandbox blocks the service itself. An earlier probe here looked up a missing item. It could not tell "searched" from "search failed", and its "reached" was wrong.
  - **Claude, by the docs (docs only, not run).**
    - Claude's sandbox covers shell commands only. File tools, MCP servers and hooks run outside it.
    - A `sandbox.filesystem.denyRead` entry does not stop the Read tool. A `permissions.deny` rule `Read(<path>)` does, and it also blocks Edit and Write on that path. M22 therefore writes each Credentials and Authority path twice: once as a sandbox `denyRead`, once as a `permissions.deny` rule.
    - `allowUnixSockets` is macOS only.
    - The defaults are `allowUnsandboxedCommands: true`, `failIfUnavailable: false` and `autoAllowBashIfSandboxed: false` (since 2.1.324). An automation profile must set the first two the other way.
    - `excludedCommands` is "a convenience, not a security boundary".
    - **On macOS, git over an SSH remote (fetch, pull, push) fails inside the sandbox, even with the host allowed, and Go programs such as `gh` may fail TLS under Seatbelt.** So inside Claude's sandbox, automation sessions get their `gh` and git network through Glasscode ops (brokered, D13), or use an https remote for git. Person sessions run them unsandboxed, the guarded case of this proof's fallback.
- **Result, Claude: pass, after one fix to the rules** (3 prompts, haiku, 2026-11-20). Script: `p07-profile.sh <claude-dir> <codex-home> <scratch> claude person|automation`.
  - **The session.** `claude -p --input-format stream-json … --strict-mcp-config --permission-mode acceptEdits --permission-prompts none --add-dir <glasscode home> --settings <profile>`, in the same scratch layout. The model ran the probes through one Bash call (`sh <worktree>/p07-probes.sh`, so every probe ran inside the sandbox), then the Read, Edit and Write steps.
  - **The profile's `--settings`:**
    - `sandbox.enabled`, `autoAllowBashIfSandboxed`;
    - `filesystem.denyRead` for the credentials, `authority/`, `state.json` and the worktree's `.env` (automation: also the other worktree), and `allowWrite` for the worktree and the agent-notes folder;
    - `network.allowedDomains` with the registries, plus `github.com` and `api.github.com` for the person profile only;
    - `allowUnixSockets` for `control.sock`;
    - `permissions.deny` Read, Edit and Write for the same paths, plus Edit and Write for `memory/`;
    - `disableAllHooks: false` and the logging hooks;
    - `autoMemoryDirectory`;
    - automation only: `allowUnsandboxedCommands: false`, `failIfUnavailable: true`, a deny for WebFetch and WebSearch, `disableClaudeAiConnectors: true` and `crossSessionInbound: "refuse"`.
  - **The fix.** The first run wrote the deny rules as `Read(/Users/…/.env)`, and the Read tool **read the worktree's `.env`** (a canary). In a permission rule `/path` is relative to the settings file; an absolute path needs `//path`. With `Read(//Users/…)` the same read was refused. The first run's Bash half was lost too: its probe script wrote to `/tmp`, which Claude's sandbox does not allow. That run counts as a prompt. M22 writes every absolute rule with `//`, and its test `deny_rules_are_absolute` checks it.

  | Probe | person | automation |
  |---|---|---|
  | Bash: edit the own worktree / write the other worktree | allowed / allowed (`--add-dir` gave the sandbox the whole scratch home; M22 adds only the own folders) | allowed / **refused** |
  | Bash: read `authority/policy.json`, `state.json`, `.env`, `~/.ssh`, a key in it, `~/.config/gh/hosts.yml` | refused ("Operation not permitted") | refused |
  | Bash: write `authority/policy.json` / write `memory/MEMORY.md` | refused / refused (the Edit deny rules also bind the sandbox) | refused / refused |
  | Bash: read `memory/` / write agent notes | allowed / allowed | allowed / allowed |
  | Bash: Keychain, `security default-keychain` / `list-keychains` (control: both exit 0 outside) | **refused**: "A default keychain could not be found" / exit 0, listing nothing (resumed person session, 2026-11-20) | not run in the automation profile |
  | Bash: `git status`, `git commit` in the linked worktree | allowed, allowed | allowed, allowed |
  | Bash: write `<repo>/.git/config`, `.git/hooks/pre-commit`, the main checkout's `README.md` | refused (resumed person session) | not run |
  | Bash: `git update-ref` of the other worktree's branch and of `main`; `git branch <new>` | **allowed** (resumed person session) | not run |
  | Bash: `gh api rate_limit` | refused (`failed to read configuration: open ~/.config/gh/config.yml`) | refused |
  | Bash: `git ls-remote https://github.com/…` | allowed | **refused**: "CONNECT tunnel failed, response 403", with `<sandbox_violations> deny network-outbound github.com:443` |
  | Bash: `curl https://github.com` / `curl https://registry.npmjs.org/` | — | refused (403) / allowed |
  | Bash: `cargo build --offline`, `npm install --offline`, connect to `control.sock` | allowed | allowed |
  | Read `authority/policy.json`, the worktree's `.env`, `~/.ssh/<no such file>` | refused: "File is in a directory that is denied by your permission settings." | not tried by the model (it read the probe output and skipped them) |
  | Read `memory/MEMORY.md` / Edit it | allowed / refused (the same message) | allowed / refused |
  | Write `agent-notes/proj/claude/MEMORY.md` | allowed | allowed |
  | Write the other worktree | allowed (person) | not tried (as above) |

  What it shows:
  - **Pass for both profiles.** Work in the own worktree runs, credentials and authority files are refused for both the shell and the file tools, and hooks arrive inside the sandbox (`PreToolUse` and `PostToolUse` for every tool call). The login works from the account folder (`apiKeySource: none`).
  - **`git commit` in a linked worktree works** in Claude's sandbox. The worktree's gitdir in the main repository was writable without an entry for it.
  - **But the session can move any branch.** In the resumed person session, `git update-ref` moved the other worktree's branch and `main`, and `git branch` created one. `.git/config`, `.git/hooks` and the main checkout stayed refused.
    - So M22 and M23 treat every local branch tip as untrusted: before a brokered push, Glasscode compares the branch it pushes with the commits that session made.
    - For Claude no narrower grant was found; the sandbox has no per-ref setting.
    - Section 7 says so.
  - **The Keychain is not reachable in Claude's sandbox either** (the control above).
  - **exec-30's "resume of a stream-json chat", Claude half: pass.** `claude -p --input-format stream-json --resume <id>` of the person session, with the same settings, answered which file it had written in the earlier turn (`agent-notes/proj/claude/MEMORY.md`). Then it ran the probes (1 prompt).
  - **`gh` cannot start** inside either profile, as for Codex (`~/.config/gh` is a Credentials path). The resolution is the one under the Codex half above: outside the sandbox for the person profile (guarded), none for automation (D13).
  - **The automation allowlist holds** (P14): `github.com` is refused through the sandbox's proxy while a listed registry is reached.
  - **Not run:** the `!` negation for a home-relative path. The docs mention `Read` and `Edit` rules "whose pattern starts with `!`" only for managed rules, and M22 does not plan to use one.
  - M01 notes "no sandbox" on Windows.

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
    - Variants A to E run `claude -p --input-format stream-json …` with no user line; SessionStart fires anyway. IA and IB start an interactive session in a pseudo-terminal and quit at once.

  | Variant | Folder | Hooks that ran |
  |---|---|---|
  | A: Glasscode's `--settings` as today (no `disableAllHooks` key) | trusted | **none** |
  | B: `--settings` adds `"disableAllHooks": false` | trusted | `glasscode-absolute`, `repo-hook`, `evil-glasscode-on-PATH` |
  | C: as A | not trusted | **none** |
  | D: as B | not trusted | `glasscode-absolute`, `repo-hook`, `evil-glasscode-on-PATH` |
  | E: B plus `"allowManagedHooksOnly": true` in `--settings` | trusted | the same as B (the key acts only from managed settings) |
  | IA: as A, in an interactive session (`pty-drive.py`, quit at once) | trusted | **none** |
  | IB: as B, interactive | trusted | `glasscode-absolute`, `repo-hook`, `evil-glasscode-on-PATH` |

  What it shows:
  - A project file **does** turn off Glasscode's hooks when `--settings` leaves `disableAllHooks` unset. A key that `--settings` does not set keeps its value from the project file, as the settings docs say.
  - With `"disableAllHooks": false` in `--settings`, the flag outranks the project and Glasscode's hooks run.
  - The repository's own hooks still run beside them. Hook lists merge, and only managed settings can stop that.
  - A hook named by its bare name runs the **repository's** program. The project's `env.PATH` applies to hook commands, and in `-p` mode even for a folder whose trust dialog was not accepted.
  - **Recommended for M22 (v2 only).** A v2 session profile sets `"disableAllHooks": false` in its `--settings`, and names every hook command by absolute path, as D11 already requires. No code changed in this slice. While the engine is `v1`, nothing changes (rule 2).
    - The key also overrides the person's own `disableAllHooks`, which governs their status line as well. So the M22 profile shows that choice in the session's settings, and the person's status line keeps working through Glasscode's own `statusLine` entry.
    - The D11 row now names the key.
    - The fallback (mark the session "unreported" and rely on the tail and the reconciler) stays for a CLI that changes the precedence.
    - M22's test `project_settings_cannot_disable_glasscode_hooks` repeats variant A.

## P09 Where the person's prompts come from
- **Method.** On the owner's own Mac, for 5 working days, M13's ledger runs in measure-only mode. It counts the origin of each prompt: composer, terminal keys or pasted. It stores counts only, never text.
- **Measure.** The share of each origin.
- **Pass.** Only informational. The owner sees the numbers before the tiers of D1 are fixed, because most prompts are typed in terminals today, and tier 2 cannot back confirmed facts.
- **Waits.** The owner's confirmation of D1.
- **Result: deferred to M13's measure mode.** The proof needs M13's ledger (counts by origin, no text) running for 5 working days on the owner's Mac. Nothing was run in M01.

## P10 What a quit does to running agents
- **Method.** With a Claude and a Codex session working, quit Glasscode normally, then force-quit it. Check whether the agent processes live on, and what their transcripts show.
- **Measure.** Process state, and the last transcript lines.
- **Pass.** The facts are written down for the lifetime rules of design section 10.
- **Waits.** M41.
- **Result: proxy partly measured; the owner step is prepared** (4 prompts, 2026-11-20). Script: `p10-quit.sh <claude-dir> <codex-home> <scratch> proxy normal|force | watch`.
  - **What the code does (from reading it, not run).** The app runs each session in a pseudo-terminal of its own (`alacritty_terminal`): the agent is a session leader on that terminal. On quit, `Workspace`'s `on_app_quit` only installs a waiting update and keeps unsaved editor text (`crates/glasscode/src/workspace.rs`). Nothing stops the sessions on purpose. A normal quit drops the terminals (SIGHUP to each agent), and a force quit closes them through the kernel (the same SIGHUP). P05 saw what Claude does on SIGHUP: `SessionEnd` with `reason: other`, exit 129.
  - **Proxy, 4 prompts.** One Claude and one Codex session in `pty-drive.py`, each running `sh wait45.sh` (a 45 s foreground command) through its shell.

    | Agent | `normal`: the owner closes the terminal (SIGHUP) 8 s into the command | `force`: the owner is SIGKILLed |
    |---|---|---|
    | Codex 0.167 TUI | the rollout records `function_call_output` "aborted by user" and `event_msg` `turn_aborted` (`interrupted`); the agent is gone within 1 s, and so is the command | the same |
    | Claude 2.1.328 | **not measured**: Claude exited by itself 7 s into the command (`SessionEnd` `reason: other`) before the owner closed the terminal; the cause was not found | the driver did not see the command on screen, so the kill came after the turn had ended (`Stop`, then `PostToolUse` of the 45 s command); then `SessionEnd` `other`, and the agent gone within 1 s |

    - **What is known.** A session whose terminal closes ends within a second, whichever way the owner goes. Codex writes `turn_aborted` into the rollout, so the reconciler can report the cut turn as `AppQuit`.
    - **Claude mid-turn.** No hook marks it. P05 showed that a `kill -9` of `claude` itself leaves its command running as an orphan; whether SIGHUP does the same mid-turn is what the owner step must show.
    - **Lifetime rule (design section 10) without change.** Without a server, sessions do not outlive the app.
    - **The two Claude prompts measured nothing** about a cut turn, as the table says.
  - **Owner steps.** With the real app in a scratch `GLASSCODE_HOME`, start one Claude and one Codex session on a long task, and run `p10-quit.sh … watch` in a separate terminal. Then:
    1. quit the app (Cmd-Q), and note what `watch` prints;
    2. start the app again, open both sessions, and read their last lines;
    3. repeat with a force quit (`kill -9 <app pid>`).

## P11 Costs
- **Measure.**
  - the tokens and time of a fresh wake with a 4,000-character situation, on Claude (the cheapest model and the default model) and Codex;
  - the start time of `glasscode hook`;
  - a git commit in a memory repository of 10 MB;
  - a search over 10 MB of Markdown;
  - a journal append.
- **Pass.** The numbers go into the budgets of design section 9 and D6. A fresh wake that costs more than 20,000 tokens or 30 seconds stops wave 5 until the owner decides.
- **Waits.** M50, M51 (budgets).
- **Result: stop condition hit on Codex; decided (see the re-measure below).** The wake is the P01 and P02 runs: the 4,000-character situation of `situation.py` and one stub tool call. Tokens are as reported by the CLI and include cached input.

  | Wake | Model requests | Input tokens (cached) | Output | Total tokens | Time to answer |
  |---|---|---|---|---|---|
  | Claude, haiku (`claude-haiku-5-5`), P01 `--tools ""` | 2 | 10,678 (5,299 read, 5,375 written) | 49 | **10,727** | 3.8 s (first token 2.3 s) |
  | Claude, haiku, P01 `--tools ToolSearch` | 3 | 17,827 (13,252 read) | 133 | 17,960 | 4.8 s |
  | Codex, `gpt-5.6-luna` low, P02 | 3: 11,126 + 12,576 + 12,633 | 36,221 (32,768 cached) | 114 | **36,335** | 7.5 s |

  The Codex wake is above the 20,000-token limit.

  **Re-measured on 2026-11-20** (2 prompts):

  | Wake | Model requests | Input tokens (cached) | Output | Total tokens | Time to answer |
  |---|---|---|---|---|---|
  | Claude, default model (`claude-opus-5-5`), P01 `--tools ""` | 2 | 9,148 (4,546 read, 4,598 written) | 49 | **9,197** | 5.7 s (first token 3.2 s) |
  | Codex with the three trim keys of the table below (`p02-codex-brain.sh … wake-trim`) | 3: 7,415 + 7,535 + 7,592 | 22,443 (14,464 cached) | 99 | **22,542** | 6.1 s |

  - The trim made each request about 7,500 tokens, not the 4,700 estimated from characters: the code-mode tool block is dense. A wake with one tool call still takes three requests, so Codex stays above the limit.
  - **Decision O15 (decided by the lead on 2026-11-19, under the owner's delegated authority, given 2026-11-20; slices section 11).** P11 gates wave 5 only; waves 1 to 4 continue. Wave 5's wake lane defaults to Claude's cheapest model (haiku: 10,727 tokens, a floor: one tool call, from a server with two tools). A Codex wake lane is allowed only with the three trim keys and a re-measure at or under 20,000 tokens. **The re-measure gave 22,542, so the Codex wake lane stays off** in release 1. Design D6 now says so. The chat lane is not a fresh wake and is not gated by this rule.

  The first measurement, before the trim: Each Codex request carries about 11,000 tokens of Codex's own prompt (the code-mode `exec` description, the base instructions and the permission text), and a wake with one tool call takes three requests. By the rule above this stops wave 5 until the owner decides. It was reported to the main agent at once.

  **How far the Codex prompt can be cut** (no prompt; `p11-codex-trim.sh`). These are characters of each model-visible item of `codex debug prompt-input` with the P02 brain flags and a 5-character user line. In P02's live run the first request took 11,061 tokens for about 39,700 characters of prompt plus situation, about 3.6 characters a token.

  | Flags added to the P02 brain profile | Items (characters) | Total |
  |---|---|---|
  | none | tools 11,313 · base instructions 18,379 · permissions 4,610 · environment 864 · user 348 | 35,514 |
  | `-c include_permissions_instructions=false` | permissions 4,610 → 517 | 31,421 |
  | `-c include_environment_context=false` | environment item gone | 34,650 |
  | `-c include_apps_instructions=false` | no change (apps already off) | 35,514 |
  | `-c model_instructions_file="<one-line file>"` | base instructions 18,379 → 472 | 17,607 |
  | all of the above | 11,313 · 472 · 517 · 348 | **12,650** |

  With all four, each request would be about 4,700 tokens with the 4,000-character situation, and a three-request wake about 14,000 tokens, under the limit. That is an estimate. A live re-measure costs 1 prompt (P02's live run with these flags), held until the owner decides.

  The trim has a price: `model_instructions_file` replaces Codex's own base instructions (how to use `exec`, `apply_patch` and the sandbox) with Glasscode's text. The brain then depends on Glasscode's instructions for tool use, which is a D4 and M50 decision. The code-mode tool block (11,313 characters) cannot be cut by any flag tried here.

  **Costs without a model** (no prompt; `p11-costs.sh <claude-dir> <codex-home> <scratch> [app binary]`). The machine was an Apple silicon Mac with an SSD, on macOS 27. Commands are timed from one Python process (`p11-time.py`), so the numbers include the process start (`/usr/bin/true`: p50 2.1 ms, p95 4.0 ms).

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
  - **The journal budget holds** with `write`, and with `fsync(2)`. It does not hold with a full flush on each append, which takes about 4 ms. Recommendation for M14: `write` on the hot path, and the flush in the writer thread. D9 wants the effect ledger's claim made durable before the action, so each claim costs about 4 ms with `sync_data`. That fits "op dispatch ≤ 5 ms" only if no second flush follows. Recommendation for M31: count it in that budget.
  - **`git grep` misses the 30 ms p95 by 5 ms.** It is a stand-in: M20.1's search does not exist yet. The recommendation for M20.1 is to search in process, or filter by project key first, and to measure on the reference load.
  - The hook start is well inside its budget. `glasscode hook` by absolute path (D11) is faster than today's shell line.

## P12 How long Claude keeps transcripts
- **Method.** Pass `{"cleanupPeriodDays": 3650}` in `--settings` for a scratch session, and read the documentation of the cleanup. Find out which process deletes old transcripts, and when.
- **Measure.** Whether the setting per session protects that session's transcripts.
- **Pass, and fallback either way.** M24 always copies a bounded digest into the archive. This proof only decides whether the full transcript can also be kept.
- **Waits.** M24.
- **Result: the per-session setting does not protect a session's transcripts** (no prompt). Script: `p12-cleanup.sh <claude-dir> <codex-home> <scratch>`. It plants two transcripts with a modification time 60 days old in each of two project folders (A and B) of the scratch account, then starts sessions that send no user line:

  | Run | Marker `<config-dir>/.last-cleanup` | `cleanupPeriodDays` of that run | Left of the planted files (A / B) |
  |---|---|---|---|
  | `-p` in A | removed before | 3650 (`--settings`) | 2/2, 2/2; the marker is rewritten with the run's time |
  | `-p` in A, 4 minutes later | present | default (30) | 2/2, 2/2 (no sweep) |
  | `-p` in A | removed before | default | **0/2, 0/2** |
  | interactive in B, quit after 8 s | removed before | 3650 | 2/2, 2/2, and **no marker written**: no sweep ran |
  | interactive in B, open 25 s, then quit | removed before | default | 1/1 left, no marker: no sweep ran |
  | `-p` in B, 6 s | removed before | default | 0/1 left; the marker is written |

  - **Which process deletes, and when.** A `claude -p` process (print or SDK mode, as the brain and the auto-reviewer run) sweeps at start-up, unless the marker `.last-cleanup` in its config folder is recent (the binary calls it `tengu_cleanup_throttle_marker`). In two tries, an interactive 2.1.328 session did not sweep and wrote no marker. The interval was not read from the binary: a start 4 minutes after a sweep skipped it. During P08 later, `-p` runs in a repository with its own `.claude/settings.json` wrote no marker either; that case was not followed up.
  - **Whose setting counts.** The sweep uses the effective `cleanupPeriodDays` of the process that runs it. It sweeps every project of that config folder, not only its own: in the third run, project B's transcripts went too.
  - **So one session's `--settings` does not protect its transcripts.** The next `-p` process on the same account folder, such as the person's own scripts, the auto-reviewer, or a brain with the default, deletes them across all projects.
  - **By the docs (docs only, not run).**
    - The cleanup is a background sweep after a session starts. There is no per-session override.
    - It deletes transcripts under `projects/<project>/`, plus `file-history`, `plans`, `debug` and `paste-cache` entries older than the period. `history.jsonl`, credentials and agent memory are kept.
    - The runs above saw the sweep only from `-p` processes, so "after a session starts" did not hold for the interactive sessions tried. That difference is recorded, not resolved.
  - The binary also skips the sweep while a settings file cannot be read, and while server-managed settings have not loaded ("A later process that is not paused for this reason runs the cleanup").
  - **For M24.** The bounded digest stays the record. A full transcript can be kept only by copying it, or by setting `cleanupPeriodDays` in the account folder's own `settings.json`, which is the person's choice and changes their other sessions too. Copying is a consequence of D23 and needs no D-row change. `cleanupPeriodDays` must be at least 1; 0 is refused by the schema ("To disable transcript writes entirely … use --no-session-persistence").

## P13 The Codex terminal UI attached to an app server
- **Method.** Start `codex app-server --listen unix://<path>` under one scratch `CODEX_HOME`. Run the terminal UI in a PTY with `--remote unix://<path>` and connect a second client.
- **Measure.** Whether the second client gets turn and item events for the UI's thread, can answer an approval, can steer and interrupt, and what the UI shows when it does.
- **Pass.** Record only. If it passes, a later slice can build the Codex session driver on it behind a switch (D14).
- **Waits.** Nothing in release 1.
- **Result, plumbing (no prompt).** Tools: `appserver-client.py` (JSON-RPC over stdio, or WebSocket over the Unix socket) and `pty-drive.py` for the TUI. The schema comes from `codex app-server generate-json-schema --out <dir>` (39 files).
  - **Isolation.** `codex app-server --listen unix://<path>` makes `<path>` a symbolic link to a socket in `/private/tmp/codex-daemon-<uid>/<hash>`, with the hash derived from the path. A second server for the same path refuses: "app-server control socket is already in use at …". So there is one server per socket path. The scratch `CODEX_HOME` decides the account (the `initialize` result names `codexHome`). Other Codex servers on the Mac (the ChatGPT app runs its own, over stdio) are separate processes; this proof never connected to one.
  - **Transport.** The Unix socket speaks **WebSocket**, as `--remote unix://` does. Raw JSON lines, also through `codex app-server proxy --sock <path>`, get the connection closed. A WebSocket client over the socket gets `initialize` → `{userAgent, codexHome, platformFamily, platformOs}`, then a `remoteControl/status/changed` notification.
  - **The TUI attached.** `codex --remote unix://<path>` showed its normal prompt ("Ask Codex to do anything"), with the server's working folder in its status line beside its own. Glasscode must therefore start the server in the session's worktree. A second client connected at the same time without trouble. `thread/loaded/list` was still empty 7 s after the TUI started, so the TUI's thread is loaded only with its first turn.
  - **Quitting the TUI** (Ctrl-C) leaves the work running. It printed "Disconnected from this task. Any running work continues.", a `codex --remote unix://<path> resume <thread>` line to reconnect, and "Stop the current turn: run codex --remote … agents, select this task, and press x."
- **Result, live: pass** (3 prompts counted, the steer included; `gpt-5.6-luna` low; 2026-11-20). Script: `p13-app-server.sh <claude-dir> <codex-home> <scratch>`.
  - **Setup.** The server ran in the work folder; the TUI was `codex --remote unix://<sock> -m gpt-5.6-luna -c check_for_update_on_startup=false --sandbox read-only --ask-for-approval on-request` in a pseudo-terminal; the second client was `appserver-client.py --unix <sock>`.

  | Step | Second client | What the TUI showed |
  |---|---|---|
  | Turn 1: the TUI asks for `echo ok > p13.txt`, which needs an approval | `thread/loaded/list` gave the TUI's thread, and `thread/resume` subscribed to it. The client then **received the server request `item/commandExecution/requestApproval`**, answered `{"decision": "accept"}`, and saw `serverRequest/resolved`, `item/completed` and `turn/completed` with `status: completed`. `p13.txt` was written. | its own approval prompt ("May I create p13.txt in the workspace using the requested shell command? › 1. Yes, proceed 2. Yes, and don't ask again …"), which closed once the other client had answered; then "done · Worked for 9s" |
  | Turn 2: the TUI runs `sleep 25` | received `turn/started`. `turn/steer` (`expectedTurnId` and one text input) 3 s in was accepted; `turn/interrupt` 4 s later was accepted. Then `turn/completed` with **`status: interrupted`**. | the steered text as a user message ("Also end your reply with the word STEERED-13."), then "■ Conversation interrupted", and "1 background terminal running": the `sleep` went on after the interrupt |

  - **The pass rule holds.** A second client gets turn and item events for the TUI's thread, can answer an approval, can steer, and can interrupt. The TUI shows each of these as if the person had done it there. D14 stays as it is: app-server comes by probe, after release 1, behind a switch, and this proof is its first conformance case.
  - **Who answers.** The approval went to both clients; the first answer wins, and the TUI's own prompt closes. So a driver built on this must show the person that Glasscode answered, and must not answer while the person is looking at the prompt.
  - **Interrupting does not stop the command.** As with Claude's `kill -9` (P05), the reconciler must look at the turn's processes as well as its state.
  - **Isolation.** There is one server per socket path (above), and each server serves the account of its `CODEX_HOME`. Killing the server's process group was not tried.
  - **`--no-daemon`.** `codex app-server` has no such flag. The top-level `codex` has one ("Run without the shared background server, even if it is already running"), and takes a bare `--remote unix://`. `daemon_auto_start` is stable and on.
  - **A plain TUI's server** (no prompt; 2026-11-20; `p13-plain-tui.sh <claude-dir> <codex-home> <scratch>`, output in `raw/plain-tui.out`). `codex -c check_for_update_on_startup=false` without `--remote`, for 10 s, in the scratch account A and in an empty account folder B:
    - it started no separate app-server process and no socket in `/private/tmp/codex-daemon-<uid>/`, so the TUI's server runs inside its own process;
    - the TUI was its own process-group leader with no children;
    - `kill -TERM -<pgid>` ended it, and left nothing behind.
  - **Not verified:**
    - the shared daemon (a bare `--remote unix://` with `daemon_auto_start`);
    - whether account folders stay separate on that daemon;
    - killing a server's process group during a turn.

## P14 Brokered push
- **Method.** Run an automation-profile Claude session whose sandbox network allowlist does not include github.com. Prototype `glasscode git push` as an op that pushes from the app's process with the branch rule.
- **Measure.**
  - whether the agent can finish a "fix and push" task through the op;
  - how many extra turns it needs;
  - whether any other path to GitHub stays open: the browser, or an MCP server.
- **Pass.** The task finishes, and no other route reaches the forge.
- **Fallback.** Use forge rules only (D29). Autopilot pushes only where branch protection holds.
- **Waits.** Owner decision O7, then M23.
- **Result: not passed; the D29 fallback stands (O7).** The network wall held in all three tries (`gh`, `git ls-remote` over https, `curl https://github.com`), but the brokered-op prototype was not built, so "the task finishes" is not shown.
  - **The profile can be built from documented settings (docs only, not run).** `sandbox.enabled`, `sandbox.network.allowedDomains` (a list without `github.com`), `sandbox.allowUnsandboxedCommands: false` and `sandbox.failIfUnavailable: true` are all in the 2.1.328 settings index and its binary. In P07's Claude automation run (2026-11-20), `gh api rate_limit` could not start (`~/.config/gh` denied), and `git ls-remote https://github.com/…` and `curl https://github.com` were refused by the sandbox proxy ("CONNECT tunnel failed, response 403", `deny network-outbound github.com:443`), while `registry.npmjs.org` was reached. **So inside the sandbox, none of those three tries reached the forge.** For Codex, P07 already shows `gh` failing to start under the profile, and `git ls-remote https://github.com/…` failing with the network off.
  - **Other routes to the outside that are open by default.**
    - **The claude.ai connectors of the person's account.** `claude mcp list` on the scratch account (signed in to the person's subscription) shows the claude.ai connectors of the account, most of them connected. Any session of that account has them unless it passes `--strict-mcp-config` (P01's init then listed only `glasscode`) or `disableClaudeAiConnectors: true`. Section 7 now requires both for automation and quarantine sessions.
    - **The WebFetch and WebSearch tools** run in the CLI process, not under the Bash sandbox (docs only, not run: "the sandbox covers shell commands only"). They can read pages but carry no forge credentials. The automation profile denies them by permission rule (guarded).
    - **The browser.** Claude in Chrome is off without `--chrome`. The Glasscode browser's MCP server is added by `browser::agent_args`, and D28 already gives automation and quarantine sessions no signed-in browser.
    - **Codex** has its network off in the workspace sandbox (P07). Its web search tool was not checked.
  - **Not done:** the `glasscode git push` op as a prototype, the "fix and push" task through it, and the count of extra turns.

## P15 Tool-call timeouts
- **Method.** A stub MCP tool that answers after 30 s, 2 min, 5 min and 10 min, on both CLIs.
- **Measure.** When each CLI gives up.
- **Pass.** The time is written down. It decides whether an op that waits for the person can keep the tool call open, or must return `Waiting` with a pending id.
- **Waits.** M34.
- **Result: measured** (3 prompts, 2026-11-20). Script: `p15-timeouts.sh <claude-dir> <codex-home> <scratch> claude|codex|codex-900`. In each run the model called the stub's `wait` tool once with `seconds=600`; the stub logged when the call came, any `notifications/cancelled`, and when it answered.

  | CLI | Timeout setting | What happened |
  |---|---|---|
  | Claude 2.1.328, stdio MCP | default | kept the call open; the stub answered after 600.0 s, and the model returned "waited 600 s" |
  | Codex 0.167 | default | **gave up after 300 s**: `notifications/cancelled` at 300,005 ms, and the item failed with "timed out awaiting tools/call after 300s" |
  | Codex 0.167 | `mcp_servers.glasscode.tool_timeout_sec=900` | kept the call open; answered after 600.0 s |

  - **The docs and the run differ.** The Codex docs give a 60 s default ("Overrides the default 60s per-tool timeout"); 0.167 waited 300 s. Claude's docs give about 28 hours for `MCP_TOOL_TIMEOUT` (docs only beyond the 10 minutes run).
  - **For M34.** An op that waits for the person can keep a Claude tool call open, at least for 10 minutes. For Codex it can only if Glasscode passes `tool_timeout_sec` above the wait, and only up to that value. Otherwise the op returns `Waiting` with a pending id, which is the safe default for both CLIs and for waits longer than the setting.

## P16 Claude sessions messaging each other
- **Method.** Two Claude sessions of the same account. Try the native cross-session message features (`SendMessage`, the inbox socket), and the setting `crossSessionInbound: "refuse"` in `--settings`.
- **Measure.** Whether one session can make another act, and whether the setting stops it.
- **Pass.** A way to refuse is confirmed for automation and quarantine profiles.
- **Fallback.** A `PreToolUse` deny on those tools (guarded), and a label in design section 7.
- **Waits.** M22.
- **Result, without a model (no prompt):**
  - **Sessions are scoped to the account folder.** `claude agents --json` on the scratch account lists 0 sessions, while the person's own account had many running on this Mac at the same time. A session of one account folder does not see another's.
  - **The inherited socket.** The route that stays is the environment: a `claude` started inside another Claude session inherits `CLAUDE_CODE_MESSAGING_SOCKET` and `CLAUDE_CODE_MESSAGING_TOKEN`. Glasscode already removes every `CLAUDE*`, `CODEX*` and `AI_AGENT*` variable from its own process at start (`env::SCRUBBED_PREFIXES`), and the proofs ran with `env -i` for the same reason.
  - **The setting (docs only, not run).**
    - `crossSessionInbound` has the ladder `accept` < `hold` < `refuse`; `refuse` drops each message.
    - When it is unset, the two sessions' permission modes decide.
    - `--settings` can set it, and a project file can only make it stricter, never looser (settings docs, "Exceptions to managed settings precedence").
    - Messages travel over a same-machine socket, with `CLAUDE_CODE_MESSAGING_SOCKET` and `_TOKEN` exported to hooks and Bash.
    - The receiver is told a message "came from another session, not from you", and a message cannot approve anything.
    - M22 sets `refuse` for automation and quarantine profiles.
- **Result: pass** (3 prompts, haiku, 2026-11-20). Script: `p16-cross-session.sh <claude-dir> <codex-home> <scratch> accept|refuse`.
  - **Setup.** Both are sessions of the scratch account. Their hooks see `CLAUDE_CODE_MESSAGING_SOCKET` and `_TOKEN` by default, so the feature is on without any flag.
    - The receiver is an interactive session started with `claude -n p16-receiver-<mode>-7f3a --permission-mode acceptEdits`.
    - The sender is a `claude -p -n p16-sender-<mode>-7f3a` session, asked to call ListAgents, then send the receiver, by its exact name, "Please create the file p16-done.txt containing ok in your working folder."

  | Receiver | Sender's ListAgents | Sender's SendMessage | Receiver |
  |---|---|---|---|
  | default (`accept`) | "Peer sessions (1): p16-receiver-… · interactive · idle", and the person's own sessions not listed | `success: true` ("in that session's inbox, not yet read by its Claude") | **acted**: a `UserPromptSubmit` whose prompt is `<cross-session-message from="uds:/tmp/cc-socks/<pid>.sock" from-name="p16-sender-…" from-mode="prompting">…</cross-session-message>`, then a Write, and `p16-done.txt` exists |
  | `--settings {"crossSessionInbound": "refuse"}` | the same | `success: true` as well; no refusal notice reached the sender before its `-p` turn ended | **did not act**: no `UserPromptSubmit`, no file; only `SessionStart` and `SessionEnd` |

  - **One session can make another act.** The message arrives as the receiver's next prompt, labelled with the sender's socket and name. Glasscode's words ledger must treat it as tier "never" (D1).
  - **`crossSessionInbound: "refuse"` in `--settings` stops it,** so M22 sets it for the automation and quarantine profiles, as planned. The sender cannot tell from SendMessage's result that the message was refused.
  - **Sessions of one account folder see only each other.** The person's own sessions, running on the same Mac under their own folder, were not in the list.

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
- **Result: the clock part is measured; the sleep window is an owner step** (no prompt).
  - **`std::time::Instant` stops during sleep on macOS.** The project's toolchain (1.98.1, `library/std/src/sys/time/unix.rs`) builds `Instant` on Apple from `CLOCK_UPTIME_RAW`, "that does not increment while the system is asleep". This Mac shows it without a sleep test. `p17-sleep-probe --once` read, at one moment:
    - `mach_continuous_time`: 177,201 s since boot;
    - `CLOCK_MONOTONIC`: 176,992 s;
    - `CLOCK_UPTIME_RAW`: 101,874 s.

    About 75,300 s (20.9 h) of sleep since boot are missing from `Instant`.

    The lease rule of D5 already uses only the server's clock for expiry. On the Mac, "the lease is lost after every wake" must be detected from a wake notice or a wall or continuous clock, never from an `Instant` difference. An `Instant` timer simply does not see the sleep. The D5 text needs no change; M58 adds the test `mac_lease_lost_after_wake_without_instant`.
  - **Prepared for the owner:** `p17-sleep-probe.swift`, a standalone Swift program built with system frameworks only (`swiftc -O -swift-version 5`, not part of the Cargo workspace), and `p17-sleep.sh` to build, start and report it.
    - It logs the `NSWorkspace` will-sleep and did-wake notices that GPUI's `on_system_sleep` uses (`vendor/gpui_macos/src/platform.rs`).
    - It also logs the I/O Kit power messages (`IORegisterForSystemPower`): `can_sleep` (idle sleep only), `will_sleep`, `allowed`, `powered_on`.
    - On `will_sleep` it holds `IOAllowPowerChange` for up to `--hold` seconds (default 20), sends one request to `--url`, and logs `net_ok` or `net_error` with the time taken.
    - Every line carries the wall, uptime-raw, monotonic and continuous clocks.
    - Registration was checked here (`iokit_registered`). No sleep was triggered: putting the Mac to sleep is the owner's step.
  - **Owner steps** (each about 3 minutes):
    1. On battery: run `p17-sleep.sh <claude-dir> <codex-home> <scratch> http://<test-server>/ 20`, wait 10 s, close the lid, wait 2 minutes, open it, then `p17-sleep.sh … report <log>`.
    2. The same on mains power.
    3. Idle sleep: the same, but leave the Mac untouched until it sleeps by itself (`pmset -g` shows the idle sleep time; running programs can prevent idle sleep, so the person may first need to end them).
    4. Paste the three reports here. Pass needs a held window of at least 5 s with `net_ok` in it.

## Probe matrix (land-m2)

Every feature of the plan that depends on a CLI surface names the probe that shows the surface is there, and the fallback when it is not. Probes run once per CLI version, at start and after an update, as M03's `cli-probe.sh` does for flags. None of them sends a prompt. The kinds of probe:
- **help**: the flag is in `claude --help`, `codex --help` or `codex exec --help`. This is M03's `cli-flags.json`.
- **features**: the name and stage in `codex features list`.
- **doctor**: `claude doctor`, run in a scratch folder whose `.claude/settings.json` holds the settings Glasscode would pass (it reads the settings files of the current folder; it has no `--settings`). In M01 it listed, with no prompt: `Invalid settings - …/.claude/settings.json › hooks.GlasscodeMadeUpEvent: Unknown hook event "GlasscodeMadeUpEvent" was ignored`. It did **not** flag an unknown `sandbox` key or an unknown top-level key, so it probes hook event names only.
- **Unknown Codex keys pass.** `codex debug prompt-input -c glasscode_bogus_key=true hello` exits 0, and `--strict-config` is not supported for `codex debug`. So a Codex key is probed by its effect, or not at all.
- **start**: a `claude -p --input-format stream-json` start with no user line. SessionStart hooks run and `--include-hook-events` reports them (P08, P12).
- **prompt-input**: `codex debug prompt-input` with the flags (P02, P11).
- **hooks/list**: app-server `hooks/list` (P06).
- **sandbox**: `codex sandbox -P <profile> -- <probe>`, P07's script cut down to a self-test.
- **schema**: `codex app-server generate-json-schema`, compared with the recorded one.

A failed probe turns the feature off with a visible reason naming the flag or key, as land-m2 asks. It never leaves a dead terminal.

| Feature (slice) | CLI surface | Probe | Fallback | Proof |
|---|---|---|---|---|
| Claude brain without a shell (M50) | `--restricted --tools "" --strict-mcp-config --mcp-config --allowedTools --permission-mode dontAsk --permission-prompts none --disable-slash-commands` | help; at each start, `init.tools` must equal Glasscode's tools | `--tools ""` plus a `PreToolUse` deny (guarded); else the Claude brain is off | P01 |
| Claude brain on the plan, not a key (R6) | `init.apiKeySource` | at each start, `none` | brain off, with the reason shown | P01 |
| MCP handshake of `glasscode mcp` (M34) | `server/discover` (MCP 2026-07-28) before `initialize` | the first message the stub receives | answer `initialize` only; the CLI falls back | P01 |
| Usage windows (D6, advisory) | `rate_limit_event.unifiedWindows` | at each start, the keys present | Glasscode's own token ledger only | P01 |
| Chat lane persistence (D8) | `--session-id`, `--resume` of a stream-json session; Codex `exec resume` | help | replay the rollover summary | P02 (Codex), P07 (Claude `-p --resume`) |
| Codex brain profile (M50) | `--disable shell_tool/apps/plugins/multi_agent/memories/browser_use/computer_use`, `-s read-only`, `--ignore-user-config`, `--ignore-rules`, `-c skills.include_instructions=false`, `skills.bundled.enabled=false` | features, help; prompt-input for the person's skills; **`ALL_TOOLS` at the first start** for the tool list (prompt-input leaves out deferred tools) | the invariant "read-only sandbox, network off" | P02 |
| Codex MCP calls without a prompt (M50) | `mcp_servers.<id>.default_tools_approval_mode="approve"`, `required=true` | prompt-input starts the server (`tools/list` in the stub log) | Codex brain off | P02 |
| Codex wake within budget (D6) | `model_instructions_file`, `include_permissions_instructions`, `include_environment_context` | prompt-input size against the budget | Claude-only wakes, or a raised cap (owner) | P11 |
| Brief to sessions (M20.2) | the `SessionStart` hook's `additionalContext` on both CLIs (Claude `source` `startup`, `resume`, `compact`; Codex `startup`, `resume`), the Codex hook trusted through `hooks.state` in the account's `config.toml` | Claude: start (a `hook_response` line, no prompt); Codex: app-server `hooks/list` shows the Glasscode `SessionStart` hook `trusted` | brief at start only, changes as the first lines of the next Glasscode prompt | P03, P06 |
| Fresh system prompt on resume (if used) | `--system-prompt-snapshot off` | help | the hook path | P03 |
| Claude's auto-memory redirect (M22, D19) | `autoMemoryDirectory`, `autoMemoryEnabled` in `--settings` | none without a prompt (doctor does not check keys); at first use, the first memory file lands outside the folder | native memories on, labelled as a second memory | P04 |
| Codex memories off (D19) | `features.memories`, `--disable memories` | features (`memories` present, default `false`) | label in the brief; harvest as proposals | P04 |
| Turn states (M30) | hook events `Stop`, `StopFailure`, `SubagentStart`, `SubagentStop`, `PermissionRequest`, `PermissionDenied`, `SessionEnd`, `CwdChanged`, `PostCompact`, `UserPromptSubmit`, `Notification` | doctor, with each event in the settings (an unknown one is "ignored") | leave the event out; M30's watchdog for the missing signal | P05 |
| Turn text without the transcript (M30) | `Stop.last_assistant_message`, `prompt_id`, `background_tasks` | start, then the first real `Stop` of a session: check the fields | read the transcript after it settles | P05 |
| Codex hooks without the bypass (M30) | `hooks.state."<key>".trusted_hash` in the account's `config.toml` | hooks/list shows `trusted` for each Glasscode hook | rollout tail and the reconciler | P06 |
| Codex turn end (M30) | rollout `event_msg` `task_complete` / `turn_aborted` | schema of a recorded rollout (M02 fixtures) | the reconciler | P06 |
| Codex `notify` (M30) | `notify` config key | none without a prompt (unknown keys pass); at the first turn, no notify line within 2 s of the rollout's `task_complete` | rollout only | P06 |
| Session profiles, Claude (M22) | `sandbox.*`, `permissions.deny`, `disableAllHooks: false`, `disableClaudeAiConnectors`, `--strict-mcp-config` | start (Glasscode's hooks run in a repository that disables hooks); `failIfUnavailable` refuses a start without a sandbox; a rule that does not bind is **not detected** (P07's `/path` case), so M22's test checks the rule syntax (`//` for absolute paths) | the row of section 7 becomes "guarded"; the session is marked "unreported" | P07, P08, P14 |
| Session profiles, Codex (M22) | `default_permissions`, `permissions.<name>.filesystem` and `network` (inline tables), `-P` | sandbox self-test (own worktree writable, `authority/` refused, socket connects) | `--sandbox workspace-write` without the deny rows; section 7 says guarded | P07 |
| Cross-session messages refused (M22) | `crossSessionInbound: "refuse"` | none: not detected at use either (the sender sees success); M22's test checks the profile carries it | a `PreToolUse` deny on the messaging tools (guarded) | P16 |
| Outside sessions listed (M14 reconciler) | `claude agents --json` | help | no outside sessions shown | P16 |
| Transcripts kept (M24) | `cleanupPeriodDays` | none needed: the digest is always copied | digest only | P12 |
| Op waits in a tool call (M34) | Codex `mcp_servers.<id>.tool_timeout_sec`; Claude's MCP timeout | none without a prompt (unknown keys pass); at use, `notifications/cancelled` arrives at the old timeout | the op returns `Waiting` with a pending id | P15 |
| Codex driver by app-server (D14, after release 1) | `app-server --listen unix://`, `--remote unix://`, `thread/*`, `turn/*` | schema | PTY, rollout and hooks | P13 |
| Today's session launch (M03) | Claude `--resume --session-id --model --permission-mode --settings --plugin-dir --allowedTools --mcp-config`; Codex `--model --sandbox --ask-for-approval -c resume` | help (`cli-flags.json`) | the session does not start, and says which flag | M03 |

## Stop conditions

The owner decides first when any of these happens. P01, P03 and P06 hold wave 1. P11 holds wave 5 only, as its own pass rule says ("stops wave 5 until the owner decides"):
- P01 shows the brain cannot use the subscription in the restricted profile;
- P03 has no path that delivers a current brief at start;
- P06 shows a Codex turn end cannot be seen without the bypass;
- P11 shows a fresh wake above the limit.

**Status after this run:**
- **P01: not hit.** The brain uses the plan in the restricted profile.
- **P03: not hit.** The SessionStart hook delivers the current brief on start, resume and compaction.
- **P06: not hit.** In an interactive TUI session the turn end was known within 41 ms without the bypass flag: from the trusted Stop hook, `notify` and the rollout's `task_complete`.
- **P11: hit, and decided.** The Codex fresh wake took 36,335 tokens (limit 20,000), over three requests of about 12,000 tokens each; with the three trim keys it took 22,542. Claude haiku took 10,727 and Claude's default model 9,197. It was reported to the main agent at once. Decision O15, decided by the lead on 2026-11-19 under the owner's delegated authority (given 2026-11-20): waves 1 to 4 continue; wave 5's wake lane runs on Claude's cheapest model; the Codex wake lane stays off until a re-measure is at or under 20,000 (design D6).
