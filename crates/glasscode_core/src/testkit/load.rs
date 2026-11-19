//! The reference load of slice plan section 14 (M02 step 5), as generators that later slices
//! measure their budgets against (design section 9): 20 sessions on 8 projects, one repository with
//! at least 100,000 files, 150 imported memory folders and 10 MB of memory, 2,000 events an hour
//! and 30 days of journal. Everything comes from a seed, so two runs see the same load.
//!
//! The tests here run a small part of it; the full load is for the soak (M55) and the budget
//! measures of the slices that own each path (M14, M20.2, M31).

use std::path::Path;
use std::time::{Duration, Instant};

use super::clock::HOUR;
use super::sim::Input;

/// A small, fast generator of numbers from a seed (SplitMix64): the same seed, the same numbers,
/// on every system and every version of the kit.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed)
    }

    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A number from 0 to `n - 1`.
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }
}

/// The numbers of the reference load.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reference {
    pub sessions: usize,
    pub projects: usize,
    /// Files of the largest repository.
    pub big_repo_files: usize,
    pub memory_folders: usize,
    pub memory_bytes: usize,
    pub events_per_hour: usize,
    pub journal_days: usize,
}

impl Reference {
    /// Slice plan section 14.
    pub const FULL: Reference = Reference { sessions: 20, projects: 8, big_repo_files: 100_000, memory_folders: 150, memory_bytes: 10 * 1024 * 1024, events_per_hour: 2_000, journal_days: 30 };

    /// The same shape, smaller, for a test that runs every time.
    pub fn small() -> Reference {
        Reference { big_repo_files: 300, memory_folders: 12, memory_bytes: 64 * 1024, journal_days: 1, ..Reference::FULL }
    }

    /// The session ids, `s01`…, each with its project, `p1`…, spread over the projects.
    pub fn sessions(&self) -> Vec<(String, String)> {
        (0..self.sessions).map(|n| (format!("s{:02}", n + 1), format!("p{}", n % self.projects + 1))).collect()
    }
}

/// The events of `days` days from `start_ms`, at the reference rate, a little uneven: hook events
/// of Claude sessions (a prompt, its stop, now and then a notification) and the turn ends of Codex
/// sessions (every fourth session), in time order. It is lazy: 30 days are 1.44 million events.
pub fn events(reference: Reference, start_ms: u64, days: usize, seed: u64) -> impl Iterator<Item = (u64, Input)> {
    let sessions = reference.sessions();
    let gap = HOUR / reference.events_per_hour as u64;
    let total = reference.events_per_hour as u64 * 24 * days as u64;
    let mut rng = Rng::new(seed);
    // Each session's turn is open or not, so prompts and stops alternate.
    let mut open = vec![false; sessions.len()];
    (0..total).map(move |n| {
        // Evenly spaced, each moved by up to a third of the gap: the order stays, the rate holds.
        let at = start_ms + n * gap + rng.below(gap / 3);
        let index = rng.below(sessions.len() as u64) as usize;
        let (session, _project) = &sessions[index];
        let input = if index % 4 == 3 {
            let kind = if open[index] { "task_complete" } else { "task_started" };
            open[index] = !open[index];
            Input::Rollout { session: session.clone(), line: format!(r#"{{"timestamp":"{}","type":"event_msg","payload":{{"type":"{kind}"}}}}"#, super::clock::iso_ms(at)) }
        } else {
            let event = match (open[index], rng.below(10)) {
                (true, 0) => "Notification",
                (true, _) => "Stop",
                (false, _) => "UserPromptSubmit",
            };
            if event != "Notification" {
                open[index] = !open[index];
            }
            Input::Hook { session: session.clone(), payload: format!(r#"{{"session_id":"{session}","hook_event_name":"{event}"}}"#) }
        };
        (at, input)
    })
}

/// Words of the memory the generator writes: enough for search to rank, the same for a seed.
const WORDS: &[&str] = &[
    "billing", "invoice", "retry", "backoff", "login", "session", "token", "release", "deploy", "staging", "review", "migration", "schema", "cache", "queue", "worker", "timeout", "flaky",
    "test", "build", "pipeline", "owner", "decision", "rollback", "feature", "flag", "webhook", "export", "import", "search", "index", "latency", "budget", "alert", "metrics",
];

/// Writes `folders` memory folders (`projects/<key>/` with Markdown files of headed sections)
/// under `dir`, about `bytes` in all, and returns the bytes written.
pub fn write_memory(dir: &Path, folders: usize, bytes: usize, seed: u64) -> usize {
    let mut rng = Rng::new(seed);
    let per_folder = bytes / folders.max(1) + 1;
    let mut written = 0;
    for folder in 0..folders {
        let key = format!("project-{folder:03}");
        let project = dir.join("projects").join(&key);
        std::fs::create_dir_all(project.join("knowhow")).unwrap();
        let mut text = format!("# {key}\n\n");
        let mut section = 0;
        while text.len() < per_folder {
            section += 1;
            text.push_str(&format!("## {} {}\n\n", rng.pick(WORDS), rng.pick(WORDS)));
            for _ in 0..3 {
                let sentence: Vec<&str> = (0..12).map(|_| *rng.pick(WORDS)).collect();
                text.push_str(&format!("- {}. <!-- id=e{folder}x{section} status=learned kind=fact -->\n", sentence.join(" ")));
            }
            text.push('\n');
        }
        written += text.len();
        std::fs::write(project.join("decisions.md"), &text).unwrap();
    }
    written
}

/// Writes a repository's working files: `count` small files in folders of 100.
pub fn write_repo_files(dir: &Path, count: usize) {
    for n in 0..count {
        let folder = dir.join(format!("src/m{:04}", n / 100));
        if n % 100 == 0 {
            std::fs::create_dir_all(&folder).unwrap();
        }
        std::fs::write(folder.join(format!("f{n:06}.ts")), format!("export const v{n} = {n};\n")).unwrap();
    }
}

/// The 95th percentile of `samples` (the nearest rank).
pub fn p95(samples: &mut [Duration]) -> Duration {
    assert!(!samples.is_empty(), "no samples");
    samples.sort();
    let rank = (samples.len() * 95).div_ceil(100);
    samples[rank.max(1) - 1]
}

/// Runs `f` `runs` times and gives its 95th percentile: how a slice checks a budget of design
/// section 9.
pub fn measure(runs: usize, mut f: impl FnMut()) -> Duration {
    let mut samples: Vec<Duration> = (0..runs)
        .map(|_| {
            let started = Instant::now();
            f();
            started.elapsed()
        })
        .collect();
    p95(&mut samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reference_load_is_section_14() {
        let full = Reference::FULL;
        assert_eq!((full.sessions, full.projects, full.big_repo_files, full.memory_folders, full.memory_bytes, full.events_per_hour, full.journal_days), (20, 8, 100_000, 150, 10_485_760, 2_000, 30));
        let sessions = full.sessions();
        let projects: std::collections::BTreeSet<&str> = sessions.iter().map(|(_, p)| p.as_str()).collect();
        assert_eq!((sessions.len(), projects.len()), (20, 8));
    }

    #[test]
    fn events_come_at_the_reference_rate_in_order() {
        let start = super::super::clock::utc_ms(2026, 11, 19, 0, 0);
        let hour: Vec<(u64, Input)> = events(Reference::FULL, start, 1, 7).take_while(|(at, _)| *at < start + HOUR).collect();
        assert!((1_990..=2_000).contains(&hour.len()), "{}", hour.len());
        assert!(hour.windows(2).all(|w| w[0].0 <= w[1].0), "in time order");
        let sessions: std::collections::BTreeSet<&str> = hour.iter().map(|(_, input)| input.session().unwrap()).collect();
        assert_eq!(sessions.len(), 20);
        assert!(hour.iter().any(|(_, input)| matches!(input, Input::Rollout { .. })) && hour.iter().any(|(_, input)| matches!(input, Input::Hook { .. })));
        // A day is 48,000 events, and the same seed gives the same day.
        assert_eq!(events(Reference::FULL, start, 1, 7).count(), 48_000);
        assert!(events(Reference::FULL, start, 1, 7).zip(events(Reference::FULL, start, 1, 7)).all(|(a, b)| a == b));
        assert!(events(Reference::FULL, start, 1, 7).zip(events(Reference::FULL, start, 1, 8)).any(|(a, b)| a != b));
        // Every payload is JSON, as a hook command writes it.
        for (_, input) in &hour {
            let text = match input {
                Input::Hook { payload, .. } => payload,
                Input::Rollout { line, .. } => line,
                _ => unreachable!(),
            };
            serde_json::from_str::<serde_json::Value>(text).unwrap();
        }
    }

    #[test]
    fn memory_and_repository_reach_their_size() {
        let dir = tempfile::tempdir().unwrap();
        let small = Reference::small();
        let written = write_memory(dir.path(), small.memory_folders, small.memory_bytes, 1);
        assert!(written >= small.memory_bytes && written < small.memory_bytes * 2, "{written}");
        assert_eq!(std::fs::read_dir(dir.path().join("projects")).unwrap().count(), small.memory_folders);
        let again = tempfile::tempdir().unwrap();
        write_memory(again.path(), small.memory_folders, small.memory_bytes, 1);
        let file = "projects/project-003/decisions.md";
        assert_eq!(std::fs::read(dir.path().join(file)).unwrap(), std::fs::read(again.path().join(file)).unwrap());
        write_repo_files(&dir.path().join("repo"), small.big_repo_files);
        let files: usize = std::fs::read_dir(dir.path().join("repo/src")).unwrap().flatten().map(|d| std::fs::read_dir(d.path()).unwrap().count()).sum();
        assert_eq!(files, 300);
    }

    #[test]
    fn the_95th_percentile() {
        let mut samples: Vec<Duration> = (1..=100).map(Duration::from_millis).collect();
        assert_eq!(p95(&mut samples), Duration::from_millis(95));
        assert_eq!(p95(&mut [Duration::from_millis(3)]), Duration::from_millis(3));
        let mut runs = 0;
        measure(20, || runs += 1);
        assert_eq!(runs, 20);
    }
}
