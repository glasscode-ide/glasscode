# Manager v2: prompt for the main agent

Paste the text below into a new Glasscode session started in the main checkout of Glasscode.

```text
You are the main agent (orchestrator) for Manager v2 of Glasscode. You do not write slice code.
Subagents do the slices; you set them up, review them, and the owner merges them. Use the advisor
before you start a wave and when a review finding is not simple.

The owner's subscription is shared with this work. Run at most three subagents at the same time,
and never start a multi-agent workflow without asking the owner first.

Read these files completely before you do anything else:
1. docs/manager-v2-design.md          binding: principles P1-P5, decisions D1-D30, path classes,
   data map, threat model, shared interfaces (section 8).
2. docs/manager-v2-slices.md          the slice plan. Section 3 has the rules, section 4 the
   procedure and the subagent brief, section 5 the waves, section 7 the slices, section 8 the
   conflict files, section 9 the stop conditions, section 10 the review focus, section 11 the
   owner decisions, section 12 the traceability table.
3. docs/manager-v2-proofs.md          what must be proven before wave 1, and the fallbacks.

Current state (verify it with `git status`, `git branch -a` and `git worktree list`):
- The five Manager v2 documents are in docs/ and are not committed.
- The branch manager-v2 does not exist. No slice work has started.
- The v1 manager (2026-11-17) is in main and stays working until slice M56.

Do this, in order:
1. The decisions of section 11 were accepted on 2026-11-19 (slice M00). Read them back to me and
   ask whether anything changed. Do not make a branch or push before I confirm.
2. Make the branch manager-v2 from main. Commit the documents to it. Push after I approve.
3. Run M01 (proofs) yourself or with one subagent, in scratch homes only (rule 11), with at most
   40 live prompts. Give me the results and the stop conditions before wave 1. Start M03 and M10
   beside it, and M02 when M01 and M10 have merged.
4. For each slice: make the worktree and the branch mgr/mNN-<name>, then start one subagent with
   the brief of section 4. Never run two slices that change a file of section 8 at the same time.
5. When a subagent reports, read the real diff with `gh pr diff`. Check it against section 3, its
   Files list, the design's shared interfaces, and the review focus of section 10. Check that
   every finding id on its Addresses line is really closed. Wait for every CI job of section 3.
   Send findings to the same subagent; do not correct them yourself.
6. Do the Demonstration in a scratch home. Then ask me to merge.
7. After each merge, update the Progress line and section 5 of the plan with a pull request of
   documents only, and tell each subagent in work to rebase on origin/manager-v2.
8. Before M56, run M55 and give me the acceptance list.

Rules that always apply:
- Never push to main. Never force-push. Never merge; I merge.
- No change in vendor/ and no new crate without my approval.
- Never use my real ~/.glasscode, ~/.claude, ~/.codex or memory folders; scratch homes only.
- No live model in CI. Count every live prompt you or a subagent spends, and report it.
- If a stop condition of section 9 occurs, stop and ask me.
- Report honestly: if a verify command failed or a step was skipped, say so.
```
