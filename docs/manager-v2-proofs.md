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
- **Result, Codex: the switches cannot be checked without a model; off is the default** (no prompt). Script: `p04-memories.sh <claude-dir> <codex-home> <scratch> codex`.
  - **Method.** The script planted `$CODEX_HOME/memories/memory_summary.md` and `MEMORY.md` holding the marker `CODEX-MEM-MARKER-41` (the file names come from the 0.167 binary) and read `codex debug prompt-input` six ways: feature off, `--enable memories`, plus `use_memories=false`, plus `generate_memories=false`, plus `disable_on_external_context=true`, and `-c memories.version="v1"|"v2"`. The folder was removed afterwards.
  - **Result.** The marker, and the word "memory", appeared in none of them, so `prompt-input` does not render memory injection.
  - **What is known without a model.**
    - `codex features list` shows `memories  stable  false`: memories are off unless a person turns them on.
    - `--disable memories` (the same as `-c features.memories=false`) overrides the person's `config.toml` for that run. So D19's off switch for Careful and local-only projects is `--disable memories` on the session's command line.
  - **Not verified.** That a session with memories on actually injects them, and that `use_memories=false` stops it, needs two prompts. They are held back until the main agent answers the stop condition.
  - The Claude half (`autoMemoryDirectory`, `autoMemoryEnabled`, `/memory`) also needs a prompt and is held the same way.

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
- **Result: partial; four of the five cases are not run yet.** What P03's three interactive sessions showed is below. They had every event of `claude-hooks.py` in `--settings`. `p03-brief.sh` and the zero-prompt starts of this proof produced them.
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
  - **Not run yet** (about 5 prompts, held back with the stop condition): an interrupt (Esc during a turn) and its transcript marker; a forced API error (`--model` with an invalid name) for `StopFailure`; a `kill -9` of the agent; a stop-hook continuation (`hook.py --stop-once`; two requests for one prompt); and `PermissionRequest`, `PermissionDenied`, `CwdChanged` and `SubagentStart` with their fields.
  - **Server and Windows lines of M01** (P03 and P05 in a tmux session on the Docker test server with stand-ins, and once on the owner's Windows machine) were not run: see "Could not verify".

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
- **Result, turn end: likely pass, timing not measured.** Without any hook and without the bypass, the rollout of each P02 `codex exec` turn holds an `event_msg` of type `task_complete` (2 turns, 2 `task_complete`, beside `task_started` and `token_count`). So the rollout alone reports a turn end, which is this proof's fallback. Not measured yet:
  - the time from the end of the turn to the rollout line, in an interactive TUI session (the 2 s rule);
  - whether the trusted Stop hook and `notify` fire there, and how a `-c notify` replaces a configured one.

  These need 2 prompts (`p06-codex-turn-end.sh … live start|resume`, which also gives P03's Codex half) and are held back with the stop condition.

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
  | Read `.env` in the worktree (deny glob `:workspace_roots."**/.env*"`) | refused | refused |
  | List `~/.ssh`, read a key in it, read `~/.config/gh/hosts.yml` | refused | refused |
  | Reach the Keychain service (`security find-generic-password -s <no such item>` answers "could not be found") | **reached** | **reached** |
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
  - **`.git` is read-only inside Codex's workspace sandbox**, in a linked worktree and in a plain clone alike ("Unable to create '<clone>/.git/index.lock'"). Granting `write` on the gitdir's `worktrees/<name>`, `objects`, `refs` and `logs` gets further, but writes at the top of `.git` (`packed-refs.lock`) are still refused. **No `git commit` ran in the Codex sandbox** with these grants. Person sessions should keep today's behaviour: with `--ask-for-approval on-request`, Codex asks to run the command outside the sandbox. That is expected and was not run here. P07's fallback covers that case: the commit runs outside the sandbox for the person profile, and the limit is guarded there. **Open for the owner (no written fallback):** how a Codex *automation* session commits, which by these probes it cannot do inside the sandbox. The options are a Glasscode commit op, as D13 plans for pushes; finer gitdir grants (the subfolder grants got further, so a complete list may work, untested); or Claude-only automation. The design is not changed for this.
  - **`gh` cannot start** when `~/.config/gh` is denied, because it reads `config.yml` first. Person sessions get `GH_CONFIG_DIR` pointed at a Glasscode-made copy holding `config.yml` only, never `hosts.yml` (the token stays in the Keychain), or no `gh` at all. Automation sessions get no `gh` (D13 and P14).
  - **The Keychain is reachable through its service**, although `~/Library/Keychains` is denied. Whether a stored secret can be read depends on the item's own access list, and this proof did not try to read one. Section 7 now says the sandbox denies the Keychain *files*, not the Keychain service.
  - **Not covered here:** the Claude half (the file tools, hooks inside the sandbox, the login from an account folder, and the `!` negation) needs the model and waits for the live budget. M01 notes "no sandbox" on Windows.

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
- **Result: not run; prepared** (owner step, and a proxy that waits for the live budget). Script: `p10-quit.sh <claude-dir> <codex-home> <scratch> proxy normal|force | watch`.
  - **What the code does.** The app runs each session in a pseudo-terminal of its own (`alacritty_terminal`): the agent is a session leader on that terminal. On quit, `Workspace`'s `on_app_quit` only installs a waiting update and keeps unsaved editor text (`crates/glasscode/src/workspace.rs`). Nothing stops the sessions on purpose. A normal quit drops the terminals (SIGHUP to each agent), and a force quit closes them through the kernel (the same SIGHUP). P05 saw what Claude does on SIGHUP: `SessionEnd` with `reason: other`, exit 129.
  - **Proxy, 4 prompts.** One Claude and one Codex session in `pty-drive.py`, each running `sleep 45` through its shell. 8 s into the turn, the terminal's owner either exits (`normal`) or is SIGKILLed (`force`). It then records for 20 s whether the agents and their `sleep` live on, and the last transcript and rollout lines.
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
- **Result so far: stop condition hit on Codex.** The wake is the P01 and P02 runs: the 4,000-character situation of `situation.py` and one stub tool call. Tokens are as reported by the CLI and include cached input.

  | Wake | Model requests | Input tokens (cached) | Output | Total tokens | Time to answer |
  |---|---|---|---|---|---|
  | Claude, haiku (`claude-haiku-5-5`), P01 `--tools ""` | 2 | 10,678 (5,299 read, 5,375 written) | 49 | **10,727** | 3.8 s (first token 2.3 s) |
  | Claude, haiku, P01 `--tools ToolSearch` | 3 | 17,827 (13,252 read) | 133 | 17,960 | 4.8 s |
  | Codex, `gpt-5.6-luna` low, P02 | 3: 11,126 + 12,576 + 12,633 | 36,221 (32,768 cached) | 114 | **36,335** | 7.5 s |

  The Codex wake is above the 20,000-token limit; Claude's default model has not been run yet. Each Codex request carries about 11,000 tokens of Codex's own prompt (the code-mode `exec` description, the base instructions and the permission text), and a wake with one tool call takes three requests. By the rule above this stops wave 5 until the owner decides. It was reported to the main agent at once.

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
  - The binary also skips the sweep while a settings file cannot be read, and while server-managed settings have not loaded ("A later process that is not paused for this reason runs the cleanup").
  - **For M24.** The bounded digest stays the record. A full transcript can be kept only by copying it, or by setting `cleanupPeriodDays` in the account folder's own `settings.json`, which is the person's choice and changes their other sessions too. Copying is a consequence of D23 and needs no D-row change. `cleanupPeriodDays` must be at least 1; 0 is refused by the schema ("To disable transcript writes entirely … use --no-session-persistence").

## P13 The Codex terminal UI attached to an app server
- **Method.** Start `codex app-server --listen unix://<path>` under one scratch `CODEX_HOME`. Run the terminal UI in a PTY with `--remote unix://<path>` and connect a second client.
- **Measure.** Whether the second client gets turn and item events for the UI's thread, can answer an approval, can steer and interrupt, and what the UI shows when it does.
- **Pass.** Record only. If it passes, a later slice can build the Codex session driver on it behind a switch (D14).
- **Waits.** Nothing in release 1.
- **Result so far: the plumbing works; the turn part is not run yet** (no prompt so far). Tools: `appserver-client.py` (JSON-RPC over stdio, or WebSocket over the Unix socket) and `pty-drive.py` for the TUI. The schema comes from `codex app-server generate-json-schema --out <dir>` (39 files).
  - **Isolation.** `codex app-server --listen unix://<path>` makes `<path>` a symbolic link to a socket in `/private/tmp/codex-daemon-<uid>/<hash>`, with the hash derived from the path. A second server for the same path refuses: "app-server control socket is already in use at …". So there is one server per socket path. The scratch `CODEX_HOME` decides the account (the `initialize` result names `codexHome`). Other Codex servers on the Mac (the ChatGPT app runs its own, over stdio) are separate processes; this proof never connected to one.
  - **Transport.** The Unix socket speaks **WebSocket**, as `--remote unix://` does. Raw JSON lines, also through `codex app-server proxy --sock <path>`, get the connection closed. A WebSocket client over the socket gets `initialize` → `{userAgent, codexHome, platformFamily, platformOs}`, then a `remoteControl/status/changed` notification.
  - **The TUI attached.** `codex --remote unix://<path>` showed its normal prompt ("Ask Codex to do anything"), with the server's working folder in its status line beside its own. Glasscode must therefore start the server in the session's worktree. A second client connected at the same time without trouble. `thread/loaded/list` was still empty 7 s after the TUI started, so the TUI's thread is loaded only with its first turn.
  - **Quitting the TUI** (Ctrl-C) leaves the work running. It printed "Disconnected from this task. Any running work continues.", a `codex --remote unix://<path> resume <thread>` line to reconnect, and "Stop the current turn: run codex --remote … agents, select this task, and press x."
  - **Still to do** (about 3 prompts, held back with the stop condition):
    - one TUI turn that needs an approval, while the second client calls `thread/resume` on the TUI's thread to receive `turn/*` and `item/*`;
    - answering the approval from the second client;
    - `turn/steer` and `turn/interrupt` from the second client;
    - what the TUI shows for each.
  - `--no-daemon` does not exist in 0.167's `app-server` help; `app-server daemon start|stop|…` manages a long-lived daemon instead.

## P14 Brokered push
- **Method.** Run an automation-profile Claude session whose sandbox network allowlist does not include github.com. Prototype `glasscode git push` as an op that pushes from the app's process with the branch rule.
- **Measure.**
  - whether the agent can finish a "fix and push" task through the op;
  - how many extra turns it needs;
  - whether any other path to GitHub stays open: the browser, or an MCP server.
- **Pass.** The task finishes, and no other route reaches the forge.
- **Fallback.** Use forge rules only (D29). Autopilot pushes only where branch protection holds.
- **Waits.** Owner decision O7, then M23.
- **Result: partial; the brokered-op prototype is not done** (no prompt so far).
  - **The profile can be built from documented settings.** `sandbox.enabled`, `sandbox.network.allowedDomains` (a list without `github.com`), `sandbox.allowUnsandboxedCommands: false` and `sandbox.failIfUnavailable: true` are all in the 2.1.328 settings index and its binary. Whether `gh` fails inside such a sandbox needs one Bash call by the model; it is folded into P07's Claude automation run (1 prompt, held). For Codex, P07 already shows `gh` failing to start under the profile, and `git ls-remote https://github.com/…` failing with the network off.
  - **Other routes to the outside that are open by default.**
    - **The claude.ai connectors of the person's account.** `claude mcp list` on the scratch account (signed in to the person's subscription) shows nine claude.ai connectors (mail, drive, calendar, an issue tracker and others), seven of them connected. Any session of that account has them unless it passes `--strict-mcp-config` (P01's init then listed only `glasscode`) or `disableClaudeAiConnectors: true`. Section 7 now requires both for automation and quarantine sessions.
    - **The WebFetch and WebSearch tools** run in the CLI process, not under the Bash sandbox. They can read pages but carry no forge credentials. The automation profile denies them by permission rule (guarded).
    - **The browser.** Claude in Chrome is off without `--chrome`. The Glasscode browser's MCP server is added by `browser::agent_args`, and D28 already gives automation and quarantine sessions no signed-in browser.
    - **Codex** has its network off in the workspace sandbox (P07). Its web search tool was not checked.
  - **Not done:** the `glasscode git push` op as a prototype, the "fix and push" task through it, and the count of extra turns.

## P15 Tool-call timeouts
- **Method.** A stub MCP tool that answers after 30 s, 2 min, 5 min and 10 min, on both CLIs.
- **Measure.** When each CLI gives up.
- **Pass.** The time is written down. It decides whether an op that waits for the person can keep the tool call open, or must return `Waiting` with a pending id.
- **Waits.** M34.
- **Result: not run** (3 prompts, held back with the stop condition). The stub's `wait` tool (`stub-mcp.py --tools wait`) answers after the given seconds and logs any `notifications/cancelled`. The planned runs, each with one call of `wait 600`, measure when each CLI gives up:
  - Claude with the default timeout;
  - Codex with the default;
  - Codex with `mcp_servers.glasscode.tool_timeout_sec=900`.

  The Codex docs give the default as 60 s per tool call ("Overrides the default 60s per-tool timeout"). So unless M34 sets `tool_timeout_sec`, an op that waits for the person cannot keep a Codex tool call open longer than that.

## P16 Claude sessions messaging each other
- **Method.** Two Claude sessions of the same account. Try the native cross-session message features (`SendMessage`, the inbox socket), and the setting `crossSessionInbound: "refuse"` in `--settings`.
- **Measure.** Whether one session can make another act, and whether the setting stops it.
- **Pass.** A way to refuse is confirmed for automation and quarantine profiles.
- **Fallback.** A `PreToolUse` deny on those tools (guarded), and a label in design section 7.
- **Waits.** M22.
- **Result: partial, no prompt so far.**
  - **Sessions are scoped to the account folder.** `claude agents --json` on the scratch account lists 0 sessions, while the person's own account had many running on this Mac at the same time. A session of one account folder does not see another's.
  - **The inherited socket.** The route that stays is the environment: a `claude` started inside another Claude session inherits `CLAUDE_CODE_MESSAGING_SOCKET` and `CLAUDE_CODE_MESSAGING_TOKEN`. Glasscode already removes every `CLAUDE*`, `CODEX*` and `AI_AGENT*` variable from its own process at start (`env::SCRUBBED_PREFIXES`), and the proofs ran with `env -i` for the same reason.
  - **The setting.** `crossSessionInbound` is documented with the ladder `accept` < `hold` < `refuse`. `--settings` can set it, and a project file can only make it stricter, never looser (settings docs, "Exceptions to managed settings precedence").
  - **Not run** (about 3 prompts): two sessions of the scratch account, one asked to message the other; then the receiver with `{"crossSessionInbound": "refuse"}`.

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
    3. Idle sleep: the same, but leave the Mac untouched until it sleeps by itself (`pmset -g` shows the idle sleep time; on this Mac sleep is currently "prevented by powerd, Claude, caffeinate", so the person may first need to end those).
    4. Paste the three reports here. Pass needs a held window of at least 5 s with `net_ok` in it.

## Stop conditions

Wave 1 does not start, and the owner decides first, when any of these happens:
- P01 shows the brain cannot use the subscription in the restricted profile;
- P03 has no path that delivers a current brief at start;
- P06 shows a Codex turn end cannot be seen without the bypass;
- P11 shows a fresh wake above the limit.
