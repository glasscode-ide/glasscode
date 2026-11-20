//! Children: large work split into sessions (M61, crit-13). A session (or the manager for one)
//! starts a child for a part of its work with `session.child`:
//!
//! - **Fresh context.** The child is a new session with a new conversation: it gets the task and
//!   nothing of the parent's turns.
//! - **No new tools.** It runs in the parent's worktree, with the parent's agent and account, and
//!   as automation (`gate::SessionOrigin::Automation`): never with more than the parent has.
//! - **A summary back.** When the child ends its first turn, what it said last goes back to the
//!   parent through its delivery queue, once ([`summary_message`]).
//!
//! `manager/children.json` keeps which child is whose, and whether its summary went back.

use serde::{Deserialize, Serialize};

/// One child session.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Child {
    pub child: String,
    pub parent: String,
    pub task: String,
    pub started_ms: u64,
    /// When its summary went back to the parent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reported_ms: Option<u64>,
    /// Its summary, made when its turn ended, until it is typed into the parent (the turn's id and
    /// the text): kept here, so a parent that is closed, or a quit, loses nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending: Option<(String, String)>,
}

/// `manager/children.json`. Damaged, it starts empty: a child's summary is then not sent back,
/// and its parent can read the child itself.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Children {
    pub children: Vec<Child>,
}

impl crate::store::Store for Children {
    const NAME: &'static str = "manager/children";
    const VERSION: u32 = 1;
    const ON_DAMAGE: crate::store::OnDamage = crate::store::OnDamage::Empty;

    fn path() -> std::path::PathBuf {
        crate::manager::store::dir().join("children.json")
    }
}

/// Children kept: the newest this many (a child long reported is history).
const KEEP: usize = 200;

impl Children {
    /// Note a child that started.
    pub fn add(&mut self, child: Child) {
        self.children.retain(|c| c.child != child.child);
        self.children.push(child);
        if self.children.len() > KEEP {
            let over = self.children.len() - KEEP;
            self.children.drain(..over);
        }
    }

    /// The child `session` is, while its summary has not gone back.
    pub fn waiting(&self, session: &str) -> Option<&Child> {
        self.children.iter().find(|c| c.child == session && c.reported_ms.is_none())
    }

    /// Its summary went back.
    pub fn reported(&mut self, session: &str, at_ms: u64) -> bool {
        match self.children.iter_mut().find(|c| c.child == session && c.reported_ms.is_none()) {
            Some(child) => {
                child.reported_ms = Some(at_ms);
                child.pending = None;
                true
            }
            None => false,
        }
    }

    /// The child ended a turn: its summary waits to go back (the newest turn's, when there are
    /// several before it goes).
    pub fn summary(&mut self, session: &str, turn: &str, text: &str) -> bool {
        match self.children.iter_mut().find(|c| c.child == session && c.reported_ms.is_none()) {
            Some(child) => {
                child.pending = Some((turn.to_string(), text.to_string()));
                true
            }
            None => false,
        }
    }

    /// The summaries that wait to go back.
    pub fn due(&self) -> Vec<&Child> {
        self.children.iter().filter(|c| c.reported_ms.is_none() && c.pending.is_some()).collect()
    }
}

/// The child's first message: its task, and what it gives back.
pub fn first_message(task: &str, parent_title: &str) -> String {
    format!(
        "{}\n\nThis is one part of the work of the session “{parent_title}”, which started you for it, in its worktree. Do only this part, and do not commit: leave your changes in the worktree for “{parent_title}” to commit. When you are done, end your turn with a short summary of what you found or changed (files, what is left): it goes back to “{parent_title}” as it is.",
        task.trim()
    )
}

/// The longest summary that goes back.
const SUMMARY_MAX: usize = 3_000;

/// What goes back to the parent when its child ended its turn: what the child said last, or the
/// question it stopped on, or how its turn ended when it said nothing.
pub fn summary_message(child_title: &str, task: &str, report: &crate::turns::TurnReport) -> String {
    let said = report.last_reply.trim();
    let said = if said.chars().count() > SUMMARY_MAX { format!("{}…", said.chars().take(SUMMARY_MAX).collect::<String>()) } else { said.to_string() };
    let task: String = task.trim().lines().next().unwrap_or_default().chars().take(120).collect();
    let mut out = format!("The child session “{child_title}” you started for “{task}” ended its turn ({}).", report.end.name());
    if let Some(question) = report.question.as_deref().filter(|q| !q.trim().is_empty()) {
        out.push_str(&format!(" It stopped on a question: {}", question.trim()));
    }
    if !report.commits.is_empty() {
        out.push_str(&format!(" Its commits: {}.", report.commits.join(", ")));
    }
    if said.is_empty() {
        out.push_str(" It said nothing more.");
    } else {
        out.push_str(&format!(" Its summary:\n\n{said}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_child_reports_back_once() {
        let mut children = Children::default();
        children.add(Child { child: "c1".into(), parent: "p1".into(), task: "Find where login fails.\nMore.".into(), started_ms: 1, reported_ms: None, pending: None });
        assert_eq!(children.waiting("c1").map(|c| c.parent.as_str()), Some("p1"));
        // The summary waits (across a quit: it is in the file) until it went back.
        assert!(children.summary("c1", "t1", "Found it.") && children.due().len() == 1);
        let again: Children = serde_json::from_str(&serde_json::to_string(&children).unwrap()).unwrap();
        assert_eq!(again.due()[0].pending.as_ref().map(|(t, _)| t.as_str()), Some("t1"));
        assert!(children.reported("c1", 5));
        assert!(children.due().is_empty() && !children.summary("c1", "t2", "more"));
        assert!(children.waiting("c1").is_none() && !children.reported("c1", 6));
        let report = crate::turns::TurnReport {
            v: 1,
            session: "c1".into(),
            turn: "t1".into(),
            started_ms: 1,
            ended_ms: 2,
            end: crate::turns::TurnEnd::Completed,
            tools: 3,
            failed_tools: Vec::new(),
            commands: Vec::new(),
            files: Vec::new(),
            commits: vec!["abc123".into()],
            pushed: false,
            last_reply: "It fails in auth.rs: the token is not refreshed.".into(),
            question: None,
            background: Vec::new(),
            by_agent: true,
            partial: false,
        };
        let text = summary_message("Login part", "Find where login fails.\nMore.", &report);
        assert!(text.starts_with("The child session “Login part” you started for “Find where login fails.” ended its turn"), "{text}");
        assert!(text.contains("Its commits: abc123.") && text.ends_with("the token is not refreshed."));
        assert!(first_message("Find where login fails.", "Fix login").contains("goes back to “Fix login”"));
        for n in 0..(KEEP + 5) {
            children.add(Child { child: format!("x{n}"), ..Child::default() });
        }
        assert_eq!(children.children.len(), KEEP);
    }
}
