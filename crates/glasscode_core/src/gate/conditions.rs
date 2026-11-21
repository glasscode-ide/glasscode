//! Typed conditions of a project's policy (crit-14): the branch, paths, hours and person rules a
//! careful project is shown to have, checked by the gate against an op's targets. Each names its
//! plane: the gate enforces them on Glasscode ops; what a session's own git or shell does is for
//! the session profile (M22) and the forge (D29).

use std::path::Path;

use serde::{Deserialize, Serialize};

/// One condition of a project's policy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Condition {
    /// Ops on these branches ask, whatever the level ("main", "release/*").
    Branch { not: Vec<String> },
    /// Ops on paths that match these globs are refused ("secrets/**", "~/work/vault/**"). A glob
    /// that starts with `/` or `~/` is matched against the whole path, any other against its end.
    Paths { deny: Vec<String> },
    /// Non-person callers act only in these hours of the local day (minutes since midnight, from
    /// `local.0` up to `local.1`, past midnight when the end is the smaller) on these days (bit 0 is
    /// Sunday; 0 is every day). Outside them every op but a read asks.
    Hours { local: (u16, u16), days: u8 },
    /// Only these people may OK the op: the card says so and asks them.
    Person { only: Vec<String> },
}

/// What a condition says of an op.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Ask(String),
    Never(String),
}

/// What a condition is checked against: what the op touches, and the time.
#[derive(Clone, Copy, Debug)]
pub struct Facts<'a> {
    pub branches: &'a [String],
    pub paths: &'a [std::path::PathBuf],
    /// The local wall time, in [`crate::clock::local_minutes`]; None when it cannot be read.
    pub local_minutes: Option<i64>,
    pub home: &'a Path,
}

impl Condition {
    /// The condition, checked against what an op touches. Reads always pass: conditions are about
    /// what changes.
    pub fn check(&self, facts: &Facts) -> Verdict {
        match self {
            Condition::Branch { not } => match facts.branches.iter().find(|branch| not.iter().any(|glob| glob_match(glob, branch))) {
                Some(branch) => Verdict::Ask(format!("the policy asks before anything on the branch {branch}")),
                None => Verdict::Pass,
            },
            Condition::Paths { deny } => {
                for path in facts.paths {
                    if let Some(glob) = deny.iter().find(|glob| path_matches(glob, path, facts.home)) {
                        return Verdict::Never(format!("the policy keeps {} ({glob}) out of reach", path.display()));
                    }
                }
                Verdict::Pass
            }
            Condition::Hours { local, days } => {
                // A time that cannot be read is outside the hours.
                let inside = facts.local_minutes.is_some_and(|minutes| within_hours(*local, *days, minutes));
                if inside { Verdict::Pass } else { Verdict::Ask(format!("outside the hours the policy allows ({})", hours_text(*local, *days))) }
            }
            Condition::Person { only } => Verdict::Ask(format!("only {} may allow this", only.join(" or "))),
        }
    }

    /// The condition in words, for Settings and the header of `POLICY.md`.
    pub fn describe(&self) -> String {
        match self {
            Condition::Branch { not } => format!("Asks before anything on the branches {}.", not.join(", ")),
            Condition::Paths { deny } => format!("Never touches {}.", deny.join(", ")),
            Condition::Hours { local, days } => format!("Acts only {}; outside them it asks.", hours_text(*local, *days)),
            Condition::Person { only } => format!("Only {} may allow what it asks.", only.join(" or ")),
        }
    }
}

fn within_hours(local: (u16, u16), days: u8, minutes: i64) -> bool {
    let day = crate::clock::weekday(minutes);
    if days != 0 && days & (1 << day) == 0 {
        return false;
    }
    let now = crate::clock::minute_of_day(minutes);
    let (from, to) = local;
    if from <= to { (from..to).contains(&now) } else { now >= from || now < to }
}

const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

fn hours_text(local: (u16, u16), days: u8) -> String {
    let time = |m: u16| format!("{:02}:{:02}", m / 60, m % 60);
    let on = if days == 0 || days & 0x7f == 0x7f { "every day".to_string() } else { (0..7).filter(|d| days & (1 << d) != 0).map(|d| DAYS[d]).collect::<Vec<_>>().join(", ") };
    format!("{} to {}, {on}", time(local.0), time(local.1))
}

/// A path as text with `/` between its parts, `.` and `..` taken out (by the text only: no link
/// is read, as `decide` does no IO).
fn lexical(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let absolute = text.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in text.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    let joined = parts.join("/");
    if absolute { format!("/{joined}") } else { joined }
}

fn path_matches(glob: &str, path: &Path, home: &Path) -> bool {
    let text = lexical(path);
    if let Some(rest) = glob.strip_prefix("~/") {
        return glob_match(&format!("{}/{rest}", home.to_string_lossy().replace('\\', "/").trim_end_matches('/')), &text);
    }
    if glob.starts_with('/') {
        return glob_match(glob, &text);
    }
    // Any end of the path, by whole parts.
    let parts: Vec<&str> = text.split('/').filter(|p| !p.is_empty()).collect();
    (0..parts.len()).any(|start| glob_match(glob, &parts[start..].join("/")))
}

/// A glob: `*` any run within one part of a path, `**` any run across parts, `?` one character.
/// No crate (P4): this is all the policy's globs use.
pub fn glob_match(glob: &str, text: &str) -> bool {
    fn go(g: &[char], t: &[char]) -> bool {
        match g.first() {
            None => t.is_empty(),
            Some('*') if g.get(1) == Some(&'*') => {
                // `**/` may match no part at all ("src/**/x" matches "src/x"), and otherwise ends
                // where a part does.
                if g.get(2) == Some(&'/') {
                    (0..=t.len()).filter(|&i| i == 0 || t[i - 1] == '/').any(|i| go(&g[3..], &t[i..]))
                } else {
                    (0..=t.len()).any(|i| go(&g[2..], &t[i..]))
                }
            }
            Some('*') => (0..=t.len()).take_while(|&i| i == 0 || t[i - 1] != '/').any(|i| go(&g[1..], &t[i..])),
            Some('?') => t.first().is_some_and(|c| *c != '/') && go(&g[1..], &t[1..]),
            Some(c) => t.first() == Some(c) && go(&g[1..], &t[1..]),
        }
    }
    let g: Vec<char> = glob.chars().collect();
    let t: Vec<char> = text.chars().collect();
    go(&g, &t)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn globs() {
        assert!(glob_match("main", "main"));
        assert!(!glob_match("main", "maint"));
        assert!(glob_match("release/*", "release/1.2"));
        assert!(!glob_match("release/*", "release/1.2/hotfix"));
        assert!(glob_match("release/**", "release/1.2/hotfix"));
        assert!(glob_match("src/**/x.rs", "src/x.rs"));
        assert!(glob_match("src/**/x.rs", "src/a/b/x.rs"));
        assert!(!glob_match("src/**/x.rs", "src/ab/cx.rs"));
        assert!(glob_match("*.env", "prod.env"));
        assert!(glob_match("v?", "v1"));
        assert!(!glob_match("v?", "v12"));
    }

    fn facts<'a>(branches: &'a [String], paths: &'a [PathBuf], local_minutes: Option<i64>) -> Facts<'a> {
        Facts { branches, paths, local_minutes, home: Path::new("/home/dev") }
    }

    /// A path condition is matched on the path with `.` and `..` taken out: an op's path written
    /// through a sibling (`app/../vault/x`) is the path it names, and is kept out the same way.
    #[test]
    fn a_path_through_dot_dot_is_the_path_it_names() {
        let paths = Condition::Paths { deny: vec!["vault/**".into(), "~/secret/**".into(), "/srv/keys/**".into()] };
        for through in ["/code/app/../vault/x.txt", "/home/dev/work/../secret/key", "/srv/./keys/a", "/srv/app/../keys/a"] {
            let path = [PathBuf::from(through)];
            assert!(matches!(paths.check(&facts(&[], &path, None)), Verdict::Never(_)), "{through}");
        }
        // `..` that leaves the kept folder is out of it.
        let out = [PathBuf::from("/code/vault/../app/x")];
        assert_eq!(paths.check(&facts(&[], &out, None)), Verdict::Pass);
    }

    #[test]
    fn branch_and_paths() {
        let main = ["main".to_string()];
        let feature = ["feat/x".to_string()];
        let branch = Condition::Branch { not: vec!["main".into(), "release/*".into()] };
        assert!(matches!(branch.check(&facts(&main, &[], None)), Verdict::Ask(_)));
        assert_eq!(branch.check(&facts(&feature, &[], None)), Verdict::Pass);
        let paths = Condition::Paths { deny: vec!["vault/**".into(), "~/secret/**".into()] };
        let inside = [PathBuf::from("/home/dev/code/app/vault/keys")];
        let home = [PathBuf::from("/home/dev/secret/x")];
        let other = [PathBuf::from("/home/dev/code/app/src")];
        assert!(matches!(paths.check(&facts(&[], &inside, None)), Verdict::Never(_)));
        assert!(matches!(paths.check(&facts(&[], &home, None)), Verdict::Never(_)));
        assert_eq!(paths.check(&facts(&[], &other, None)), Verdict::Pass);
    }

    #[test]
    fn hours_and_days() {
        // 1970-01-05 was a Monday: day 4 counted from the Thursday epoch.
        let monday_10 = 4 * 1440 + 10 * 60;
        let monday_20 = 4 * 1440 + 20 * 60;
        let sunday_10 = 3 * 1440 + 10 * 60;
        let weekdays = 0b0111110;
        let office = Condition::Hours { local: (9 * 60, 18 * 60), days: weekdays };
        assert_eq!(office.check(&facts(&[], &[], Some(monday_10))), Verdict::Pass);
        assert!(matches!(office.check(&facts(&[], &[], Some(monday_20))), Verdict::Ask(_)));
        assert!(matches!(office.check(&facts(&[], &[], Some(sunday_10))), Verdict::Ask(_)));
        assert!(matches!(office.check(&facts(&[], &[], None)), Verdict::Ask(_)), "an unknown time is outside");
        let night = Condition::Hours { local: (22 * 60, 6 * 60), days: 0 };
        assert!(matches!(night.check(&facts(&[], &[], Some(monday_10))), Verdict::Ask(_)));
        assert_eq!(night.check(&facts(&[], &[], Some(4 * 1440 + 23 * 60))), Verdict::Pass);
        assert!(office.describe().contains("09:00 to 18:00, Mon, Tue, Wed, Thu, Fri"));
    }
}
