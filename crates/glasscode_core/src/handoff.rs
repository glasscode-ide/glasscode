//! Hand a session from one agent to another with its context.
//!
//! Glasscode writes a handoff note (what was happening, the uncommitted diff, and where the
//! previous transcript lives) and starts the next agent with a prompt that points at it. The
//! receiving agent does its own summarizing, so the handoff costs no extra model calls.

use std::path::PathBuf;

use crate::git;
use crate::model::Session;
use crate::paths;

#[derive(Clone, Debug)]
pub struct HandoffOptions {
    pub include_terminal: bool,
    pub include_diff: bool,
    pub include_transcript: bool,
    pub note: String,
}

impl Default for HandoffOptions {
    fn default() -> Self {
        Self {
            include_terminal: true,
            include_diff: true,
            include_transcript: true,
            note: String::new(),
        }
    }
}

pub struct HandoffPacket {
    pub path: PathBuf,
    pub prompt: String,
}

pub fn write_packet(
    session: &Session,
    from_label: &str,
    to_label: &str,
    terminal_tail: &str,
    options: &HandoffOptions,
) -> anyhow::Result<HandoffPacket> {
    paths::ensure_dirs()?;
    let path = paths::handoff_dir().join(format!(
        "{}-{}.md",
        &session.id[..8.min(session.id.len())],
        crate::model::now()
    ));
    let mut doc = String::new();
    doc.push_str(&format!("# Handoff: {from_label} → {to_label}\n\n"));
    doc.push_str(&format!(
        "- Session: {}\n- Worktree: {}\n- Branch: {}\n",
        session.title,
        session.worktree.display(),
        session.branch
    ));
    if !session.lineage.is_empty() {
        doc.push_str(&format!("- History: {}\n", session.lineage.join(" → ")));
    }
    if !options.note.trim().is_empty() {
        doc.push_str(&format!("\n## Note from the developer\n\n{}\n", options.note.trim()));
    }
    if options.include_transcript {
        if let Some(transcript) = &session.transcript_path {
            doc.push_str(&format!(
                "\n## Previous conversation\n\nThe full transcript (JSON Lines) is at:\n`{}`\n\
                 Read it when you need the earlier reasoning, decisions or open tasks.\n",
                transcript.display()
            ));
        }
    }
    if options.include_terminal && !terminal_tail.trim().is_empty() {
        doc.push_str("\n## Last screen of the previous session\n\n```text\n");
        doc.push_str(terminal_tail.trim_end());
        doc.push_str("\n```\n");
    }
    if options.include_diff {
        let diff = git::diff_all(&session.worktree, 80_000);
        if diff.trim().is_empty() {
            doc.push_str("\n## Uncommitted changes\n\nNone.\n");
        } else {
            doc.push_str("\n## Uncommitted changes\n\n```diff\n");
            doc.push_str(diff.trim_end());
            doc.push_str("\n```\n");
        }
    }
    std::fs::write(&path, doc)?;
    let prompt = format!(
        "You are taking over this coding session from {from_label}. Read the handoff notes at \
         {} first: they describe the work so far, the uncommitted changes in this worktree and \
         where the previous transcript is. Then give a short summary of where things stand and \
         continue with the next step.",
        path.display()
    );
    Ok(HandoffPacket { path, prompt })
}
