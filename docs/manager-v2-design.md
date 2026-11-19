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
| D4 | **The brain has no shell.** Claude runs with `--restricted --tools "" --strict-mcp-config --mcp-config <glasscode mcp> --allowedTools "mcp__glasscode__*" --permission-mode dontAsk --permission-prompts none --disable-slash-commands`; all of these flags are in `claude --help` 2.1.322, and their behaviour waits on proof P01. Codex runs with its shell, apps, plugins, multi-agent and memories turned off, a read-only sandbox, network off, `--ignore-user-config --ignore-rules`, and the Glasscode MCP server set to `default_tools_approval_mode="approve"` and `required=true` (P02). Reads become typed ops. `DENIED_TOOLS` is deleted. One `HeadlessProfile` builder serves every non-interactive launch. | brain-1, alt-2, trust-10, ops-m1, alt-m1, trust-m2 |
| D5 | **One manager, present everywhere, acting from one home at a time, moving by itself** (owner, O12, 2026-11-19). The same conversation, memory, work and approvals are on the Mac and on the server. Only the *home* acts. Settings › Manager › Home has three choices: **Automatic** (the default once a server is paired), **Always on the server**, and **Always this Mac**. Under Automatic, the Mac is home while you are at it, and the server is home when the Mac sleeps, quits or is away. glasscoded is the single arbiter: it holds a lease with a generation number, and outward effects are fenced by that generation. The rules are below this table. With no server paired, the Mac is home and nothing switches. `decide()` lives in core; authority flows from the Mac to the server only. | crit-5, events-8, ops-8, crit-10 |
| D6 | **Budget.** The manager keeps its own token ledger, read from each brain result's usage, with a daily cap and an hourly wake cap. Vendor usage readings are advisory. | events-17, brain-6, exec-18 |
| D7 | **No small-model quick check in release 1.** Rules and native playbooks decide; wake volume is measured first. | events-16, brain-7, land-13 |
| D8 | **The chat lane persists.** Claude uses its own session id with persistence on; Codex uses `codex exec resume`, without `--ephemeral`. The system prompt is static, and memory and the situation go in user turns, because `--system-prompt-snapshot` records appended text on the first request. The conversation rolls over to a fresh one, with a summary, past a set size. | brain-4, land-3, alt-9 |
| D9 | **One effect ledger, which is also the audit record.** Each effect has a deterministic key. The claim is fsynced before the action and the result written after it. Ops declare a retry class: idempotent, verifiable or at-most-once. | events-7, trust-16, alt-4 |
| D10 | **Authority store.** Policy keyed by op name and op group, playbooks, schedules, approvals and device keys are JSON in `~/.glasscode/authority/`, edited only through the app. A file that changes outside the app counts as not approved. The 13 kinds in `state.json` stay frozen for older builds. `POLICY.md` in memory becomes prose with a generated header and carries no authority. | trust-4, events-10, exec-7, ops-12 |
| D11 | **Hooks are guarded**, never called enforced. Hook commands use absolute paths and an integrity check. | crit-15 |
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
| `authority/` (policy, playbooks, schedules, approved snapshots, approvals, devices, memory head, pending actions, engine switch) | Authority | deny read and write | deny read and write | the app only |
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
| `~/.ssh`, `~/.aws`, `~/.config/gh`, `~/.netrc`, `~/.gnupg`, keychain files (`~/Library/Keychains`, `~/.local/share/keyrings`, Windows' `Microsoft/Credentials` and `Microsoft/Protect` folders), a file (never a folder) whose name starts with `.env` anywhere: `.env`, `.env.local`, `.env-prod`, `.env_local`, `.envrc` | Credentials | deny read | deny read | not Glasscode |

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
- **The table in code.** This table is also `store::DATA_MAP`. It has a row for every entry of the layout (section 5) that is not a cache: the stores, the stores that slices to come make (marked planned, with their slice), and today's files that are not stores yet. A cell of today's files that nobody checked in its module says "not decided (M25)", for M25 (export, backup, forget) to settle. A test fails when a `Store` has no row or no line in `store::ALL_STORES`, when an entry of the layout has no row, and when a planned store exists.

| Store | Module | Format | Retention | On damage | Export | Local only | Forget | Sync |
|---|---|---|---|---|---|---|---|---|
| `state.json` | `store` | JSON | lasting | today's rule (backup and fresh start); the manager part refuses | yes | — | — | through `mirror` |
| `authority/machine.json` | `store` | JSON, schema v1 | lasting | refuse; what needs the machine id (sync, the words ledger) stops | no | yes: each machine has its own | — | never |
| `authority/engine.json` | `store` | JSON, schema v1 | lasting, until M56 removes v1 | refuse; v1 runs with every v2 feature off | no | yes | — | no |
| `authority/policy.json` | `gate` | JSON, schema v1 | lasting | refuse; every project counts as Careful | yes | — | project | Mac to server |
| `authority/playbooks.json`, `schedules.json` | `playbooks` | JSON | lasting | refuse; nothing runs | yes | — | — | Mac to server |
| `authority/pending.json` | `gate` | JSON | until done or expired, plus 7 days | refuse; cards show "could not load" | no | — | — | Mac to server and back (M57); carries no authority |
| `authority/memory-head.json` | `memory` | JSON (commit id per project key) | lasting | refuse; briefs empty and the person is told | no | — | — | Mac to server |
| `words/<machine>/<yyyy-mm>.jsonl` | `words` | JSON lines | while cited, else 90 days | skip bad lines | with the facts that cite them | per project | person, project | both ways (M57); the Mac never accepts tier 1 from the server |
| `journal/<yyyy-mm-dd>.jsonl` | `journal` | JSON lines | 30 days | skip bad lines, log once | no | — | person (redact), project | both ways, by cursor (M57) |
| `effects/<yyyy-mm>.jsonl` | `effects` | JSON lines | 1 year (setting) | refuse new at-most-once effects until repaired | yes | — | redact payloads | merged by key and generation; outward claims registered with the server (M58) |
| `memory/` | `memory` | git, Markdown with entry headers | lasting; archive kept | `git fsck`; refuse writes and keep reads from the last good commit | yes | per project | person, project, employer | per entry, by HLC stamp and tombstone (M57); "stays on this Mac" projects never sent |
| `agent-notes/` | `memory` | Markdown (vendor's own) | lasting | not used for briefs | yes | per project | project | per project policy |
| `manager/conversation/` | `conversation` | JSON lines per topic | 90 days of text, summaries lasting | skip bad lines | yes | — | person | changes both ways (M57) |
| `manager/work.json` | `work` | JSON | until done plus 30 days | refuse; rebuilt by the reconciler | no | — | — | both ways (M57) |
| `manager/data.json` (v1) | v1 `store` | JSON | read once by migration, then kept as `data.v1.json` | migration stops | — | — | — | — |
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
| The brain runs no command and reads no file outside its work folder | enforced (CLI flags; the tool list is checked at start) | M50 `brain_tool_list_is_exactly_glasscode` (both CLIs) |
| Every op of a non-person caller passes `decide()` | enforced (Rust) | M21 `every_handler_goes_through_decide`; adversarial suite |
| Only the app's own window is `Ui` | enforced (in-process channel) | M13 `interactive_flag_from_a_socket_is_terminal` |
| Policy, playbooks and schedules change only through the app | enforced against sandboxed sessions on macOS and Linux (they cannot write `authority/`); guarded on Windows (file-tool deny only); a program running as the person can change them, and the gate then reads only the approved snapshot, at least as strict as Careful | M21 `changed_policy_counts_as_careful`, M40 `synced_playbook_is_not_approved` |
| A brief contains only text Glasscode committed | enforced (built from the recorded commit) | M20 `direct_edit_is_not_in_the_brief` |
| Sessions do not write memory files directly | guarded (file-tool deny rules) | M22 |
| Sessions do not read credentials | enforced by the sandbox for shell commands on macOS and Linux; guarded for file tools; **none on native Windows** | M22 |
| Quarantine sessions reach only the forge | enforced by the sandbox network allowlist (Claude) or network off (Codex) | M22 |
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
pub fn authority_dir() -> PathBuf; pub fn memory_dir() -> PathBuf; pub fn agent_notes_dir(key: &ProjectKey, agent: AgentKind) -> PathBuf;
pub fn words_dir(machine: &MachineId) -> PathBuf; pub fn journal_dir() -> PathBuf; pub fn effects_dir() -> PathBuf;
pub fn manager_work_dir() -> PathBuf; pub fn cache_dir() -> PathBuf;
// ProjectKey and MachineId live here, as path-safe newtypes: one folder name, 1 to 64 of a-z 0-9 . _ -,
// starting with a letter or a digit, not ending in a dot, not a Windows device name (con, prn, aux, nul,
// com0-9, lpt0-9, with or without an extension), checked on parse and on deserialize. identity re-exports them
// (`pub use crate::paths::{MachineId, ProjectKey};`) and adds its own impl blocks.
pub struct ProjectKey(String); impl ProjectKey { pub fn parse(text: &str) -> Option<ProjectKey>; pub fn as_str(&self) -> &str; }
pub struct MachineId(String);  impl MachineId  { pub fn parse(text: &str) -> Option<MachineId>;  pub fn as_str(&self) -> &str; }

// glasscode_core::identity
pub use crate::paths::{MachineId, ProjectKey};  // defined in paths (M10)
// ProjectKey: "glasscode-3f9a1c": slug + 6 hex of the first alias
pub struct Aliases { pub remotes: Vec<String>, pub roots: Vec<String>, pub paths: Vec<PathBuf>, pub project_ids: Vec<String> }
pub enum KeyMatch { Known(ProjectKey), AskFork { candidate: ProjectKey }, New(ProjectKey) }
pub fn resolve(dir: &Path, known: &[(ProjectKey, Aliases)]) -> KeyMatch;
pub enum WorkKind { Branch, Ticket, Pr, Review }
pub struct WorkKey { pub project: ProjectKey, pub kind: WorkKind, pub id: String }
// MachineId: random (16 hex digits), kept in authority/machine.json (MachineId::this, in store)
pub struct Hlc { pub wall_ms: u64, pub count: u32, pub machine: MachineId }
impl Hlc { pub fn now(last: &Hlc) -> Hlc; pub fn merge(&self, seen: &Hlc) -> Hlc; }

// glasscode_core::caller
pub enum Lane { Chat, Wake }
pub enum Caller {
    Ui,
    Session { id: String, project: Option<ProjectKey> },
    Manager { lane: Lane, wake: Option<String>, project: Option<ProjectKey> },
    Phone { device: String },
    Machine { id: MachineId },        // another Glasscode machine's sync connection (the Mac, seen from glasscoded)
    Terminal { pid: Option<u32> },
}

// glasscode_core::words
pub enum ComposerKind { TodayBox, ManagerChat, ChatView }
pub enum InjectKind { SessionSend, Playbook, FirstPrompt, Handoff, Review }
pub enum Origin { Composer(ComposerKind), TerminalKeys, Device(String), Voice, Pasted, Injected(InjectKind), Schedule, Peer(String), Outside }
pub enum Tier { One, Two, Never }
pub struct WordsId(String);
pub struct WordsRecord { pub id: WordsId, pub at: Hlc, pub session: Option<String>, pub project: Option<ProjectKey>, pub origin: Origin, pub text: String, pub hash: [u8; 32], pub spans: Vec<(usize, usize, Origin)> }
pub fn record(origin: Origin, text: &str, session: Option<&str>, project: Option<&ProjectKey>) -> WordsId;
pub fn match_prompt(session: &str, text: &str, at_ms: u64) -> Option<WordsId>;   // hook event -> ledger record
pub fn tier_of(origin: &Origin) -> Tier;
pub fn quote_in(id: &WordsId, quote: &str) -> Option<(usize, usize)>;             // only typed spans count

// glasscode_core::ops (M12.1)
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
// Project, Task and Ticket (in capitals) stay words: their handler looks them up (M12.2).
pub enum ArgKind { Word, Text, Switch, Count, Day, Until, Session, Project, Task, Pr, Ticket, List, Section, Movable, Every, Many }
// Where it is on the command line (ops-17); word 0 is the first after the op's own words.
pub enum At { Word(u8), Rest(u8), Flag(&'static [&'static str]), FlagOrWord(&'static [&'static str], u8) }
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
}
pub static OPS: &[OpSpec];             // the tables (ops/table_*.rs), one after the other
pub fn find(name: &str) -> Option<&'static OpSpec>;
pub struct Noun { pub path: &'static [&'static str], pub also: &'static [&'static str], pub unknown: &'static str }
pub static NOUNS: &[Noun];             // "session": "sessions", "s"; and what an unknown word after it says
pub const NOT_YET: &[(&[&str], &[&str])];  // the commands M12.2 adds: parse says "not in the registry yet"; M12.2 empties it
pub struct Op { pub spec: &'static OpSpec, pub args: serde_json::Value }   // JSON: {"op": "board.done", "args": {"task": "zysg"}}
pub struct Targets { pub projects: Vec<ProjectKey>, pub sessions: Vec<String>, pub repos: Vec<String>, pub paths: Vec<PathBuf>, pub leaves_machine: bool }
pub struct SessionView { pub id: String, pub title: String, pub branch: String, pub review: Option<u64>, pub project: Option<String> }
pub struct ParseCtx { pub caller_is_person: bool, pub caller_session: Option<String>, pub today: day::Day, pub sessions: Vec<SessionView> }
pub type ParseError = OpError;
// Pure; no IO. A session by the start of its id or words from its title: the person only; any other caller
// gives its whole id or "this", or gets Ambiguous with the sessions the word could mean (ops-17).
pub fn parse(words: &[String], ctx: &ParseCtx) -> Result<Op, ParseError>;
pub fn json_schema(spec: &OpSpec) -> serde_json::Value;                           // for MCP and help: the shape of Op.args
pub enum OpReply { Done { text: String, data: serde_json::Value }, Waiting { pending: String, summary: String }, Failed(OpError) }
pub struct OpError { pub code: ErrorCode, pub text: String }                     // JSON: {"code": "usage", "text": …}
pub enum ErrorCode { Failed, Usage, NotFound, Ambiguous { candidates: Vec<Candidate> }, Denied, Unavailable { host: OpHost, queued: bool } }  // exit 1, 2, 1, 2, 5, 3
pub struct Candidate { pub id: String, pub title: String, pub project: Option<String> }  // six at most

// glasscode_core::gate
pub enum Level { Careful, Standard, Autopilot }
pub enum Rule { Allow, Ask, Never }                                               // the same names as control::Rule
pub struct ProjectPolicy { pub level: Level, pub ask: Vec<String>, pub never: Vec<String>, pub allow_commands: Vec<String>, pub conditions: Vec<Condition>, pub model_account: Option<String>, pub local_only: bool }
pub enum Condition { Branch { not: Vec<String> }, Paths { deny: Vec<String> }, Hours { local: (u16, u16), days: u8 }, Person { only: Vec<String> } }
pub enum Decision { Allow, Ask { why: String }, Never { why: String } }
pub struct Grant { pub message: WordsId, pub quote: (usize, usize), pub op_hash: [u8; 32], pub targets: Targets, pub turn: String }
pub fn decide(caller: &Caller, op: &Op, targets: &Targets, policy: &PolicySet, grant: Option<&Grant>) -> Decision;
pub enum PendingState { Waiting, Approved, Denied, Expired, Ran, Uncertain }
pub struct PendingAction { pub id: String, pub op: Op, pub arg_hash: [u8; 32], pub caller: Caller, pub reason: String, pub created: Hlc, pub expires_ms: u64, pub state: PendingState }

// glasscode_core::journal
pub enum Severity { Info, Notice, Warn, Urgent }
pub struct Event { pub v: u16, pub id: String, pub at: Hlc, pub source: Source, pub project: Option<ProjectKey>, pub session: Option<String>, pub kind: EventKind, pub severity: Severity, pub summary: String, pub data: serde_json::Value }
pub fn append(event: Event) -> std::io::Result<()>;                               // one writer thread per process
pub struct Cursor { pub file: String, pub offset: u64 }
pub fn read_from(cursor: &Cursor, max: usize) -> (Vec<Event>, Cursor);

// glasscode_core::effects
pub struct EffectKey(pub [u8; 32]);
pub fn effect_key(origin: &str, subject: &str, op: &str, target: &str) -> EffectKey;
pub enum EffectState { Claimed, Done(serde_json::Value), Failed(String), Uncertain }
pub enum Claim { New(EffectKey), Already(EffectRecord) }
pub fn claim(key: &EffectKey, op: &Op, home: &MachineId) -> std::io::Result<Claim>; // fsync before return
pub fn finish(key: &EffectKey, state: EffectState) -> std::io::Result<()>;
pub enum ProbeResult { Happened(serde_json::Value), DidNotHappen, Unknown }

// glasscode_core::turns
pub enum TurnEnd { Completed, Paused, Failed(String), Interrupted, Aborted(Option<i32>), Crashed, Slept, AppQuit }
pub struct FileDelta { pub path: String, pub added: u32, pub removed: u32 }
pub struct TurnReport { pub session: String, pub turn: String, pub started_ms: u64, pub ended_ms: u64, pub end: TurnEnd, pub tools: u32, pub failed_tools: Vec<String>, pub commands: Vec<String>, pub files: Vec<FileDelta>, pub commits: Vec<String>, pub pushed: bool, pub last_reply: String, pub question: Option<String> }

// glasscode_core::work
pub enum DoneCondition { PrMerged, PrReadyForReview, CiGreen, ReviewRequested, BoardTaskDone(String), AnsweredWithoutQuestion, Person }
pub enum WorkState { Open, Waiting(String), Stuck(String), Done, Dropped }
pub struct WorkItem { pub key: WorkKey, pub sessions: Vec<String>, pub pr: Option<(String, u64)>, pub ticket: Option<String>, pub board_task: Option<String>, pub done: Vec<DoneCondition>, pub state: WorkState, pub updated: Hlc }

// glasscode_core::playbooks
pub enum Autonomy { Watch, Tell, Ask, Act }
pub enum ScheduleSpec { Every { secs: u64 }, At { minutes: u16, days: u8, tz: String }, Once { at_ms: u64 } }
pub enum Trigger { Event(EventFilter), Schedule(ScheduleSpec), After { event: EventFilter, delay_secs: u64 } }
pub struct Playbook { pub id: String, pub name: String, pub trigger: Trigger, pub when: Vec<Cond>, pub then: Action, pub autonomy: Autonomy, pub limit: Limit, pub builtin: bool, pub enabled: bool, pub approved_hash: Option<[u8; 32]> }

// glasscode_core::runtime — what only the app or only glasscoded can do; the runtime itself is shared
pub trait Host: Send + Sync {
    fn machine(&self) -> &MachineId;
    fn run_op(&self, op: &Op, caller: &Caller) -> OpReply;            // Window and SessionHost ops
    fn has(&self, host: OpHost) -> bool;
    fn notify(&self, title: &str, body: &str, matter: &str);
}
// AppHost (crates/glasscode) and DaemonHost (crates/glasscoded) implement it (M50).

// glasscode_core::profile
pub enum SessionOrigin { Person, Automation, Quarantine }
pub struct SessionProfile { pub claude_settings: serde_json::Value, pub claude_args: Vec<String>, pub codex_args: Vec<String>, pub env_set: Vec<(String, String)>, pub env_remove: Vec<String>, pub label: &'static str }
pub fn session_profile(origin: SessionOrigin, agent: AgentKind, policy: &ProjectPolicy, worktree: &Path, account_dir: Option<&Path>, cli: &CliVersion) -> SessionProfile;
pub enum HeadlessKind { BrainChat, BrainWake, AutoReview, Study }
pub fn headless_profile(kind: HeadlessKind, agent: AgentKind, mcp_config: &Path, cli: &CliVersion) -> SessionProfile;
```

## 9. Cross-cutting rules

**Performance.** These budgets are checked in CI with the reference load of slice plan section 14 (exec-19):
- No git, file or process work on the main thread. This extends the rule in `architecture.md`; the new stores do their IO on background threads.
- A tick asks for a draw only when something it shows changed.
- Budgets:

| Path | Budget |
|---|---|
| Journal append | ≤ 1 ms p95 |
| Hook event to journal | ≤ 300 ms p95 |
| `decide()` | ≤ 50 µs |
| Memory search on 10 MB | ≤ 30 ms p95 |
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
| The journal cannot be written (disk full) | the writer thread | A status bar warning; playbooks pause | Retries every 30 s; nothing acts without a journal | M14 (events-m4) |
| A brain is signed out, out of usage, or on an unsupported version | `runtime::health` | One notification in the CLI's own words; the chat explains | Wakes stop; answers from code keep working | M50 |
| A claim has no result after a crash | the effect ledger at start | Nothing when the probe settles it; else an "uncertain" card | The probe runs with no model | M31 |
| The clock jumps or the time zone changes | the HLC and the scheduler | One Activity line | Schedules re-plan from UTC; the order of events is kept | M41 |
| The server cannot be reached | sync heartbeat | "Manager: server unreachable" in the status bar | Under Automatic the Mac runs Degraded, with nothing outward; it may act alone after N minutes only if you turned that on | M58 |
| A hook is not reporting | the reconciler | "This session is not reporting" on the session | Transcript or rollout tail plus the reconciler | M14, M30 |

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
