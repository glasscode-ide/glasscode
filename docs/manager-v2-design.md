# Manager v2: Design

| | |
|---|---|
| Document | GC-MGR-002, issue 1 (owner decisions accepted 2026-11-19; not committed yet) |
| Date | 2026-11-19 |
| Checked against | `glasscode/session-393a` at `0cf333f` (main after PR 76) |
| Slice plan | [`manager-v2-slices.md`](manager-v2-slices.md) |
| Proofs | [`manager-v2-proofs.md`](manager-v2-proofs.md) |
| Replaces | the design page "Glasscode Manager v2" (versions 1 to 4) and [`manager.md`](manager.md) once v2 ships |
| Review | 19 agents, 268 verified findings (27 critical, 171 major). Ids in brackets, such as (mem-2), refer to that review. |

This document says **what** Manager v2 is and **which rules** every slice must keep. The slice plan says **how** and **in which order**.

## Contents

1. What the person asked for
2. The shape
3. Principles
4. Decisions
5. Path layout and path classes
6. Data map
7. Threat model
8. Shared interfaces
9. Cross-cutting rules
10. Lifetime: quit, update, sleep
11. What the manager cannot do
12. Compared with other systems, and rejected designs

## 1. What the person asked for

These twelve requirements are the acceptance basis. Section 13 of the slice plan traces each one to slices and tests.

| # | Requirement |
|---|---|
| R1 | The manager can do every single thing the IDE can do: sessions, settings, checks, navigation. |
| R2 | Every Claude or Codex session reports to the manager when it finishes something or goes wrong. The manager always knows what is happening and acts according to its configuration. |
| R3 | It takes decisions when asked, and manages things. |
| R4 | Scheduling, and awareness without being asked. |
| R5 | 100 % Rust, minimal, no extra dependencies, fast. |
| R6 | It runs on the person's Claude Code and Codex subscriptions, not API keys. |
| R7 | Chat: talk to it from Today; it controls the IDE ("go to this session and tell me what is happening"). Voice is a separate plan. |
| R8 | A very good memory: Markdown, one memory per project, scoped. |
| R9 | On install it looks around, asks questions and learns, so moving to Glasscode feels like a migration. |
| R10 | Per-project behaviour: some projects fully managed (it finds work, opens PRs), careful company projects where it always asks. |
| R11 | Glasscode owns memory in one place for both agents. Removing a worktree archives its memory instead of losing it, and any session or the manager can still find it. **The person's top priority.** |
| R12 | State of the art: smooth, every case handled, compared with every available technology, a complete step-by-step plan. |

## 2. The shape

Glasscode does not build an agent loop. Claude Code and Codex are the loops, and they run on the person's subscriptions. Glasscode builds the harness around them:

```
 sources                              Glasscode (Rust)                                  agents
 ───────                              ────────────────                                  ──────
 Claude hooks ──┐                 ┌─ words ledger (whose words) ─┐
 Codex rollout ─┤                 │                              │
 git, GitHub ───┼──▶ journal ──▶ rules ──▶ playbooks ──▶ wake queue ──▶ brain: wake lane ─┐
 approvals ─────┤      │          │                                                      │
 your actions ──┤      │          └─ reconciler (snapshot safety net)                     │   typed ops
 clock ─────────┘      ▼                                                                  ├──▶ (glasscode mcp)
                  turn reports ──▶ work ledger                you ──▶ brain: chat lane ───┘        │
                                                                                                   ▼
 memory (git, Markdown) ──▶ briefs for every session          gate: decide(caller, op, targets, policy)
 authority store (app only) ──▶ policy, playbooks, approvals                │
                                                                            ▼
                                                     effect ledger ──▶ registry handlers ──▶ sessions, UI,
                                                     (claim, act, result)                    board, PRs, settings
```

Five parts, built in this order of trust:

1. **Words and callers.** Who is speaking, decided by the channel, never by the text.
2. **Memory.** One store owned by Glasscode, per project, for both agents.
3. **The gate.** Every action of every non-person caller passes one Rust function. Sessions also get an OS-level profile.
4. **Events.** One journal, turn reports with an end cause, one work ledger, one effect ledger.
5. **The brain.** The official `claude` and `codex` programs, with no shell, using only typed Glasscode tools.

## 3. Principles

These override anything else in the plan.

- **P1. No authority without a wall.** Every limit names the plane that carries it.
  - *Enforced*: the Rust gate on Glasscode ops, an OS sandbox, or forge rules.
  - *Guarded*: vendor hooks, deny rules on file tools, PreToolUse checks. They catch the usual case and are never called a wall.
  - *Advised*: prompt text.

  A guarded or advised limit is never described as absolute. (trust-1, alt-1)
- **P2. Nothing acts before the session profile and the v2 gate exist.** Everything that needs no model comes before the brain runtime: the journal, turn reports, native playbooks, the scheduler, delivery. The brain never ships with a shell. (crit-3)
- **P3. Memory is first.** It lands as early as its three prerequisites allow: the project key, the registry parse, and the words ledger.
- **P4. 100 % Rust and no new crate.**
  - Authority data is JSON through `serde_json`, which is already a dependency.
  - No TOML or YAML parser is added. MCP stays hand-written: a shared `mcp_stdio` module, extracted from `crates/glasscode_core/src/browser/mcp.rs`.
  - A slice that needs a new crate names it and waits for the owner.
  - The LiveKit voice worker is Python and stays an optional add-on outside this claim. (exec-17, crit-19)
- **P5. Every claim has a test or a number.** "Every case" is shown by the traceability table, the adversarial suite, the soak and fault injection. (crit-16, land-14)

## 4. Decisions

D1 to D30 are binding. "Owner" marks a decision that waits for the person. The proposal stands until they change it.

| # | Decision | Findings |
|---|---|---|
| D1 | **Whose words count.** `glasscode_core::words` keeps a person message ledger. Each record's origin is set by Glasscode on the input side, never by the caller, the hook or the text. Three tiers decide what a message can do (table below). Hook events and Codex rollout user messages are only *matched* against the ledger. Secret spans are replaced before hashing. Each machine writes its own folder (`words/<machine>/`). Phone input is written by glasscoded and reaches the Mac at tier 2 at most. | mem-2, trust-6, land-1, ux-m1, crit-2 |
| D2 | **Callers by channel**: `Ui` (in-process only), `Session`, `Manager(lane)` on its own MCP server instance, `Phone(device)` (signed), `Machine(id)` (another Glasscode machine's sync connection), `Terminal` (anything else). The client's `interactive` flag no longer decides trust. A person typing in a Glasscode shell tab is recognised by the app's own key events. | trust-2, ops-4 |
| D3 | **Chat grants** in release 1 are checked by Rust (rules below). A separate model checker is a measured option after release 1, and never sits in the path of reads. | land-12, trust-5, crit-4a |
| D4 | **The brain has no shell.** Claude runs with `--restricted --tools "" --strict-mcp-config --mcp-config <glasscode mcp> --allowedTools "mcp__glasscode__*" --permission-mode dontAsk --permission-prompts none --disable-slash-commands`; all of these flags are in `claude --help` 2.1.322 and 2.1.328, and P01 shows their behaviour (the tool list holds only Glasscode's tools, and the login is the plan). Codex runs with its shell, apps, plugins, multi-agent and memories turned off, a read-only sandbox, network off, `--ignore-user-config --ignore-rules`, and the Glasscode MCP server set to `default_tools_approval_mode="approve"` and `required=true` (P02). It also passes `-c skills.include_instructions=false -c skills.bundled.enabled=false`: otherwise the prompt lists the person's own skills from `~/.agents/skills`, whatever `CODEX_HOME` is (P02). In code mode (0.167) `apply_patch` and `view_image` stay visible inside the `exec` tool. No flag removes them, so for Codex the stated invariant is "read-only sandbox, network off": they cannot write (P02 fallback). In Codex's code mode the model's one tool is `exec`, a JavaScript isolate with no file system or network of its own, through which the nested tools are called. M50 also passes `--disable browser_use --disable computer_use`, and reads the brain's tool list from `ALL_TOOLS` at its first start, because prompt-input leaves out the deferred tools. That no model-visible tool reaches the network outside the sandbox is not verified (P02). Reads become typed ops. `DENIED_TOOLS` is deleted. One `HeadlessProfile` builder serves every non-interactive launch. | brain-1, alt-2, trust-10, ops-m1, alt-m1, trust-m2 |
| D5 | **One manager, present everywhere, acting from one home at a time, moving by itself** (owner, O12, 2026-11-19). The same conversation, memory, work and approvals are on the Mac and on the server. Only the *home* acts. Settings › Manager › Home has three choices: **Automatic** (the default once a server is paired), **Always on the server**, and **Always this Mac**. Under Automatic, the Mac is home while you are at it, and the server is home when the Mac sleeps, quits or is away. glasscoded is the single arbiter: it holds a lease with a generation number, and outward effects are fenced by that generation. The rules are below this table. With no server paired, the Mac is home and nothing switches. `decide()` lives in core; authority flows from the Mac to the server only. | crit-5, events-8, ops-8, crit-10 |
| D6 | **Budget.** The manager keeps its own token ledger, read from each brain result's usage, with a daily cap and an hourly wake cap. Vendor usage readings are advisory. **Wake lane (O15, decided by the lead on 2026-11-19 under the owner's delegated authority, given 2026-11-20; on proof P11):** wakes run on Claude's cheapest model; a fresh wake measured 10,727 tokens on haiku and 9,197 on the default model, a floor: one tool call, from a server with two tools. A Codex wake lane is allowed only with `-c model_instructions_file=<Glasscode's instructions> -c include_permissions_instructions=false -c include_environment_context=false` and a re-measure at or under 20,000 tokens. It measured 36,335 before and 22,542 after those keys, so in release 1 it stays off. Codex can still serve the chat lane. | events-17, brain-6, exec-18 |
| D7 | **No small-model quick check in release 1.** Rules and native playbooks decide; wake volume is measured first. | events-16, brain-7, land-13 |
| D8 | **The chat lane persists.** Claude uses its own session id with persistence on; Codex uses `codex exec resume`, without `--ephemeral`. The system prompt is static, and memory and the situation go in user turns, because `--system-prompt-snapshot` records appended text on the first request. The conversation rolls over to a fresh one, with a summary, past a set size. | brain-4, land-3, alt-9 |
| D9 | **One effect ledger, which is also the audit record.** Each effect has a deterministic key. The claim is made durable before the action and the result written after it: `fsync(2)` costs 65 µs p95, and `F_FULLFSYNC` (Rust's `File::sync_data` on Apple) 4.6 ms p95 (P11). M31 chooses one and counts it in op dispatch. Ops declare a retry class: idempotent, verifiable or at-most-once. | events-7, trust-16, alt-4 |
| D10 | **Authority store.** Policy keyed by op name and op group, playbooks, schedules, approvals and device keys are JSON in `~/.glasscode/authority/`, edited only through the app. A file that changes outside the app counts as not approved. The 13 kinds in `state.json` stay frozen for older builds. `POLICY.md` in memory becomes prose with a generated header and carries no authority. | trust-4, events-10, exec-7, ops-12 |
| D11 | **Hooks are guarded**, never called enforced. Hook commands use absolute paths and an integrity check. Under v2, Claude session settings (M22) set `"disableAllHooks": false`: without it, a repository's `.claude/settings.json` with `disableAllHooks: true` turns Glasscode's `--settings` hooks off, and a bare command name runs whatever the project's `env.PATH` finds first (proof P08). A repository's own hooks still run beside Glasscode's. Codex hooks run without `--dangerously-bypass-hook-trust`: Glasscode writes a `hooks.state."<key>".trusted_hash` entry for each of its own hooks into the account's `config.toml`, with the hash that app-server `hooks/list` reports (P06). | crit-15 |
| D12 | **A session's permission prompts**, in this order: (1) approved allow-commands are compiled into the agent's own allow rules at launch; (2) a `PermissionRequest` hook asks the Rust gate, which never returns `updatedPermissions` and declines compound commands; (3) the screen path is used only when the person answers, with the prompt id checked again before each key. | ops-5, land-4, trust-14 |
| D13 | **Brokered push** for automation sessions, behind proof P14: fetch, push and PR creation become gated ops. The person's own sessions are unchanged. Owner: O7. | alt-1 |
| D14 | **Codex driver.** Release 1 uses `codex exec resume` for the brain, and PTY plus rollout plus hooks (once trusted without the bypass) for sessions. App-server comes by probe, after a conformance suite passes on three approved versions. | brain-8, alt-m3 |
| D15 | **Pause.** Any caller may pause the manager or a playbook; only the person can release it. | crit-4o |
| D16 | **Undo.** Each op declares undo as `None` or `Inverse(op)`. | ops-18 |
| D17 | **Accounts.** A hand-off between accounts is offered, never automatic. Which account's model may read which project is a per-project setting; the default is the project's own account. | mem-7, trust-m6 |
| D18 | **The runtime** is one piece of code. On the Mac it lives inside the app, on its own threads; on the server it lives inside glasscoded (`Host`: `AppHost` and `DaemonHost`). The lifetime rules are in section 10. A Mac background helper is rejected for now, with its trigger written down. | alt-m6, crit-11 |
| D19 | **Native memories.** Glasscode is the one trusted memory. Claude's auto-memory is redirected into `agent-notes/` (P04). Codex memories are off for Careful and local-only projects (P04). Owner: O4. | mem-6, land-6, alt-8 |
| D20 | **Trusted memory integrity.** Briefs and search results come only from the last commit Glasscode made; its id is kept in the authority store. Anything else is a pending edit the person accepts. A no-model scan runs on every write. | mem-1, mem-m1, trust-7 |
| D21 | **One path layout** (section 5) generates every session profile. A test fails when a folder has no class. | crit-1 |
| D22 | **Identity.** `ProjectKey` is a slug plus a hash, with aliases, and is separate from the project UUID of each machine. `WorkKey` names a piece of work. `MachineId` and a hybrid logical clock serve sync. | mem-5, exec-12, crit-9, crit-10 |
| D23 | **Archive by reconciliation.** It runs for every way a piece of work ends, and it copies a bounded digest of each transcript, because Claude deletes old transcripts. | mem-3, mem-11, exec-14, alt-m5 |
| D24 | **A turn is a state machine** with an explicit end cause, closed exactly once. The snapshot reconciler stays as the safety net. | events-2, events-3, events-4, alt-5 |
| D25 | **Voice is a separate topic with its own plan** (owner, 2026-11-19): configurable voice-to-voice providers, chosen with their cost in mind. This plan does not change `voice/`, the ElevenLabs copilot or the glasses' control API. M20.3 only copies the copilot's memory facts into proposals, and leaves its file in place. The chat's text interface (M52) is what a voice plan will connect to. | ux-6, exec-9 |
| D26 | **Done conditions are typed** and checked by code. | events-14, land-8 |
| D27 | **Source trust.** A `trusted_author` predicate gates every default playbook that points a session at outside text. Public repositories and fork PRs are detected. Untrusted input runs under the quarantine profile. | trust-8 |
| D28 | **The Glasscode browser** gets the op kinds `browser.read`, `browser.act` and `browser.eval`. Automation and quarantine sessions get no signed-in browser. The DevTools port is bound to a per-launch secret. | trust-m1 |
| D29 | **The forge carries the wall** for merge and force-push. Autopilot may push only where branch protection blocks force-push and requires review, with no bypass for the person. Owner: O7. | trust-20 |
| D30 | **Release 1** is waves 0 to 5, including the server home (M57) and automatic home (M58), closed by the release gate (M55) and the switch to v2 (M56). Windows runs the core stores, the registry, the gate, memory, the journal, playbooks and the chat. Autopilot on native Windows is offered only where forge rules carry the limits, because there is no Claude sandbox there. | crit-16, exec-26 |
| D31 | **Vendor terms, with their conditions.** The rules of D31 are listed below this table. | brain-9 |

**Vendor terms (D31).**

- **What Anthropic allows.** A person may sign in to the *unmodified* Claude Code binary with their own plan, "including where a platform hosts Claude Code". A product must not:
  - offer Claude.ai login itself;
  - route requests through plan credentials;
  - store or intermediate tokens;
  - modify the binary;
  - resell or intermediate usage.

  Plan limits "assume ordinary, individual usage".
- **What OpenAI allows.** App-server sign-in is allowed for local or open-source applications. Glasscode is GPLv3. It is not allowed for commercial or hosted services.
- **Glasscode's rules:**
  - It never calls a vendor login API and never handles a token. People sign in with the vendor CLIs.
  - On the server, people sign in through `claude setup-token` and the Codex device login. No credential file is ever copied.
  - Background wakes are off until the person switches them on. They are capped (D6), and their spend is shown.
  - Each vendor and each lane has its own off switch in Settings › Manager, and each lane can be pointed at an API key instead.
  - Glasscode has no remote switch, so a change of vendor terms reaches people through a release.
  - The owner writes to both vendors describing the wake lane and the server case, and keeps the answers in `docs/`.
  - UI text says "runs Claude Code" or "runs Codex", and no feature is named after a vendor.

**Home (D5)**, built in M57 and M58:

- **The record.** glasscoded keeps `{ home: MachineId, generation, lease_until (server clock), mode: Full | Degraded }`.
- **The lease.** The Mac is home only while it holds an unexpired lease, renewed with each sync (the lease is 30 s). Only the server's clock decides expiry. The Mac counts its lease as lost after every wake, and whenever no renewal answer came in time.
- **When the Mac is home** (Automatic): it is awake, it reaches the server, and you were present for 60 s (input recently, not a dark wake). Otherwise the server is home.
- **The switch: the planned hand-over (owner, 2026-11-19).** The title bar's "Server mode" and "Laptop mode" switch (today's `switch_ui.rs`) is the main way home and sessions move. You press it before you close the lid or leave, and see "Safe to close" when it is done:
  1. **Check first.** The server is reachable. Each session's Claude or Codex account is signed in there. Each project exists there.
  2. **The manager pauses work at a safe point.** Working sessions are told to finish their current step, note where they are in their work file, and stop. Idle sessions need nothing. "Move now" interrupts the sessions that are slow.
  3. **Every open session moves,** working or idle, several at a time. **A worktree moves as one unit with all its sessions,** never split between Mac and server. Worktrees with no open session stay, unless ticked, and move on demand when opened on the other side.
  4. **Each worktree has one owner at a time, with a counter** (the server-sync plan's session runner epoch, kept per worktree). A copy that sees a newer counter refuses to run. The old copy closes only after the new one runs.
  5. **The manager moves last** (D5 lease, generation + 1). It hands over its conversation, work items, pending cards and the list of paused tasks. On the other side it resumes each one with "you were paused for a move; carry on".

  "Laptop mode" does the same in reverse. If the server stops during a move, the sessions not yet moved stay on the Mac, and the switch says which.
- **Going to sleep without pressing the switch** (best effort). GPUI's sleep notice is queued on the main queue and does not hold sleep (`vendor/gpui_macos/src/platform.rs`; the docs of `on_system_sleep` in `vendor/gpui/src/app.rs` say to "record state or cancel work rather than start it"). For a held window, `sys` registers with I/O Kit (`IORegisterForSystemPower`; a system framework, no new crate). It delays `IOAllowPowerChange` until the hand-over ends, or 20 s pass (proof P17). In that window the Mac:
  1. starts no new outward effect, and lets running ones finish for at most 10 s;
  2. ends its brain turn or marks it `Slept`;
  3. sends its last changes;
  4. hands home to the server, which takes it at generation + 1.

  Without a held window, or without network in it, the server takes home after the lease plus 15 s, as for an unplanned loss.
- **Unplanned loss** (lid closed with no network, a crash): the server waits for the lease plus 15 s, then takes home at generation + 1.
- **Fencing.** While a server is paired, every outward or at-most-once effect is claimed with the server before it runs. A claim of an older generation that has no result is `Uncertain`, and is checked before any retry, so nothing runs twice.
- **Waking up.** The Mac wakes as a client. After 60 s of presence it asks for home back. The server finishes its brain turn (at most 2 minutes), sends its changes, and hands over.
- **Server unreachable while the Mac is awake.** The Mac runs Degraded: reads, rules, notifications, answers from code and queueing, nothing outward. The status bar says so. A setting, off by default, lets the Mac act alone after N minutes, and states the risk.
- **The brain.** Each home runs its own brain process with its own signed-in accounts. On the server, accounts are signed in through the vendor CLIs; no credential is copied. After a switch, the chat lane starts from the rollover summary and the recent conversation items (D8).
- **Approvals while the Mac is closed** wait as cards until the Mac answers. Phone and glasses access is a separate, later plan.

**Words tiers (D1).**

| Tier | Origins | May back |
|---|---|---|
| 1 | Today box, Manager chat, Chat view composer (typed spans only) | Confirmed facts, chat grants, the shortcut "remember this" |
| 2 | Keys typed in a session's terminal view, paired phone or glasses, voice | "Mentioned once" entries and "Keep this?" proposals only |
| never | Text Glasscode typed (`session.send`, playbooks, first prompts made from a ticket or PR, handoffs), pasted spans, schedules, other sessions, outside programs | Nothing. It is stored and shown as theirs. |

**Memory caps (M20.1).** A file holds at most this many characters of entry text that is not let go: the text a brief can carry. Headers, headings, tombstones and prose do not count. A write that takes a file over its cap, or further over it, fails with "Merge or let go first" and lists the file's entries with their ids and sizes; a file over its cap can always be made smaller. A monorepo is one project key, so its one `PROJECT.md` has the same cap.

| File | Cap |
|---|---|
| `you/profile.md` | 1,500 |
| `org/<employer>/people.md`, `glossary.md` | 2,000 each |
| `projects/<key>/PROJECT.md` | 3,000 |
| `projects/<key>/POLICY.md` | 1,500 |
| `projects/<key>/decisions.md` | 4,000 |
| `projects/<key>/knowhow/<topic>.md` | 4,000 each |
| `projects/<key>/work/<work file>.md` | 3,000 |
| `inbox/proposals.md` | 20,000 (a session cannot fill the inbox without end) |
| `archive/`, `INDEX.md`, `inbox/rejected.md` | none: never put in a brief whole |

**Memory ages (M20.1).** Ages count active days: UTC days on which the journal has an event for the project. Wall-clock days in the local zone are never used.

| Status | Leaves after | Then |
|---|---|---|
| `mentioned` | 30 active days without a tick | `let-go` |
| `learned` | 90 active days without a tick, unless it cites code that still holds | `let-go` |
| `let-go` | 30 active days | moved to `archive/<yyyy-mm>/let-go.md` |
| `confirmed`, or an entry with no stamp | never | — |

**A chat grant (D3)** allows one op only when all of these hold:

- the quote is inside a tier-1 message of this conversation;
- the target ids are among the ids Rust resolved from that message, or from the `ui.state` snapshot taken when it was sent;
- the op is within the level's ceiling;
- the grant is used once, is bound to the hash of the arguments, and ends with the turn.

Outward ops always show a card with the full payload. A typed "yes" binds only when exactly one card is open. A spoken "yes" never binds in release 1.

The order of precedence is: hard limit, then never, then the level's ceiling, then an explicit ask, then the request, then the level's default.

## 5. Path layout and path classes

`glasscode_core::paths` gives each path a class. `session_profile()` and `headless_profile()` build their deny and allow rules from this table only (`paths::layout()`), never from a literal path. The test `every_folder_has_a_class` fails when `paths.rs` gains a function that returns a path with no class, and `every_name_joined_to_the_glasscode_folder_is_in_the_layout` fails when any crate joins a name to the Glasscode folder that has no row. (crit-1)

| Path (under `~/.glasscode/` unless absolute) | Class | Sessions: file tools | Sessions: shell (sandbox) | Written by |
|---|---|---|---|---|
| `authority/` (policy, playbooks, schedules, approved snapshots, approvals, devices, memory head, pending actions, engine switch, machine id, last clock stamp) | Authority | deny read and write | deny read and write | the app only |
| `state.json` | Authority | deny | deny | the app and glasscoded |
| `server.json`, `glasscoded.json`, `glasscoded.sock`, `cli-versions.json`, `clis.json`, `control.key` (glasscoded's session-signing key), `push.json`, `push-devices.json`, `relay.json`, `auto-review.json`, `ci-fix.json`, `tmux.conf`, `review-prompt.md`, `device/`, `machine-id`, `board.json`, `activity.json` | Authority | deny | deny | the app and glasscoded (`review-prompt.md` and `push.json`: the person) |
| `words/<machine>/` | Words | deny | deny | the app and glasscoded |
| `effects/`, `journal/`, `agent-log.jsonl`, `glasscoded.log`, `manager/` (all but `manager/work/`) | Ledger | deny | deny | the app and glasscoded |
| `memory/` (git) | Trusted memory | read: per proof P07 (owner); write: deny | deny | `glasscode memory …` only |
| `agent-notes/<key>/<agent>/` | Agent notes | read and write for that project's sessions | read and write for that project's sessions | sessions, and Claude's redirected auto-memory |
| `handoff/` | Handoff | person profile: read, as today; automation and quarantine profiles: deny (a note can hold another project's diff) | same | the app, one note per handoff |
| `worktrees/<project>/<branch>/` | Worktree | the session's own one read and write; others denied for automation and quarantine sessions | same | sessions |
| `accounts/<id>/`, `~/.claude`, `~/.claude.json`, `~/.codex` | Accounts | deny (the agent process reads its own folder directly, outside the tools) | deny | the vendor CLI |
| `events/`, `settings/` | Hook IO | deny | deny | hooks, which run outside the sandbox |
| `manager/work/` | Brain work folder | the brain only | none (the brain has no shell) | the brain |
| `bin/` (the `glasscode` command), `agent/` (Claude's plugin, `--plugin-dir`), `pasted/` (files pasted into a terminal), `control.sock` | Session tools | read, never write | read and run, never write; `control.sock`: connect only | the app |
| `cache/`, `github-cache.json`, `usage-cache.json`, `linear.json`, `browser/`, `mirrors/`, `usage/`, `updates/` | Cache | deny | deny | the app, or glasscoded (`mirrors/`) |
| `unsaved/` | Cache | deny | deny | the app (editor text it could not save when quitting; it has a data-map row, as it is not rebuilt) |
| `~/.ssh`, `~/.aws`, `~/.config/gh`, `~/.netrc` (and `~/.netrc.gpg`, `~/.authinfo`, `~/.authinfo.gpg`, `~/.git-credentials`), `~/.gnupg`, keychain files (`~/Library/Keychains`, `~/.local/share/keyrings`, Windows' `Microsoft/Credentials` and `Microsoft/Protect` folders), a file (never a folder) whose name starts with `.env` anywhere: `.env`, `.env.local`, `.env-prod`, `.env_local`, `.envrc` | Credentials | deny read | deny read | not Glasscode |

The table is `paths::layout()`, and `paths::class_of` reads a path by it (M10):
- **No class means denied.** A path inside the Glasscode folder with no row (`class_of` returns `Classified::InsideUnlisted`) is denied to sessions. M22 relies on this. A path outside it with no row is `Classified::Outside`.
- **The longest entry decides.** `manager/work/` is Brain work; `manager/work.json` and the rest of `manager/` are Ledger.
- **`control.sock`** is Session tools because sessions connect to it (the `glasscode` command). There is nothing to read or write in it. M22 allows only the connection (the sandbox's `allowUnixSockets`), not file access.
- **Siblings.** A file's siblings with a suffix (`.broken`, `.prev`, `.lock`, `.tmp`) have the file's class.
- **`.env`.** A file whose name starts with `.env` (`.env`, `.env.local`, `.env-prod`, `.env_local`, `.envrc`) is Credentials wherever sessions could otherwise read it, below its folder. A folder never is (`.envs/`, `.envoy/`, `.env.d/`); a name that only ends like it (`prod.env`) is not either. `class_of` reads the disk to tell the folder from the file; a name that is not there yet counts as a file.
- **Links.** The path is read as written: letter case ignored, `.` and `..` taken back, and Windows' `\\?\` prefixes folded. It matches the Glasscode and home folders both as they are named and with their links resolved, so a canonical path (`/private/var/…` on macOS) has its class too. A folder that does not exist yet is resolved by its nearest ancestor that exists, with the rest after it. When not even that resolves, everything under the Glasscode folder as written is `InsideUnlisted`, which is denied. Links inside the path itself are not resolved. `class_of` reads the disk, so `decide()` (M21: no IO) must not call it.

## 6. Data map

Every store implements the `Store` trait of section 8. "On damage" names what happens when the file is corrupt: *refuse* means Glasscode keeps a `.broken` copy, stops the feature that needs the store and says so; it never starts empty. (crit-6, crit-7)

- **Refuse.** The callers of a refusing store either fail closed (policy: every project is Careful; pause: paused; playbook approvals: all off) or suspend and reconcile (effect ledger, home generation, tombstones, memory commit: nothing acts until the file is repaired).
- **Two generations.** Every save keeps the generation before it as `<file>.prev`, but only a file that loads; any other is kept as `.broken` first, and a save that cannot keep that copy fails. A store made once (the machine id) is made with its first generation. A damaged file falls back to `.prev` before "On damage" applies, and Glasscode says which one it used (`store::open` and `Found`).
- **Stores that fail closed take no generation.** The engine switch, and any store whose older value may allow more (policy), says `FALLS_BACK = false`: no way of reading it (`load`, `open`) ever gives `.prev`; a damaged file is a refusal.
- **A refusing store's file gone beside its `.prev`** is a refusal, not a first run: only "no file and no `.prev`" starts from the default.
- **The version rule.** The version is read first, as any number, before the data. A build never reads or writes a store whose schema is newer than it knows: it opens the file as an error and leaves it alone, whatever else the file holds.
- **One save at a time.** A save holds `<file>.lock` from its read to its rename, and removes the temporary files of saves that died more than ten minutes ago.
- **The table in code.** This table is also `store::DATA_MAP`. It has a row for every entry of the layout (section 5) that is not a cache: the stores, the stores that slices to come make (marked planned, with their slice), and today's files that are not stores yet. M25 settled today's files: an export does not hold them (each is this machine's own setup), they are this machine's, and they go with everything at uninstall; a cell nobody checked in its module says "as its module does (not checked yet)". A test fails when a `Store` has no row or no line in `store::ALL_STORES`, when an entry of the layout has no row, and when a planned store exists.

| Store | Module | Format | Retention | On damage | Export | Local only | Forget | Sync |
|---|---|---|---|---|---|---|---|---|
| `state.json` | `store` | JSON | lasting | today's rule (backup and fresh start); the manager part refuses | yes | — | — | through `mirror` |
| `authority/machine.json` | `store` | JSON, schema v1 | lasting | refuse; what needs the machine id (sync, the words ledger) stops | no | yes: each machine has its own | — | never |
| `authority/owners.json` | `owners` (server-sync plan; glasscoded is its only writer) | JSON, schema v1 | lasting: one record per worktree that has moved | refuse, with no older generation: moves stop and no new agent starts until it is repaired | no | yes: the server's | — | the Mac reads it through `owners.get` and `mirror.sync` |
| `authority/engine.json` | `store` | JSON, schema v1 | lasting, until M56 removes v1 | refuse; v1 runs with every v2 feature off | no | yes | — | no |
| `authority/projects.json` | `identity` | JSON, schema v1 (key → remotes, roots, and per machine its folders, pinned folders and project ids) | lasting: a removed project keeps its key, and adding it again finds it | refuse, with no fallback to `.prev`; no project key is made or looked up, so memory and agent notes stop until it is repaired | yes | per project: a "stays on this Mac" project's key is never sent | project | Mac to server (M57), without "stays on this Mac" projects; the server reads its own machine's folders and ids |
| `authority/clock.json` | `identity` | JSON, schema v1 (this machine's last clock stamp) | lasting | empty: stamps start again from the clock, so their order holds unless the clock also went back | no | yes: each machine has its own | — | never |
| `authority/policy.json` | `gate` | JSON, schema v1 (the live policy the app last wrote) | lasting | refuse, with no fallback to `.prev`; the approved snapshot applies at most as Careful | yes | — | project | never: the server gets the approved snapshot |
| `authority/approved/policy.json` | `gate` | JSON, schema v1 (the approved snapshot and its hash: the only policy the gate reads) | lasting | refuse, with no fallback to `.prev`; every project counts as Careful | yes | — | project | Mac to server inside `mirror.sync`, without "stays on this Mac" projects; never back |
| `authority/devices.json` | `gate` | JSON, schema v1 (the phones this Mac paired) | until the phone is removed | refuse, with no fallback to `.prev`; no phone is trusted | no | yes: each Mac pins its own | — | the ids go to the server with the policy; never back |
| `authority/pause.json` | `gate` | JSON, schema v1 | until released | refuse, with no fallback to `.prev`; the manager counts as paused | no | — | — | both ways; the stricter wins (crit-5) |
| `authority/forge.json` | `gate` | JSON, schema v1 (per project key: review required, no force-push, no bypass, when read) | until read again | refuse, with no fallback to `.prev`; no project counts as guarded by its forge | no | — | project | Mac to server with the policy (M57) |
| `authority/approved/review-prompt.json` | `review` | JSON, schema v1 (the SHA-256 of the approved `review-prompt.md`) | lasting | refuse, with no fallback to `.prev`; auto-review uses its built-in prompt | yes | — | — | never |
| `authority/origins.json` | `gate` | JSON, schema v1 (sessions a non-person caller started or drives) | while the session exists | refuse, with no fallback to `.prev`; every session counts as one the manager drives | no | — | — | never |
| `authority/from-mac.json` (server) | `gate` | JSON, schema v1 (the Mac's approved policy, pause and phones) | until the next sync | refuse, with no fallback to `.prev`; Careful, and the manager there paused | no | — | project | Mac to server only |
| `authority/playbooks.json` | `playbooks` | JSON, schema v1 (playbooks: the person's, the manager's proposals, the built-ins' settings and pauses) | lasting | refuse, with no fallback to `.prev`; nothing runs but the built-ins, which are app behaviour | yes | — | — | Mac to server; not approved on arrival |
| `authority/schedules.json` | `schedule` | JSON, schema v1 (schedules: spec in local time, action, misfire, expiry in UTC, where it runs) | lasting | refuse, with no fallback to `.prev`; nothing runs | yes | — | — | Mac to server; approved per machine (`schedule:<id>` in the approved playbooks) |
| `authority/schedules-state.json` | `schedule` | JSON, schema v1 (each schedule's last evaluation, last run started and run going; the latest wall time seen) | lasting | refuse, with no fallback to `.prev`; no schedule runs until it is repaired | no | yes: each machine's own runs | — | never |
| `authority/approved/playbooks.json` | `playbooks` | JSON, schema v1 (this machine's approved hash, by playbook id) | lasting | refuse, with no fallback to `.prev`; no playbook but the built-ins runs | yes | yes: each machine approves for itself | — | never |
| `authority/pending.json` | `gate` | JSON, schema v1 (each action: stable id, op and arguments with their hash, targets, who asks and why, expiry, state) | until done or expired, plus 7 days | refuse, with no fallback to `.prev`; cards show "could not load" and nothing waiting runs | no | — | — | Mac to server and back (M57); carries no authority |
| `authority/memory-head.json` | `memstore` | JSON, schema v1 (Glasscode's last memory commit, and the commit that last changed each scope: `you`, `org/<employer>`, `projects/<key>`) | lasting | refuse, with no fallback to `.prev`; briefs empty and the person is told | no | — | — | Mac to server |
| `words/<machine>/<yyyy-mm>.jsonl` | `words` | JSON lines (text only in a v2 home; counts only, no text or hash, in measure mode) | while cited, else 90 days (pruned once facts cite records, M20.1) | skip bad lines | with the facts that cite them | per project | person, project | both ways (M57); the Mac never accepts tier 1 from the server |
| `journal/<yyyy-mm-dd>.jsonl` | `journal` | JSON lines, `v` per line (UTC days) | 30 days | skip bad lines, log once | no | — | person (redact), project | the Mac pulls the server's by cursor (`journal.since`, M14); both ways in M57 |
| `manager/tokens.json` | `runtime::budget` | JSON, schema v1 (tokens per UTC day and lane, the last hour's wakes, each vendor window, the caps) | 30 days | empty: the count starts again (the vendors' own limits still hold) | no | yes | — | never |
| `journal/cursors.json` | `journal` | JSON, schema v1 (each hook spool's offset, each server's cursor) | lasting | empty: each spool is read from its end again, and the reconciler fills the gap | no | yes | — | never |
| `effects/<yyyy-mm>.jsonl` | `effects` | JSON lines: a claim, then a finish, by key and attempt; append only under `effects/.lock`, synced before it returns (no rename, so the Windows rename caveat of section 7 does not reach a claim) | 1 year (setting): whole month files are deleted | refuse new at-most-once effects until repaired; a line a crash cut off is cut away before the next is written | yes | — | redact payloads | merged by key and generation; outward claims registered with the server (M58) |
| `memory/` | `memstore` | git, Markdown with entry headers | lasting; archive kept | `git fsck`; refuse writes and keep reads from the last good commit | yes | per project | person, project, employer | per entry, by HLC stamp and tombstone (M57); "stays on this Mac" projects never sent |
| `agent-notes/` | `memory` | Markdown (vendor's own) | lasting | not used for briefs | yes | per project | project | per project policy |
| `manager/conversation/` | `conversation` | JSON lines per topic (`<topic>.jsonl`): each line a whole version of an item or a topic, with its HLC stamp | 90 days of text, summaries and rollover links lasting | skip bad lines (and lines of a newer schema), which retention leaves in place | yes | — | person | changes both ways (M57): the newest stamp of each item and topic wins |
| `manager/work.json` | `work` | JSON | until done plus 30 days | refuse; rebuilt by the reconciler | no | — | — | both ways (M57) |
| `manager/from-mac.json` (server) | `glasscoded::brain` | JSON (`ManagerFromMac`: on or off, brain, model, account, the projects as a wake sees them) | until the Mac says otherwise | not read: the server's manager waits for the next sync | no | — (names only projects that may leave the Mac) | — | one way, from the primary Mac (M57) |
| `manager/window-queue.json` (server) | `glasscoded::brain` | JSON (the window ops its manager asked for while the Mac was away) | until the Mac has each, at most 6 hours and 50 ops | read as empty | no | — | — | to the Mac until acknowledged; run once there, through its gate (M57) |
| `manager/data.json` (v1) | v1 `store` | JSON | read by the migration (M44) at each v2 start until v2's manager takes over, backed up once as `data.v1.json`, and left in place: v1's manager reads and writes it until then | migration stops (no marker), and runs again once it is repaired | — | — | — | — |
| `manager/outbox.json` | `manager::messages` | JSON, schema v1 (messages to teammates; v1's outbox moved here once, M44) | lasting | refuse, with no fallback to `.prev`: nothing is sent until it is repaired, and a sent message never reads as unsent | yes | — | person | not decided (M57) |
| `manager/children.json` | `children` | JSON, schema v1 (which child session is whose, and whether its summary went back, M61) | the newest 200 | empty: a child's summary is not sent back | no | yes | — | never |
| `manager/builtins.json` | `playbooks::native` | JSON, schema v1 (the built-ins' state: who was told in this stop, which reviews wait to be told) | lasting | empty: at worst one notice is said again | no | yes | — | never |
| `manager/migrated.json` | `manager::migrate_v2` | JSON, schema v1 (the migration's version, when, and what it moved) | lasting | empty: the migration runs again, and each step is idempotent | no | yes | — | never |
| `glasscode-memory.json` (voice copilot) | `voice/memory.mjs` | JSON | migrated in M20.3, kept until D25 | — | — | — | — | — |

**"Stays on this Mac"** is one project setting, and it has the same meaning for every store (ux-m2). For such a project:
- nothing that references it leaves the Mac: memory, journal events, turn reports, conversation items, the manager's sync to the server, phone and glasses snapshots, and voice;
- while the server is home (M57, M58), the manager can do nothing for the project, and shows that on the project's trust page;
- an eval checks the rule: with one such project and the server as home, nothing that references it appears in what the Mac sends.

**Forget** (M25) means four things:
- **a person**: entries that name them, their words records, journal summaries that name them, and their payloads in effects;
- **a project**: its memory folder, agent notes, authority rows and journal entries;
- **an employer**: every project the person marks as that employer's;
- **everything**: uninstall.

## 7. Threat model

**Actors**:
- the person;
- a session's agent, which may be fooled by planted text;
- the manager's brain, which may also be fooled;
- teammates and strangers who write PRs, reviews, tickets and messages;
- a program running as the person.

**Assets**:
- code and branches;
- credentials;
- memory;
- authority data;
- messages sent in the person's name;
- the subscription budget.

| Limit the plan promises | Plane | Test |
|---|---|---|
| The brain runs no command and reads no file outside its work folder | Claude: enforced (CLI flags; the tool list is checked at start). Codex: enforced by the read-only sandbox with network off for writes and commands; `apply_patch` and `view_image` stay visible in code mode, and the sandbox refuses the writes (P02). Reads are **not** limited to the work folder: the read-only profile reads `:root`, so `view_image` can open an image anywhere the person can. The tool list is checked from `ALL_TOOLS` at the first start; that no tool reaches the network outside the sandbox is not verified (P02) | M50 `brain_tool_list_is_exactly_glasscode` (both CLIs; for Codex the expected list includes them) |
| Every op of a non-person caller passes `decide()` | enforced (Rust) | M21 `every_handler_goes_through_decide`; adversarial suite |
| Only the app's own window is `Ui` | enforced (in-process channel) | M13 `interactive_flag_from_a_socket_is_terminal` |
| Policy, playbooks and schedules change only through the app | enforced against sandboxed sessions on macOS and Linux (they cannot write `authority/`); guarded on Windows (file-tool deny only); a program running as the person can change them, and the gate then reads only the approved snapshot, at least as strict as Careful | M21 `changed_policy_counts_as_careful`, M40 `synced_playbook_is_not_approved` |
| A brief contains only text Glasscode committed | enforced (built from the recorded commit) | M20 `direct_edit_is_not_in_the_brief` |
| Sessions do not write memory files directly | guarded (file-tool deny rules) | M22 |
| Sessions do not read credentials | enforced by the sandbox for shell commands on macOS and Linux, for the credential *files* and the login keychain: inside both CLIs' sandboxes `security` finds no default keychain (P07); for Codex, `.env` files are denied by absolute path at launch, because a deny glob stops every new folder in the workspace (P07), so a `.env` made later in the session is not covered; guarded for file tools; **none on native Windows** | M22 |
| A Codex automation session writes only its own worktree and its own branch's git metadata | enforced by Codex's sandbox (decision O16 on P07: decided by the lead on 2026-11-19 under the owner's delegated authority, given 2026-11-20, and accepted in its narrowed form on 2026-11-20; tested with `codex sandbox -P`, not in a session): the permission profile adds `write` for `<repo>/.git/worktrees/<name>`, `<repo>/.git/objects`, `refs/heads/<own branch>` with its `.lock`, and `logs/refs/heads/<own branch>`. `git commit` works; other branches, new branches, `packed-refs`, `.git/config`, `.git/hooks`, the main checkout and other worktrees stay refused. Commits are local, and pushing stays a Glasscode op (D13) | M22 |
| A Claude session cannot move other branches | **none**: in Claude's sandbox `git update-ref` moved another worktree's branch and `main`, and `git branch` created one (P07); `.git/config`, `.git/hooks` and the main checkout stayed refused. So Glasscode treats every local branch tip as untrusted, and a brokered push (D13) pushes only the commits the session made on its own branch | M22, M23 |
| `gh` in sessions | person: `gh` runs outside the sandbox (Claude `sandbox.excludedCommands`, Codex `on-request`), guarded; automation and quarantine: no `gh`, forge reads and writes are Glasscode ops (D13). Inside either sandbox `gh` cannot start, because `~/.config/gh` is a Credentials path (P07) | M22, M23 |
| Quarantine sessions reach only the forge | enforced by the sandbox network allowlist (Claude) or network off (Codex), for commands; automation and quarantine sessions also pass `--strict-mcp-config` and `disableClaudeAiConnectors: true`, because an account signed in to a plan otherwise carries its claude.ai connectors (P14); WebFetch and WebSearch run outside the Bash sandbox and are denied by rule (guarded) | M22 |
| No merge, no force-push, nothing on the default branch | held outside by forge rules where present (D29); guarded elsewhere (PreToolUse hook) | M22, M23 |
| The manager never merges or approves | enforced: no such op exists, and outward ops show a card | M33 coverage test |
| Browser actions on signed-in sites | enforced, because the browser MCP server is Glasscode's own (`browser/mcp.rs`) | M23 |
| A message to a teammate with a promise, date or decision waits for the person | enforced by "every message waits" (the default `SendPolicy::AskAlways`); the routine tier is advised | M42 |
| An action runs at most once after a crash | enforced (effect ledger and probe) | M31 fault injection |
| Only one home acts at a time | enforced by the server's lease and generation, with outward claims registered there | M58 partition and sleep tests |

**Not protected**, said plainly:
- a program running as the person, such as malware, can do anything the person can;
- a session in the person profile, fooled by planted text, can act within the person's own permissions until a hook or the forge stops it;
- native Windows has no Claude sandbox;
- on native Windows, a store's save is atomic but is not proven to last a power cut at the moment it returns:
  - it renames with `MoveFileExW` (replacing the file, with `MOVEFILE_WRITE_THROUGH`), but Microsoft documents that flag's flush only for a move done as a copy;
  - Windows has no folder sync;
  - so a power cut right after a save can leave the previous version, never half a file. M31's claims depend on this;
- the private modes of the stores (files 0600, new folders 0700) exist on macOS and Linux only. On Windows a store's file takes its folder's access list, which is the person's profile;
- a compromised server can act as the manager within the approved policy snapshot, but it cannot change the policy;
- the model can be persuaded. The data notice on outside text is a label, not isolation.

## 8. Shared interfaces

Signatures are binding. A slice that needs a change records it in its pull request, and the next slice of the same module changes this section.

```rust
// glasscode_core::store — every new store (M10)
pub enum OnDamage { Empty, Refuse }     // Refuse: the callers fail closed or suspend and reconcile (section 6)
pub trait Store: Default + serde::Serialize + serde::de::DeserializeOwned {
    const NAME: &'static str;           // "authority/policy"
    const VERSION: u32;                 // schema version; older files are migrated
    const ON_DAMAGE: OnDamage;
    const FALLS_BACK: bool = true;      // false: a damaged file never falls back to <file>.prev (fail closed)
    fn path() -> std::path::PathBuf;
    fn load() -> Result<Self, StoreError>;               // missing file: Ok(Default) (Refuse: only with no .prev); damaged: <file>.prev, then ON_DAMAGE
    fn save(&self) -> Result<(), StoreError>;            // under <file>.lock: <file>.prev, temp file, fsync, rename, folder fsync; any failure is Err
    fn migrate(from: u32, value: serde_json::Value) -> Result<Self, StoreError>;
}
// On disk: {"store": NAME, "version": VERSION, "data": …}; a file without the envelope is damaged.
// Renames write through on Windows (MoveFileExW; create_at without replacing). <file>.prev is the generation before
// the last save (only a file that loads becomes it; a store made once is made
// with it); <file>.broken a damaged copy, kept before a save replaces it; <file>.lock keeps two saves apart.
// The version is read first, as a u64: a newer schema is StoreError::Newer from load and from save, never read, never written over.
// A store that fails closed (the engine switch, policy) says FALLS_BACK = false: load() and open() never give its .prev.
pub enum StoreError { Io { store, path, source }, Damaged { store, why, kept: Option<PathBuf> }, Newer { store, found: u64, known: u32 }, Migrate { store, from, why }, Encode { store, why } }
pub enum Found { File, Missing, Previous { broken: Option<PathBuf> }, Empty { broken: PathBuf } }
pub struct Opened<S> { pub value: S, pub found: Found }
pub fn open<S: Store>() -> Result<Opened<S>, StoreError>;                       // Store::load, and which generation it used
pub fn update<S: Store, R>(change: impl FnOnce(&mut S) -> R) -> Result<R, StoreError>;  // load, change, save under one <file>.lock (M13.1);
                                                                               // a load error: change is not called, nothing written
pub struct DataRow { pub path: &'static str, pub store: Option<&'static str>, pub planned: Option<&'static str> /* "M21.1" */, pub module: &'static str, pub writer: &'static str, pub format: &'static str, pub retention: &'static str, pub on_damage: &'static str, pub export: &'static str, pub local_only: &'static str, pub forget: &'static str, pub sync: &'static str }
pub const DATA_MAP: &[DataRow];                                                // section 6 as a core table: every store, planned store and non-cache entry of the layout
pub struct StoreInfo { pub name: &'static str, pub version: u32, pub on_damage: OnDamage, pub falls_back: bool, pub path: fn() -> PathBuf, pub check: fn() -> Result<Found, StoreError> }
impl StoreInfo { pub const fn of<S: Store>() -> StoreInfo; }
pub const ALL_STORES: &[StoreInfo];
// A new store adds one line to ALL_STORES and one row to DATA_MAP (or takes "planned" off its row); a test fails otherwise.
pub enum Engine { V1, V2 }
pub struct EngineSwitch { pub engine: Engine, pub switches: BTreeMap<String, bool> }  // authority/engine.json; refuse
impl EngineSwitch { pub fn current() -> EngineSwitch; pub fn is_on(&self, feature: &str) -> bool; }  // anything but the file or no file: v1, every feature off
pub const ENGINE_VARIABLE: &str;        // GLASSCODE_MANAGER_ENGINE=v2, in a scratch home only (not ~/.glasscode by a link or other letter case)
impl MachineId { pub fn this() -> Result<MachineId, StoreError>; }  // authority/machine.json, made once with its .prev; refuse

// glasscode_core::clock (M10) — all time for v2 code
pub trait Clock: Send + Sync { fn now_ms(&self) -> u64; fn local_minutes(&self, at_ms: u64) -> Option<i64>; }  // wall-clock minutes since the local epoch
pub struct SystemClock;
pub fn now_ms() -> u64; pub fn local_minutes(at_ms: u64) -> Option<i64>;
pub fn minute_of_day(local_minutes: i64) -> u16; pub fn weekday(local_minutes: i64) -> u32;  // 0 = Sunday
pub fn use_test_clock(clock: Arc<dyn Clock>) -> TestClockGuard;                // process-wide until dropped (in any order: by token); not cfg(test), for the test kit

// glasscode_core::paths (M10)
pub enum PathClass { Authority, Words, Ledger, TrustedMemory, AgentNotes, Handoff, Worktree, Accounts, HookIo, BrainWork, SessionTools, Cache, Credentials }
// Handoff: the person profile reads it, automation and quarantine profiles are denied. SessionTools: read (and run), never written.
pub enum Classified { Class(PathClass), InsideUnlisted /* denied to sessions */, Outside }
impl Classified { pub fn class(self) -> Option<PathClass>; }
pub fn class_of(path: &Path) -> Classified;                                    // section 5; the two folders as named and resolved
                                                                               // (by an ancestor); unresolvable: InsideUnlisted. Reads the disk: not in decide()
pub enum Base { Glasscode, Home }
pub struct LayoutRow { pub base: Base, pub entry: &'static str, pub class: PathClass }
impl LayoutRow { pub fn path(&self) -> PathBuf; }
pub fn layout() -> &'static [LayoutRow];                                       // for M22: from classes to paths
pub fn row_of(path: &Path) -> Option<&'static LayoutRow>;                      // the longest row that matches
pub fn same_folder(a: &Path, b: &Path) -> bool;                                // canonical, case-insensitive on macOS and Windows
pub enum FolderForm { Folded(Vec<String>), Exact(PathBuf) } pub fn folder_form(path: &Path) -> FolderForm;  // what same_folder compares (M11)
pub fn authority_dir() -> PathBuf; pub fn memory_dir() -> PathBuf; pub fn agent_notes_dir(key: &ProjectKey, agent: AgentKind) -> PathBuf;
pub fn words_dir(machine: &MachineId) -> PathBuf; pub fn journal_dir() -> PathBuf; pub fn effects_dir() -> PathBuf;
pub fn manager_work_dir() -> PathBuf; pub fn cache_dir() -> PathBuf;
// ProjectKey and MachineId live here, as path-safe newtypes: one folder name, 1 to 64 of a-z 0-9 . _ -,
// starting with a letter or a digit, not ending in a dot, not a Windows device name (con, prn, aux, nul,
// com0-9, lpt0-9, with or without an extension), checked on parse and on deserialize. identity re-exports them
// (`pub use crate::paths::{MachineId, ProjectKey};`) and adds its own impl blocks.
pub struct ProjectKey(String); impl ProjectKey { pub fn parse(text: &str) -> Option<ProjectKey>; pub fn as_str(&self) -> &str; }
pub struct MachineId(String);  impl MachineId  { pub fn parse(text: &str) -> Option<MachineId>;  pub fn as_str(&self) -> &str; }

// glasscode_core::identity (M11)
pub use crate::paths::{MachineId, ProjectKey};  // defined in paths (M10)
// ProjectKey: "glasscode-3f9a1c": slug of the repository's (or folder's) name + 6 hex of the sha256 of the
// first alias (the folder's own remote, else its upstream, else its folder); more digits when that key is taken.
// The same remote gives the same key on every machine.
pub struct Aliases { pub remotes: Vec<String>, pub roots: Vec<String>, pub paths: Vec<PathBuf>, pub pinned: Vec<PathBuf>, pub project_ids: Vec<String> }
// One machine's view (Projects::known): remotes and roots of every machine; paths, pinned and project_ids of this one.
// pinned: folders the person tied to the key (the answer to AskFork), the explicit mapping.
pub enum KeyMatch { Known(ProjectKey), AskFork { candidate: ProjectKey, fresh: ProjectKey, by: Shared }, New(ProjectKey) }
pub enum Shared { Upstream, RootCommit, Folder }                              // what an AskFork folder shares with the candidate
pub struct Facts { pub path: PathBuf, pub git: bool, pub remotes: Vec<String>, pub upstream: Option<String>, pub others: Vec<String>, pub roots: Vec<String>, pub shallow: bool }
impl Facts { pub fn project_remotes(&self) -> Vec<String>; }                 // its own remotes, or its upstream when it has no other
pub fn facts(dir: &Path) -> anyhow::Result<Facts>;                            // git: the main checkout, glasscode.repo, origin, upstream, roots;
                                                                              // git failing on a repository is Err, never a plain folder
pub fn matching(facts: &Facts, known: &[(ProjectKey, Aliases)]) -> KeyMatch;  // no git, but it resolves folders (paths::folder_form): not on the main thread;
                                                                              // pinned, own remotes, upstream, root, path; a folder now holding another
                                                                              // repository is not its old key; the first of `known` wins a tie
pub fn resolve(dir: &Path, known: &[(ProjectKey, Aliases)]) -> anyhow::Result<KeyMatch>;  // matching(&facts(dir)?, known)
pub struct Projects { pub keys: BTreeMap<ProjectKey, Entry> }                 // authority/projects.json; refuse, FALLS_BACK = false
pub struct Entry { pub remotes: Vec<String>, pub roots: Vec<String>, pub machines: BTreeMap<MachineId, OnMachine> }
pub struct OnMachine { pub paths: Vec<PathBuf>, pub pinned: Vec<PathBuf>, pub project_ids: Vec<String> }
impl Projects {
    pub fn known(&self, machine: &MachineId) -> Vec<(ProjectKey, Aliases)>;
    pub fn key_of(&self, machine: &MachineId, project_id: &str) -> Option<&ProjectKey>;   // the binding
    pub fn bound(&self, machine: &MachineId) -> BTreeMap<String, ProjectKey>;            // every binding of the machine (the registry's World, M12.2)
    pub fn key_for(&self, machine: &MachineId, project_id: &str, dir: &Path) -> anyhow::Result<KeyMatch>;  // the binding, else resolve
    pub fn attach(&mut self, key: &ProjectKey, facts: &Facts, machine: &MachineId, project_id: Option<&str>);  // project_remotes, roots, folder: only added;
                                                                                                               // never a remote another key has
    pub fn pin(&mut self, key: &ProjectKey, facts: &Facts, machine: &MachineId, project_id: Option<&str>);     // the AskFork answer
    pub fn update<R>(change: impl FnOnce(&mut Projects) -> R) -> Result<R, StoreError>;  // every write: store::update (M13.1)
}
pub enum WorkKind { Branch, Ticket, Pr, Review }
pub struct WorkKey { pub project: ProjectKey, pub kind: WorkKind, pub id: String }  // "feat/login", "ENG-42", "acme/app#42"
impl WorkKey {
    // Review (session.review), then the PR it was started from, then its ticket (State::ticket_of), else its branch.
    pub fn for_session(project: &ProjectKey, session: &Session, ticket: Option<&str>, pr: Option<(&str, u64)>) -> WorkKey;
    pub fn file_name(&self) -> String;                                        // "feat-login-3f9a1c": slug + 6 hex of "<kind>:<id>"
}
// MachineId: random (16 hex digits), kept in authority/machine.json (MachineId::this, in store)
pub struct Hlc { pub wall_ms: u64, pub count: u32, pub machine: MachineId }  // Ord: wall_ms, count, machine
pub const MAX_AHEAD_MS: u64;                                                  // one hour: a stamp further ahead is quarantined
impl Hlc {
    pub fn zero(machine: MachineId) -> Hlc;
    pub fn now(last: &Hlc) -> Hlc; pub fn merge(&self, seen: &Hlc) -> Hlc;   // later than every input; merge ignores a stamp too far ahead
    pub fn too_far_ahead(&self) -> bool;                                      // also of this machine's own last stamp after its clock went back
}                                                                             // more than MAX_AHEAD_MS: it lasts until the clock passes that stamp
// Display and FromStr: "1763560000000.3@0123456789abcdef", for headers such as memory's at=<hlc>.
pub struct LastStamp { pub last: Option<Hlc> }                                // authority/clock.json; empty on damage (M13.1)
pub fn next_stamps(machine: &MachineId, count: usize) -> Result<Vec<Hlc>, StoreError>;  // later than every stamp this machine gave out, in any
                                                                              // process (store::update); saved before they are returned

// glasscode_core::memstore (M20.1) — the only writer of memory/
pub struct MemoryHead { pub commit: Option<String>, pub scopes: BTreeMap<String, String> }  // authority/memory-head.json; refuse, FALLS_BACK = false
pub enum Status { Mentioned, Learned, Confirmed, LetGo }  pub enum Kind { Fact, Knowhow }
pub enum Source { Words(String), Session(String), Import(String), V1, PersonEdit }
pub struct CodeRef { pub path: String, pub commit: String, pub first: u32, pub last: u32, pub sha256: String }  // code=<path>@<commit>#L<a>-<b>:<sha256>
impl CodeRef { pub fn of(path: &str, commit: &str, content: &str, first: u32, last: u32) -> Option<CodeRef>; pub fn holds(&self, content: Option<&str>) -> bool; }
pub struct Header { pub id: String, pub status: Status, pub kind: Kind, pub source: Source, pub code: Option<CodeRef>, pub at: Option<Hlc>, pub to: Option<String>, pub org: Option<Employer>, pub more: Vec<(String, String)> }
pub struct Entry { pub text: String, pub header: Header }   // "- <text> <!-- <header> -->"
pub struct Doc { pub lines: Vec<Line>, pub newline: bool }   // Line: Entry, Tombstone { id, at }, Text
pub fn merge(ours: &Doc, theirs: &Doc) -> Doc;               // by stamp; a tombstone beats any stamp (M57 syncs)
pub enum Admit { Confirmed, Mentioned, Proposal(String), Refused(String) }
pub fn admit_fact(record: &WordsRecord, quote: &str, fact: &str, project: Option<&ProjectKey>) -> Admit;  // pure
pub fn active_age(since_ms: u64, active_days: &BTreeSet<u64>) -> usize; pub fn aging(entry: &Entry, active_days: &BTreeSet<u64>, code_holds: bool) -> Aging;
pub enum MemFile { Profile, People(Employer), Glossary(Employer), Project(ProjectKey), Policy(ProjectKey), Decisions(ProjectKey), Knowhow(ProjectKey, String), Work(ProjectKey, String), Archive(ProjectKey, String, String), Proposals, Rejected, Index }
pub enum Scope { You, Org(Employer), Project(ProjectKey), Glasscode }  pub struct Employer(String);
pub enum Who { You, Manager, Session(String) }               // the commit's "Who:" trailer
pub struct Memory;  pub struct Snapshot;                     // Snapshot: files(), text(), doc(), find(id), work_file(&WorkKey), employer_of(&ProjectKey)
impl Memory {
    pub fn open() -> Memory; pub fn snapshot(&self) -> Result<Snapshot, MemoryError>;   // Glasscode's last commit only (D20)
    pub fn remember(&self, record: &WordsRecord, quote: &str, fact: &str, file: &MemFile, section: Option<&str>) -> Result<Remembered, MemoryError>;
    pub fn learn(&self, who: &Who, file: &MemFile, section: Option<&str>, text: &str, code: Option<CodeRef>) -> Result<Remembered, MemoryError>;
    pub fn note(&self, session: &str, work: &WorkKey, text: &str) -> Result<Remembered, MemoryError>;   // "Notes from sessions"
    pub fn propose(&self, who: &Who, file: &MemFile, section: Option<&str>, text: &str, source: Source) -> Result<(String, Written), MemoryError>;
    pub fn accept_proposal(&self, id: &str) -> Result<Written, MemoryError>; pub fn reject_proposal(&self, id: &str) -> Result<Written, MemoryError>;   // human only
    pub fn merge(&self, who: &Who, ids: &[&str], text: &str) -> Result<(String, Written), MemoryError>;
    pub fn let_go(&self, who: &Who, id: &str) -> Result<Written, MemoryError>; pub fn rewrite(&self, who: &Who, id: &str, text: &str) -> Result<Written, MemoryError>;
    pub fn forget(&self, ids: &[&str], secret: bool) -> Result<Forgot, MemoryError>;   // human only; secret: a fresh history
    pub fn pending(&self) -> Result<Pending, MemoryError>; pub fn accept(&self, paths: &[String]) -> Result<Written, MemoryError>; pub fn discard(&self, paths: &[String]) -> Result<(), MemoryError>;
    pub fn tidy(&self, project: &ProjectKey, active_days: &BTreeSet<u64>, holds: impl Fn(&CodeRef) -> bool) -> Result<Written, MemoryError>;
    pub fn set_employer(&self, who: &Who, key: &ProjectKey, employer: &Employer) -> Result<Written, MemoryError>;
}
pub fn readable(file: &MemFile, project: Option<&ProjectKey>, employer: Option<&Employer>) -> bool;  // mem-18; None: the manager
pub fn visible(entry: &Entry, employer: Option<&Employer>) -> bool;
pub fn active_days(project: &ProjectKey) -> BTreeSet<u64>;   // UTC days with a journal event for it
pub fn code_holds(repo: &Path, code: &CodeRef) -> bool;
pub fn bind_project(project_id: &str, dir: &Path) -> anyhow::Result<KeyMatch>;   // only a folder that exists
// memstore::search (M20.2)
pub struct Scope { pub project: Option<ProjectKey>, pub employer: Option<Employer>, pub work: Option<MemFile>, pub history: bool }  // project None: the manager
pub struct Hit { pub section: String /* <file>#<heading> */, pub file: MemFile, pub score: f32, pub entries: Vec<Entry>, pub prose: Vec<String> }
pub fn search(snap: &Snapshot, query: &str, scope: &Scope, limit: usize) -> Result<Vec<Hit>, MemoryError>;   // the index of a commit is kept
pub fn scope_for(snap: &Snapshot, project: Option<&ProjectKey>, work: Option<&WorkKey>) -> Result<Scope, MemoryError>;
pub fn as_data(hits: &[Hit], commit: Option<&str>) -> String;   // <memory-results>: data, not instructions
// memstore::brief (M20.2)
pub struct BriefFor<'a> { pub project: Option<&'a ProjectKey>, pub work: Option<&'a WorkKey>, pub folder: Option<&'a Path>, pub policy: Option<String>, pub projects: Vec<ProjectLine>, pub today: String }
pub const BUDGET: usize = 8_000;
pub fn build(snap: &Snapshot, what: &BriefFor, holds: &dyn Fn(&CodeRef) -> bool) -> Result<Brief, MemoryError>;   // Brief { text, commit }
// memstore::index (M20.2): INDEX.md, one line per file, regenerated with each commit
pub fn lines_of(index: &str, scope: &str) -> Vec<String>;
// memstore::scan
pub fn scan(text: &str, env_values: &[String]) -> Vec<Finding>; pub fn env_values(folders: &[PathBuf]) -> Vec<String>;  // Finding { rule: Rule, what }
// memstore::caps (the table in section 4)
pub fn cap_of(file: &MemFile) -> Option<usize>; pub fn check(file: &MemFile, before: &Doc, after: &Doc) -> Result<(), OverCap>;

// glasscode_core::github (M11)
pub fn normalize_remote(remote: &str) -> Option<String>;  // "github.com/acme/app" for every form and alias; None for a local path
pub fn repo_override(dir: &Path) -> Option<String>;       // the glasscode.repo setting, as repo_slug reads it

// glasscode_core::caller (M13.1)
pub enum Lane { Chat, Wake }
pub enum Caller {                     // serde: {"kind": "session", "id": …}
    Ui,
    Session { id: String, project: Option<ProjectKey> },
    ShellTab { session: String, project: Option<ProjectKey> },  // the person typing in a Glasscode shell tab: the terminal view saw the
                                                                // Enter on a `glasscode` line within SHELL_WINDOW_MS (attribution, not a wall).
                                                                // A background process in the shell tab could fire inside the window, so M21
                                                                // keeps the in-app sheet for person-present ops.
    Manager { lane: Lane, wake: Option<String>, project: Option<ProjectKey> },
    Phone { device: String },
    Machine { id: MachineId },        // another Glasscode machine's sync connection (the Mac, seen from glasscoded)
    Terminal { pid: Option<u32> },
}
impl Caller { pub fn is_person(&self) -> bool; }                             // Ui or ShellTab
pub enum Channel { Socket { pid: Option<u32> }, RequestFile, Server { session: Option<String> } }  // no channel gives Ui
pub struct Signed { pub shell: bool }
pub fn by_channel(request: &control::Request, channel: &Channel, signed: impl Fn(&str, &str) -> Option<Signed>,
                  under_brain: impl Fn(u32) -> bool, typed_in_tab: impl Fn(&str) -> bool) -> Caller;  // never reads request.interactive
pub fn rule(caller: &Caller, kind: control::Kind, settings: &AgentSettings) -> control::Rule;  // v1's rules by caller, until decide()
pub fn outside_rule(kind: control::Kind, settings: &AgentSettings) -> control::Rule;          // reads as Settings say, never stays, else Ask
pub const SHELL_WINDOW_MS: u64;       // 15 s
pub fn note_typed_command(session: &str); pub fn typed_command_recently(session: &str) -> bool;  // only the time, never the line
pub fn runs_glasscode(line: &str) -> bool;                                    // the line the cursor is on names the command
// glasscoded's caller per connection (M13.2, the server-sync plan's phase 0a)
pub const DAEMON_FEATURE: &str = "caller"; pub const DECLARE_METHOD: &str = "caller.declare";  // params: the Caller JSON
pub struct DaemonConnection;          // undeclared is Ui; the first declaration wins; only machine, phone and ui are declared
impl DaemonConnection { pub fn caller(&self) -> Caller; pub fn declare(&mut self, params: &Value) -> Result<Caller, String>; }
pub fn daemon_refusal(caller: &Caller, method: &str) -> Option<String>;      // phone: restart, shutdown, phones.add, caller.declare; owners.*: Machine only
pub fn device_id_for_key(blob: &str) -> String; pub fn device_id_ok(id: &str) -> bool;  // gate --device <id>; "unknown" for older lines

// glasscode_core::words
pub enum ComposerKind { TodayBox, ManagerChat, ChatView }
pub enum InjectKind { SessionSend, Playbook, FirstPrompt, Handoff, Review }
pub enum Origin { Composer(ComposerKind), TerminalKeys, Device(String), Voice, Pasted, Injected(InjectKind), Schedule, Peer(String), Outside }
pub enum Tier { One, Two, Never }
pub struct WordsId(String);
pub struct WordsRecord { pub id: WordsId, pub at: Hlc, pub session: Option<String>, pub project: Option<ProjectKey>, pub origin: Origin, pub text: String, pub hash: [u8; 32], pub spans: Vec<(usize, usize, Origin)>,
                         pub measured: bool }  // measured: measure-only mode, no text and a zero hash (M13.1)
impl WordsRecord { pub fn tier(&self) -> Tier; pub fn quote(&self, quote: &str) -> Option<(usize, usize)>; }
pub fn record(origin: Origin, text: &str, session: Option<&str>, project: Option<&ProjectKey>) -> WordsId;  // no IO: one writer thread per process
pub fn record_with(origin: Origin, text: &str, untyped: &[Range<usize>], session: Option<&str>, project: Option<&ProjectKey>) -> WordsId;  // untyped: Pasted spans
pub fn match_prompt(session: &str, text: &str, at_ms: u64) -> Option<WordsId>;   // hook event -> ledger record: one whose text is the whole prompt, in the
                                                                                 // 30 min before, matched once; the least trusted wins. None: nobody's words
pub fn tier_of(origin: &Origin) -> Tier;
pub fn quote_in(id: &WordsId, quote: &str) -> Option<(usize, usize)>;             // only typed spans count
pub fn command_origin(caller: &Caller) -> Origin;                                // text a command carries (manager ask): never a composer's
pub fn from_server(record: WordsRecord) -> Option<WordsRecord>;                  // the Mac refuses tier one, or a span of it, from the server
pub fn scrub(text: &str) -> String; pub const SECRET: &str;                      // "[secret]": token prefixes, JWTs, keys, Bearer, URL passwords, NAME=…
pub enum Mode { Off, Measure, Ledger } pub fn mode() -> Mode;                    // Ledger in a v2 home; Measure with GLASSCODE_WORDS_MEASURE=1 (any home)
pub fn recent(limit: usize) -> Vec<WordsRecord>; pub fn find(id: &WordsId) -> Option<WordsRecord>; pub fn flush();
pub fn append_lines(path: &Path, lines: &[String]) -> std::io::Result<()>;      // JSON lines: <file>.lock, a torn last line ended, synced per batch
pub struct Typed;      // a text box's untyped ranges: edit(replaced, inserted, typed), set(len, typed), untyped(), within(part)
pub fn trimmed<'a>(text: &'a str, typed: &Typed) -> (&'a str, Vec<Range<usize>>);
pub struct InputLine;  // a terminal view's line from the person's own keys and pastes: key(bytes) gives the line on Enter, paste, typed, clear

// glasscode_core::conversation (M43) — manager/conversation/<topic>.jsonl; not a Store (JSON lines, newest stamp wins)
pub struct ItemId(String);                                  // 24 hex, random; "v1-" + 21 hex of the content for an imported v1 line
pub struct TopicId(String);                                 // a safe file name (ProjectKey's rules); parse and deserialize check it
impl TopicId { pub fn parse(text: &str) -> Option<TopicId>; pub fn general() -> TopicId; }
pub enum Ref { Session(String), Pr { repo: String, number: u64 }, Project(ProjectKey) }
impl Ref { pub fn pr(repo: &str, number: u64) -> Ref; pub fn topic_id(&self) -> TopicId; }  // "pr-acme-app-42-1a2b3c": the same on every machine
pub enum Author { Person, Chat, Wake(String), App, V1 }   // set by the writing side, never from the text
pub enum Stream { Streaming, Stopped, Done }
pub enum Kind { You { words: WordsId, reply_to: Option<ItemId> }, Manager { state: Stream }, Action { effect: String /* 64 hex */, event: Option<String> },
                Nudge { matter: String }, Card { pending: String }, System, Error, Summary { through: Option<Hlc> },
                Rollover { brain: String, summary: Option<ItemId> }, V1 { who: manager::Who } }  // Summary and Rollover last past retention
pub struct Item { pub v: u16, pub id: ItemId, pub topic: TopicId, pub posted: Hlc, pub at: Hlc, pub author: Author, pub kind: Kind, pub text: String, pub refs: Vec<Ref>, pub closed: bool }
impl Item { pub fn words(&self) -> Option<&WordsId>; pub fn tier(&self) -> Tier; pub fn quote(&self, quote: &str) -> Option<(usize, usize)>; }  // no tier on disk: read from the words ledger, only while the text is the record's
pub struct Topic { pub v: u16, pub id: TopicId, pub at: Hlc, pub title: String, pub key: Option<Ref>, pub merged_into: Option<TopicId>, pub split_from: Option<TopicId> }
pub enum Record { Item(Item), Topic(Topic) }                // one line; an item goes to its topic's file, a topic to its own
pub struct Draft { pub author: Author, pub kind: Kind, pub text: String, pub refs: Vec<Ref>, pub topic: Option<TopicId> }
pub const TEXT_DAYS: u64; /* 90 */ pub const SINCE_LIMIT: usize; /* 20 */ pub const IDLE_ROLLOVER_MS: u64; /* 4 h */ pub const ROLLOVER_ITEMS: usize; /* 200 */
pub enum RollReason { NewDay, Idle, Size }
pub fn rollover_due(started_ms: u64, last_ms: u64, now_ms: u64, items: usize) -> Option<RollReason>;  // the brain's conversation is fresh per day, after idling, past a size
impl Conversation {                                         // pure, but the methods marked IO (never on the main thread; they refuse while the engine is v1)
    pub fn from_records(now_ms: u64, records: impl IntoIterator<Item = Record>) -> Conversation;  // any order gives the same
    pub fn take(&mut self, record: Record) -> bool;         // newest `at` of each id wins; an item posted more than 90 days ago is not taken
    pub fn topic_for(&self, refs: &[Ref]) -> TopicId;       // a PR's or session's topic, or the PR or session topic of the first item naming it; a project only its own; else general
    pub fn resolve(&self, topic: &TopicId) -> TopicId;      // through merges; a circle of merges holds for neither
    pub fn topics(&self) -> Vec<&Topic>; pub fn topic_items(&self, topic: &TopicId) -> Vec<&Item>; pub fn items(&self) -> Vec<&Item>;
    pub fn search(&self, query: &str, topic: Option<&TopicId>, limit: usize) -> Vec<&Item>;  // every word, from its start; summaries too
    pub fn since_last_spoke(&self) -> Vec<&Item>;           // what wakes and the app posted after the chat lane's last message, with ids
    pub fn open(&self) -> Vec<&Item>; pub fn reply_target(&self, on: Option<&ItemId>) -> Option<ItemId>;  // a bare reply binds only when one is open
    pub fn started_from(&self, brain: &str) -> Option<&Item>; pub fn last_rollover(&self) -> Option<&Item>; pub fn items_since_rollover(&self) -> usize;
    pub fn changes_after(&self, after: Option<&Hlc>) -> Vec<Record>;  // for M57
    pub fn plan_v1(&self, lines: &[ChatLine], machine: &MachineId) -> Vec<Record>;  // v1 lines: Author::V1, no words, ids from content
    pub fn load() -> Conversation;                                                  // IO: skip bad lines
    pub fn post(&mut self, draft: Draft) -> io::Result<Item>;                       // IO, and the rest below; text kept with secrets replaced (words::scrub)
    pub fn finish(&mut self, id: &ItemId, text: &str, state: Stream) -> io::Result<()>; pub fn close(&mut self, id: &ItemId) -> io::Result<()>;
    pub fn retitle(&mut self, topic: &TopicId, title: &str) -> io::Result<()>;
    pub fn split(&mut self, items: &[ItemId], title: &str) -> io::Result<TopicId>; pub fn merge(&mut self, from: &TopicId, into: &TopicId) -> io::Result<()>;
    pub fn roll_over(&mut self, brain: &str, summary: Option<&str>, through: Option<Hlc>) -> io::Result<ItemId>;  // D8 rollover link
    pub fn take_theirs(&mut self, records: Vec<Record>) -> io::Result<usize>;       // M57; stamps too far ahead are quarantined
    pub fn import_v1(&mut self, lines: &[ChatLine], machine: &MachineId) -> io::Result<usize>;  // M44
    pub fn prune(&mut self) -> io::Result<usize>;                                   // retention and older versions; unreadable lines stay
}

// glasscode_core::ops (M12.1, M12.2)
pub enum OpGroup { Read, Navigate, Board, Memory, SessionStart, SessionInput, SessionControl, Worktree, Project, Settings, Tickets, ForgeRead, ForgeWrite, Message, Browser, Manager }
pub enum EffectClass { Read, Navigate, LocalWrite, SessionInput, Create, Destructive, Outward }
pub enum Retry { Idempotent, Verifiable, AtMostOnce }
// Core: pure data, runs wherever the runtime is. Window: the Mac's UI only. SessionHost: the app for local
// sessions, glasscoded for tmux sessions. Network: whoever holds the login. Absent host -> OpReply::Failed
// with ErrorCode::Unavailable { host, queued }.
pub enum OpHost { Core, Window, SessionHost, Network }
pub enum Undo { None, Inverse(&'static str) }
// What an argument holds. In the op's JSON: Day "2026-11-20" or null (none); Until a day after today; Session
// the whole id; Pr a number; Switch true or left out; Count a number; List, Section, Movable a key; Many a list.
// Project, Task and Ticket (in capitals) stay words: their handler looks them up (M12.2). A Switch may read the
// options of other arguments: on when one is given, with a value or without, as v1's Args::has read them
// (board add's as_written; board list's all, which --section turns on). Number a number as given ("#7" is 7;
// another word says `missing`: wait <id>, and the pr changes that take a number only).
pub enum ArgKind { Word, Text, Switch, Count, Day, Until, Session, Project, Task, Pr, Ticket, List, Section, Movable, Every, Many, Number }
// Where it is on the command line (ops-17); word 0 is the first after the op's own words. FlagOrRest: the
// option, else the rest of the line (ticket move ENG-42 In Review). RestAfter: the rest of the line, after the
// value an option took when it is not a yes or a no (manager ask --voice what is blocked?).
pub enum At { Word(u8), Rest(u8), Flag(&'static [&'static str]), FlagOrWord(&'static [&'static str], u8), FlagOrRest(&'static [&'static str], u8), RestAfter(&'static [&'static str], u8) }
pub enum Options { Refuse, Ignore }     // unknown options: exit 2, or let through (status, whoami, approvals)
pub struct ArgSpec {
    pub name: &'static str, pub kind: ArgKind, pub required: bool, pub help: &'static str,
    pub at: At,
    pub missing: &'static str,         // what a missing required one says (exit 2); {verb} is the word typed
    pub default: Option<&'static str>, // words read like given ones: "this", "20", "tomorrow"
}
pub struct OpSpec {
    pub name: &'static str,            // "session.open"
    pub cli: &'static [&'static str],  // ["session", "open"]
    pub aliases: &'static [&'static str], // other words for cli's last word; "" it may be left out, "*" any word
    pub group: OpGroup, pub effect: EffectClass, pub retry: Retry, pub undo: Undo,
    pub human_only: bool,              // never offered to a non-person caller
    pub host: OpHost,                  // where it can run (ops-8)
    pub kind_v1: control::Kind,        // the frozen kind of state.json
    pub summary: &'static str, pub args: &'static [ArgSpec], pub examples: &'static [&'static str],  // HELP's words
    pub options: Options,
    pub v2_only: bool,                 // only the v2 engine knows it (ParseCtx.v2): the server-sync plan's switch ops
}
pub static OPS: &[OpSpec];             // the tables (ops/table_*.rs), one after the other
pub fn find(name: &str) -> Option<&'static OpSpec>;
pub struct Noun { pub path: &'static [&'static str], pub also: &'static [&'static str], pub unknown: &'static str }
pub static NOUNS: &[Noun];             // "session": "sessions", "s"; and what an unknown word after it says
pub struct Renamed { pub v1: &'static str, pub v2: &'static str, pub by: &'static str }
pub const RENAMED: &[Renamed];         // v1 words that stay until M56 and become another op (exec-8): manager remember -> memory.propose (M20.2), …
pub struct Op { pub spec: &'static OpSpec, pub args: serde_json::Value }   // JSON: {"op": "board.done", "args": {"task": "zysg"}}
impl Op { pub fn text(&self, name: &str) -> Option<&str>; pub fn on(&self, name: &str) -> bool; pub fn number(&self, name: &str) -> Option<u64>;
          pub fn texts(&self, name: &str) -> Vec<String>; pub fn day(&self, name: &str) -> Option<Option<day::Day>>; }  // None: left out
pub struct Targets { pub projects: Vec<ProjectKey>, pub sessions: Vec<String>, pub repos: Vec<String>, pub paths: Vec<PathBuf>, pub leaves_machine: bool }
pub struct SessionView { pub id: String, pub title: String, pub branch: String, pub review: Option<u64>, pub project: Option<String> }
pub struct ParseCtx { pub caller_is_person: bool, pub caller_session: Option<String>, pub today: day::Day, pub sessions: Vec<SessionView>, pub v2: bool }
pub type ParseError = OpError;
// Pure; no IO. A session by the start of its id or words from its title: the person only; any other caller
// gives its whole id or "this", or gets Ambiguous with the sessions the word could mean (ops-17). With the
// v1 engine the app counts every caller as the person, as v1 did (delta 3 of PR 84).
pub fn parse(words: &[String], ctx: &ParseCtx) -> Result<Op, ParseError>;
pub fn name_of(words: &[String], v2: bool) -> Option<&'static str>;              // the op's name from the words alone (glasscoded's RemoteCall.op)
pub fn find_session(word: &str, ctx: &ParseCtx) -> Result<String, OpError>;     // as parse does, for handlers that take a session among words
// What words name and what an op touches, resolved from what the app holds in memory (no git, no files).
pub struct ProjectView { pub id: String, pub name: String, pub root: PathBuf, pub hidden: bool, pub key: Option<ProjectKey>, pub repo: Option<String> }
pub struct WorktreeView { pub project: String, pub path: PathBuf, pub branch: String }
pub struct SessionPlace { pub id: String, pub project: String, pub worktree: PathBuf }
pub struct World { pub person: bool, pub caller_session: Option<String>, pub caller_project: Option<String>, pub cwd: Option<PathBuf>, pub home: PathBuf,
                   pub projects: Vec<ProjectView>, pub worktrees: Vec<WorktreeView>, pub sessions: Vec<SessionPlace> }
pub fn find_project(world: &World, word: &str) -> Result<String, OpError>;      // id, name, folder; its start for the person only
pub fn project_for(world: &World, given: Option<&str>) -> Result<String, OpError>;  // --project, the caller's session's, a server session's, the cwd's, the only one
pub fn find_worktree<'a>(world: &'a World, word: &str, project: Option<&str>) -> Result<&'a WorktreeView, OpError>;
pub fn find_task(board: &Board, outside: &[Outside], word: &str, person: bool) -> Result<String, OpError>;  // words of its title: the person only
pub fn targets(op: &Op, world: &World) -> Result<Targets, OpError>;             // from ids, never a caller's --project alone (trust-3)
// What each reply of an op leaves for the journal (ops-21; M14 writes it as an op.* event).
pub enum Decided { Allowed, Approved, Denied, Never }
pub struct Action { pub op: String, pub args: serde_json::Value, pub targets: Targets, pub effect: EffectClass, pub undo: Option<String>,
                    pub caller: String, pub session: Option<String>, pub decision: Decided, pub code: i32, pub at_ms: u64 }
impl Action { pub fn of(op: &Op, targets: Targets, caller: &str, session: Option<String>, decision: Decided, code: i32, at_ms: u64) -> Action; }
pub fn json_schema(spec: &OpSpec) -> serde_json::Value;                           // for MCP and help: the shape of Op.args
pub fn description(spec: &OpSpec) -> String;                                     // "session open: show it in the window": the schema's description,
                                                                                  // its command words (the op's key nouns) first (ops-23)
pub fn typed(name: &str, args: serde_json::Value) -> Result<Op, OpError>;     // a typed caller's op (a key, a tool of M34): args checked
                                                                                  // against json_schema before anything runs (M33.1)
pub fn help(topic: Option<&str>) -> String;                                      // `glasscode help`, made from OPS and a layout table (M33.1)
// The app (M33.1): Workspace::run_op(&Op, Caller, done) runs an op for the person in the app by the command's rules, and its
// reply leaves the journal's record; RunOp { name, args } is the one GPUI action that binds a key to an op;
// capabilities::INVENTORY (Capability { screen, control, handler, class: Op | Planned | PersonOnly { say, place } | ViewLocal }),
// Place (View | Settings(Section) | Tab(RightTab), "settings:automation"), card_key(CardId) -> Key | ActionOnly, cannot_do().
pub enum OpReply { Done { text: String, data: serde_json::Value }, Waiting { pending: String, summary: String }, Failed(OpError) }
pub struct OpError { pub code: ErrorCode, pub text: String }                     // JSON: {"code": "usage", "text": …}
pub enum ErrorCode { Failed, Usage, NotFound, Ambiguous { candidates: Vec<Candidate> }, Denied, Unavailable { host: OpHost, queued: bool },
                    StalePrompt, NeedsPerson }  // exit 1, 2, 1, 2, 5, 3, 1, 5 (the last two: M33.2)
// M33.2's ops, all v2_only: ui.state/open/back/forward/panel/point/say/tour/scene (table_ui), session.answer/interrupt/keys/mode
// (table_session_ext), pr.view (`pr details`)/pr.checks/pr.diff/review.threads (`pr threads`)/ci.log, git.status/log/diff,
// file.read, usage.get, settings.get/set (table_reads). Their handlers are the app's ops_v2.rs; who is the person is read from
// the channel (Caller::is_person), never from v1's trust.
// The app's Plan (M33.2, ops-9) gains `check: Option<Box<dyn FnOnce() -> Result<(), Reply> + Send>>`: what a handler asks git, gh or
// a file before anyone is asked, run off the main thread by Workspace::after_check before the rules, the gate's ask or the run.
// M33.2's last part: OpSpec::dry_runs() (an op that changes something, without a dry run of its own) and Op::dry_run();
// json_schema adds "dry_run" to them; parse takes --dry-run with the v2 engine only. ops::undo_of(&Op, &data) -> Option<Value>
// ({"op", "args"}), put in a v2 reply's data.undo. settings::may_set(&Setting, person) -> Result<(), String>.
// control::Request gains detach: bool (serde default): the app answers {"status": "started", "id"} and keeps the result for wait <id>.
// glasscode_core::mcp_stdio (M34): pub trait Tools { fn info(&self) -> Info; fn list(&self) -> Vec<Value>; async fn call(&mut self, &Value) -> Value }
// pub async fn handle(&mut impl Tools, &Value) -> Option<Value>; pub fn serve(&mut impl Tools); REVISIONS ["2025-11-25", "2025-06-18",
// "2025-03-26", "2024-11-05"], FALLBACK "2025-03-26"; -32601 for every other method.
// glasscode_core::mcp (M34): pub enum Profile { Manager, Session }; tools(Profile) -> Vec<Value> (name = op name with "_", ALWAYS_LOADED
// carry anthropic/alwaysLoad); Server (Tools: ops::typed, then control::call with Request.op); Brain { lane, wake, sig } from
// GLASSCODE_BRAIN_LANE/WAKE/SIG; brain_env, brain_caller, unchecked_brain, init_tools_ok, claude_config, codex_flags.
// control::Request gains op: Option<Value> ({"op", "args"}) and brain: Option<BrainToken { lane, wake, sig }> (serde default).
// glasscode_core::settings (M33.2, ops-3): the settings a caller may read or change by name.
pub enum Tier { Free, Ask, PersonOnly, Secret }  // the gate, outward automation and programs to run are PersonOnly; secrets never shown
pub enum Type { Switch, Choice(&'static [&'static str]), Number { min: u64, max: u64 }, Text, Words }
pub struct Setting { pub key: &'static str, pub kind: Type, pub tier: Tier, pub help: &'static str, pub section: &'static str, /* get, set */ }
impl Setting { pub fn get(&self, &State) -> Value; pub fn check(&self, &Value) -> Result<(), String>; pub fn set(&self, &mut State, &Value) -> Result<Value, String> /* the value before */ }
pub static SETTINGS: &[Setting]; pub fn find(key: &str) -> Option<&'static Setting>; pub fn all(&State) -> Value;
// glasscode_core::reads (M33.2): git_status/git_log/git_diff, file_allowed/file_read (managed roots only, never credentials or
// authority, 200 KB), pr_view/pr_checks/pr_diff/review_threads/ci_log (github_api, else gh); as_data() wraps what they return.
// glasscode_core::prompt (M33.2, ops-5): prompt_id(session, &PendingPrompt) (not the highlight), grants_for_good(&PromptOption),
// answer_on(screen, session, id, label) -> Result<(PendingPrompt, PromptOption), Refused { Stale, NoOption }>.
pub struct Candidate { pub id: String, pub title: String, pub project: Option<String> }  // six at most
// The app's replies keep control::Reply's shape: a failure's data is {"op", "error_code", "candidates"?}; a
// success's data gains "op" only with the v2 engine and only when it is an object (v1's --json is unchanged).
// remote::RemoteCall gains `op: Option<String>` (serde default): what glasscoded's name_of made of the words.

// glasscode_core::gate
pub enum Level { Careful, Standard, Autopilot }
pub enum Rule { Allow, Ask, Never }                                               // the same names as control::Rule
pub struct ProjectPolicy { pub level: Level, pub ask: Vec<String>, pub never: Vec<String>, pub allow_commands: Vec<String>, pub conditions: Vec<Condition>, pub model_account: Option<String>, pub local_only: bool }
pub enum Condition { Branch { not: Vec<String> }, Paths { deny: Vec<String> }, Hours { local: (u16, u16), days: u8 }, Person { only: Vec<String> } }
pub enum Decision { Allow, Ask { why: String }, Never { why: String } }
pub struct Grant { pub message: WordsId, pub quote: (usize, usize), pub op_hash: [u8; 32], pub targets: Targets, pub turn: String }
pub fn decide(caller: &Caller, op: &Op, targets: &Targets, policy: &PolicySet, grant: Option<&Grant>) -> Decision;
// M21.1: what decide() reads. The app builds it off the main thread, from the approved snapshot
// (made at most Careful when the live file differs) and its own records of its sessions.
pub struct Policy { pub default: ProjectPolicy, pub projects: BTreeMap<ProjectKey, ProjectPolicy> }  // ask/never name ops ("session.new") or groups ("session_start")
pub enum SessionOrigin { Person, Automation /* default: a session the gate knows nothing of */, Quarantine }
pub struct SessionFacts { pub project: Option<ProjectKey>, pub origin: SessionOrigin, pub started_by: Option<String>, pub branch: Option<String>, pub mode: Option<String>, pub account: Option<String> }
pub struct ForgeFacts { pub review_required: bool, pub no_force_push: bool, pub no_bypass: bool, pub read_ms: u64 }  // D29, filled by M23
pub struct Pause { pub all: bool, pub projects: BTreeSet<ProjectKey>, pub since_ms: Option<u64>, pub by: Option<String> }
pub struct PolicySet { pub policy: Policy, pub live_differs: bool, pub person_rules: AgentSettings, pub pause: Pause, pub devices: BTreeSet<String>,
                       pub sessions: BTreeMap<String, SessionFacts>, pub server_projects: Vec<ProjectKey>, pub manager_account: Option<String>,
                       pub forge: BTreeMap<ProjectKey, ForgeFacts>, pub project_modes: BTreeMap<ProjectKey, String>, pub home: PathBuf }
pub const TABLE: &[Row]; pub fn row_of(spec: &OpSpec) -> Option<&'static Row>;  // the default table (trust-11), data for Settings (M53)
pub fn op_hash(op: &Op) -> [u8; 32];                  // name and arguments, keys in order: what approvals and grants bind to
pub fn load() -> Authority; pub fn approve(policy: &Policy) -> Result<(), StoreError>; pub fn import_live(seen: &Policy) -> Result<(), StoreError>;
pub fn diff(from: &Policy, to: &Policy) -> Vec<String>;  // "app-3f9a1c: Trust level: Careful to Autopilot"
pub struct FromMac { pub from: Option<MachineId>, pub received_ms: u64, pub policy: Policy, pub hash: String, pub pause: Pause, pub devices: BTreeSet<String> }  // the server's copy
pub enum PendingState { Waiting, Approved, Denied, Expired, Ran, Uncertain }
pub struct PendingAction { pub id: String, pub op: Op, pub arg_hash: [u8; 32], pub caller: Caller, pub reason: String, pub created: Hlc, pub expires_ms: u64, pub state: PendingState }
// M21.2 as built: `op` is kept as its JSON (an op a later build drops is shown and refused, not a
// store that fails), with `number` (for `glasscode wait`, popups, phones), `targets` (re-checked on
// approve), `who`, `session`, `what`, `command`, `project`, `created_ms` (wall time; `Pending`
// keeps the latest it saw, `high_water_ms`, so a clock set back never extends an expiry), `outcome`.
pub struct Pending { pub last_number: u64, pub high_water_ms: u64, pub actions: Vec<PendingAction> }  // authority/pending.json; refuse, FALLS_BACK = false
impl PendingAction { pub fn recheck(&self, targets_now: Result<&Targets, &str>, decision_now: &Decision, now_ms: u64, high_water_ms: u64) -> Result<Op, String>; }
pub struct ChatGrants;  // grant(&Proposal, named: &Targets) -> Result<Grant, Refused>: tier one, a quote in typed spans and not only a yes,
                        // targets among those resolved from the message, once per message and op, within the turn
pub struct SessionGrants;  // "allow for the rest of the session": op + targets, monotonic, dropped on restart and on a wall/monotonic gap
pub fn yes_binds(origin: &Origin, text: &str, open_cards: usize) -> bool;  // typed, only a yes, exactly one card
// ProjectPolicy gains `allow: Vec<String>` ("Always allow this in <project>"): an ask of the level's default becomes allow, up to the ceiling.
// ProjectPolicy gains `intake: Option<intake::Intake>` (M60): on Autopilot the manager picks work by itself from Linear tickets with
// `linear_label` and GitHub issues with `github_label` by trusted authors (D27), at most `max_sessions` automation sessions and
// `daily_tokens` a day, none while `max_waiting` PRs wait for review or CI is red (`stop_on_red`). intake::plan(project, policy, found,
// load, taken, person, confirmed) -> Plan { picks, held, blocked } is pure; each pick is a work item with typed done conditions
// (PrReadyForReview, CiGreen) and one effect (origin "intake", once per project and item), started with ticket.start or session.new
// through the gate as the manager's wake "intake".
// ProjectPolicy gains `delegate: Vec<delegation::Delegation { name, question: Vec<String>, answers: Vec<String> }>` (M61): on Autopilot the
// manager's session.answer is allowed only when delegation::delegated(list, prompt, option) covers the question on the session's screen
// (never an option for good or one that wants text); else Ask (a card). A delegated answer goes to projects/<key>/decisions.md through
// Memory::record_decision (learned, source = the words record of the question, Origin::Peer(session)).
// pr.ready (M61, v2): for anyone but the person an independent headless run with no tools (auto-review's profile) reads the PR first
// (review::validator_input, verdict_schema -> Verdict { ready, summary, problems }); review::after_check(verdict, attempt) -> Ready | Fix(text)
// (back to the session) | Person(text) (turned down ATTEMPTS = 2 times: the person decides). github::mark_ready runs `gh pr ready`.
// session.child (M61, v2): a child for one part of a session's work, fresh context, the parent's worktree, agent, account and mode, as
// automation; children::Children (manager/children.json) keeps child -> parent, and the child's first turn end sends
// children::summary_message back to the parent through its delivery queue (effect origin "child", once per child turn).

// glasscode_core::journal (M14; the catalogue of kinds is journal_kinds::CATALOGUE)
pub enum Severity { Info, Notice, Warn, Urgent }
pub struct EventKind;                 // a dotted name, "session.started"; a kind a newer build wrote reads too
pub struct Event { pub v: u16, pub id: String, pub at: Hlc, pub source: Source, pub project: Option<ProjectKey>, pub session: Option<String>, pub kind: EventKind, pub severity: Severity, pub summary: String, pub data: serde_json::Value,
                   pub trace: Option<String>, pub age_ms: Option<u64> }       // trace: exec-31; age_ms: a poll's age
pub fn append(event: Event);                                                      // no IO: one writer thread per process stamps, redacts, writes
pub fn append_stamped(event: Event);                                              // another machine's event (the server's, pulled by the Mac)
pub fn init() -> bool; pub fn enabled() -> bool; pub fn known_enabled() -> Option<bool>;  // a v2 home only (rule 2)
pub fn flush(); pub fn health() -> Health;                                        // Health { last_write_ms, error, failing_since_ms, waiting }
pub struct Cursor { pub file: String, pub offset: u64 }
pub fn read_from(cursor: &Cursor, max: usize) -> (Vec<Event>, Cursor);           // a damaged line is skipped and logged once
pub enum Scope { All, Projects(Vec<ProjectKey>) }  impl Scope { pub fn of(caller: &Caller, managed: &[ProjectKey]) -> Scope; }
pub struct Filter { pub project: Option<ProjectKey>, pub session: Option<String>, pub kind: Option<String>, pub since_ms: Option<u64> }
pub fn query(filter: &Filter, scope: &Scope, limit: usize) -> Vec<Event>;          // the newest, oldest first
pub fn since(cursor: &Cursor, scope: &Scope, filter: &Filter, max: usize) -> (Vec<Event>, Cursor);  // glasscoded's journal.since
pub struct HookIntake; pub struct GitWatch; pub struct ClockWatch; pub fn forge_events(…) -> Vec<Event>; pub fn decision(step, outcome, reason, inputs, trace) -> Event;

// glasscode_core::reconcile (M14, D24's safety net)
pub struct Snapshot { pub sessions: Vec<SessionNow>, pub worktrees: Vec<(Option<ProjectKey>, PathBuf)> }
impl Reconciler { pub fn from_events<'a>(events: impl IntoIterator<Item = &'a Event>) -> Reconciler; pub fn observe(&mut self, event: &Event); pub fn run(&mut self, now: &Snapshot) -> Vec<Event>; }

// glasscode_core::effects (M31) — effects/<yyyy-mm>.jsonl; not a Store (JSON lines, append only)
pub struct EffectKey(pub [u8; 32]);                         // 64 hex in JSON
pub fn effect_key(origin: &str, subject: &str, op: &str, target: &str) -> EffectKey;  // length-prefixed parts
pub enum EffectState { Claimed, Done(serde_json::Value), Failed(String), Uncertain }
pub struct Intent { pub key: EffectKey, pub op: String, pub retry: ops::Retry, pub origin: String, pub subject: String, pub target: String,
                    pub caller: String, pub reason: String, pub project: Option<ProjectKey>, pub session: Option<String>,
                    pub launch: String /* this run of the app */, pub evidence: serde_json::Value /* what the probe needs */ }
pub struct EffectRecord { /* the intent's fields, and */ pub home: MachineId, pub generation: u64 /* 0 until M58 */, pub claimed_ms: u64, pub attempt: u32, pub state: EffectState, pub finished_ms: Option<u64> }
pub enum Claim { New(EffectKey), Already(EffectRecord) }
pub fn claim(intent: &Intent, home: &MachineId) -> std::io::Result<Claim>;  // M31 changed it from (key, op, home): the record needs the rest.
                                                                            // Synced before return; a failure of a verifiable or idempotent op
                                                                            // may be claimed again after its backoff (MAX_ATTEMPTS); at-most-once never;
                                                                            // at-most-once refused while a file is damaged
pub fn finish(key: &EffectKey, state: EffectState) -> std::io::Result<()>; pub fn finish_later(key: EffectKey, state: EffectState);
pub fn record_settled(intent: &Intent, home: &MachineId, state: EffectState) -> std::io::Result<bool>;  // M44's "seen" keys
pub fn unsettled(launch: &str) -> std::io::Result<Vec<EffectRecord>>;      // claims of earlier runs with no result
pub enum ProbeResult { Happened(serde_json::Value), DidNotHappen, Unknown }
pub fn settle(record: &EffectRecord, probe: Option<ProbeResult>) -> EffectState;  // no probe: idempotent fails (runs again), else uncertain
pub fn needs_person(record: &EffectRecord) -> bool;
pub fn recent(days: u64) -> std::io::Result<Vec<EffectRecord>>; pub const ACTIVITY_DAYS: u64;  // 30, the Activity view
pub fn merge_theirs(records: &[EffectRecord]) -> std::io::Result<usize>;   // by key and generation (M57)
pub fn prune() -> std::io::Result<usize>; pub const KEEP_MONTHS: u32;      // 12

// glasscode_core::delivery_session (M31) — the one writer of a session's terminal in a v2 home; pure
pub struct Delivery { pub id: String, pub session: String, pub text: String, pub note: Option<String>, pub kind: words::InjectKind, pub effect: Option<EffectKey>,
                      pub group: Option<String>, pub version: Option<String> /* a new version of a group drops the older */, pub queued_ms: u64, pub expires_ms: u64, pub urgent: bool, pub tries: u32 }
impl Delivery { pub fn new(session: &str, text: &str, kind: InjectKind, now_ms: u64) -> Delivery; pub fn marker(&self) -> String /* "[gc:1a2b3c4d]" */; pub fn typed(&self) -> String; }
pub struct Inputs { pub running: bool, pub busy: bool, pub asking: bool, pub draft: bool, pub typed_ms_ago: Option<u64> }
pub enum Wait { NotRunning, Working, Asking, Draft, Typing, InFlight }
pub fn ready(inputs: &Inputs) -> Result<(), Wait>;                         // idle, no prompt, no draft, no typing for QUIET_TYPING_MS (10 s)
pub fn may_press_enter(box_text: Option<&str>, asking: bool, typed: &str) -> bool;  // land-m3: checked again right before the Enter
pub enum Outcome { Delivered(serde_json::Value), NotSent(String), Uncertain(String), Dropped(String) }
pub enum Pushed { Queued, Same(String), Superseded(Vec<Delivery>) }      // the same group and version: one delivery (two playbooks, one event)
pub enum Step { Nothing, Wait(Wait), Send(Delivery) }
impl Queues { pub fn push(&mut self, d: Delivery) -> Pushed; pub fn expire(&mut self, now_ms: u64) -> Vec<Delivery>; pub fn next(&mut self, session: &str, inputs: &Inputs) -> Step;
              pub fn not_sent(&mut self, session: &str, id: &str) -> Option<Delivery>; pub fn finish(&mut self, session: &str, id: &str) -> Option<Delivery>; }
pub fn probe(marker: &str, events: &str) -> ProbeResult;                   // the UserPromptSubmit with the marker; a SessionStart after: did not happen
pub fn probe_claim(session: &str, evidence: &serde_json::Value) -> Option<ProbeResult>;  // reads the session's events file (M14's journal later)

// glasscode_core::trust (M53): what Settings shows and changes of trust, with no TOML or YAML
pub fn level_table() -> Vec<TableLine>; pub fn consequences(level: Level) -> Vec<String>;  // from gate::TABLE
pub fn moved(before: &Policy, after: &Policy, key: &ProjectKey, forge: &BTreeMap<ProjectKey, ForgeFacts>) -> Vec<Moved>;  // decide() before and after
pub fn parse_condition(text: &str) -> Result<Condition, String>;          // "not on main", "never secrets/**", "hours 09:00-18:00 weekdays", "only Sam"
pub fn playbook_sentence(p: &Playbook) -> String; pub fn schedule_sentence(s: &Schedule) -> String;
pub enum Change { Level{..}, AddCondition{..}, RemoveCondition{..}, Playbook{..}, ApprovePlaybook{..}, Schedule{..}, ApproveSchedule{..} }
pub fn prepare(change: Change, world: &World) -> Result<Pending, String>; pub fn apply(pending: &Pending, confirmed: bool) -> Result<String, String>;  // never unconfirmed

// glasscode_core::turns (M30)
pub enum TurnEnd { Completed, Paused, Failed(String), Interrupted, Aborted(Option<i32>), Crashed, Slept, AppQuit }
pub struct FileDelta { pub path: String, pub added: u32, pub removed: u32 }
pub struct TurnReport { pub v: u16, pub session: String, pub turn: String, pub started_ms: u64, pub ended_ms: u64, pub end: TurnEnd, pub tools: u32, pub failed_tools: Vec<String>, pub commands: Vec<String>, pub files: Vec<FileDelta>, pub commits: Vec<String>, pub pushed: bool, pub last_reply: String, pub question: Option<String>,
                        pub background: Vec<String>, pub by_agent: bool, pub partial: bool }  // what a paused turn left running; a hand-back nobody typed; what could not be read
pub enum Signal { Hook(Value), Rollout(Value), TranscriptLine(String), Exited(Option<i32>), Killed, AppQuit, Slept, Watchdog { alive: bool, quiet_ms: u64 } }
impl TurnMachine { pub fn feed(&mut self, now_ms: u64, signal: &Signal) -> Vec<Change>; pub fn restore(&mut self, turn: OpenTurn); }  // each opened turn closes once
pub fn gather(agent: AgentKind, transcript: Option<&Path>, closed: &Closed, worktree: &Path, wait: bool) -> Gathered;  // waits ≤ 3 s for the transcript; git since the turn's head
pub fn report(session: &str, closed: &Closed, gathered: &Gathered) -> TurnReport;  // Stop's reply first, the transcript after

// glasscode_core::work
pub enum DoneCondition { PrMerged, PrReadyForReview, CiGreen, ReviewRequested, BoardTaskDone(String), AnsweredWithoutQuestion, Person }
pub enum WorkState { Open, Waiting(String), Stuck(String), Done, Dropped }
pub struct WorkItem { pub key: WorkKey, pub sessions: Vec<String>, pub pr: Option<(String, u64)>, pub ticket: Option<String>, pub board_task: Option<String>, pub done: Vec<DoneCondition>, pub state: WorkState, pub updated: Hlc }

// glasscode_core::playbooks (M40) — typed tables in JSON (events-11: no expression language, no TOML)
pub enum Autonomy { Watch, Tell, Ask, Act }                // when two would do one thing: watch, then ask, tell, act
pub enum ScheduleSpec { Every { secs: u64 }, At { minutes: u16, days: u8, tz: String }, Once { at_ms: u64 } }
pub struct EventFilter { pub kind: String, pub fields: BTreeMap<String, Match> }  // JSON "where": field = value, or a list of allowed values
pub enum Match { One(Value), Any(Vec<Value>) }
pub enum Trigger { Event(EventFilter), Schedule { schedule: ScheduleSpec }, After { event: EventFilter, delay_secs: u64 } }
pub enum Cond { Level { is: Vec<gate::Level> }, AuthorTrusted, PrAuthorIsMe, Label { name: String }, NotDefaultBranch }  // typed; they read typed fields
pub enum Slot { Text(String) /* the person's */, Field(String) /* a typed field: never free outside text */ }
pub struct Action { pub op: String, pub args: BTreeMap<String, Vec<Slot>> }
pub enum Per { Commit, Pr, Session, Hour, Day } pub struct Limit { pub count: u32, pub per: Per }
pub struct Playbook { pub id: String, pub name: String, pub trigger: Trigger, pub when: Vec<Cond>, pub then: Action, pub autonomy: Autonomy, pub limit: Limit, pub builtin: bool, pub enabled: bool,
                      pub manager_events: bool /* reacts to events the manager caused */, pub proposed_by: Option<String>, pub paused: Option<Paused>,
                      pub approved_hash: Option<[u8; 32]> /* this machine's, filled in on load, never in the live file */ }
impl Playbook { pub fn hash(&self) -> [u8; 32] /* the definition and its switch, not its pause */; pub fn live(&self) -> bool; }
pub enum FieldType { Number, Bool, Id, Url, Branch, Word, Words, Text } pub struct Schema { pub kinds: BTreeMap<String, BTreeMap<String, FieldType>> }  // from the journal's catalogue (M14)
pub fn validate(playbook: &Playbook, schema: &Schema) -> Result<(), Vec<String>>;  // an invalid playbook is switched off with one alert; the rest load
pub struct PlaybookFile { pub playbooks: Vec<Playbook>, pub all_paused: Option<Paused> }  // authority/playbooks.json; refuse, FALLS_BACK = false
pub struct ApprovedPlaybooks { pub approved: BTreeMap<String, Approval> }                // authority/approved/playbooks.json; local only; refuse
pub fn load(schema: &Schema) -> Result<Book, StoreError>; pub fn approve(id: &str, person: bool) -> Result<(), BookError>;
pub fn propose(playbook: Playbook, by: Option<&str>) -> Result<(), BookError>;          // anyone but the person: switched off, only proposed
pub fn pause(id: Option<&str>, by: &str, why: &str) -> Result<(), BookError>; pub fn release(id: Option<&str>, person: bool) -> Result<(), BookError>;  // D15
// engine: pure
pub trait EventView { fn id(&self) -> &str; fn kind(&self) -> &str; fn field(&self, name: &str) -> Option<Value>; fn project(&self) -> Option<&ProjectKey>;
                      fn session(&self) -> Option<&str>; fn depth(&self) -> u8; fn causes(&self) -> Vec<EffectKey>; fn by_manager(&self) -> bool; }  // journal::Event implements it: data fields,
                                                           // data.depth and data.causes (events-13), source Manager
pub fn model::journal_schema() -> Schema;                  // the catalogue's typed fields: ci.*, pr.*, session.*, git.*
pub fn simulate::last_days(playbooks, schema, scope, facts) -> Report;  // the journal's last 7 days
pub struct Firing { pub playbook: String, pub event: String, pub op: String, pub args: Value, pub target: String, pub subject: String, pub autonomy: Autonomy, pub key: EffectKey,
                    pub lineage: EffectKey /* engine::lineage(playbook, op, target) */, pub chain: Vec<EffectKey> /* the event's causes and its lineage: what the events it causes carry in `causes` */,
                    pub depth: u8, pub merged: Vec<String> }
// stamping (events-13): a v2 delivery with depth > 0 notes its marker (journal::expect_drive); the session's UserPromptSubmit with that
// marker makes its events carry data.depth and data.causes (hook events as read, the rest at append), until a prompt with no marker
// (the person) or 2 h (journal::DRIVEN_FOR_MS). The manager's sends are depth 1; a session's send passes its own drive on.
pub fn fire(playbooks: &[Playbook], event: &dyn EventView, schema: &Schema, facts: &Facts, counts: &dyn Counts) -> (Vec<Firing>, Vec<HeldBack>);
                                                           // key = effect_key(event id, subject, op, target): one effect per (event, op, target)
// limits (events-13): MAX_DEPTH 3; SENDS_PER_HOUR 4, SENDS_PER_DAY 12 per session; one run per (playbook, subject);
// breaker: SAME_SUBJECT_TRIPS 5 in a day, FAILURES_TRIP 3 in a row; GLOBAL_PER_HOUR 60. Counts come from the effect ledger (origin "playbook:<id>").
pub fn simulate::dry_run(...) -> Report;                   // past events, nothing acts
pub const builtins::BUILTINS: &[Builtin];                  // native: ci-fix, conflict-fix (no manager, approval or level: exec-16), needs-you,
                                                           // review-tell, follow-ups, daily-brief, stuck-session, usage-handoff
pub mod builtins::ci_fix { pub fn step(..) -> (Step, bool); pub fn subject(..) -> String; pub fn group(..) -> String; pub fn key(..) -> EffectKey; }  // M31's key, to the letter
pub struct native::Native; impl Native { pub fn on_event(&mut self, event: &dyn EventView, view: &View) -> Vec<manager::Action>; pub fn tell_ready(&mut self, view: &View) -> Vec<manager::Action>; }
                                                           // needs-you, review-tell (trusted_author, D27), stuck-session, usage-handoff;
                                                           // parity with v1's harness at its react seam (parity_with_harness_step)
pub struct Facts { pub level: &dyn Fn(Option<&ProjectKey>) -> Level, pub me: &[String], pub confirmed: &[String], pub now_ms: u64 }  // AuthorTrusted: gate::trusted_author
// ops (M40): playbook.check (each playbook as this machine reads it) and playbook.test <id> [--since 7d] (a dry run over the journal); both read, v2 only

// glasscode_core::schedule (M41) — due-ness from the wall clock every tick, never from sleeping timers
pub enum Misfire { Skip /* on time only */, Once /* the last missed, once, within its window */ }
pub enum Place { Home, Either }
pub struct Schedule { pub id: String, pub name: String, pub spec: playbooks::ScheduleSpec, pub then: playbooks::Action, pub autonomy: Autonomy, pub misfire: Misfire,
                      pub expires_ms: Option<u64> /* UTC */, pub place: Place, pub enabled: bool, pub proposed_by: Option<String>, pub made_ms: u64 }
impl Schedule { pub fn hash(&self) -> [u8; 32]; pub fn approval_id(&self) -> String /* "schedule:<id>" */; pub fn grace_ms(&self) -> u64; }  // 6 h daily; min(period/2, 2 h)
pub struct ScheduleFile { pub schedules: Vec<Schedule> }                    // authority/schedules.json; refuse, FALLS_BACK = false
pub struct ScheduleState { pub seen_ms: u64 /* the latest wall time seen */, pub runs: BTreeMap<String, RunState> }  // authority/schedules-state.json; refuse
pub fn daily_at(local_day: i64, minutes: u16) -> Option<u64>;             // a skipped time: the next valid minute; a repeated one: the first
pub fn last_occurrence(spec: &ScheduleSpec, made_ms: u64, after_ms: u64, until_ms: u64) -> Option<(u64, u32)>;
pub fn evaluate(schedules: &[Schedule], approved: &dyn Fn(&Schedule) -> bool, state: &mut ScheduleState, now_ms: u64, home: bool) -> (Vec<Due>, Vec<Skip>);
                                                                           // missed ones collapse into one; overlap skipped; expiry against seen_ms
pub fn started(..); pub fn finished(..); pub fn after_restart(state) -> Vec<(String, u64)>;  // marked before it acts: never twice
pub fn pace(due: Vec<Due>, since_wake_ms: u64, last_late_ms: Option<u64>, now_ms: u64) -> Vec<Due>;  // a minute after a wake; late ones one a minute
pub fn key(schedule: &str, occurrence_ms: u64, op: &str, target: &str) -> EffectKey;  // the schedule's id and the occurrence's UTC instant
pub fn propose(schedule: Schedule, by: Option<&str>) -> Result<(), ScheduleError>;  // anyone but the person: off, only proposed (M44's checks)
pub fn approve(id: &str, person: bool) -> Result<(), ScheduleError>; pub fn is_approved(schedule: &Schedule, approved: &ApprovedPlaybooks) -> bool;
pub fn round(now_ms, home, clear, since_wake_ms, last_late_ms) -> Result<(Vec<(Due, Schedule)>, Vec<(Skip, String)>, Vec<(String, u64)>), ScheduleError>;
                                                                           // under the state's lock: evaluate, pace, mark started, save; a due one
                                                                           // held back by pace stays due until started or past its window
// autonomy: watch records only; tell tells the person, nothing runs; ask and act run the op through the gate as the manager's wake

// glasscode_core::delivery (M42) — telling the person, by matter; pure
pub struct Notice { pub matter: String /* "session:<id>", "approval:<n>", "pr:<repo>#<n>" */, pub severity: journal::Severity, pub title: String, pub body: String }
pub struct Presence { pub window_active: bool, pub watching: bool } pub struct Rules { pub quiet: Option<(u16, u16)> /* Settings */, pub silent: bool }
pub enum Decision { Popup { sound: bool }, Update { count: u32 } /* new evidence within BUNDLE_MS (10 min): the one on screen, quietly */,
                    Line(Hold) /* Info, Watching, Snoozed, Burst (> 3 a minute), Cap (12 an hour) */, Held /* quiet hours */, Nothing /* said before */ }
impl Delivery { pub fn offer(&mut self, notice: Notice, presence: Presence, rules: &Rules, now_ms: u64, minute: u16) -> Decision;
                pub fn seen(&mut self, key: &str); pub fn snooze(&mut self, key: &str, until_ms: u64); pub fn close(&mut self, key: &str);
                pub fn digest(&mut self, rules: &Rules, now_ms: u64, minute: u16) -> Option<Notice> /* one, when the quiet hours end */;
                pub fn waiting(&self, now_ms: u64) -> Vec<(&Matter, u64)> /* the brief's "still waiting on you" */; }
pub fn manager::messages::send_intent(message: &OutMessage, launch: &str) -> effects::Intent;  // at most once, per message and version
pub fn manager::brief::waiting_lines(waiting: &[(String, u64)]) -> Vec<BriefLine>;

// glasscode_core::runtime — what only the app or only glasscoded can do; the runtime itself is shared
pub trait Host: Send + Sync {
    fn machine(&self) -> &MachineId;
    fn run_op(&self, op: &Op, caller: &Caller) -> OpReply;            // Window and SessionHost ops
    fn has(&self, host: OpHost) -> bool;
    fn notify(&self, title: &str, body: &str, matter: &str);
}
// AppHost (crates/glasscode) and DaemonHost (crates/glasscoded) implement it (M50), with one more method:
//     fn brain_checked(&self, lane: Lane, wake: Option<&str>);   // its MCP server's calls are served from now on
// M50's runtime: pub trait BrainDriver { new_session, prompt, next(wait) -> Option<Update>, cancel, stop, pid, session_id, stderr_tail }
// (ACP-shaped); ClaudeDriver, CodexDriver; pub enum Update { Session, Init { tools, model, version }, Text, ToolCall, Permission, RateLimit,
// Usage, Ended(TurnEnd) }; pub fn run_turn(driver, text, checked, expected, on_checked, on_update, Limits) -> Outcome;
// pub struct Brain { … } with ask(host, &mut Ledger, text, Turn::{Chat, Wake}, now, on_update) -> Result<Outcome, Refusal { Health, Budget }>;
// pub enum BrainHealth { Ready, NotInstalled, SignedOut, LoginExpiresSoon, OutOfUsage, Overloaded, VersionNotApproved, Crashed,
// ToolListWrong, SwitchedOff } with sentence() and backoff(); budget::Ledger (store manager/tokens); wrap::wrap(source, text);
// layout(instructions, memory, situation, question) -> (system, user); restart_due(turns, rss).
// M51: Host also has propose(op, reason, lane, wake) -> Result<card, why> and run_under(op, caller, playbook) -> OpReply;
// runtime::queue::{Wake { key, priority, precondition, events, projects, notes, playbook, … }, Queue { push, pop_where, cancel }};
// runtime::wake::{INSTRUCTIONS, parse(reply) -> Answer { tell_person, matters, proposals, notify }, decide(answer, under) -> Vec<Effect>};
// runtime::situation::for_wake(wake, facts, projects); runtime::lanes::{ChatLane, Hold, Locks, wake_spec, run_wake, next_wake,
// wake_for_event, still_holds, under_playbook, lane_may_act, link_rollover}; playbooks::engine::wake_of(firing) -> Option<Wake>.

// glasscode_core::profile
pub enum SessionOrigin { Person, Automation, Quarantine }
pub struct SessionProfile { pub claude_settings: serde_json::Value, pub claude_args: Vec<String>, pub codex_args: Vec<String>, pub env_set: Vec<(String, String)>, pub env_remove: Vec<String>, pub label: &'static str }
pub fn session_profile(origin: SessionOrigin, agent: AgentKind, policy: &ProjectPolicy, worktree: &Path, account_dir: Option<&Path>, cli: &CliVersion) -> SessionProfile;
// M22 as built: `session_profile(origin, agent, policy, facts: &ProfileFacts)`, where ProfileFacts holds what the app knows at
// launch: the worktree, the project's other worktrees (denied one by one), its GitFacts (common dir, own git dir, branch), the
// agent notes, the `.env` files at launch, the hook program (absolute), the events file and the permission file (arguments of
// the hooks, crit-15), the registries and the forge's hosts, the default branch and the control socket. SessionProfile adds
// `origin`, `permission: PermissionFile` (what `glasscode hook permission` answers from) and `pretrust` (Codex's folder trust,
// person only). `SessionOrigin` is `gate::SessionOrigin`, re-exported here. `agents::launch_with(.., Some(&profile))` applies it;
// `None` launches as before. `gate_tool(ctx, tool, input) -> Result<(), String>` is the PreToolUse gate; `permission_answer`
// the PermissionRequest hook's (allow, deny or nothing; never updatedPermissions; compound commands declined).
pub fn trusted_author(login: &str, association: Option<&str>, person: &[String], confirmed: &[String]) -> bool;  // gate, D27
pub enum HeadlessKind { BrainChat, BrainWake, AutoReview, Study }
pub fn headless_profile(kind: HeadlessKind, agent: AgentKind, mcp_config: &Path, cli: &CliVersion) -> SessionProfile;
// M23 as built: `headless_profile(kind, agent, schema: &Value, schema_file: &Path, answer_file: &Path)`: no tools, Codex read-only,
// the answer bound to `schema`; `agents::headless_run(agent, config_dir, &profile, input, answer, env)` feeds `input` on stdin.
// The brain's lanes (M50) add their MCP config to it: for BrainChat and BrainWake it is `brain_profile(wake)` (stream-json, ToolSearch,
// Glasscode's MCP server only, a wake with no session persistence and no cron), and `runtime::{claude, codex}::args` add the launch's files. `own_commits_only(pushed, made)` and `session_commits(session)` back the
// rule that a push asked by anyone but the person sends only that session's commits.
```

## 9. Cross-cutting rules

**Performance.** These budgets are checked in CI with the reference load of slice plan section 14 (exec-19):
- No git, file or process work on the main thread. This extends the rule in `architecture.md`; the new stores do their IO on background threads.
- A tick asks for a draw only when something it shows changed.
- Budgets:

| Path | Budget |
|---|---|
| Journal append | ≤ 1 ms p95 (P11: `write` 4 µs, with `fsync(2)` 65 µs, with `F_FULLFSYNC` 4.6 ms) |
| Hook event to journal | ≤ 300 ms p95 (P11: `glasscode hook` start 6.1 ms) |
| `decide()` | ≤ 50 µs |
| Memory search on 10 MB | ≤ 30 ms p95 (P11: `git grep` as a stand-in, 35 ms; M20.1 must do better) |
| Brief assembly | ≤ 20 ms |
| Op dispatch, without the handler's own work | ≤ 5 ms |
| Idle CPU added by v2 | ≤ 0.5 % of one core |
| Resident memory added | ≤ 60 MB, without the brain processes |

**Formats and dependencies** (crit-19):
- JSON through serde_json, JSON lines, Markdown, and git through the `git` program, which is already used.
- No new crate in release 1.

**Language** (crit-18). Rules that read text use English word lists, and their result is advisory: a pause word, the urgent word list. Decisions never depend on English wording. The brain answers in the person's language.

**Forges** (crit-17). Release 1 knows GitHub. A project on another forge gets:
- no PR events, no CI source and no forge wall;
- the Careful level only, with its Settings line saying so;
- done conditions limited to the board and to "answered".

**The server side of each slice** (exec-3). Every slice has a "Server" line. Core types compile in `glasscoded`. A store that `glasscoded` writes uses the same module. glasscoded runs `decide()` for its own sessions from the one-way authority cache.

**Windows** (exec-26). Every slice has a "Windows" line. Paths come from `paths`, never literals. The sandbox rows of section 7 say "none" for native Windows.

**Older builds** (exec-7). `state.json` keeps every field an older build reads. New policy lives in `authority/`. `AgentSettings.rules` keeps its 13 kinds; v2 reads them as the defaults of the `Standard` level for person-started sessions.

**Failure and recovery** (exec-23). Every slice adds its rows to this table. `glasscode manager doctor` checks each row.

| Failure | Detected by | What the person sees | Recovery | Test |
|---|---|---|---|---|
| A store file is damaged | `Store::load` | One line naming the store and the feature that stopped | `.broken` copy kept; the previous generation (`.prev`) is used when it is whole; otherwise "refuse" stores stop their feature and "empty" stores start fresh | fault injection per store (M10 `fault_injection_for_every_store`; M55) |
| A memory git error, or a stale `index.lock` | the memory writer thread, the only owner of git calls | "Memory could not save; your text is kept in inbox/rejected" | The lock is removed only when no Glasscode git process runs; the text is kept | M20.1 |
| Memory changed outside Glasscode (a direct edit, a commit made with plain git) | `Memory::pending`: the working tree against Glasscode's last commit | The edit with its diff in Settings › Manager › Memory, to accept or discard; briefs read only Glasscode's commit | Accept (it becomes the person's) or discard (the file and the branch go back) | M20.1 `direct_edit_is_not_in_the_brief` |
| Memory's head record is damaged, or names a missing commit | `MemoryHead` (refuses, no fallback) and `Memory::snapshot` | "Memory is read-only until it is repaired"; briefs are empty | Repair the record; nothing writes on an older head | M20.1 `a_damaged_head_refuses` |
| `git fsck` fails on memory | the first write of a run | "Memory is damaged; it takes no writes until it is repaired" | Writes refused; reads keep the last good commit | M20.1 |
| The journal cannot be written (disk full) | the writer thread (`journal::health`) | One toast (M14); a status bar warning and paused playbooks when they exist (M40) | Retries every 30 s, keeping up to 10,000 events; nothing acts without a journal | M14 (events-m4) |
| A brain is signed out, out of usage, or on an unsupported version | `runtime::health` | One notification in the CLI's own words; the chat explains | Wakes stop; answers from code keep working | M50 |
| A claim has no result after a crash | the effect ledger at start | Nothing when the probe settles it; else an "uncertain" card | The probe runs with no model | M31 |
| The clock jumps or the time zone changes | the HLC and the scheduler | One Activity line | Schedules re-plan from UTC; the order of events is kept | M41 |
| The server cannot be reached | sync heartbeat | "Manager: server unreachable" in the status bar | Under Automatic the Mac runs Degraded, with nothing outward; it may act alone after N minutes only if you turned that on | M58 |
| A hook is not reporting | the reconciler | "This session is not reporting" on the session | Transcript or rollout tail plus the reconciler | M14, M30 |
| A line of the conversation does not read | `conversation::load` | Nothing; one log line naming the file | The line is skipped and left in place (retention keeps it), so a newer build can still read it | M43 `bad_lines_are_skipped` |

## 10. Lifetime: quit, update, sleep

Release 1 rule, shown in Settings and on the Autopilot card: **with a server paired, the manager keeps working when the Mac sleeps or Glasscode quits: it moves to the server (D5). Without a server, the manager and its sessions run only while Glasscode runs and the Mac is awake.** (crit-11)

- **Quit and update.** When an automation turn is running, quitting shows a sheet naming the turns. The person chooses to wait, or to quit and mark them interrupted. "Restart to update" waits for the turns to end, at most 10 minutes. Claims follow D9.
- **Sleep.** While an automation turn runs on mains power, Glasscode holds an idle-sleep assertion (owner O6, default on, shown in the status bar). It never blocks lid-close sleep.
- **After wake or relaunch:**
  1. reconcile (M14);
  2. report each cut turn once, with the cause `Slept` or `AppQuit`;
  3. resume sessions;
  4. run each missed schedule at most once (M41).
- **Rejected for now: a background helper** (a launch agent hosting the runtime and tmux, as glasscoded does on Linux). Trigger to reconsider, for people without a server: the person asks for schedules while Glasscode is closed, or more than 20 % of automation turns end in `AppQuit` or `Slept` over a month.

## 11. What the manager cannot do

The inventory of M33 generates this list. Release 1:
- It cannot merge, approve or close a pull request. No such op exists.
- It cannot change its own policy, playbooks or schedules without the person's confirmation in the app.
- It cannot press Allow on an approval: approvals are `human_only`.
- It cannot read credentials.
- It cannot type into a session the person has open with a draft in its message box. The send waits.
- It cannot act on a non-GitHub forge.
- Without a server, it cannot act while the app is closed.
- It does not speak. Voice is a separate plan.

## 12. Compared with other systems, and rejected designs

| System | What it does well | Taken | Left |
|---|---|---|---|
| Nod (n8n Labs, 2026) | memory only from the owner's quoted words; trust check "your request covers its steps"; read-and-prepare background; one use, one result; published evals | words ledger and tiers (D1), grants (D3), wake lane prepares and acts only by playbook (M51), effect ledger (D9), evals (M02) | TypeScript, Postgres and Docker; its own computer and browser |
| Hermes Agent | capped memory always in the prompt; self-written skills with approval; a fresh agent per scheduled job; delegation hygiene | caps and consolidation (M20), know-how as guidance (M62), fresh wakes (M51), children with fresh context and no new tools (M61) | Python; its logins to Claude and ChatGPT |
| Claude Code | hooks (33 events), sandbox, agent view, agent teams ("a message from another agent never counts as the user's approval"), auto-memory | hooks as guarded input (D11), sandbox in profiles (M22), the agent-teams rule (D1 tier never) | channels (research preview), background sessions (the app owns sessions) |
| Codex | app-server (threads, turns, approvals), hooks, memories, guardian approvals | exec resume now, app-server by probe (D14) | a commercial app-server login |
| JetBrains, Zed, VS Code | the IDE as a typed action registry, exposed over MCP | the registry and `glasscode mcp` (M12, M33, M34) | — |
| Cursor and Copilot background agents, Devin, Conductor | one VM or worktree per agent, ending in a PR; parallel sessions | worktrees per session (already there), work ledger (M32), validator (M61) | cloud VMs |
| ACP | one protocol to many agents | the brain driver trait shaped like ACP (M50) | the TypeScript adapters today |

The "Left" column is the list of what Manager v2 deliberately does not copy (land-22). Nod is weighted by its published 2026 design and evals (land-20). Owner process questions are O9 of the slice plan (exec-29). The app-server paragraph is split into maturity, sign-in rule and licence in D14 and D31 (alt-10).

**Rejected designs and their triggers.** These are the red team's verdicts on alternatives A to D (alt-14, alt-15, alt-16).

| Design | Why not now | Reconsider when |
|---|---|---|
| The manager is itself a long-lived Claude or Codex session with Glasscode over MCP | vendor churn; no control of memory and scheduling; two vendors would mean two designs | a vendor ships a stable, documented supervisor API with hooks for both |
| ACP end to end, for the brain and the sessions | the adapters are TypeScript and lag the CLIs; PTYs stay for sessions | official Rust or native ACP from both vendors |
| A durable-workflow engine (Temporal or similar) | a new runtime; the effect ledger and the state machines give the needed guarantees | more than three hosts need to share work |
| Hermes or Codex app-server as a sidecar | a Python runtime; its login handling; experimental | never for Hermes; app-server per D14 |
| A small-model triage before every wake | each call is a CLI start on a subscription and costs seconds | measured wake volume exceeds the D6 cap for a week |
