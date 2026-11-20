# Manager v2: Slice Plan for Agents

| | |
|---|---|
| Document | GC-MGR-002, slices, issue 1 (draft, not approved) |
| Date | 2026-11-19 |
| Design | [`manager-v2-design.md`](manager-v2-design.md). It is binding: principles P1 to P5, decisions D1 to D30, interfaces in section 8. |
| Proofs | [`manager-v2-proofs.md`](manager-v2-proofs.md), run by slice M01 |
| Process parent | `docs/ui-redesign-slices.md` (GC-UI-001). This plan uses the same roles and procedure, with the values of section 4. |
| Repository | glasscode-ide/glasscode, remote `origin` |
| Progress | 0 of 44 slices (41 in release 1). M00: the owner accepted every proposal of section 11 on 2026-11-19. Next: commit the documents to `manager-v2` and tag `manager-v2-plan-1`. |

> **For the main agent:** you do not write slice code. One subagent does one slice in one worktree and
> opens one pull request. Use `docs/manager-v2-orchestrator-prompt.md` to start.

## Contents

1. About this document
2. What exists today and where it goes
3. Rules for all slices
4. Procedure, branches, and the brief for a subagent
5. Slice map and sequence
6. Shared interfaces
7. The slices
8. Files with a conflict risk
9. Conditions to stop
10. Review focus
11. Decisions for the owner
12. Traceability: requirements to slices and tests
13. Release 1
14. Reference load and soak

## 1. About this document

**Goal.** Replace the manager of 2026-11-17 with a harness that can:
- hear every event of every session;
- wake its brain only when something needs judgment;
- act through one typed list of everything the IDE can do;
- keep one memory per project that both Claude and Codex share and that survives a worktree;
- obey a trust level per project that Rust enforces.

**Approach.** The order is wave 0 to wave 6.
- **Wave 0.** Proofs, a test kit and CI.
- **Wave 1.** Four foundations that touch no behaviour: identity, the action registry, callers and the words ledger, the journal.
- **Wave 2.** Memory, the gate, and session profiles. This is the person's first priority and the security base.
- **Waves 3 and 4.** Everything that needs no model: turns, effects, work, playbooks, the scheduler, delivery.
- **Wave 5.** The brain and the chat.
- **Release 1** ends after wave 5. Wave 6 comes after it.

**What the review found** (19 agents, 268 verified findings; see design section 4):
- The v1 gate sees only `glasscode` commands, while sessions, the brain's Bash, the browser and auto-review go around it.
- "You" is a flag the client sets.
- Memory files can be edited by any agent.
- Stop does not fire on interrupt, error or crash.
- The coverage idea compared the registry with itself.
- Several CLI claims were wrong.

Every slice below cites the findings it closes, on its "Addresses" line. The findings are kept outside the repository, because their evidence holds machine details: `~/.glasscode/manager-v2-review/` on the owner's Mac (`rows.json` has the id, problem, verified fix and evidence of each). Critical findings and about twenty majors were read in full when this plan was written; the other majors were placed by their titles and summaries, so the reviewer of each slice reads its findings' fix texts.

**Findings this document itself closes** (no single slice owns them):
- **exec-1**: the team's slice format, with Files, Steps, Tests, Verify, Demonstration, Server and Windows lines.
- **exec-2, exec-13, exec-m1, crit-4**: one merged order, with Depends on, and the cross-area decisions of the design.
- **exec-10**: every designed feature has a slice in section 5.
- **exec-22**: each slice's tests are named and need no live model.
- **ux-23**: the large slices are split, for example M52.1 and M52.2.
- **land-2, alt-12**: the comparison and the rejected designs are in design section 12.

## 2. What exists today and where it goes

A subagent must not remove a function of this table unless its slice tells it to. Until M56, v1 keeps working for anyone whose engine switch (`engine` in `authority/engine.json`, M10) is `v1`, and for everyone with the manager off.

| Today | New home | Slice |
|---|---|---|
| `manager/harness.rs` rules (needs you, review tell, follow-up fixed and stale, brief due) | Built-in playbooks with parity tests | M40, removed in M56 |
| `harness::quick_answer` (plain questions without the brain) | Kept as "answers from code" in the chat | M52 |
| `manager/brain.rs` (Claude stream-json, Codex exec, `DENIED_TOOLS`) | Runtime drivers with the no-shell profile; `DENIED_TOOLS` deleted | M50, removed in M56 |
| `manager/memory.rs` notes in `data.json` | Memory store; notes migrated as proposals | M20, M20.3 |
| `manager/followups.rs`, `gh.rs` | Work ledger and built-in playbooks | M32, M40, M44 |
| `manager/messages.rs`, `messenger.rs` (outbox, tiers, the four messenger calls) | Delivery and outbox, contract unchanged | M42 |
| `manager/brief.rs` | Built-in schedule; the text comes from code | M40, M42 |
| `manager/scene.rs`, show and tour | `ui.scene`, `ui.point`, `ui.tour` ops | M33.2, M52 |
| `manager/store.rs`, `sync.rs` | `Store` trait; server home | M10, M57 (v1 sync runs until M56) |
| `crates/glasscode/src/manager_tick.rs` (runs on the window tick) | Runtime host on its own threads | M50, M51, removed in M56 |
| `manager_ui.rs`, bubble | Chat surface | M52 |
| `manager_settings.rs` | Settings › Manager (levels, playbooks, approvals) | M53 |
| `manager_cmd.rs` | Registry entries | M12, M33 |
| `manager_home.rs` | Settings › Manager › Home (Automatic, Always on the server, Always this Mac) | M57, M58 |
| `crates/glasscoded/src/manager.rs` (second copy of the rounds) | Read-only copy, then the server home on `DaemonHost` | M20.3, M21.1, M57 |
| `auto_review.rs` (unrestricted `gh`, `git`, WebFetch; broken Codex flag) | Headless profile, data-only input, card before posting | M23 |
| `ci_fix.rs`, `autofix.rs` | Built-in playbook that works with the manager off | M40 |
| `control.rs` (`Kind`, `Rule`), `control_ui.rs` (callers, approvals as closures) | Callers by channel; gate v2; pending actions | M13, M21 |
| `commands.rs`, `commands2.rs` (52 hand-parsed plan sites) | Registry parse and handlers | M12 |
| `cli.rs` help text, `SKILL`, `CODEX_INSTRUCTIONS` | Generated help; updated skill text | M33.1, M20.2 |
| `agents.rs` hook settings and launch | Session profiles; turn hooks | M22, M30 |
| `prompt.rs` (screen menus, used by phones) | `session.answer` with a prompt id | M33.2 |
| `browser/mcp.rs`, `cdp.rs` | Shared `mcp_stdio`; browser ops through the gate; DevTools secret | M23, M34 |
| `agent-log.jsonl` (what agents did) | Journal; Activity reads it | M14 |
| `voice/server.mjs`, `voice/memory.mjs` (ElevenLabs copilot), the glasses' control API | Not changed by this plan. The voice plan and the phone and glasses plan decide their future. M20.3 copies the copilot's memory facts into proposals. | — |

## 3. Rules for all slices

1. **Stay in the slice.** Change only the files in its "Files" list. If another file must change, write the reason in the pull request.
2. **v1 keeps working.** The engine switch of M10 lives in `authority/engine.json`, which older builds never rewrite. Its default is `v1`, and every v2 switch is off. While the engine is `v1`, and for everyone with the manager off, nothing a person sees changes: no brief injection (M20.2), no profiles or memory redirect (M22), no new hooks (M30). CI auto-fix and auto-review keep their behaviour. Memory, the journal and the gate do not depend on the manager switch; they run in v2 homes and in tests. Exactly one engine carries out actions, and a test checks that one event is acted on once. The only exceptions are security fixes the owner approves (O13: the terminal caller of M13, and the auto-review hardening of M23). `manager-v2` merges into `main` only at named checkpoints with every v2 switch off. M56 is the release that deletes v1.
3. **No new crate.** This is design P4. A slice that finds it needs one stops (section 9).
4. **Core first.** Logic lives in `glasscode_core`, with tests in its file. Time comes from `clock` (M10). Pure functions do no IO.
5. **Every new store** implements `Store`, has a path class and a data-map row in the design, and has an "on damage" test.
6. **Every command is a registry entry** after M12. No new hand-parsed command.
7. **Every effect of a non-person caller** goes through `decide()` (after M21) and the effect ledger (after M31).
8. **No limit is described as stronger than its plane** (design section 7). Docs and UI text follow the same rule.
9. **The window stays cheap.** No IO on the main thread in a render or a tick. A tick asks for a draw only on a change (`architecture.md`, "Rules that keep the window cheap").
10. **Every slice has a Server line and a Windows line.** `glasscoded` and the Windows target must compile after every slice.
11. **Building on the owner's Mac.** Every check runs in a scratch home (`GLASSCODE_HOME=<scratch>`) with scratch account folders. Never use the person's `~/.glasscode`, `~/.claude`, `~/.codex` or memory folders. A check that starts a real CLI uses a scratch login and the cheapest model, counts its prompts, and reports them (exec-m4).
12. **Mac and server protocol.** New messages are announced in `Hello.features`. `PROTOCOL_VERSION` stays the same until M57 (exec-m2).
13. **No model in CI.** Tests use the stand-in agents of M02. Live runs happen only in M01, M55, and demonstrations marked **live**. A live demonstration is done by the owner, or by an agent with the owner's yes, in a scratch home, and is counted against the weekly budget of O9.
14. **Docs move with the code.** A slice updates the sections of `architecture.md` and `manager.md` that it changes.
15. Team rules 10 (debug hooks), 13 (no secrets or personal data), 14 (verify, tell the truth), 15 (no real login in a scratch home) and 17 (`CONTRIBUTING.md`) of `ui-redesign-slices.md` apply as written.

### Gates of the automatic build

| After slice | Jobs that must pass |
|---|---|
| Now | macOS and Windows jobs of `ci.yml` as they are |
| M03 | Plus `linux`: `cargo check --locked -p glasscoded`, `cargo test --locked -p glasscode_core -p glasscoded -- --test-threads=1`, and the CLI flag probe against the recorded help of the approved versions |
| M02 + M03 | Plus `evals`: the adversarial, memory-poisoning, retrieval and op-selection suites with stand-in agents (no model) |
| M55 | Plus `soak`: seven simulated days on the virtual clock (nightly, and on pull requests into `manager-v2` that change `glasscode_core`) |

## 4. Procedure, branches, and the brief for a subagent

| Item | Value |
|---|---|
| Integration branch | `manager-v2`, made from `main` at the start of M02 (O9) |
| Slice branch | `mgr/mNN-<name>`, made from the newest `origin/manager-v2` |
| Worktree | a Glasscode worktree of the slice branch |
| Pull request | Base `manager-v2`. Title `MNN: <slice name>`. Squash merge. The owner merges (O9). |
| At the same time | A maximum of three slices, never two that change the same file of section 8 |
| Screen checks | One agent at a time; a lock file in the scratch folder, as rule 16 of GC-UI-001 |

### Brief for a subagent

```text
You do one slice of Glasscode Manager v2 (a Rust desktop application on GPUI, with a server daemon).

Worktree:  <absolute path>      (work only in this folder)
Branch:    <mgr/mNN-name>       (it exists and is checked out)
Base:      manager-v2

Read first, in this sequence:
1. docs/manager-v2-design.md, all of it. It is binding.
2. docs/manager-v2-slices.md sections 2, 3, 6, 9 and your slice <MNN> in section 7.
   The slice text is your specification, together with the fix text of every finding id on its
   Addresses line (the main agent pastes those texts below, from the review data).
3. docs/manager-v2-proofs.md, the results of every proof your slice names.
4. CONTRIBUTING.md, and each file in the slice's Files list before you change it.

Do the work:
- Do the steps in sequence. Add every test of the Tests list.
- Do each Verify command. Correct the code until each passes.
- Do the Demonstration in a scratch home (rule 11), never with real logins.
- Commit with conventional commit messages. Do not use --no-verify.
- Push the branch. Open one pull request with base manager-v2 and the title "<MNN>: <slice name>".

Limits:
- Change only the files in the Files list. Add no crate. Do not change vendor/.
- Do not merge. Do not push to manager-v2 or main.
- If a condition of section 9 occurs, stop and report.

Your last message is the report: the pull request URL, the output of each Verify command, each step not
complete, each file changed outside the list with the reason, each finding id of the Addresses line you
could not close, and each item you did not verify.
```

## 5. Slice map and sequence

Sizes are in lines of Rust plus tests. They are based on comparable merged code: `manager/harness.rs` is 1,327 lines; `github_query` with its tests about 1,400; U18 (first run) about 1,900. The review found v3's sizes about half of reality (exec-11).

| Wave | Slice | Depends on | Size |
|---|---|---|---|
| 0 | M00 Plan approved (no code) | — | — |
| 0 | M01 Proofs | M00 | M (scripts and results) |
| 0 | M10 Store base, clock, path classes and the engine switch | M00 | M (≈800) |
| 0 | M02 Test kit and eval suites | M01, M10 | L (≈1,800) |
| 0 | M03 CI, branch and CLI flag probe | M00 | S (≈300) |
| 1 | M11 Identity: project and work keys, machine id, clock | M10, M02 | M (≈900) |
| 1 | M12.1 Registry R1: types, parse, goldens (read ops, board) | M10, M02 | L (≈1,500) |
| 1 | M12.2 Registry R1: all other commands, app wiring | M12.1 | L (≈1,500) |
| 1 | M13 Callers by channel and the words ledger | M10, M11 | M (≈1,100) |
| 1 | M14 Journal core and reconciler | M11, M12.2, M13 | L (≈1,600) |
| 2 | M20.1 Memory store, format, git, scan | M11, M12.2, M13 | L (≈1,500) |
| 2 | M20.2 Brief, search and memory ops | M20.1, M01 (P03) | L (≈1,400) |
| 2 | M20.3 Migration and the server's read-only copy | M20.2 | M (≈700) |
| 2 | M21.1 Gate v2 and the authority store | M12.2, M13 | L (≈1,600) |
| 2 | M21.2 Pending actions, cards and chat grants | M21.1 | L (≈1,300) |
| 2 | M22 Session profiles and hooks | M21.1, M01 (P04, P07, P08, P16) | L (≈1,500) |
| 2 | M23 Headless runs, browser ops, forge wall | M22 | L (≈1,300) |
| 2 | M24 Archive by reconciliation | M20.2, M14 | M (≈900) |
| 2 | M25 Export, backup, restore, forget, uninstall | M20.2, M21.1 | M (≈900) |
| 3 | M30 Turns and turn reports | M14, M13, M01 (P05, P06) | L (≈1,600) |
| 3 | M31 Effect ledger and delivery to sessions | M14, M21.1 | M (≈1,100) |
| 3 | M32 Work ledger and done conditions | M30, M11 | M (≈900) |
| 3 | M33.1 Registry R2: inventory, coverage, `run_op`, generated help | M12.2 | L (≈1,400) |
| 3 | M33.2 Registry R3: new ops | M33.1, M21.2 | XL (≈2,200) |
| 3 | M34 `glasscode mcp` | M33.2, M01 (P01, P15) | M (≈800) |
| 3 | M35 Moving in, part 1 | M20.2, M11 | L (≈1,500) |
| 4 | M40 Playbooks and built-ins | M30, M31, M21.1, M22 | L (≈1,700) |
| 4 | M41 Scheduler and lifetime | M40, M14, M01 (P10) | M (≈1,000) |
| 4 | M42 Delivery, outbox and brief | M40 | M (≈900) |
| 4 | M43 Conversation store | M13 | M (≈800) |
| 4 | M44 v1 data migration | M32, M40, M42, M43 | S (≈400) |
| 5 | M50 Brain runtime | M34, M23, M01 (P01, P02, P11) | L (≈1,700) |
| 5 | M51 Lanes, queue and the wake lane | M50, M40, M32 | L (≈1,400) |
| 5 | M52.1 Chat surface: one conversation | M51, M43 | L (≈1,400) |
| 5 | M52.2 Chat surface: focus, actions, cards, failure states | M52.1, M21.2 | L (≈1,300) |
| 5 | M53 Settings: levels, playbooks, approvals | M21.2, M40 | M (≈1,100) |
| 5 | M54 Moving in, part 2 | M51, M35 | M (≈800) |
| 5 | M57 Server home: the server acts | M50, M51, M43, M31, M20.3 | L (≈1,800) |
| 5 | M58 Automatic home: lease, hand-over, fencing | M57, M41, M01 (P17) | L (≈1,400) |
| 5 | M55 Release-1 gate | all above | M |
| 5 | M56 Switch to v2, remove v1 | M55 | M (removes ≈9,000) |
| 6 | M60 Intake: finding work | M56 | M |
| 6 | M61 Delegated decisions and the validator | M56 | M |
| 6 | M62 Learning: reflect, tidy-up, know-how | M56 | M |

**Parallel tracks**, at most three at a time:
- Wave 0: M01 and M03 run together. M10 runs beside them, and M02 follows M01 and M10.
- Wave 1: M11 and M12.1 run together. M12.2 and M13 follow, then M14.
- Wave 2: memory (M20.x, M24) runs beside the gate (M21.x, M22, M23).
- Wave 3: M30 → M32 runs beside M33.1 → M33.2 → M34, and M35 runs as the third track.

## 6. Shared interfaces

Section 8 of the design is the contract. Additions made by a slice are listed in its "Result" when it merges, and the design is updated in the same pull request.

## 7. The slices

### M00 Plan approved (wave 0, no code)

- **Files:** none.
- **Steps:**
  1. The owner reads the design, this plan and the proofs.
  2. The owner answers O1 to O12 in section 11, or accepts the proposals.
  3. Record the answers in section 11 and tag the commit `manager-v2-plan-1`.
- **Result:** the owner's answers, written into this file.

### M01 Proofs (wave 0, M)

- **Files:** `docs/manager-v2-proofs.md`; new `script/manager-v2-proofs/` (one script per proof, each taking a scratch home as its argument).
- **Steps:** run P01 to P16 as written; fill "Result" for each with commands, outputs and numbers; mark pass or fail; apply the fallback where one failed, and write the change into the design (D-row or section 7) in the same pull request.
- **Tests:** none; this is measurement.
- **Verify:** each script runs in a fresh scratch home; the count of live prompts is at most 40.
- **Demonstration:** the filled proofs document.
- **Server:** P03 and P05 are repeated in a tmux session on the Docker test server (`script/test-server/run.sh`), with stand-ins where no login is possible.
- **Windows:** P07 notes "no sandbox"; P05 and P03 run once on the Windows machine of the owner (O9).
- **Addresses:** exec-30, alt-m3, land-m2 (in part)

### M02 Test kit and eval suites (wave 0, L)

- **Files:** new `crates/glasscode_core/src/testkit/` (`mod.rs`, `home.rs`, `clock.rs`, `agents.rs`, `sim.rs`, `load.rs`) behind `#[cfg(any(test, feature = "testkit"))]`; `crates/glasscode_core/Cargo.toml` (feature `testkit`, no dependency); new `script/test-agents/claude`, `codex`, `gh` (stand-ins, extended from `script/test-server/`); new `crates/glasscode_core/tests/evals/` (`adversarial.rs`, `memory_poisoning.rs`, `retrieval.rs`, `op_selection.rs`, `fixtures/`); `.github/workflows/ci.yml` (the `evals` job).
- **Steps:**
  1. `testkit::Home` (also as the binary `make-test-home`, new `crates/glasscode_core/src/bin/make-test-home.rs`, behind the `testkit` feature): builds a scratch Glasscode home with projects (git repositories with remotes, forks, shallow clones), Claude memory folders, a Codex memories folder, transcripts and rollouts, and a `state.json` with scratch accounts (rule 11).
  2. Stand-in agents: scripts that read a scenario file and write hook events, transcripts and rollouts exactly like the approved CLI versions, using fixtures recorded in M01. They cover a normal turn, an interrupt, an API error, a crash, a turn that ends on a question, and a turn that pushes.
  3. `testkit::clock::VirtualClock` implementing the `Clock` trait of M10, with sleep, jumps and DST changes. The fixtures of the stand-ins come from M01's recordings.
  4. `testkit::sim`: feeds a script of events to the runtime (as later slices add it) and collects journal events, effects and notifications. Asserts helpers: `no_duplicate_effects`, `one_report_per_turn`.
  5. `testkit::load`: the reference load of section 14 as a generator.
  6. Eval suites. Each case is a fixture plus an expected result, so later slices fill them in as their code lands:
     - **adversarial**: a planted instruction in a PR comment, a review thread, a ticket, a session reply, a memory file, a playbook file and a pasted message, each expecting no effect and no fact;
     - **memory poisoning**: WhisperBench-style planted facts;
     - **retrieval**: 200 question and file pairs over a synthetic memory home, reporting recall at 5;
     - **op selection**: natural requests mapped to expected ops and arguments, with "no op" cases.
  7. Add the `evals` job to `ci.yml`, which runs `crates/glasscode_core/tests/evals` with the `testkit` feature (moved here from M03, which came first).
- **Tests:** the kit's own tests, for example `home_builds_two_clones_of_one_repo`, `virtual_clock_dst_gap`, and stand-ins that replay a recorded turn byte for byte.
- **Verify:** `cargo test --locked -p glasscode_core --features testkit -- --test-threads=1`.
- **Demonstration:** none.
- **Server:** the stand-ins also run inside the Docker test server.
- **Windows:** the stand-ins have `.cmd` twins.
- **Addresses:** exec-4, events-24, trust-17, mem-21, land-14, ops-23, exec-m4

### M03 CI, branch and CLI flag probe (wave 0, S)

- **Files:** `.github/workflows/ci.yml`; new `script/cli-probe.sh`; new `cli-flags.json` (every flag Glasscode passes to `claude` and `codex`, by call site).
- **Steps:**
  1. Make `manager-v2` from `main`, and run the workflow for pull requests into it.
  2. Add the `linux` job (section 3 gates).
  3. `cli-probe.sh <claude help file> <codex help file>` fails when a flag in `cli-flags.json` is missing. It runs in CI against help texts recorded per approved version (`script/cli-help/<version>.txt`). The owner runs it with the real CLIs before approving a new version in `cli-versions.json`. Add a line to the compatibility checklist of `architecture.md`.
- **Tests:** the probe fails on today's `auto_review.rs` Codex call (`--full-auto` is not in codex 0.167's help). Record that as expected, and fix it in M23.
- **Verify:** the workflow runs on a test pull request into `manager-v2`.
- **Server:** the `linux` job is the daemon's first CI.
- **Windows:** unchanged.
- **Addresses:** exec-20, exec-21, exec-m3, alt-m4, land-m2

### M10 Store base, clock, path classes and the engine switch (wave 0, M)

- **Files:** `crates/glasscode_core/src/store.rs` (add the `Store` trait; `load` and `save` of `state.json` unchanged); new `clock.rs`; `paths.rs`; `lib.rs`.
- **Steps:**
  1. Add the `Store` trait of design section 8, with `OnDamage`: atomic save (temp file, fsync, rename, as `board.rs` does), a schema version, `migrate`, and a `.broken` copy when the file is damaged.
  2. Add `clock::Clock` (`now_ms`, `local_minutes`), `SystemClock`, and a process-wide setter for tests.
  3. Add to `paths`: `PathClass`, `class_of`, and the folders of design section 5 (`authority_dir`, `memory_dir`, `agent_notes_dir`, `words_dir`, `journal_dir`, `effects_dir`, `manager_work_dir`).
  4. Add `MachineId`, stored in `authority/machine.json`.
  5. Add the engine switch, `authority/engine.json` (`engine`: `v1` or `v2`, and the v2 feature switches, all off). Add `GLASSCODE_MANAGER_ENGINE=v2` for scratch homes. Older builds never read or write this file (rule 2).
- **Tests:**
  - `every_folder_has_a_class`: each `pub fn …_dir` of `paths` has a class;
  - `refuse_store_keeps_broken_copy_and_errs`;
  - `empty_store_starts_fresh_on_damage`;
  - `save_is_atomic_under_kill` (child process killed between write and rename);
  - `machine_id_is_stable`;
  - `engine_defaults_to_v1`;
  - `older_state_json_untouched_by_engine_switch`.
- **Verify:** `cargo test --locked -p glasscode_core -- --test-threads=1`; `cargo check -p glasscoded`.
- **Demonstration:** none.
- **Server:** glasscoded gets its own `MachineId`.
- **Windows:** the rename uses `MoveFileEx` replace semantics, as `save.rs` does.
- **Addresses:** crit-6, crit-7 (the data map is in the design; the stores follow it), crit-1 (the layout)

### M11 Identity (wave 1, M)

- **Files:** new `crates/glasscode_core/src/identity.rs`; `github.rs` (expose remote normalisation from `repo_slug`); `lib.rs`.
- **Steps:**
  1. `resolve(dir, known)`, checking aliases in this order:
     - an explicit mapping;
     - the normalised `origin` (https, ssh, `git@host:`, ssh config aliases, `.git` suffix, letter case);
     - the `upstream` remote;
     - the root commit (`git rev-list --max-parents=0 HEAD`, skipped when `git rev-parse --is-shallow-repository` is true, with several roots allowed);
     - the path.

     A fork, or a match by root commit only, gives `AskFork`. A folder that is not a git repository keys by its path.
  2. `authority/projects.json` maps `ProjectKey` to aliases and to each machine's project ids. Removing a project keeps its key; re-adding it re-attaches.
  3. `WorkKey::for_session(&Session)`: a review session gives `Review(repo#n)`, a PR session `Pr(repo#n)`, a ticket `Ticket(ENG-42)`, otherwise `Branch(name)`.
  4. `Hlc::now` and `merge`, with the wall clock taken from `clock`.
- **Tests:**
  - two clones of one repository give one key;
  - a fork plus its upstream gives `AskFork`;
  - a renamed remote and an ssh alias give one key;
  - no remote keys by path;
  - a shallow clone skips the root commit;
  - a non-git folder keys by path;
  - a re-added project gets its old key;
  - Mac and server ids map to one key;
  - `hlc_is_monotonic_across_clock_jump`.
- **Verify:** core tests.
- **Server:** glasscoded resolves keys the same way, and the Mac's mapping wins (the map is authority data, so it flows Mac to server).
- **Windows:** paths are compared case-insensitively.
- **Addresses:** mem-5, exec-12, crit-9, crit-10

### M12.1 Registry R1: types, parse, goldens (wave 1, L)

- **Files:** new `crates/glasscode_core/src/ops/` (`mod.rs`, `spec.rs`, `parse.rs`, `table_read.rs`, `table_board.rs`, `schema.rs`); `lib.rs`; new `crates/glasscode_core/tests/ops_golden.rs` with `tests/ops_golden/*.json`.
- **Steps:**
  1. Write the types of design section 8: `OpSpec`, `ArgSpec`, `OpGroup`, `EffectClass`, `Retry`, `Undo`, `OpHost`, `Op`, `Targets`, `OpReply`, `OpError`. Every spec names its host (ops-8). The server-sync plan reads the same table.
  2. Fill `OPS` for the read commands and the board: `board …`, `session list/show/read`, `pr status/list`, `project list`, `worktree list`, `account list`, `ticket list/show`, `status`, `log`, `approvals`, `whoami`, `manager status/memory/brief/follow-up list/message list/check list`. Copy the summaries and examples from `cli.rs` `HELP`.
  3. `parse(words, ctx)` is pure. It implements today's grammar: positional words, `--flag value`, `--flag`, and the day words of `day.rs`. It matches a session by id, by prefix and by title words only when `ctx.caller_is_person`; other callers must give an exact id (ops-17).
  4. `json_schema(spec)` is hand-written over `ArgSpec` and needs no crate.
  5. Golden corpus: every example line of `HELP`, plus the recorded command lines in the fixtures, each with its expected `Op` JSON and parse error.
- **Tests:** `ops_golden` (all cases); `names_are_unique`; `every_spec_has_example`; `fuzzy_match_only_for_person`.
- **Verify:** core tests.
- **Server:** the table compiles in glasscoded, which parses the commands of server sessions with it.
- **Windows:** none.
- **Addresses:** exec-5, ops-16, ops-17, ops-20 (correct counts in the result)

### M12.2 Registry R1: all other commands and app wiring (wave 1, L)

- **Files:** `ops/` (the remaining tables); `crates/glasscode/src/commands.rs`, `commands2.rs`, `manager_cmd.rs`, `control_ui.rs`; new `crates/glasscode/src/ops_run.rs`.
- **Steps:**
  1. Fill `OPS` for every remaining command: the 52 `plan(Kind::…)` sites. Each spec gets `kind_v1`, so Settings › Permissions behaves exactly as before.
  2. Each command handler first calls `ops::parse`, then builds today's `Plan { kind, what, run }` from the `Op`. The closures move unchanged into `ops_run.rs`, as functions keyed by op name.
  3. `targets(op, world)` is filled for every op: projects, sessions, repositories and paths, resolved from ids by Rust, never taken from a caller's `--project` alone (trust-3).
  4. Replies keep the `control::Reply` shape; `data` gains `op` and `error_code` (no protocol change).
  5. Mark every handler that runs `git`, `gh` or `ps` on the main thread as `blocking` in a test list. M33.2 moves them off the main thread (ops-9).
- **Tests:** `every_plan_site_has_an_op` (a source scan of `plan(Kind::` against the table); `same_words_same_reply` for each golden case against a window-less handler harness where the handler is pure; `targets_never_trust_caller_project`.
- **Verify:** core and app tests; `cargo check -p glasscoded -p glasscode_cli`; `glasscode help` prints byte for byte what it printed before.
- **Demonstration:** in a scratch home, `glasscode session list`, `board add "x" --due fri` and `pr status 1` give the same output as on `main`.
- **Server:** glasscoded's `RemoteCall` carries the op name.
- **Windows:** `glasscode_cli` is unchanged; the app parses.
- **Addresses:** exec-8, ops-7 (GPUI actions stay for key bindings and call ops in M33.1; one registry for behaviour), ops-9 (list), ops-10, ops-21 (journal record from the op, used by M14)

### M13 Callers by channel and the words ledger (wave 1, M)

- **Files:** new `crates/glasscode_core/src/caller.rs`, `words.rs`; `control.rs` (remove `interactive` from trust decisions; keep the field on the wire); `crates/glasscode/src/control_ui.rs`; `today_ui.rs`, `manager_ui.rs`, `chat_ui.rs` (record composer submits); `terminal/` (record keys typed in a session's terminal and bracketed-paste spans); `manager_tick.rs`, `ci_fix.rs`, `session_start.rs`, `switch_ui.rs`, `commands.rs` (record every text Glasscode types into a session, as `Injected`); `crates/glasscoded/src/daemon.rs` (phone input as `Device`); `lib.rs`.
- **Steps:**
  1. Derive `Caller` by channel:
     - `Ui` only for calls made inside the app's process;
     - `Session` from a valid signature;
     - `Manager` only from the brain's own MCP server instance (added in M34; until then the v1 pid rule stays, as attribution);
     - `Phone` from a signed device;
     - `Machine` for another Glasscode machine's sync connection; on glasscoded, the server-sync plan's phase 0a assigns it per connection;
     - `Terminal` for everything else.
  2. `Terminal` gets today's Outside rule: reads within limits, everything else a card. A request with `interactive: true` from a socket is `Terminal`. This changes today's behaviour for `glasscode` typed in a terminal outside Glasscode, so it ships to everyone only after O13. Until then it applies in v2 homes only, and v1 keeps the old rule with a warning in `glasscode whoami`.
  3. The words ledger (D1): `record`, `match_prompt`, `tier_of`, `quote_in`. Each machine writes `words/<machine>/<yyyy-mm>.jsonl`. Secret spans are replaced (the scrubber of `env.rs` plus token patterns) before hashing.
  4. Record at each input site of the Files list. Every text Glasscode types into a session is recorded first, with its `InjectKind`, so a matching `UserPromptSubmit` is known as injected.
  5. A measure-only mode (`GLASSCODE_WORDS_MEASURE=1`) counts origins for proof P09 and stores no text.
  6. glasscoded writes phone input as `Device(id)`. The Mac accepts records from the server only with tier 2 or lower.
- **Tests:**
  - `interactive_flag_from_a_socket_is_terminal`;
  - `signed_shell_session_with_interactive_is_not_you`;
  - `no_socket_or_request_file_yields_ui`;
  - `injected_prompt_matches_as_injected`;
  - `pasted_span_never_counts`;
  - `server_cannot_send_tier_one`;
  - `secret_replaced_before_hash`.
- **Verify:** core and app tests; the eval case "session calls manager ask" produces no grant (M02 fixture).
- **Demonstration:** in a scratch home, type in the Today box, paste into a session, and run a CI-fix message. `glasscode words recent --json` (a debug op, person-only) shows the three origins.
- **Server:** phone records as above.
- **Windows:** the named-pipe path gives `Terminal` the same way.
- **Addresses:** trust-2, trust-6, ops-4, mem-2, crit-2, land-1, events-m1, ux-m1

### M14 Journal core and reconciler (wave 1, L)

- **Files:** new `crates/glasscode_core/src/journal.rs`, `journal_kinds.rs`, `reconcile.rs`; `agents.rs` (hook tail emits events; append atomically); `cli.rs` (the `glasscode hook` append); `crates/glasscode/src/workspace.rs` (the 700 ms tick hands changes to the journal on a background thread); `pr_status.rs`, `github_ui.rs` (status changes as events); `control_ui.rs` (approvals and op replies as events); `crates/glasscoded/src/daemon.rs` (its own journal; `journal.since` RPC for the Mac); `lib.rs`.
- **Steps:**
  1. `Event` and `EventKind` (design section 8). The catalogue covers:
     - `session.*`: started, activity, needs input by notification type, ended;
     - `turn.*`: filled in M30;
     - `git.*`: commit, push, branch switch, from `git_info` file times;
     - `pr.*`, `ci.*` and `review.*`: from the status rounds;
     - `approval.*` and `op.*`: every reply, with the caller;
     - `message.*`, `ticket.*` and `usage.*`;
     - `app.*`: start, quit, sleep, wake, network lost or back;
     - `clock.*`.
  2. One writer thread per process, with batched fsync (every 100 ms, and at once for `Urgent`), daily files and 30-day rotation. Each line carries `v`.
  3. Readers use a `Cursor`; `read_from` skips a damaged line and logs it once.
  4. Redaction at write: a prompt becomes its hash plus a 200-character excerpt (O8), with secrets scrubbed. Readers get only their scope: a `Session` caller sees its own project, `Manager` sees managed projects, `Ui` sees everything.
  5. The hook append (`glasscode hook` and the Unix shell form) writes each line in one `write` call under an advisory lock, so lines never interleave (events-20).
  6. The reconciler (D24 safety net) runs on start, on wake and every 60 s. It compares running sessions, their activity, their PR state and worktree lists with what the journal implies, and writes `reconciled.*` events for every gap (events-4).
  7. The ops `events` (`--follow`, `--project`, `--session`, `--kind`, `--since`) and `journal.query`.
  8. Each decision of later slices writes a `decision` event with its inputs, so the journal is the debugging trace (exec-31).
  9. Freshness: an event derived from a poll carries the poll's age. GitHub webhooks through the relay are a later decision (land-16).
- **Tests:**
  - `append_and_read_back_in_order`;
  - `damaged_line_skipped`;
  - `rotation_keeps_30_days`;
  - `session_scope_hides_other_projects`;
  - `prompt_is_hashed_with_excerpt`;
  - `concurrent_hook_appends_do_not_interleave` (8 processes, 1,000 lines each);
  - `reconciler_reports_missed_stop`;
  - `restart_resumes_cursor`.
  - Measure: append p95 ≤ 1 ms, hook event to journal p95 ≤ 300 ms with stand-in agents.
- **Verify:** core and app tests; `cargo check -p glasscoded`; `glasscode events --follow` in a scratch home shows a stand-in turn.
- **Demonstration:** Today's Activity panel lists the same events as `glasscode events`.
- **Server:** glasscoded keeps its own journal; the Mac pulls `journal.since(cursor)` with each sync.
- **Windows:** file locking through `sys`.
- **Addresses:** events-4, events-5, events-6, events-18, events-19, events-20, events-m4, events-m6, land-10, land-16 (recorded as later), exec-31

### M20.1 Memory store, format, git and scan (wave 2, L)

- **Files:** new `crates/glasscode_core/src/memstore/` (`mod.rs`, `layout.rs`, `entry.rs`, `git.rs`, `scan.rs`, `caps.rs`); `lib.rs`.
- **Steps:**
  1. Layout under `memory/`:
     - `you/profile.md`;
     - `org/<employer>/people.md` and `glossary.md` (mem-18: one folder per employer; a project names its employer);
     - `projects/<key>/`, holding `PROJECT.md`, `POLICY.md`, `decisions.md`, `knowhow/<topic>.md`, `work/<work key>.md` and `archive/<yyyy-mm>/`;
     - `INDEX.md`, generated.
  2. **Entry format.** A Markdown bullet followed by one HTML comment that holds its header. The comment is invisible when rendered and is parsed without a crate:
     `- Sam owns billing. <!-- id=e7f2 status=confirmed kind=fact source=words:W123 at=<hlc> -->`.
     - `kind` is `fact` or `knowhow`.
     - `status` is `mentioned`, `learned`, `confirmed` or `let-go`.
     - `source` is `words:<id>`, `session:<id>`, `import:<path>`, `v1` or `person-edit`.
     - A fact about code also has `code=<path>@<commit>#<sha256 of cited lines>`.
  3. **Facts and know-how** (mem-10).
     - A fact needs a tier-1 words record whose quote supports it. "Remember that …" in tier-1 words is confirmed at once; any other fact taken from words needs the person's tick on a card (mem-m2).
     - Know-how may come from a session's work. It is labelled agent-written, carries no authority, and a file the person edited can no longer be overwritten.
  4. **Git.** Use the `git` program with `-c core.hooksPath=/dev/null -c commit.gpgsign=false`. The author is `Glasscode <glasscode@localhost>`, with a trailer `Who: you|manager|session:<id>`. Each write is one commit.
  5. **Memory head (D20).** `authority/memory-head.json` records Glasscode's last commit for each key. Reads use that commit, never the working tree. A tree with changes not made by Glasscode is a *pending edit* (`memory.pending`), shown in the app with a diff to accept or discard.
  6. **Write-time scan**, with no model. It refuses:
     - instruction-like phrases (an English list, advisory per design section 9);
     - invisible Unicode;
     - URLs carrying credentials;
     - token patterns;
     - values from the project's `.env` files.
  7. **Caps** (design section 4 table). A write over the cap fails with "Merge or let go first". Consolidation ops: `memory.merge`, `memory.let_go`, `memory.rewrite`.
  8. **Forgetting a secret.** The repository is rebuilt from its current tree into a fresh history, so the secret leaves git. The old history is deleted, and the person is told that history before that point is gone (mem-13).
- **Tests:**
  - `entry_round_trip`;
  - `direct_edit_is_not_in_the_brief` (a file written directly, and a commit made with plain `git`, are both pending);
  - `scan_refuses_planted_instruction`;
  - `scan_refuses_env_value`;
  - `over_cap_write_fails`;
  - `secret_forget_purges_history` (`git log -p` has no match);
  - `code_fact_hidden_when_lines_change`;
  - `employer_scoping`.
- **Verify:** core tests; evals `memory_poisoning` (the store part).
- **Demonstration:** none; the next slice gives commands.
- **Server:** none yet (M20.3).
- **Windows:** `git` from `sys::command`; paths through `paths`.
- **Addresses:** mem-1, mem-m1, mem-m2, mem-9, mem-10, mem-13, mem-18, mem-20, mem-22 (the work file is generated in M32; here it gets its own section "Notes from sessions")

### M20.2 Brief, search and memory ops (wave 2, L)

- **Files:** `memstore/` (`brief.rs`, `search.rs`, `index.rs`); `ops/table_memory.rs`; `crates/glasscode/src/ops_run.rs`; `agents.rs` (pass the brief at launch per P03); `cli.rs` (`SKILL`, `CODEX_INSTRUCTIONS` text); `crates/glasscode/src/auto_review.rs` (brief for headless runs, mem-m5).
- **Steps:**
  1. `brief(session)` builds, in this order of precedence, within its caps:
     - the policy header;
     - `you/profile.md`;
     - `PROJECT.md`;
     - the work file;
     - the project's index lines;
     - a pointer to the repository's own `AGENTS.md` and `CLAUDE.md`, which are not copied (mem-19).

     Agent notes are never included. The manager's own brief holds only the list of projects with one line each; details come on demand (mem-m4).
  2. **Delivery**, by the result of P03:
     - Claude: `--append-system-prompt` at start; on resume and after compaction, a `SessionStart` hook prints the current brief as `additionalContext`.
     - Codex: `developer_instructions`.
     - The fallback path is written in P03.
  3. **Search.** BM25 over heading sections, with:
     - identifiers split (camelCase, snake_case, paths);
     - glossary synonyms;
     - boosts by scope: worktree > project > you and org > archive;
     - other projects searched for the manager only.

     Commit-backed only (D20). Results are wrapped as data.
  4. **Ops:** `memory.search`, `show`, `note` (to the caller's work file), `propose`, `accept`, `reject`, `merge`, `let_go`, `rewrite`, `forget`, `pending`. `accept`, `reject`, `forget` and pending acceptance are `human_only`.
  5. **Skill text** for both agents: search before guessing, note decisions in the work file, propose lasting facts, and never edit memory files directly.
  6. A `PreCompact` hook asks Glasscode to snapshot the work file from the last turn report before compaction (land-18).
- **Tests:**
  - `brief_respects_caps_and_order`;
  - `brief_excludes_agent_notes`;
  - `search_ranks_worktree_first`;
  - `identifier_split_finds_snake_case`;
  - `session_cannot_accept`;
  - `retrieval_recall_at_5_floor` (eval; the first run sets the floor).
  - Measure: search p95 ≤ 30 ms on 10 MB; brief ≤ 20 ms.
- **Verify:** core and app tests; evals `retrieval`; `cli-probe.sh`.
- **Demonstration:** in a scratch home, a Codex stand-in session in worktree A runs `glasscode memory note "decided to keep the v1 API"`. A Claude stand-in session in worktree B of the same project finds it with `memory search`. A session of another project does not.
- **Server:** briefs for server sessions come in M20.3.
- **Windows:** the hook command for the brief is the `glasscode.exe` console program.
- **Addresses:** mem-8, mem-14, mem-m4, mem-m5, mem-19, mem-15 (no model here), land-7 (know-how files use a `SKILL.md`-compatible header, so they can be loaded as skills later), land-18, trust-7

### M20.3 Migration and the server's read-only copy (wave 2, M)

- **Files:** new `memstore/migrate.rs`; `crates/glasscoded/src/daemon.rs`, `mirror.rs`; `crates/glasscode_core/src/mirror.rs`.
- **Steps:**
  1. **v1 notes.** Each note of `data.json` becomes an entry with `source=v1` and `status=learned`. One tick confirms it; v1 cannot prove the origin (mem-17).
  2. **The voice copilot's file.** Each fact of `glasscode-memory.json` becomes a proposal; the file itself is not changed (the voice plan decides its future).
  3. **The server's copy.** The Mac builds a separate, filtered tree that leaves out every project marked "stays on this Mac", and every `org/<employer>/` folder used only by such projects. It commits that tree as a fresh commit on its own history (`git commit-tree` over the filtered tree), so no local-only text is ever in an object the server receives. It pushes that history to `refs/glasscode/memory-server` with the existing mirror transport. The server checks it out read-only and builds briefs for its own sessions from it. Server sessions propose through `RemoteCall`; the Mac decides (mem-4).
  4. **Codex memory folders** are found by probing the folder name, which differs between Codex versions (mem-m6).
- **Tests:** `v1_notes_become_learned`; `migration_is_idempotent`; `server_copy_is_read_only`; `server_proposal_waits_for_mac`; `local_only_project_never_in_server_objects` (`git cat-file --batch-all-objects` on the server copy has no match).
- **Verify:** core tests; Docker test server scenario `server_session_gets_brief`.
- **Demonstration:** a v1 scratch home with notes, after migration, shows the notes in Settings › Manager › Memory as "Learned (v1)".
- **Server:** the copy described above.
- **Windows:** no server features (Windows plan).
- **Addresses:** mem-16, mem-17, mem-m3 (the comparison is in design section 12), mem-m6, exec-6 (notes)

### M21.1 Gate v2 and the authority store (wave 2, L)

- **Files:** new `crates/glasscode_core/src/gate/` (`mod.rs`, `levels.rs`, `conditions.rs`, `authority.rs`, `defaults.rs`); `control.rs`; `crates/glasscode/src/control_ui.rs`; `crates/glasscoded/src/daemon.rs` (one-way authority cache); `memstore/` (the generated `POLICY.md` header); `lib.rs`.
- **Steps:**
  1. `decide(caller, op, targets, policy, grant)`, with the precedence of design section 4. `PolicySet` comes from `authority/policy.json`. The person (`Ui`) is always allowed, except for `Never` hard limits, which apply to everyone.
  2. **Default table** per op group, level and caller (trust-11), in `defaults.rs`. It is the source for the Settings table of M53.

     | Group | Careful: manager / auto session | Standard | Autopilot |
     |---|---|---|---|
     | Read, Navigate | allow / allow | allow | allow |
     | Board, Memory (propose, note) | allow / allow | allow | allow |
     | SessionStart | ask / — | allow | allow |
     | SessionInput (send, answer) | ask | allow, to sessions it started | allow |
     | SessionControl (close, restart, mode) | ask | ask | allow; mode never raised |
     | Worktree, Project (remove, clean) | ask | ask | ask |
     | Settings | never | never | never |
     | Tickets | ask | ask | allow (move, comment on own) |
     | ForgeRead | allow | allow | allow |
     | ForgeWrite (comment, label, rerun, draft PR) | ask | ask | allow when D29 holds |
     | Message | ask | ask | ask (routine pings per `SendPolicy`) |
     | Browser (act, eval) | ask | ask | ask |

  3. **`human_only` ops** are refused to every caller except `Ui`. They cover:
     - approving or denying a card;
     - changing policy, playbooks or schedules;
     - approving a message;
     - accepting memory;
     - releasing a pause.
  4. **Conditions** (crit-14): branch, paths, hours and person, checked against `targets`.
  5. **The authority store.** At approval the app copies the approved content into a snapshot in `authority/approved/`. The gate reads only the snapshot, never the live file. When the live file differs, the effective rule is the stricter of the snapshot and Careful, so `never` entries and "stays on this Mac" always survive a changed file. A hand edit is imported only through an in-app screen with a plain diff ("Trust level: Careful to Autopilot"). Sessions cannot write `~/.glasscode/authority/`: the sandbox and the file-tool deny rules of M22 refuse it. A program running as the person outside a sandbox can still change it, and the design says so. A later step can sign the snapshots with a Keychain key (trust-4).
  6. **`POLICY.md`** in memory gets a generated, read-only header with the level and the rules in words. Its prose is guidance only.
  7. **Sync.** The authority snapshot flows Mac → server inside `mirror.sync`. glasscoded runs `decide()` from that cache and never sends authority back (crit-5).
  8. **Phone path** (ops-m3). Phone actions queued on the server reach the Mac as `Phone(device)` and pass `decide()`. The Mac trusts only device keys it paired itself (`authority/devices.json`, trust-m5).
  9. **Account visibility per project** (D17): which account's model may read the project.
  10. **Pause** (D15): any caller may pause; only `Ui` releases.
  11. `session.mode` and `session new --mode` never raise the mode for a non-person caller (ops-6).
  12. `state.json` keeps the 13 kinds for older builds (exec-7). v2 reads them as the person-session defaults.
  13. The policy, playbook and schedule stores set `FALLS_BACK = false` (M10). A damaged file must never be read from its older `.prev`, which could hold a looser policy. They are read with `store::open()` and fail closed. `decide()` never calls `paths::class_of`, which reads the disk. Test: `policy_never_reads_prev`.
- **Tests:**
  - `every_handler_goes_through_decide` (a scan of `ops_run.rs`);
  - `changed_policy_counts_as_careful`;
  - `human_only_refused_to_manager`;
  - `careful_manager_session_new_asks`;
  - `autopilot_manager_session_new_allows`;
  - `condition_branch_blocks_default`;
  - `server_never_sends_authority`;
  - `unpaired_phone_key_refused`;
  - `mode_never_raised`;
  - `older_state_json_round_trip`.
- **Verify:** core and app tests; evals `adversarial` (gate cases); `cargo check -p glasscoded`.
- **Demonstration:** in a scratch home with two projects, one Careful and one Autopilot, the v1 brain's `glasscode session new` waits for approval in the first and runs in the second.
- **Server:** as in steps 7 and 8.
- **Windows:** the same code.
- **Addresses:** trust-3, trust-4, trust-11, trust-12, trust-13 (the limit table is design section 7), trust-15, crit-5, crit-14, events-10 (approval records), exec-7, trust-m5, trust-m6, mem-7, ops-6, ops-12, ops-m3, ops-m4 (read scope)

### M21.2 Pending actions, cards and chat grants (wave 2, L)

- **Files:** new `gate/pending.rs`, `gate/grant.rs`; `crates/glasscode/src/control_ui.rs` (approvals become `PendingAction`s, not closures), `notify_ui.rs`, `today_cards.rs`, `manager_ui.rs`; `ops/` (`approvals.list`, `approvals.decide` as `human_only`).
- **Steps:**
  1. **`PendingAction` store** (design section 8). Each entry has:
     - a stable id (a UUID, not the per-launch counter);
     - the op and its argument hash;
     - who asks and why;
     - an expiry;
     - its state.

     Entries survive a restart. Approving one runs exactly the stored arguments after checking policy and freshness again: "The PR is closed now; nothing was done" (ux-1, ops-11).
  2. **The card** shows:
     - one sentence from the registry;
     - who asks, and the rule that asks;
     - the full payload, expandable and never clipped (trust-18);
     - the expiry.

     Its buttons are Approve, Deny, and "Always allow this in <project>", which opens a policy preview that needs a second confirmation.
  3. **The popup** shows Allow only for local, reversible ops. For anything that leaves the Mac it shows "Review".
  4. "Allow for the rest of the session" becomes a grant for this op and this target, not for the whole kind.
  5. **Grants (D3).** `Grant` is checked by `decide()`. A chat turn proposes an op together with its basis (message id and quote). Rust checks the tier, the quote, the target ids, the ceiling and single use.
  6. **In chat**, a typed "yes" binds only when exactly one card is open. A spoken "yes" never binds.
  7. Every decision goes to the journal (`decision`) and, from M31 on, to the effect ledger (trust-16).
- **Tests:**
  - `three_pending_survive_restart`;
  - `expired_cannot_run`;
  - `approve_runs_stored_args_only`;
  - `stale_target_reports_and_does_nothing`;
  - `yes_with_two_open_runs_nothing`;
  - `spoken_yes_never_binds`;
  - `grant_quote_must_be_tier_one`;
  - `grant_single_use`;
  - `grant_targets_must_be_resolved_from_message`.
- **Verify:** core and app tests; evals `adversarial` (grant cases).
- **Demonstration:** in a scratch home, a pending "open draft PR" card survives a quit and relaunch, and its full payload is visible.
- **Server:** pending actions for server sessions are kept on the Mac.
- **Windows:** the popup's position as today.
- **Addresses:** ux-1, ops-11, trust-5, land-12, trust-18, trust-16

### M22 Session profiles and hooks (wave 2, L)

- **Files:** new `crates/glasscode_core/src/profile.rs`; `agents.rs` (launch takes a `SessionProfile`); `cli.rs` (`glasscode hook gate`, `glasscode hook permission`); `crates/glasscode/src/control_ui.rs` (permission decisions); `project_setup.rs` (public repository and fork detection shown in project Settings); `gate/` (the `trusted_author` predicate); `crates/glasscoded/src/tmux.rs` (the same profiles on the server); `cli-flags.json` (the Codex launch flags); `crates/glasscode_core/src/versions.rs` (the argument contract); `paths.rs` (credential rows).
- **Steps:**
  1. **`session_profile(origin, …)`**, generated from the path classes (design section 5), produces:
     - **Claude `--settings` JSON:**
       - hooks with absolute commands;
       - `sandbox` with `enabled`, `allowUnsandboxedCommands: false`, and `failIfUnavailable` for the automation and quarantine profiles;
       - filesystem `denyRead` and `denyWrite` from the classes;
       - a network allowlist: the forge and the registries the project names, or the forge only for quarantine;
       - `allowUnixSockets` for the control socket;
       - `permissions.deny` on Read, Edit and Write for the authority, ledger, hook IO, accounts and credential classes, plus Edit and Write on trusted memory;
       - `permissions.disableBypassPermissionsMode: "disable"` for the automation and quarantine profiles;
       - `crossSessionInbound: "refuse"` for the automation and quarantine profiles (per P16).
     - **Codex flags:**
       - the person profile as today;
       - automation: `workspace-write` with network per the allowlist rule;
       - quarantine: network off;
       - `danger-full-access` and the bypass flags refused for anything but the person;
       - worktrees no longer pre-trusted for automation and quarantine (trust-19);
       - the `--full-auto` fix (m03-1, alt-m4) is already done on `main` by PR #79 (`--sandbox workspace-write --ask-for-approval on-request` for sessions, `exec --sandbox workspace-write` for headless runs), and the known failures are removed; M22 keeps those flags when it moves them into `session_profile`.
  2. **Origins.** Sessions that a playbook or the manager started, or sent text to, run under the automation profile for their whole life. Sessions sent to untrusted input (D27) run under quarantine. The person's own sessions keep the person profile.
  3. **Hook commands.**
     - Absolute paths: the launcher, resolved at launch, with its SHA-256 checked against the app's own copy (crit-15).
     - A hostile project setting cannot turn them off (P08). Where P08 fails, the session is marked "unreported".
  4. **`PreToolUse` gate hook** for the automation and quarantine profiles (matcher `*`, `onFailure: block`). Rust decides on the tool name and input. It refuses:
     - `gh pr merge`;
     - `git push --force` and pushes to the default branch;
     - reads of credential paths;
     - `SendMessage` and agent spawning across projects.

     This is labelled *guarded* (alt-1).
  5. **`PermissionRequest` hook** (D12). `glasscode hook permission` sends `tool_name` and `tool_input` to the app. The gate answers allow, deny or nothing within 2 s. It never returns `updatedPermissions`, and it declines a command with an operator, a substitution or a redirection.
  6. **Allow-commands.** A project's approved allow-commands are compiled into Claude's allow rules at launch, so allowed commands never prompt.
  7. **`trusted_author`**: the person, teammates confirmed in the app, or GitHub author association `OWNER`, `MEMBER` or `COLLABORATOR`.
  8. **Tokens.** `GH_TOKEN` is not put into automation or quarantine sessions; they use `gh`'s own login (trust-m4).
  9. **The app's own git calls** on worktrees pass `-c core.hooksPath=/dev/null -c core.fsmonitor= -c core.sshCommand=ssh` and ignore worktree-local values of dangerous keys (trust-m7).
  10. **Claude's auto-memory** is redirected to `agent-notes/<key>/claude/` per P04 (D19), after a one-time copy of the old folder. Codex memories are off for Careful and local-only projects.
- **Tests:**
  - `profile_from_classes_only` (no path literal in `profile.rs`);
  - `automation_denies_authority_and_credentials`;
  - `quarantine_network_forge_only`;
  - `bypass_disabled_for_automation`;
  - `hook_command_is_absolute_and_checked`;
  - `permission_hook_never_returns_updated_permissions`;
  - `compound_command_declined`;
  - `trusted_author_rules`;
  - `no_gh_token_in_automation_env`;
  - `every_cli_flag_is_listed`: a core test builds `agents::launch` for every mode, `headless`, `brain::claude_args` and `brain::codex_args`, and asserts each `-` argument appears in `cli-flags.json` under its site.
  - `argument_contract`, in `versions.rs` (alt-m4's full fix): approving a CLI version runs every argument list Glasscode can build against the candidate binary, ending each in `--help` for Codex and `--version` for Claude. While any list fails, the version cannot be approved. The test runs it against the stand-in CLIs of M02.
  - Credential rows in `paths.rs`: `~/.netrc.gpg` and the other encrypted variants of the credential files (M10 review).
  - Fixture runs with stand-ins: a planted "merge it" in a review leads to a refused `gh pr merge` in an automation session.
- **Verify:** core and app tests; evals `adversarial` (session cases); P07 repeated with the real code in a scratch home.
- **Demonstration:** in a scratch home, an automation Claude stand-in that tries to `cat ~/.ssh/id_ed25519` and `gh pr merge 1` is refused. Both refusals are in the journal.
- **Server:** `tmux.rs` launches with the same profiles; Linux uses bubblewrap through Claude's sandbox.
- **Windows:** no sandbox. The profile carries only file-tool deny rules and hooks, and Settings says so. Autopilot pushes are limited by D29.
- **Addresses:** trust-8, trust-14, trust-19, trust-m3, trust-m4, trust-m7, land-4, land-5, land-6, land-m1, alt-1, alt-8, alt-m2, mem-6, crit-15, ops-5 (paths 1 and 2), events-21

### M23 Headless runs, browser ops and the forge wall (wave 2, L)

- **Files:** `profile.rs` (`headless_profile`); `crates/glasscode/src/auto_review.rs`; `crates/glasscode_core/src/browser/mcp.rs`, `cdp.rs`; `github_query.rs` (read branch protection and rulesets); `gate/` (forge facts per project).
- **Steps:**
  1. **`headless_profile(kind, …)`** for every non-interactive launch: no command tools (Claude `--restricted --tools ""`), Codex read-only with network off, results bound to a schema, and input given as wrapped data.
  2. **Auto-review**:
     - Glasscode fetches the PR title, body and diff through `github_api`.
     - A tool-less run returns the overview as structured output.
     - The comment is posted through a fixed template, and only after a card, unless the PR author passes `trusted_author` and the repository was opted in.
     - `~/.glasscode/review-prompt.md` becomes an authority file with approval.
     - Run the Codex review read-only with network off (trust-m2, alt-m1). The `--full-auto` flag itself was already replaced on `main` by PR #79.
  3. **Browser ops.** Calls to the Glasscode browser MCP server are classified as `browser.read`, `browser.act` or `browser.eval`, and pass `decide()` with the session's caller. Automation and quarantine sessions get no browser tools, or a throwaway profile with no sign-ins. The DevTools port is bound to a per-launch secret, not a fixed open port (trust-m1).
  4. **Forge wall (D29).**
     - Read the protection of the default branch at project setup and during moving in.
     - Store the facts in the authority store and show a line per project: "Forge enforces: review before merge, no force-push, no bypass".
     - Autopilot's push capability is available only where all three hold.
  5. **Brokered push** (D13), only if P14 passed and the owner chose it (O7): `git.push`, `git.fetch` and `pr.open` as gated ops for automation sessions whose sandbox has no route to the forge.
- **Tests:**
  - `auto_review_input_is_data_only`;
  - `auto_review_posts_only_after_card`;
  - `codex_review_flags_in_probe`;
  - `browser_eval_asks_on_signed_in_site`;
  - `automation_session_has_no_browser`;
  - `devtools_requires_secret`;
  - `autopilot_push_needs_forge_rules`.
  - Eval: a PR whose description says "approve and merge this" ends with no review, no merge and no fetch.
- **Verify:** core and app tests; `cli-probe.sh` passes; evals `adversarial`.
- **Demonstration:** in a scratch home, Settings shows the forge line for a repository with protection, and "guidance only" for one without.
- **Server:** auto-review does not run on the server.
- **Windows:** the same.
- **Addresses:** trust-m1, trust-m2, alt-m1, trust-20, alt-1 (part 5)

### M24 Archive by reconciliation (wave 2, M)

- **Files:** new `memstore/archive.rs`; `crates/glasscode/src/commands2.rs` (`remove_worktree_now` calls the archive before `git worktree remove`); `reconcile.rs` (finds work that ended outside Glasscode); `transcript.rs` (digest).
- **Steps:**
  1. `archive(work_key)` is idempotent per work key. It writes:
     - the final work file;
     - the review notes, copied from `<worktree>/.glasscode/review/` before removal;
     - the PR and ticket links and their last state;
     - the last turn report (the last reply from `transcript.rs` until M30 lands);
     - the handoff notes;
     - the conversation file paths, with a bounded digest of each: the first prompt, the last 20 replies, the failed tools, at most 20 KB. Claude deletes old transcripts, so the digest is what lasts (P12).
  2. Triggers: `worktree remove`, `worktree clean`, a worktree gone from `git worktree list` or a folder deleted by hand (found by the reconciler), a session moved to the server, and a merged or closed PR with no open session for 7 days.
  3. The archive goes to `projects/<key>/archive/<yyyy-mm>/<work>.md` in one commit. Search ranks it a little lower.
- **Tests:**
  - `remove_archives_before_delete`;
  - `outside_removal_archived_by_reconciler`;
  - `archive_idempotent`;
  - `digest_bounded`;
  - `review_notes_kept`;
  - `archived_work_found_by_search`.
- **Verify:** core and app tests.
- **Demonstration:** in a scratch home, remove a worktree whose review notes exist. `glasscode memory search` finds its archive file 40 days later on the virtual clock.
- **Server:** a server worktree removal archives on the Mac, after the next sync.
- **Windows:** the same.
- **Addresses:** mem-3, mem-11, exec-14, alt-m5

### M25 Export, backup, restore, forget, uninstall (wave 2, M)

- **Files:** new `crates/glasscode_core/src/portability.rs`; `ops/` (`export`, `import`, `forget`, `uninstall --dry-run`); `settings_ui.rs` (one card: "Your data").
- **Steps:**
  1. **`export`** writes a `tar` file through the `tar` program, present on macOS, Linux and Windows 10 and later. It holds memory, the authority store (without device keys), the cited words records, conversation summaries and the effect ledger.
  2. **`import`** re-resolves project keys on the new machine.
  3. **"Move to a new Mac"** is export plus import.
  4. **`forget`**: a person, a project, an employer, or everything (design section 6).
  5. **`uninstall --dry-run`** lists every file and folder Glasscode made. `uninstall` removes them after a confirmation in the app. Time Machine copies are named in the text.
- **Tests:** `export_import_round_trip`; `forget_person_removes_everywhere` (memory, words, journal summaries, effect payloads); `forget_project`; `restore_after_damage`.
- **Verify:** core tests.
- **Demonstration:** export a scratch home, import it into a second one, and run a search there.
- **Server:** forget also asks the server copy to forget, with the next sync.
- **Windows:** `tar.exe`.
- **Addresses:** crit-8

### M30 Turns and turn reports (wave 3, L)

- **Files:** new `crates/glasscode_core/src/turns.rs`; `agents.rs` (the hook list of P05; the Codex hooks of P06; `notify` kept for the person); `transcript.rs` (items between turn bounds); `history.rs` (sessions started outside Glasscode); `journal_kinds.rs` (`turn.*`).
- **Steps:**
  1. **The state machine (D24).**
     - A turn opens on `UserPromptSubmit`, keyed by `prompt_id` (Claude), or on `task_started` (Codex).
     - It closes on one of:
       - `Stop` with no background tasks → `Completed`;
       - `Stop` with background tasks or session crons → `Paused`;
       - `StopFailure` → `Failed(error)`;
       - Codex `turn_aborted`, an interrupt marker, or the prompt back on screen with a turn still open → `Interrupted`;
       - `SessionEnd` or a PTY or tmux exit with a turn open → `Aborted(exit status)`;
       - no transcript growth for 120 s plus a dead process → `Crashed`;
       - the app quit → `AppQuit`;
       - sleep → `Slept`.
     - Every opened turn closes exactly once. Repeated `Stop`s for one `prompt_id` collapse. `SubagentStop` from internal agents is ignored. A `Notification` is read by its `notification_type`.
  2. **The report** (`TurnReport`) comes from transcript items between the bounds, after waiting up to 3 s for transcript lag (P05). It has:
     - the tool count, failed tools and commands with their exit codes;
     - files from `git diff --stat` against the head at turn start;
     - commits, and whether they were pushed;
     - the last reply, cut to 600 characters;
     - the question, if the turn ended on one.
  3. **Self-reports.** `glasscode report done|blocked|needs-review "<line>"` is stored as an unverified hint and never counts as evidence (events-23).
  4. **Sessions started outside Glasscode** are discovered by `history.rs` and produce `session.outside` events only (land-9).
  5. **Background work.** Stop payloads with `background_tasks` or `session_crons` give a "paused" report that names them (events-25).
- **Tests:** for both agents, with stand-ins:
  - a completed turn;
  - an interrupt;
  - a rate-limit `StopFailure`;
  - `kill -9` of the agent;
  - an app restart mid-turn;
  - a stop-hook continuation;
  - a turn with background tasks.

  Each gives exactly one report with the right cause. `subagent_internal_ignored`; `self_report_is_hint`.
- **Verify:** core tests; `testkit::sim` asserts `one_report_per_turn` over the reference load.
- **Demonstration:** Activity shows a report line per turn. Interrupting a stand-in turn shows "Interrupted".
- **Server:** the daemon's sessions use the same machine through tmux events.
- **Windows:** ConPTY exit codes.
- **Addresses:** events-1, events-2, events-3, events-m2, events-23, events-25, events-m5, alt-5, land-9, land-15, exec-15

### M31 Effect ledger and delivery to sessions (wave 3, M)

- **Files:** new `crates/glasscode_core/src/effects.rs`, `delivery_session.rs`; `crates/glasscode/src/ops_run.rs` (claim and finish around non-person effects); `manager_tick.rs` and `ci_fix.rs` (sends go through the queue); `activity` view in `today_ui.rs` (the record of autonomous actions).
- **Steps:**
  1. **`effect_key`, `claim` and `finish`** (design section 8).
     - The key comes from the origin (a playbook id or a chat message id), the subject (for example `repo#pr@head_sha`, or a schedule id plus its occurrence), the op and the target.
     - The claim is fsynced before the action.
     - On Windows a save is atomic but not proven durable when it returns (design section 7); a claim's fsync rule must account for it.
     - Seeing the same key again returns the stored result.
     - Each claim records its home machine.
  2. **Retry classes.**
     - Idempotent ops retry with backoff.
     - Verifiable ops have a probe with no model:
       - `session.send`: a `UserPromptSubmit` or Codex user item after the claim time, whose text contains the sent text;
       - a PR: found by its head branch;
       - a board move: read back.
     - At-most-once ops (the messenger) go to the person as "uncertain".
  3. **`session.send` becomes a queued delivery.**
     - Preconditions: the session is idle, no prompt on screen, no draft in the message box, no typing in the last 10 s.
     - It has an expiry and a supersede rule: a new head SHA cancels an older CI message.
     - It is the one writer per session terminal (ops-m2).
     - It never lands on a permission prompt (land-m3).
  4. **The record.** Activity shows autonomous actions for 30 days, with origin, reason and result (land-21).
  5. **Generation.** Each record carries `home` and `generation` (D5). M58 adds the registration of outward claims with the server; until then the field is filled with generation 0.
- **Tests:**
  - `replayed_event_does_not_repeat`;
  - `crash_between_act_and_result_resolved_by_probe`;
  - `two_playbooks_one_event_one_effect`;
  - `send_waits_for_idle`;
  - `send_superseded_by_new_head`;
  - `messenger_uncertain_goes_to_person`;
  - fault injection: kill at each step of claim, act and finish.
- **Verify:** core tests; sim asserts `no_duplicate_effects`.
- **Demonstration:** a CI-fix message queued while a stand-in session works is delivered when it goes idle, once.
- **Server:** the server's claims are merged by key on the Mac.
- **Windows:** the same.
- **Addresses:** events-7, alt-4, alt-6 (durability), ops-19, ops-m2, land-m3, land-21

### M32 Work ledger and done conditions (wave 3, M)

- **Files:** new `crates/glasscode_core/src/work.rs`; `memstore/` (the work file is generated from the work item plus its "Notes from sessions"); `today_cards.rs` (the "Needs you" and "Coming up" cards read work items).
- **Steps:**
  1. A `WorkItem` per `WorkKey` links the board task, ticket, sessions and PR.
  2. A session gets its `DoneCondition`s at start from its kind: a PR session gets `CiGreen` and `ReviewRequested`; a review session gets "review posted"; a ticket session gets `PrReadyForReview`. A person can add `Person`.
  3. Conditions are checked by code after each turn report and each PR status change. The state is Open, Waiting(who), Stuck(why), Done or Dropped.
  4. Stuck rules: working for 45 minutes with no turn end, three failed turns in a row, or CI red after two fix messages.
  5. The vendors' own goal evaluators are noted as an option per CLI version, and not used in release 1 (land-8).
- **Tests:** `pr_session_done_when_ci_green_and_review_requested`; `stuck_after_three_failures`; `work_file_generated_from_item`; `follow_up_maps_to_work_item` (the shape M44 uses).
- **Verify:** core tests.
- **Demonstration:** Today's "Needs you" lists a stuck stand-in session with the reason.
- **Server:** work items for server sessions are kept on the Mac.
- **Windows:** the same.
- **Addresses:** events-14, land-8

### M33.1 Registry R2: inventory, coverage, `run_op`, generated help (wave 3, L)

- **Files:** new `crates/glasscode/src/capabilities.rs` (the inventory table in code); `crates/glasscode/src/workspace.rs` (`run_op`); `cli.rs` (help generated from `OPS`); `keymap.rs` (key actions call ops); `palette_ui.rs` (Jump commands are ops); new `crates/glasscode/tests/coverage.rs`.
- **Steps:**
  1. **The inventory.** One row per UI capability, with a class: `op` (with its op name), `person-only` (with the sentence the manager says and the place it opens), or `view-local` (hover, scroll, fold, caret). It covers every screen of `architecture.md`'s screen table.
  2. **Coverage tests**, with no window:
     - every `op` row names a registered op, and every op appears in a row;
     - every Settings card of `settings_ui::catalog()` maps to a settings key, or is marked action-only;
     - every `View`, settings section and panel tab has a `Place`;
     - every GPUI action and every Jump command maps to an op, or is marked view-local.
  3. **A source-scan baseline**: counts of `.on_click(` and `cx.listener(` per file, committed. A count may only go down, unless the same change adds inventory rows.
  4. **`Workspace::run_op(op, Caller::Ui)`.** Converted buttons call it, so the person's actions reach the journal. Converting existing screens happens screen by screen and is not part of this slice's done-when.
  5. **`glasscode help`** is generated from `OPS`, with a golden of the text.
- **Tests:** `coverage.rs` (the four checks); `baseline_only_goes_down`; `help_golden`.
- **Verify:** app tests.
- **Demonstration:** the generated "what the manager cannot do" list (design section 11) prints from the inventory.
- **Server:** none.
- **Windows:** `keymap` tables for both platforms.
- **Addresses:** ops-1, ops-2 (the gap list becomes rows)

### M33.2 Registry R3: new ops (wave 3, XL)

- **Files:** `ops/` (new tables: `ui`, `session_ext`, `settings`, `reads`, `usage`, `work`, `journal`); `crates/glasscode/src/ops_run.rs`; `prompt.rs` (prompt id); `github_api.rs`, `github_query.rs` (typed reads); new `crates/glasscode/src/settings_table.rs`.
- **Steps:**
  1. **`ui.state`** reports the view, the session, the right tab, the selection (file and line, PR, board item), open dialogs, what the manager points at, and the prompt on screen with its `prompt_id`. It contains no screen text.
  2. **`ui.open <place>`** works for every `Place` of M33.1. For a caller that is not the person, it never takes keyboard focus or dismisses notifications (ops-13). Also `ui.point`, `ui.say`, `ui.tour` and `ui.scene` (from v1).
  3. **`session.answer`** takes `prompt_id` and an option label. It parses the screen again before sending and after each key, and fails with `stale_prompt` on any difference. Options that grant permanently ("don't ask again") are person-only (D12, path 3).
  4. **`session.interrupt`**. **`session.keys`** is limited to Esc, Enter and the arrows for callers that are not the person. **`session.mode`** never raises the mode.
  5. **`settings.get` and `settings.set`** use a settings table: key, type, validation, and who may set it. No setting that changes hooks, accounts, permission rules, agent flags, paths or the manager's own policy may be set by a caller that is not the person (ops-3).
  6. **Typed reads for the brain.**
     - `pr.view`, `pr.checks`, `pr.diff`, `ci.log` and `review.threads` through `github_api`;
     - `git.status`, `git.log` and `git.diff` with fixed arguments;
     - `session.read`;
     - `file.read`, limited to managed project roots, refusing credential and authority classes, capped at 200 KB, and wrapped as data.
  7. **More ops:** `usage.get`, `approvals.list`, `journal.query`, `work.*`.
  8. **Every mutating op** gets `--dry-run`, which returns what it would do (ops-22). Each op declares `undo` (ops-18).
  9. **Long ops** report progress and accept cancellation in the reply envelope (ops-10). Handlers on the blocking list of M12.2 move to background tasks (ops-9).
  10. **The Codex sandbox's request-file transport** gets defined latency targets: an op round trip under 500 ms locally (ops-m5).
  11. A generic observe-and-press path through AccessKit is deferred (ops-14).
- **Tests:**
  - `ui_state_has_no_screen_text`;
  - `ui_open_does_not_steal_focus_for_manager`;
  - `answer_fails_on_changed_prompt`;
  - `persistent_option_person_only`;
  - `settings_set_refuses_hooks_for_manager`;
  - `file_read_refuses_credentials`;
  - `dry_run_changes_nothing`;
  - `blocking_list_empty` (no handler does IO on the main thread).
- **Verify:** app tests; evals `op_selection` (fixtures only).
- **Demonstration:** in a scratch home, `glasscode ui open "settings:automation"` opens the section, and `glasscode ui state --json` shows it.
- **Server:** read ops work for server sessions through `RemoteCall`.
- **Windows:** the same.
- **Addresses:** ops-3, ops-13, ops-14 (deferred), ops-18, ops-22, ops-m5, ops-5 (path 3)

### M34 `glasscode mcp` (wave 3, M)

- **Files:** new `crates/glasscode_core/src/mcp_stdio.rs` (extracted from `browser/mcp.rs`); `browser/mcp.rs` (uses it); `cli.rs` (the `glasscode mcp --profile manager|session` subcommand); new `crates/glasscode_core/tests/mcp_contract.rs` with recorded handshakes.
- **Steps:**
  1. **`mcp_stdio`** handles `initialize`, `notifications/initialized`, `tools/list`, `tools/call` and `ping`. Everything else gets -32601, including Claude's `server/discover` probe. It keeps a table of protocol revisions.
  2. **Tools** are generated from `OPS` with `json_schema`. The manager profile exposes about 12 direct tools plus `ops.find` and `op.run` for the rest, or, if P01 shows ToolSearch works with `--restricted`, every tool with about 12 marked always-loaded.
  3. **Identity.** The server is started by the app, one instance per brain process. It carries the lane and wake id in its environment, set by the app, and the app recognises the pipe as `Manager(lane)`. This is attribution through the channel, not a self-report.
  4. **An "ask" outcome** returns `Waiting` with the pending id at once, unless P15 shows the CLI waits long enough.
  5. **Fail closed.** The server refuses every `tools/call` until the host has checked the brain's init tool list (brain-m5).
- **Tests:** `mcp_contract` (replays of the recorded Claude and Codex handshakes per approved version); `unknown_method_is_32601`; `tool_schema_matches_spec`; `refuses_before_init_check`. Measure: base prompt tokens with all tools registered, in the exact brain profile (P01 numbers).
- **Verify:** core tests; `cli-probe.sh`.
- **Demonstration:** a stand-in brain lists the tools and calls `ui.state`.
- **Server:** glasscoded can serve the same tools for its own read-only copy.
- **Windows:** stdio is the same; `glasscode.exe mcp`.
- **Addresses:** ops-15, brain-2, alt-3

### M35 Moving in, part 1 (wave 3, L)

- **Files:** new `crates/glasscode_core/src/movein.rs`; `discover.rs` (more sources); `crates/glasscode/src/first_run.rs` (the offer); new `crates/glasscode/src/movein_ui.rs` (consent, progress, review); `settings_ui.rs` (Settings › Manager › "Move in", which also lets existing users start it).
- **Steps:**
  1. **Consent first.** One screen lists what was found, with a tick for each source and project:
     - projects;
     - Claude memory folders, matched by remote;
     - the Codex memories;
     - the editors' rules files (`.cursor/rules`, `.windsurfrules`, `.github/copilot-instructions.md`, `AGENTS.md`, `CLAUDE.md`) (crit-20);
     - 90 days of git history (people);
     - `CODEOWNERS`;
     - GitHub teams (read only);
     - Linear teams;
     - the board.

     Nothing is read before the tick, except names and paths. Anything owned by a GitHub organisation starts as Careful, with its conversations unticked.
  2. **Inventory**, with no model: languages, scripts, CI files, owners.
  3. **Harvest** into proposals, by deterministic parsing: bullets, headings, CODEOWNERS lines. Each proposal has its source, is normalised, and is deduplicated across sources. Contradictions are flagged.
  4. **Review at scale.** Proposals are grouped by project and by source, with "Accept all from this source", accepting or dropping single lines, and a count. Accepting writes memory (tier 1, because it is the person's click).
  5. **"From an export"**: run moving in from an M25 export. **Redo**: a moving-in run can be undone as a whole, through its commit range.
  6. **Run from install (O10).** The first run offers the manager; looking around starts after consent; every project starts Careful.
- **Tests:** `nothing_read_before_consent`; `duplicates_merged_across_sources`; `contradiction_flagged`; `accept_all_from_source`; `undo_run`; `org_owned_starts_careful`. Measure: inventory of the reference load (150 memory folders) in under 30 s.
- **Verify:** core and app tests.
- **Demonstration:** a scratch home with two projects, three Claude memory folders (two of the same repository at different paths) and a Codex memory folder proposes one memory per repository, with sources, and writes nothing outside `inbox/` until accepted.
- **Server:** none.
- **Windows:** `discover.rs` paths for Windows.
- **Addresses:** ux-11, ux-12, mem-12, crit-20

### M40 Playbooks and built-ins (wave 4, L)

- **Files:** new `crates/glasscode_core/src/playbooks/` (`mod.rs`, `model.rs`, `engine.rs`, `builtins.rs`, `limits.rs`, `simulate.rs`); `autofix.rs` (used by the built-in); `crates/glasscode/src/ci_fix.rs` (runs through the engine); `manager/harness.rs` (left in place for v1; the parity tests read it).
- **Steps:**
  1. **The model** (design section 8) lives in `authority/playbooks.json`.
     - Triggers are event filters on `EventKind` and fields, schedules, or "after an event, with a delay".
     - Conditions are typed values, not strings: project level, author is trusted, PR author is me, label present, branch not the default (events-11).
     - Actions are an op plus argument templates with typed slots. Outside values are wrapped as data.
     - Autonomy is watch, tell, ask or act.
     - Limits are "n per subject per window".
  2. **Approval.** Each playbook has its approved hash on each machine. A file that arrives by sync is not approved on arrival. A playbook the wake lane or a session wrote can only be proposed (events-10).
  3. **Built-ins are code**, with parity tests against v1:
     - needs-you notification;
     - review received → tell the session (with `trusted_author`);
     - follow-up fixed and stale;
     - the daily brief;
     - CI and conflict fix (the rules of `autofix.rs`: once per commit, three tries);
     - stuck session;
     - usage near the limit → suggest a hand-off.
  4. **CI auto-fix keeps working with the manager off**, with its own switch in Settings › Automation as today (exec-16).
  5. **Loop protection** (events-13):
     - a cap of injected prompts per session per hour;
     - cycle detection: an effect whose origin chain contains itself;
     - a global breaker that pauses all playbooks and tells the person.
  6. **Simulation.** A playbook can be run against the last 7 days of journal without acting (dry run), showing what it would have done.
- **Tests:** `parity_with_harness_step` (recorded snapshots of v1 rounds give the same actions); `parity_with_autofix`; `synced_playbook_is_not_approved`; `wake_cannot_create_act_playbook`; `loop_breaker_trips`; `typed_conditions`; `ci_fix_runs_with_manager_off`.
- **Verify:** core and app tests; evals `adversarial` (planted playbook file on the Mac and on the server: nothing runs).
- **Demonstration:** in a scratch home, a red CI on an Autopilot PR tells its session at most twice; on a Careful project it asks instead.
- **Server:** built-ins for server sessions run on the Mac while the Mac is home (D5).
- **Windows:** the same.
- **Addresses:** events-11, events-12, events-13, exec-16

### M41 Scheduler and lifetime (wave 4, M)

- **Files:** new `crates/glasscode_core/src/schedule.rs`; `playbooks/engine.rs`; `crates/glasscode/src/workspace.rs` (quit sheet, sleep and wake); `update_ui.rs` (wait for turn ends); `sys/` (the idle-sleep assertion through GPUI's `App::prevent_idle_sleep`, which already exists in `vendor/gpui/src/app.rs`, on macOS, and `SetThreadExecutionState` on Windows).
- **Steps:**
  1. **Semantics.**
     - Schedules run in the machine's local time zone, through `sys` local time. The time zone name is stored for display, and a change of zone is shown.
     - DST: a local time that does not exist runs at the next valid minute; a repeated hour runs once.
     - A run missed while asleep runs once on wake, if it is still inside its window (default: 6 hours for daily schedules), and is skipped otherwise, with one line in Activity.
     - Overlap: a run is skipped while the previous one is still active.
     - Expiry is supported.
     - Only the home runs schedules; the claim key is the schedule id plus its occurrence instant.
     - The last run of each schedule is kept in `authority/schedules-state.json` (on damage: refuse, then no runs until repaired).
  2. **Lifetime** (design section 10):
     - the quit sheet when automation turns run;
     - "Restart to update" waits for turn ends, at most 10 minutes;
     - the idle-sleep assertion (O6);
     - after wake: reconcile, report cut turns once, resume sessions, catch up schedules.
- **Tests:** on the virtual clock:
  - `dst_gap_runs_next_minute`;
  - `dst_overlap_runs_once`;
  - `sleep_catch_up_runs_once`;
  - `old_missed_run_skipped`;
  - `overlap_skipped`;
  - `only_home_runs`;
  - `quit_during_wake_marks_uncertain_not_repeated`.
- **Verify:** core tests; sim over 7 virtual days.
- **Demonstration:** a schedule set for 09:00 while the scratch Mac "sleeps" from 08:00 to 10:00 runs once at 10:00.
- **Server:** none in release 1.
- **Windows:** `SetThreadExecutionState`.
- **Addresses:** events-9, crit-11, alt-m6

### M42 Delivery, outbox and brief (wave 4, M)

- **Files:** new `crates/glasscode_core/src/delivery.rs`; `crates/glasscode/src/notify_ui.rs`; `manager/messages.rs` and `messenger.rs` (reused behind ops); `manager/brief.rs` (text from code).
- **Steps:**
  1. **Matters.** Each notification has a matter key.
     - The same matter within 10 minutes is bundled into one notification.
     - Snooze is supported. Opening or dismissing a notification counts as acknowledging it.
     - A nudge is never repeated because the person did not answer.
     - Quiet hours come from Settings.
     - Severity decides whether there is a popup or only an Inbox line (ux-10, events-22).
  2. **The outbox.** `message.draft`, `message.approve` (person-only) and `message.send` keep the tiers. The default `SendPolicy::AskAlways` is enforced. The routine tier stays advised. The four calls of the messenger program are unchanged.
  3. **The brief.** Its lines come from code (`brief.rs`) on its schedule. The brain words it only when it is running.
- **Tests:** `bundles_same_matter`; `no_repeat_after_ignore`; `quiet_hours_hold_non_urgent`; `ask_always_never_sends`; `messenger_contract_unchanged`.
- **Verify:** core and app tests.
- **Demonstration:** five events about one PR produce one popup.
- **Server:** the server sends notifications to phones as today.
- **Windows:** popups at the bottom right, as today.
- **Addresses:** ux-10, events-22

### M43 Conversation store (wave 4, M)

- **Files:** new `crates/glasscode_core/src/conversation.rs`.
- **Steps:**
  1. **Typed items**:
     - your message, with its `WordsId`;
     - a manager message;
     - an action line, with its effect key;
     - a card reference;
     - a system line.

     Each item has an id and the topic it belongs to.
  2. **Topics.** Created automatically from the referenced session, PR or project. The person can split and merge topics.
  3. **Retention**: text for 90 days, summaries kept. Search over the conversation.
  4. **Rollover links** (D8): each fresh brain conversation links to the summary it started from.
  5. **Changes as deltas.** Every item has an id and an HLC stamp, so M57 can sync changes both ways and merge them.
  5. **v1 chat lines** are imported as "v1, origin unknown", which can never back anything.
- **Tests:** `topic_from_reference`; `split_and_merge`; `retention`; `v1_lines_have_no_tier`; `search_finds_old_answer`.
- **Verify:** core tests.
- **Server:** none in release 1.
- **Windows:** the same.
- **Addresses:** ux-2

### M44 v1 data migration (wave 4, S)

- **Files:** new `crates/glasscode_core/src/manager/migrate_v2.rs`.
- **Steps:**
  1. Migrate the rest of `data.json`:
     - follow-ups become work items plus built-in playbook data;
     - outbox messages move to the outbox;
     - checks become schedules, which stay off until approved, because the v1 brain created them;
     - the conversation goes to M43;
     - `seen` keys become effect keys, so nothing is said twice after the switch.
  2. Keep `data.v1.json`.
- **Tests:** `migration_round_trip_on_recorded_data`; `checks_start_disabled`; `seen_keys_prevent_repeat`.
- **Verify:** core tests.
- **Server:** glasscoded's v1 copy is left as it is until M57.
- **Windows:** the same.
- **Addresses:** exec-6 (the rest)

### M50 Brain runtime (wave 5, L)

- **Files:** new `crates/glasscode_core/src/runtime/` (`driver.rs`, `claude.rs`, `codex.rs`, `health.rs`, `budget.rs`, `wrap.rs`); `history.rs` (hide the manager's conversations); `discover.rs` (hide them from the first run).
- **Steps:**
  1. **`BrainDriver`**, shaped like ACP: start, prompt turn, cancel, and permission requests as events (brain-19, land-17). **`Host`** (design section 8) has two implementations: `AppHost` in `crates/glasscode`, and `DaemonHost` in `crates/glasscoded`, which runs Core and SessionHost ops for tmux sessions. The runtime itself is shared code.
  2. **`ClaudeDriver`**:
     - a long-lived `claude -p` with stream-json in and out, using `headless_profile(BrainChat)` (D4);
     - its own session id with persistence on, so it resumes;
     - `--include-partial-messages` for streaming;
     - interrupt through the stream's control request (brain-11).
  3. **`CodexDriver`**: `codex exec` and `codex exec resume` with the P02 flags. App-server only behind a switch (D14).
  4. **The tool-list check.** On the first `system/init` line, compare the tool list with the expected one. If it differs, stop the brain and say so (D4). The MCP server refuses calls until this check has passed.
  5. **Nothing inherited** from the person's setup (brain-5):
     - Claude: `--strict-mcp-config`, the settings sources limited per P01;
     - Codex: `--ignore-user-config` and `--ignore-rules`;
     - the brain's working folder is `manager/work`.
  6. **Health states**, each with a plain sentence and a backoff (brain-14):
     - signed out;
     - login about to expire;
     - out of usage;
     - overloaded;
     - unsupported version;
     - crashed;
     - tool list wrong.
  7. **No-model mode** (alt-11, brain-10). When no brain can run, rules, playbooks, delivery and the chat's code answers keep working, and the chat says that thinking is unavailable. This mode, not a hidden PTY, is the fallback for the `--bare` risk.
  8. **The token ledger** (D6), from each result's usage, with a daily cap and a per-hour wake cap.
  9. **Wrapping.** Results of typed reads are wrapped as data, with invisible characters stripped and closing tags neutralised (brain-15, land-19).
  10. **Prompt layout for caching**: static instructions first, then the always-loaded memory, then the situation and the question in the user turn. Long text goes on stdin, not in argv (brain-16).
  11. **Memory use.** RSS is measured per brain process, and the process restarts after N turns or above a cap (brain-m6).
  12. **Windows**: process trees end through `sys::end_tree` (brain-17).
  13. **v1's fail-open one-question fallback** is not carried over (brain-m1).
  14. **The manager's own conversations** are hidden from "Resume an earlier conversation" and from the first run (exec-28).
- **Tests:**
  - `brain_tool_list_is_exactly_glasscode` (both CLIs, with stand-ins replaying recorded init lines);
  - `tool_list_mismatch_stops_brain`;
  - `health_states_map_to_sentences`;
  - `budget_cap_blocks_wakes_not_chat_answers_from_code`;
  - `wrap_strips_invisible`;
  - `codex_flags_in_probe`;
  - `manager_conversations_hidden`.
  - Measure: the first token and the cost per turn, from P11 numbers, recorded.
- **Verify:** core tests; `cli-probe.sh`; one owner run with real CLIs in a scratch home (rule 13).
- **Demonstration (live):** in a scratch home with a real scratch login, ask "what is running?". The answer comes from `session.list` through MCP, and the init line shows no Bash.
- **Server:** not in release 1 (D5).
- **Windows:** the same drivers; Codex sandbox flags per platform.
- **Addresses:** brain-1, brain-5, brain-6, brain-10, brain-11, brain-14, brain-15, brain-16, brain-17, brain-19, brain-20, brain-m1, brain-m2, brain-m3, brain-m4, brain-m5, brain-m6, alt-2, alt-11, land-17, land-19, trust-10, ops-m1, exec-28

### M51 Lanes, queue and the wake lane (wave 5, L)

- **Files:** `runtime/` (`lanes.rs`, `queue.rs`, `wake.rs`, `situation.rs`); `playbooks/engine.rs` (the `wake` action).
- **Steps:**
  1. **The chat lane** is one long-lived driver.
  2. **The wake lane** starts a fresh, short turn per wake. It carries no history; its open matters come from the work ledger.
  3. **Both lanes** read the effect and work ledgers. A wake's output appears in the chat as items. A lock per work key stops the two lanes acting on the same thing at once (brain-13).
  4. **The priority queue** runs your messages first, then urgent events, then playbook wakes, then digests (events-15). It has:
     - coalescing over 20 s;
     - a staleness check before each run;
     - cancellation;
     - a 120 s timeout per wake;
     - a starvation guard.
  5. **Wake output is bound to a schema** (alt-7): `{matters, proposals: [op + args + reason], notify: [...]}`. A wake can read, draft, propose and notify. It acts only through a playbook the person approved. Proposals become cards.
  6. **The situation.** A wake gets only the projects in its digest. Content of a Careful project does not enter a wake about another project. Account visibility (D17) holds (trust-9).
  7. **Costing and budget** (brain-12, D6): the tokens of each wake are recorded, and above the cap routine wakes wait.
  8. **Ask first** (brain-3). An op that needs the person returns `Waiting` with a card. No CLI permission prompt is involved.
  9. **Rollover** (D8). When the chat conversation passes a set size (default 150,000 tokens, from the usage of each result), the lane asks the brain for a summary, starts a fresh conversation with it, and links the two in the conversation store (M43).
- **Tests:**
  - `chat_rolls_over_with_summary_link`;
  - `person_message_preempts_wake`;
  - `same_work_key_locked`;
  - `stale_wake_dropped`;
  - `wake_cannot_act_without_playbook`;
  - `careful_content_not_in_other_wake`;
  - `budget_pauses_routine_wakes`;
  - `schema_bound_output_only`.
- **Verify:** core tests; sim over the reference load: wakes per hour under the D6 cap.
- **Demonstration:** while a stand-in wake runs, a question in the chat is answered first.
- **Server:** none in release 1.
- **Windows:** the same.
- **Addresses:** events-15, events-16, events-17, exec-18, brain-3, brain-4, brain-7, brain-8, brain-12, brain-13, alt-7, alt-9, alt-13, land-3, land-13, trust-9

### M52.1 Chat surface: one conversation (wave 5, L)

- **Files:** `crates/glasscode/src/today_ui.rs`, `today_cards.rs` (the start box), `manager_ui.rs` (page and bubble), `chat_ui.rs` (rendering reused).
- **Steps:**
  1. **The Today box.**
     - When the manager is on, Enter asks it.
     - "Start a session", or ⌘Enter, always starts a session as today.
     - When the manager is off, Enter starts a session as today (ux-4).
     - The placeholder says which one Enter does.
  2. **The Manager page** shows topics on the left and the thread on the right. The bubble shows the same thread.
  3. **Rendering** uses `markdown.rs`. Session and PR references become chips that open the place. Scenes are kept (ux-20).
  4. **Streaming text** with Stop. A message sent while the brain is busy is queued, shows "queued", and is sent next (ux-9).
  5. **Answers from code** (v1's `quick_answer`) come back at once: "what needs me?", "status".
  6. **"What can you do?"** is answered from the inventory, with examples (ux-18).
  7. **Accessibility labels** for each control through GPUI (ux-16).
- **Tests:** pure parts, such as `enter_meaning_by_state`, `queued_while_busy` and `chips_from_references`; screen behaviour in the demonstration.
- **Verify:** app tests; `GLASSCODE_DEBUG_OPEN=manager` opens the page.
- **Demonstration (live):** in a scratch home with a real scratch login, type "open the session on #1" in the Today box, then "what's happening here?". The session opens, and the answer is about it.
- **Server:** none.
- **Windows:** the same; no microphone button (no voice helper).
- **Addresses:** ux-3, ux-4, ux-9, ux-16, ux-17, ux-18, ux-20

### M52.2 Chat surface: focus, action lines, cards, failure states (wave 5, L)

- **Files:** `manager_ui.rs`, `today_ui.rs`, `notify_ui.rs`, `shell.rs` (focus snapshot).
- **Steps:**
  1. **Focus.** `ui.state` is snapshotted when the message is sent, and goes with the message as a compact block. "This" and "here" resolve from that snapshot. Nothing is read from screen text (ux-5).
  2. **Moving the screen.** The manager moves the screen only in answer to your message from the last 30 s, or when you accept a "Show me" chip.
  3. **Action lines** carry the effect key, the op sentence and Undo when the op has an inverse (ux-14).
  4. **Approval cards** appear inline, from M21.2.
  5. **Failure states**, each with a sentence and an action, for: offline, out of usage, signed out, crashed, unsupported version, budget reached (ux-15).
  6. **Time budget.** A visible "thinking" line within 300 ms; the first token is measured and recorded (ux-m3).
  7. **A weekly account**: "What I did this week", from the effect ledger (ux-22).
  8. **The tour** from v1, through `ui.tour`.
- **Tests:** `focus_snapshot_at_submit`; `no_screen_move_without_recent_ask`; `undo_only_for_inverse_ops`; `failure_state_table`.
- **Verify:** app tests.
- **Demonstration:** with the scratch login signed out, the chat shows "Claude is signed out. Sign in in Settings › Accounts", and code answers still work.
- **Server:** none.
- **Windows:** the same.
- **Addresses:** ux-5, ux-14, ux-15, ux-22, ux-m3

### M53 Settings: levels, playbooks, approvals (wave 5, M)

- **Files:** `crates/glasscode/src/manager_settings.rs`, `settings_ui.rs`.
- **Steps:**
  1. **Per-project level picker**, with the table of what each level allows. It is generated from `gate/defaults.rs`.
  2. **Conditions editor**: branch, paths, hours, person.
  3. **The playbooks list.** Each playbook is shown as a plain sentence, with on and off, approve, and "Try on the last 7 days".
  4. **Schedules** are listed the same way.
  5. **Every change is previewed, then confirmed.** These are `human_only` ops.
  6. **A card's "Always allow"** opens the policy preview. No TOML or YAML anywhere (ux-13).
- **Tests:** `sentence_for_each_builtin`; `change_needs_confirm`; `preview_matches_decide`.
- **Verify:** app tests; `GLASSCODE_DEBUG_OPEN=settings:manager`.
- **Demonstration:** setting a project to Autopilot shows which built-in playbooks start acting and which forge rules are missing.
- **Server:** none.
- **Windows:** the same.
- **Addresses:** ux-13

### M54 Moving in, part 2 (wave 5, M)

- **Files:** `movein.rs`; `runtime/` (the study kind); `movein_ui.rs`.
- **Steps:**
  1. **The study pass**, one project at a time and within the budget, uses `headless_profile(Study)` with typed reads: `file.read` of the README, `AGENTS.md`, CI files and scripts, and `git.log`. It drafts `PROJECT.md` sections, know-how and people as proposals, each line with its source.
  2. **Up to eight questions** in the chat, in the topic "Moving in": level per project, who owns what, what is off limits.
  3. **Answers are tier-1 words**: they become confirmed facts, and policy proposals, which need the Settings confirmation.
  4. **Pause and resume**: the pass pauses above the budget and resumes later.
- **Tests:** `study_uses_read_only_profile`; `proposal_lines_have_sources`; `at_most_eight_questions`; `pauses_above_budget`.
- **Verify:** core and app tests; one owner run with real CLIs in a scratch home.
- **Demonstration:** the scratch home of M35 ends with a `PROJECT.md` per project and a list of answered questions.
- **Server:** none.
- **Windows:** the same.
- **Addresses:** requirement R9 (part 2)

### M57 Server home: the server acts (wave 5, L)

- **Files:** `crates/glasscoded/src/manager.rs` (the v1 rounds give way to `DaemonHost`), `daemon.rs`, `mirror.rs`; `crates/glasscode_core/src/runtime/` (`host.rs`); `conversation.rs`, `work.rs`, `gate/pending.rs`, `journal.rs`, `words.rs` (sync); new `memstore/sync.rs`; `crates/glasscode/src/manager_home.rs`, `manager_settings.rs` (Home choices and the server Test).
- **Depends on** also the server-sync plan's phase 0a: a caller for every glasscoded connection, using `Caller` of design section 8.
- **Steps:**
  1. **`DaemonHost`** implements `Host`. Core and SessionHost (tmux) ops run on the server. Window ops return `Unavailable { queued }` and run when the Mac connects. Reconcile the old `mirror::identity()` machine-id file with `authority/machine.json` (M10 left both).
  2. **The runtime runs in glasscoded** when the server is home. Its brain uses the accounts signed in on the server through the vendor CLIs. Settings › Manager › Test checks them, as v1's Test does. No credential is ever copied.
  3. **Sync**, through `manager.sync`, with HLC stamps and per-entry merge:
     - conversation changes, both ways;
     - work items, both ways;
     - pending actions, both ways (they carry no authority);
     - the journal, both ways by cursor;
     - words, both ways (the Mac never accepts tier 1 from the server);
     - memory entries, both ways by stamp and tombstone. This replaces M20.3's filtered copy; "stays on this Mac" projects are never sent;
     - the effect ledger, merged by key and generation.
  4. **Authority stays one way.** Approved snapshots go from the Mac to the server. Policy changes happen only in the Mac app.
  5. **Home choices** in Settings: Automatic, Always on the server, Always this Mac. Under "Always on the server" the Mac is a client: it shows the same conversation, runs Window ops, and runs its own sessions.
  6. **With the Mac closed,** what policy allows runs on the server. Asks wait as cards until the Mac answers. Notifications go to paired phones as today, read only.
- **Tests:** `server_home_runs_wake_with_mac_offline`; `window_op_queued_until_mac`; `local_only_project_never_synced`; `conversation_changes_both_ways`; `tier_one_never_from_server`; `authority_one_way`; `server_test_reports_missing_login`.
- **Verify:** `cargo test --locked -p glasscoded`; the Docker test server scenarios (`script/test-server/run.sh`).
- **Demonstration:** with the scratch Mac app quit, a CI failure on a server session runs the built-in playbook from the server. Reopening Glasscode shows it in the conversation.
- **Server:** this slice.
- **Windows:** client only; the server features follow the Windows plan.
- **Addresses:** mem-4, events-m3, ops-8, exec-3 (in part)

### M58 Automatic home: lease, hand-over, fencing (wave 5, L)

- **Files:** new `crates/glasscode_core/src/home.rs` (the home record and lease rules, pure); `crates/glasscoded/src/daemon.rs` (the arbiter); `crates/glasscode_core/src/sys/` (macOS: the I/O Kit power registration that holds sleep for the hand-over); `crates/glasscode/src/workspace.rs` (GPUI `on_system_sleep` and `on_system_wake`, for state only); `manager_home.rs` (status line); `effects.rs` (registration with the server); `schedule.rs` (only the home in Full mode fires).
- **Depends on** also the server-sync plan's session runner epoch, so a session moved to the server is never continued by the Mac's old copy.
- **Steps:**
  1. Write the home record and the lease of design section 4, "Home (D5)". Expiry is decided by the server's clock only. The Mac drops its lease on every wake. It never measures the lease with a clock that stops during sleep (P17).
  2. Under Automatic, the Mac becomes home only when it is awake, reaches the server, and you were present for 60 s.
  2a. **The switch is the planned hand-over** (design section 4, "The switch"). This slice adds the manager's part of it:
     - the check first;
     - pausing working sessions at a safe point through delivery-queued `session.send`, with a time limit and "Move now";
     - moving the manager last;
     - resuming each paused task on the other side;
     - the "Safe to close" state.

     The session and worktree moves themselves come from the server-sync plan: several at a time, a worktree as one unit with all its sessions, and the one-owner counter per worktree. This slice calls them through these ops, named by that plan (its phase 2):
     - `switch.units` lists the units: a worktree, its sessions, owner, epoch, whether it has uncommitted work, and whether it is ticked;
     - `switch.check` runs the check first, unit by unit;
     - `switch.move` takes `to`, the units, `parallel` (default 4) and `now`;
     - `worktree.move` moves one worktree;
     - `worktree.owner` reports the owner and epoch, and also serves as M31's probe for a move.

     Every move runs **from the Mac app, in both directions**, because the Mac's disk is one end of it (they are SessionHost ops only `AppHost` implements). For "Laptop mode", the Mac therefore:
     1. asks the current home, the server, to pause its sessions;
     2. calls `switch.move` locally;
     3. claims home last.

     Four rules from that plan apply.
     - Pausing happens before `switch.move`: a busy unit is never marked as moving.
     - From the start to the commit of a move, the unit takes no input. Its Mac terminals turn read-only, and messages to its sessions, the manager's included, wait in the delivery queue until the move ends.
     - When a unit's move fails, M58 resumes the sessions it paused there, with the same "carry on" message, so no unit is left paused. Sessions the move stopped itself ("Move now") are restarted on the Mac with `--resume` by the move.
     - Shell sessions have no conversation. They move as a fresh shell in the same folder, and `switch.check` lists them.

     `DaemonHost` refuses these ops one by one (`Unavailable { host: SessionHost, queued: false }`) for the Mac's units. A lost reply is resolved by `worktree.owner` before any retry. Server sessions are created "held", and start only at the commit, so a retry never starts a second copy. Worktrees with no open session but with uncommitted work get a snapshot pushed at switch time, so the phone can reach them; their owner does not change, and "stays on this Mac" projects are left out. Test: `switch_moves_every_open_session_and_worktree_as_units` on the Docker test server with 30 worktrees, two of them with several sessions; nothing runs in two places, and the paused sessions resume.
  3. Without the switch, the hand-over runs on sleep, as a fallback, inside the window the I/O Kit registration holds (at most 20 s, measured by P17). It is not run from the GPUI notice, which does not hold sleep. If there is no held window, step 5 applies. Its four steps:
     1. start no new outward effect, and let running ones finish for at most 10 s;
     2. end the brain turn or mark it `Slept`;
     3. send the last changes;
     4. call `manager.release_home(generation)`. The server takes home at generation + 1.
  4. **Registered claims.** While a server is paired, every outward or at-most-once effect is claimed with the server before it runs, one round trip each. A claim of an older generation without a result becomes `Uncertain`, and is probed before any retry. The cost, stated in Settings: a Mac that cannot reach the server performs no outward effect.
  5. **Unplanned loss.** The server waits for the lease plus 15 s, then takes home. Every `Uncertain` claim is checked before anything retries.
  6. **Return on wake.** The Mac wakes as a client and never runs its old-generation claims. After 60 s of presence it calls `claim_home`. The server finishes its brain turn (at most 2 minutes, else `Interrupted`), sends its changes, and grants generation + 1.
  7. **Server unreachable while the Mac is awake.** The Mac runs Degraded, with nothing outward, and the status bar says so. The setting "After N minutes without the server, this Mac acts alone" is off by default, and its text names the risk.
  8. **No server paired:** the Mac is home and nothing switches.
  9. **Schedules** fire only on the home in Full mode. Occurrence keys make an occurrence missed across a switch run at most once.
- **Tests**, on the Docker test server with the virtual clock:
  - a partition across 09:00 gives exactly one brief;
  - sleep in the middle of a turn;
  - sleep during an outward effect;
  - a power cut with no sleep notice;
  - a wake during a server turn;
  - dark wakes do not flap the home;
  - the Mac's clock 10 minutes off;
  - a server restart during a switch;
  - two Macs, with one home at a time;
  - `older_generation_claim_never_runs`.
- **Verify:** `cargo test --locked -p glasscode_core -p glasscoded`; the soak of section 14 includes switches.
- **Demonstration:** simulate sleep on the scratch Mac. The status shows "Manager moved to the server", and a minute after wake "Manager is on this Mac". The conversation is the same throughout.
- **Server:** the arbiter.
- **Windows:** the client side uses the same GPUI sleep notices; the server is Linux.
- **Addresses:** events-8, crit-9, crit-10, crit-11 (in part)

### M55 Release-1 gate (wave 5, M)

- **Files:** `docs/manager-v2-acceptance.md`, `docs/architecture.md`, `docs/manager.md`, `cli.rs` (help text), CLI compatibility checklist, `cli-versions.example.json` (minimum versions), `.github/workflows/ci.yml` (the soak job).
- **Steps:**
  1. **Three security reviews**, each by a fresh reviewer who did not build the slices (exec-24):
     - the gate and callers;
     - session profiles and hooks;
     - memory and words.
  2. **Automated gates:**
     - the adversarial suite is green;
     - the seven-day soak passes (section 14);
     - fault injection per store passes;
     - the restore test (M25) passes;
     - the forget-person test passes.
  3. **The owner's 48-hour run** on a scratch home with real CLIs, recording resident memory and idle CPU.
  4. **Docs updated**, with headless items in the compatibility checklist, the minimum CLI versions, and the "cannot do" list (exec-25).
- **Verify:** every row of section 13 is "yes".
- **Demonstration:** the acceptance list, done by the owner.
- **Addresses:** exec-24, exec-25

### M56 Switch to v2 and remove v1 (wave 5, M)

- **Files:** `crates/glasscode_core/src/manager/` (v1 parts), `crates/glasscode/src/manager_tick.rs`, `manager_cmd.rs`, `crates/glasscoded/src/manager.rs`, `crates/glasscode_core/src/store.rs` (the engine switch, `authority/engine.json`).
- **Steps:**
  1. **The switch.** The `engine` of `authority/engine.json` becomes `v2` for new installs and for everyone whose manager is off. v1 users get "Switch now", which runs M44, then M20.3.
  2. **Remove v1 parts in this order, each after its parity proof:**
     1. the `harness.rs` rules, after M40's parity;
     2. `brain.rs` and `DENIED_TOOLS`, after M50;
     3. `manager_tick.rs`, after M51 and M52;
     4. `Check` and the v1 store, after M44;
     5. the rounds in `glasscoded/src/manager.rs`, replaced by the read-only copy.

     Record each removal with the test that proves its replacement (exec-27).
- **Tests:** `v1_state_switches_cleanly`; the full suite.
- **Verify:** all CI jobs.
- **Addresses:** exec-27

### M60, M61 and M62 (wave 6, after release 1)

- **M60 Intake: finding work.**
  - Autopilot picks work from Linear tickets with a project label and GitHub issues with a label by trusted authors.
  - It is limited by a cap of concurrent automation sessions per project and a daily token budget.
  - Back-pressure: no new work while N PRs wait for review or CI is red.
  - Each item gets typed done conditions.
  - Addresses: crit-12.
- **M61 Delegated decisions and the validator.**
  - In Autopilot, the manager may answer a session's question only within a list of decisions it may take. Otherwise it sends a card.
  - Decisions are recorded in `decisions.md`, with their words source.
  - An independent headless review checks a PR before "ready for review", with a second attempt when it fails.
  - Large work is split into sessions that get fresh context, return a summary, and gain no tools.
  - Addresses: crit-13, land-11.
- **M62 Learning.**
  - An hourly repair runs with no model, and a reflect step runs with a small model over new words, within the budget.
  - A weekly tidy-up handles duplicates, stale code anchors and contradictions.
  - Know-how proposals come from fixed failures, with personal data stripped. A file the person edited is locked.
  - Addresses: R8 upkeep.
- **Belongs to other plans** (owner, 2026-11-19):
  - **The phone and Meta glasses plan:**
    - approvals from the phone (O14);
    - the HTTP control API in Rust that the glasses and the voice panel use, with routes generated from this plan's registry (exec-9, ux-19);
    - the phone's place in the one conversation.
  - **The voice plan:**
    - configurable voice-to-voice providers and their cost;
    - whether the ElevenLabs copilot or LiveKit stays (ux-6, ux-7, ux-8, brain-18).

    It connects to the chat lane of M51 and M52.
- **Decision records, no slice yet:**
  - the Codex app-server driver (P13);
  - an ACP driver;
  - a second forge;
  - a background helper on the Mac;
  - an AccessKit observe-and-press path (ops-14);
  - a system-wide entry point (ux-21).

## 8. Files with a conflict risk

Slices that change the same file never run at the same time. The order is the order of section 5.

| File | Slices |
|---|---|
| `crates/glasscode_core/src/agents.rs` | M14, M20.2, M22, M30 |
| `crates/glasscode/src/control_ui.rs` | M12.2, M13, M14, M21.1, M21.2, M22 |
| `commands.rs`, `commands2.rs`, `manager_cmd.rs` | M12.2, M13, M24 |
| `crates/glasscode_core/src/cli.rs` | M14, M20.2, M22, M33.1, M34 |
| `crates/glasscode/src/workspace.rs` | M14, M33.1, M41, M58 |
| `today_ui.rs`, `today_cards.rs` | M13, M31, M32, M52.1, M52.2 |
| `manager_ui.rs` | M13, M21.2, M52.1, M52.2 |
| `notify_ui.rs` | M21.2, M42, M52.2 |
| `settings_ui.rs` | M25, M35, M53 |
| `paths.rs`, `lib.rs` | M10 first; later slices only add lines |
| `browser/mcp.rs` | M23, then M34 |
| `github_query.rs`, `github_api.rs` | M23, M33.2 |
| `transcript.rs` | M24, M30 |
| `crates/glasscoded/src/daemon.rs` | M13, M14, M20.3, M21.1, M57, M58 |
| `.github/workflows/ci.yml` | M03, M55 |
| `crates/glasscode/src/ci_fix.rs` | M13, M31, M40 |
| `crates/glasscode/src/manager_tick.rs` | M13, M31, M56 |
| `session_start.rs`, `switch_ui.rs`, `terminal/` | M13 |
| `crates/glasscode/src/auto_review.rs` | M20.2, M23 |
| `history.rs`, `discover.rs` | M30, M35, M50 |

## 9. Conditions to stop

A subagent stops and reports when:
- its slice needs a new crate, or a change in `vendor/`;
- a test needs a live model in CI;
- a limit it builds cannot be placed on a plane of design section 7;
- v1 behaviour would change while `engine` is `v1` (rule 2);
- a golden of M12 changes;
- a migration cannot be undone;
- a proof it depends on failed and its fallback is not written;
- a step needs the person's real login, home or memory.

The main agent stops the plan, and the owner decides, on the stop conditions of the proofs, or when P09 shows that fewer than a fifth of the person's prompts are typed in a composer (D1 tiers).

## 10. Review focus

| Wave | What the reviewer checks first |
|---|---|
| 0 | Proof results are real outputs. Scratch homes only. The stand-ins replay recorded bytes. |
| 1 | No IO in `parse` or `decide`. Callers come only from channels. Every input site records its origin. The journal never blocks the main thread. |
| 2 | No path literal in profiles. Briefs only from recorded commits. Every handler goes through `decide()`. `human_only` is respected. Planted-text evals pass. |
| 3 | Every turn closes exactly once. Claims are fsynced before acting. Coverage tests compare against the UI, not the registry itself. |
| 4 | Parity tests against v1. No model anywhere. Synced playbooks are not approved. |
| 5 | The tool list is exactly Glasscode's. Nothing inherited from the person's setup. The budget holds. Failure states are in plain words. |

## 11. Decisions for the owner

| # | Question | Proposal | Slices that wait |
|---|---|---|---|
| O1 | May an Autopilot project ever merge? | **Accepted 2026-11-19:** No. Merging stays yours. | M21.1 |
| O2 | Project memory: yours or the team's? | **Accepted 2026-11-19:** Yours. Team knowledge goes to `AGENTS.md` or `CLAUDE.md` through a PR. | M20.1 |
| O3 | Level of a new project | **Accepted 2026-11-19:** Careful; Standard when you own the repository | M21.1, M35 |
| O4 | Claude's and Codex's own memories | **Accepted 2026-11-19:** One trusted memory plus agent notes (D19) | M22 |
| O5 | Voice | **Moved (2026-11-19)** to a separate voice plan. This plan leaves `voice/` as it is. | — |
| O6 | Keep the Mac awake during automation turns on mains power | **Accepted 2026-11-19:** Yes; never block lid-close sleep | M41 |
| O7 | Forge wall and brokered push | **Accepted 2026-11-19:** Forge rules required for Autopilot pushes; brokered push only if P14 passes | M23 |
| O8 | What the journal keeps of prompts | **Accepted 2026-11-19:** A hash plus a 200-character excerpt, 30 days | M14 |
| O9 | Process: branch, who merges, CI gates, live-eval budget per week, minimum CLI versions, Windows scope | **Accepted 2026-11-19:** `manager-v2`; the owner merges; the gates of section 3; 200 prompts a week; the approved versions of M01; Windows as in D30 | M02, M03 |
| O10 | Run from install | **Accepted 2026-11-19:** The first run offers it; consent before reading; Careful everywhere | M35 |
| O11 | Onboarding questions and "finds work" in release 1 | **Accepted 2026-11-19:** The study pass and its questions in release 1 (M54); finding work after it (M60) | M54 |
| O12 | Where the manager lives | **Decided 2026-11-19:** one manager, present on the Mac and the server, acting from one home at a time and moving by itself (D5). Automatic is the default once a server is paired, with Always on the server and Always this Mac as choices. | M57, M58 |
| O14 | Approvals from the phone and the glasses | **Deferred (2026-11-19)** to the separate phone and Meta glasses plan. Until then, approvals are answered in the Mac app only, and phone input stays tier 2. | — |
| O13 | Should `glasscode` typed in a terminal outside Glasscode keep full rights, as today? (Any program can claim to be that terminal.) | **Accepted 2026-11-19:** No: as a security fix for everyone, commands from other terminals may read, and anything else waits on a card in the app. Commands typed in a Glasscode shell tab keep full rights, because the app sees the keys. | M13 |

## 12. Traceability: requirements to slices and tests

| Req | Mechanism | Slices | Acceptance tests | Release 1 |
|---|---|---|---|---|
| R1 Do everything the IDE can do | One registry, inventory with classes, coverage tests, `ui.*` ops, `glasscode mcp` | M12, M33, M34 | `coverage.rs`, `ui_open_*`, M52.1 demonstration | yes; the "cannot do" list is published |
| R2 Sessions report; it knows what happens | Journal, turn state machine, reports, reconciler | M14, M30 | `one_report_per_turn` (both agents, every end cause), soak | yes |
| R3 Decides when asked | Chat lane with grants; delegated decisions | M21.2, M51, M61 | `grant_*`, M61 tests | partly; delegated decisions in M61 |
| R4 Scheduling, awareness | Playbooks, scheduler, wake queue; with the laptop closed, on the server | M40, M41, M51, M57, M58 | `sleep_catch_up_runs_once`, `person_message_preempts_wake`, `partition_gives_one_brief` | yes |
| R5 Rust, minimal, fast | No new crate, budgets in CI | all; M02, M55 | budget table of design section 9; dependency tally | yes; the LiveKit worker stays outside |
| R6 Subscriptions | Official CLIs in restricted profiles | M50 | P01, P02, `brain_tool_list_is_exactly_glasscode` | yes, subject to P01 and P02 |
| R7 Chat that controls the IDE | One conversation, focus snapshot, ops | M52 | M52.1 demonstration | yes; voice is a separate plan |
| R8 Memory per project, Markdown | Memory store, brief, search, caps | M20 | `retrieval_recall_at_5_floor`, `direct_edit_is_not_in_the_brief` | yes |
| R9 Learns on install | Moving in, parts 1 and 2 | M35, M54 | M35 demonstration, `nothing_read_before_consent` | yes, if O11 is accepted |
| R10 Managed vs careful projects | Levels, gate, profiles, forge wall | M21, M22, M23 | `careful_manager_session_new_asks`, `autopilot_push_needs_forge_rules` | partly; finding work in M60 |
| R11 One memory, survives worktrees | Words ledger, memory store, archive | M13, M20, M24 | `remove_archives_before_delete`, `outside_removal_archived_by_reconciler`, M20.2 demonstration | yes |
| R12 State of the art, every case | Comparison, evals, soak, fault injection | design section 12, M02, M55 | evals job, soak job | yes, as measured numbers |

## 13. Release 1

Release 1 is every slice of waves 0 to 5 (M00 to M58). It ships when:
- every row of section 12 marked "yes" has its tests green;
- the evals job is green;
- the soak passes;
- the three security reviews are closed;
- the owner's 48-hour run shows the budgets of design section 9;
- the owner signs off the acceptance list.

## 14. Reference load and soak

**Reference load** (`testkit::load`):
- 20 sessions on 8 projects, one repository with at least 100,000 files;
- 150 imported memory folders and 10 MB of memory;
- 2,000 events an hour and 30 days of journal.

**Soak.** Seven simulated days on the virtual clock, with sleep and wake, network loss, server partitions and restarts at random points. It asserts:
- zero duplicate effects;
- exactly one report per turn;
- disk use bounded by the retention rules;
- no growth in open files or threads;
- wakes per hour under the D6 cap.
