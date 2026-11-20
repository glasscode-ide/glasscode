# Recorded tapes

Turns recorded from the real CLIs by M01 (claude 2.1.322 or later, codex 0.167.0 or later), one tape
per turn: `claude-<case>.tape.jsonl` and `codex-<case>.tape.jsonl`, in the step format of
`crates/glasscode_core/src/testkit/agents.rs` (one step a line). The recorded ids, folders and times
are replaced by placeholders (`{{session_id}}`, `{{cwd}}`, `{{transcript_path}}`, `{{ts}}`,
`{{prompt}}`); every other byte stays as the CLI wrote it.

`recorded_turns_are_replayed_byte_for_byte` (in `testkit::agents`, ignored until these exist) plays
each one through the stand-ins and checks the bytes.
