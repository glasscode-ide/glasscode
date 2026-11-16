//! Which session each running program belongs to, and what each session uses together (the
//! resources of the status bar). Pure: the programs come from `sys::process_use`, the roots
//! from the terminals of the sessions (`TerminalView::child_pid`).

use std::collections::{HashMap, HashSet};

use crate::sys::ProcessUse;

/// The first program of a session: the shell or the agent that its terminal started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Root {
    pub session: String,
    pub pid: u32,
}

/// What the programs of one session use together.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionUse {
    pub session: String,
    pub cpu_percent: f32,
    pub memory_bytes: u64,
    /// In the order of the list they came from.
    pub processes: Vec<ProcessUse>,
}

/// The programs of each session. A program belongs to the nearest root at or above it on the
/// chain of parents; one under no root is in no group. One group for each session of `roots`,
/// in their order (a session with more terminals has more roots), also when its programs have
/// ended (then it is empty and uses nothing). A root with process 0 has no program.
pub fn group(all: &[ProcessUse], roots: &[Root]) -> Vec<SessionUse> {
    let mut groups: Vec<SessionUse> = Vec::new();
    for root in roots {
        if !groups.iter().any(|g| g.session == root.session) {
            groups.push(SessionUse { session: root.session.clone(), cpu_percent: 0.0, memory_bytes: 0, processes: Vec::new() });
        }
    }
    let parents = parents(all);
    let roots = root_sessions(roots);
    for p in all {
        let Some(session) = session_of(p.pid, &parents, &roots) else { continue };
        if let Some(group) = groups.iter_mut().find(|g| g.session == session) {
            group.cpu_percent += p.cpu_percent;
            group.memory_bytes = group.memory_bytes.saturating_add(p.memory_bytes);
            group.processes.push(p.clone());
        }
    }
    groups
}

/// The session of the program `pid`: the nearest root at or above it, or None.
pub fn owner(pid: u32, all: &[ProcessUse], roots: &[Root]) -> Option<String> {
    session_of(pid, &parents(all), &root_sessions(roots)).map(str::to_string)
}

fn parents(all: &[ProcessUse]) -> HashMap<u32, u32> {
    all.iter().map(|p| (p.pid, p.parent)).collect()
}

/// The session of each root's process; the first root of a process counts.
fn root_sessions(roots: &[Root]) -> HashMap<u32, &str> {
    let mut sessions = HashMap::new();
    for root in roots.iter().filter(|r| r.pid != 0) {
        sessions.entry(root.pid).or_insert(root.session.as_str());
    }
    sessions
}

/// Up the chain of parents from `pid` to the first root. The chain ends at a program with no
/// parent (0) or one not in the list, and at a number seen before: a list read while programs
/// start and end can have a loop.
fn session_of<'a>(pid: u32, parents: &HashMap<u32, u32>, roots: &HashMap<u32, &'a str>) -> Option<&'a str> {
    let mut seen = HashSet::new();
    let mut pid = pid;
    while pid != 0 && seen.insert(pid) {
        if let Some(&session) = roots.get(&pid) {
            return Some(session);
        }
        pid = *parents.get(&pid)?;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: u32, parent: u32, name: &str, cpu_percent: f32, memory_bytes: u64) -> ProcessUse {
        ProcessUse { pid, parent, name: name.to_string(), cpu_percent, memory_bytes }
    }

    fn root(session: &str, pid: u32) -> Root {
        Root { session: session.to_string(), pid }
    }

    /// Glasscode (50) with the terminals of two sessions: a shell with a dev server and its
    /// helper, and an agent. Programs of the system around them.
    fn programs() -> Vec<ProcessUse> {
        vec![
            process(1, 0, "launchd", 0.5, 10),
            process(50, 1, "glasscode", 12.0, 400),
            process(100, 50, "zsh", 0.25, 5),
            process(101, 100, "node", 30.5, 200),
            process(102, 101, "esbuild", 2.25, 50),
            process(200, 50, "claude", 4.0, 300),
            process(300, 1, "other", 9.0, 1000),
        ]
    }

    fn pids(group: &SessionUse) -> Vec<u32> {
        group.processes.iter().map(|p| p.pid).collect()
    }

    #[test]
    fn children_count_for_their_root() {
        let groups = group(&programs(), &[root("web", 100), root("agent", 200)]);
        assert_eq!(groups.iter().map(|g| g.session.as_str()).collect::<Vec<_>>(), ["web", "agent"]);
        assert_eq!(pids(&groups[0]), [100, 101, 102]);
        assert_eq!((groups[0].cpu_percent, groups[0].memory_bytes), (33.0, 255));
        assert_eq!(pids(&groups[1]), [200]);
        assert_eq!((groups[1].cpu_percent, groups[1].memory_bytes), (4.0, 300));
        assert_eq!(owner(102, &programs(), &[root("web", 100)]).as_deref(), Some("web"));
        assert_eq!(owner(100, &programs(), &[root("web", 100)]).as_deref(), Some("web"));
    }

    #[test]
    fn a_program_with_no_root_is_in_no_group() {
        let all = programs();
        let roots = [root("web", 100), root("agent", 200)];
        let grouped: Vec<u32> = group(&all, &roots).iter().flat_map(pids).collect();
        for pid in [1, 50, 300] {
            assert!(!grouped.contains(&pid), "{pid} is in a group");
            assert_eq!(owner(pid, &all, &roots), None, "{pid}");
        }
        // A number that is not in the list at all, and no roots.
        assert_eq!(owner(999, &all, &roots), None);
        assert!(group(&all, &[]).is_empty());
    }

    #[test]
    fn a_parent_loop_does_not_hang() {
        // Read while programs started and ended: 400 and 401 name each other, 402 is itself.
        let mut all = programs();
        all.extend([process(400, 401, "a", 1.0, 1), process(401, 400, "b", 1.0, 1), process(402, 402, "c", 1.0, 1), process(403, 400, "d", 1.0, 1)]);
        let groups = group(&all, &[root("web", 100)]);
        assert_eq!(pids(&groups[0]), [100, 101, 102]);
        for pid in [400, 401, 402, 403] {
            assert_eq!(owner(pid, &all, &[root("web", 100)]), None, "{pid}");
        }
        // A loop that has a root in it still finds it.
        assert_eq!(owner(403, &all, &[root("loop", 401)]).as_deref(), Some("loop"));
    }

    #[test]
    fn the_nearest_root_wins_and_a_session_with_two_terminals_is_one_group() {
        let all = programs();
        // A second terminal of "web" (the agent's), and a session whose root is under "web".
        let groups = group(&all, &[root("web", 100), root("inner", 101), root("web", 200)]);
        assert_eq!(groups.iter().map(|g| g.session.as_str()).collect::<Vec<_>>(), ["web", "inner"]);
        assert_eq!(pids(&groups[0]), [100, 200]);
        assert_eq!(pids(&groups[1]), [101, 102]);
        assert_eq!(owner(102, &all, &[root("web", 100), root("inner", 101)]).as_deref(), Some("inner"));
    }

    #[test]
    fn a_session_whose_programs_ended_is_empty() {
        let groups = group(&programs(), &[root("ended", 777), root("none", 0)]);
        assert_eq!(groups, [SessionUse { session: "ended".into(), cpu_percent: 0.0, memory_bytes: 0, processes: Vec::new() }, SessionUse { session: "none".into(), cpu_percent: 0.0, memory_bytes: 0, processes: Vec::new() }]);
        // Process 0 is no program: what has no parent is not under it.
        assert_eq!(owner(1, &programs(), &[root("none", 0)]), None);
        assert_eq!(owner(0, &programs(), &[root("none", 0)]), None);
    }
}
