//! Where memory keeps what (M20.1 step 1). Every file of the memory repository is one of
//! [`MemFile`], named by its path relative to `memory/` with `/` between the parts (git's form):
//!
//! ```text
//! you/profile.md                         the person, on every project
//! org/<employer>/people.md, glossary.md  one folder per employer (mem-18); a project names its own
//! projects/<key>/PROJECT.md              what the project is: stack, build, test, owners
//! projects/<key>/POLICY.md               prose about how to work there; it carries no authority (D10)
//! projects/<key>/decisions.md            decisions, newest last
//! projects/<key>/knowhow/<topic>.md      how to do things there, mostly written by sessions
//! projects/<key>/work/<work file>.md     one piece of work (`WorkKey::file_name`, M11)
//! projects/<key>/archive/<yyyy-mm>/…     work that ended, and entries let go (M24)
//! inbox/proposals.md                     what waits for the person's tick
//! inbox/rejected.md                      text that could not be saved; never committed
//! INDEX.md                               generated from the others
//! ```
//!
//! A path that is none of these is not memory: a write to it is refused, and a file there is a
//! pending edit like any other change Glasscode did not make.

use crate::identity::WorkKey;
use crate::paths::ProjectKey;

/// One employer's folder name (`org/<employer>/`): the same rules as a project key, so it is one
/// safe folder name on every system. Usually the owner of the project's repository (`acme` of
/// `github.com/acme/app`), unless the person names another.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Employer(String);

impl Employer {
    pub fn parse(text: &str) -> Option<Employer> {
        ProjectKey::parse(text).map(|key| Employer(key.as_str().to_string()))
    }

    /// The employer of a normalized remote (`github.com/acme/app`): its owner, in lower case.
    pub fn of_remote(remote: &str) -> Option<Employer> {
        let mut parts = remote.split('/');
        let _host = parts.next()?;
        let owner = parts.next()?;
        parts.next()?;
        Employer::parse(&owner.to_ascii_lowercase())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Employer {
    type Error = String;

    fn try_from(text: String) -> Result<Employer, String> {
        Employer::parse(&text).ok_or_else(|| format!("{text:?} is not an employer's folder name"))
    }
}

impl From<Employer> for String {
    fn from(employer: Employer) -> String {
        employer.0
    }
}

impl std::fmt::Display for Employer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A part of a file name below a project (a know-how topic, a work file, an archived file, a month
/// folder): the same rules as a project key, so `..`, `/` and device names never get in.
fn name_ok(name: &str) -> bool {
    ProjectKey::parse(name).is_some()
}

/// Whom a file is about: what decides who may read it (M20.2) and which head record it moves.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Scope {
    You,
    Org(Employer),
    Project(ProjectKey),
    /// `inbox/` and `INDEX.md`: Glasscode's own, about no one.
    Glasscode,
}

impl Scope {
    /// The scope's key in `authority/memory-head.json`: `you`, `org/acme`, `projects/app-3f9a1c`,
    /// `glasscode`.
    pub fn key(&self) -> String {
        match self {
            Scope::You => "you".into(),
            Scope::Org(employer) => format!("org/{employer}"),
            Scope::Project(key) => format!("projects/{key}"),
            Scope::Glasscode => "glasscode".into(),
        }
    }
}

/// Every file memory has.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MemFile {
    Profile,
    People(Employer),
    Glossary(Employer),
    Project(ProjectKey),
    Policy(ProjectKey),
    Decisions(ProjectKey),
    /// `knowhow/<topic>.md`.
    Knowhow(ProjectKey, String),
    /// `work/<stem>.md`: the stem is [`WorkKey::file_name`], with `-2`, `-3`… after it when another
    /// work's name collides (M20.1 step 9).
    Work(ProjectKey, String),
    /// `archive/<yyyy-mm>/<name>.md`; the name may hold one folder (`work/<stem>`).
    Archive(ProjectKey, String, String),
    Proposals,
    /// Text a write could not save. Only ever in the working tree, never committed, never a
    /// pending edit and never read for a brief.
    Rejected,
    Index,
}

/// What kind of file it is, for its cap and for who may write it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FileKind {
    Profile,
    People,
    Glossary,
    Project,
    Policy,
    Decisions,
    Knowhow,
    Work,
    Archive,
    Proposals,
    Rejected,
    Index,
}

impl MemFile {
    /// Its path below `memory/`, with `/` between the parts.
    pub fn rel(&self) -> String {
        match self {
            MemFile::Profile => "you/profile.md".into(),
            MemFile::People(employer) => format!("org/{employer}/people.md"),
            MemFile::Glossary(employer) => format!("org/{employer}/glossary.md"),
            MemFile::Project(key) => format!("projects/{key}/PROJECT.md"),
            MemFile::Policy(key) => format!("projects/{key}/POLICY.md"),
            MemFile::Decisions(key) => format!("projects/{key}/decisions.md"),
            MemFile::Knowhow(key, topic) => format!("projects/{key}/knowhow/{topic}.md"),
            MemFile::Work(key, stem) => format!("projects/{key}/work/{stem}.md"),
            MemFile::Archive(key, month, name) => format!("projects/{key}/archive/{month}/{name}.md"),
            MemFile::Proposals => "inbox/proposals.md".into(),
            MemFile::Rejected => "inbox/rejected.md".into(),
            MemFile::Index => "INDEX.md".into(),
        }
    }

    /// The file a path below `memory/` names, when it is one of memory's. `\` reads as `/`.
    pub fn parse(rel: &str) -> Option<MemFile> {
        let rel = rel.replace('\\', "/");
        let parts: Vec<&str> = rel.split('/').collect();
        let md = |name: &str| name.strip_suffix(".md").filter(|stem| name_ok(stem)).map(str::to_string);
        match parts.as_slice() {
            ["you", "profile.md"] => Some(MemFile::Profile),
            ["org", employer, "people.md"] => Employer::parse(employer).map(MemFile::People),
            ["org", employer, "glossary.md"] => Employer::parse(employer).map(MemFile::Glossary),
            ["projects", key, rest @ ..] => {
                let key = ProjectKey::parse(key)?;
                match rest {
                    ["PROJECT.md"] => Some(MemFile::Project(key)),
                    ["POLICY.md"] => Some(MemFile::Policy(key)),
                    ["decisions.md"] => Some(MemFile::Decisions(key)),
                    ["knowhow", name] => md(name).map(|topic| MemFile::Knowhow(key, topic)),
                    ["work", name] => md(name).map(|stem| MemFile::Work(key, stem)),
                    ["archive", month, name] if is_month(month) => md(name).map(|name| MemFile::Archive(key, month.to_string(), name)),
                    // An archived work file keeps its folder: `archive/<yyyy-mm>/work/<stem>.md` (M24).
                    ["archive", month, folder, name] if is_month(month) && name_ok(folder) => md(name).map(|name| MemFile::Archive(key, month.to_string(), format!("{folder}/{name}"))),
                    _ => None,
                }
            }
            ["inbox", "proposals.md"] => Some(MemFile::Proposals),
            ["inbox", "rejected.md"] => Some(MemFile::Rejected),
            ["INDEX.md"] => Some(MemFile::Index),
            _ => None,
        }
    }

    pub fn kind(&self) -> FileKind {
        match self {
            MemFile::Profile => FileKind::Profile,
            MemFile::People(_) => FileKind::People,
            MemFile::Glossary(_) => FileKind::Glossary,
            MemFile::Project(_) => FileKind::Project,
            MemFile::Policy(_) => FileKind::Policy,
            MemFile::Decisions(_) => FileKind::Decisions,
            MemFile::Knowhow(..) => FileKind::Knowhow,
            MemFile::Work(..) => FileKind::Work,
            MemFile::Archive(..) => FileKind::Archive,
            MemFile::Proposals => FileKind::Proposals,
            MemFile::Rejected => FileKind::Rejected,
            MemFile::Index => FileKind::Index,
        }
    }

    pub fn scope(&self) -> Scope {
        match self {
            MemFile::Profile => Scope::You,
            MemFile::People(employer) | MemFile::Glossary(employer) => Scope::Org(employer.clone()),
            MemFile::Project(key) | MemFile::Policy(key) | MemFile::Decisions(key) | MemFile::Knowhow(key, _) | MemFile::Work(key, _) | MemFile::Archive(key, ..) => Scope::Project(key.clone()),
            MemFile::Proposals | MemFile::Rejected | MemFile::Index => Scope::Glasscode,
        }
    }

    /// The project it belongs to.
    pub fn project(&self) -> Option<&ProjectKey> {
        match self {
            MemFile::Project(key) | MemFile::Policy(key) | MemFile::Decisions(key) | MemFile::Knowhow(key, _) | MemFile::Work(key, _) | MemFile::Archive(key, ..) => Some(key),
            _ => None,
        }
    }

    /// Generated by Glasscode from the other files: never written by a caller, and a change to it
    /// in the tree is pending like any other.
    pub fn is_generated(&self) -> bool {
        matches!(self, MemFile::Index)
    }

    /// The work file of `key` with no collision: `work/<WorkKey::file_name()>.md`.
    pub fn work(key: &WorkKey) -> MemFile {
        MemFile::Work(key.project.clone(), key.file_name())
    }

    /// The `n`th place a work file may be (0: its own name; then `-2`, `-3`…) when other works'
    /// files hold the names before it.
    pub fn work_at(key: &WorkKey, n: usize) -> MemFile {
        match n {
            0 => MemFile::work(key),
            n => MemFile::Work(key.project.clone(), format!("{}-{}", key.file_name(), n + 1)),
        }
    }
}

/// `2026-11`.
pub fn is_month(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 7 && bytes[4] == b'-' && bytes.iter().enumerate().all(|(i, b)| i == 4 || b.is_ascii_digit()) && matches!(&text[5..], "01" | "02" | "03" | "04" | "05" | "06" | "07" | "08" | "09" | "10" | "11" | "12")
}

/// The month folder of an archive for a time (UTC): `2026-11`.
pub fn month_of(at_ms: u64) -> String {
    let (year, month, _) = crate::day::to_civil((at_ms / 86_400_000) as i64);
    format!("{year:04}-{month:02}")
}

/// A know-how topic from free words: a lower-case slug, or None when nothing is left.
pub fn topic(words: &str) -> Option<String> {
    let mut slug = String::new();
    for ch in words.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
        if slug.len() >= 48 {
            break;
        }
    }
    let slug = slug.trim_end_matches('-').to_string();
    name_ok(&slug).then_some(slug)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(text: &str) -> ProjectKey {
        ProjectKey::parse(text).unwrap()
    }

    #[test]
    fn every_file_names_its_path_and_back() {
        let acme = Employer::parse("acme").unwrap();
        let files = [
            MemFile::Profile,
            MemFile::People(acme.clone()),
            MemFile::Glossary(acme),
            MemFile::Project(key("app-3f9a1c")),
            MemFile::Policy(key("app-3f9a1c")),
            MemFile::Decisions(key("app-3f9a1c")),
            MemFile::Knowhow(key("app-3f9a1c"), "deploy".into()),
            MemFile::Work(key("app-3f9a1c"), "feat-login-3f9a1c".into()),
            MemFile::Archive(key("app-3f9a1c"), "2026-11".into(), "feat-login-3f9a1c".into()),
            MemFile::Archive(key("app-3f9a1c"), "2026-11".into(), "work/feat-login-3f9a1c".into()),
            MemFile::Proposals,
            MemFile::Rejected,
            MemFile::Index,
        ];
        for file in files {
            assert_eq!(MemFile::parse(&file.rel()), Some(file.clone()), "{}", file.rel());
        }
        for bad in ["you/other.md", "projects/../x/PROJECT.md", "projects/app/knowhow/../../x.md", "projects/app/knowhow/a/b.md", "org/Acme Corp/people.md", "projects/app/archive/2026-13/x.md", "projects/app/knowhow/con.md", "notes.md", ".git/config"] {
            assert_eq!(MemFile::parse(bad), None, "{bad}");
        }
        assert_eq!(MemFile::parse("projects\\app-3f9a1c\\PROJECT.md"), Some(MemFile::Project(key("app-3f9a1c"))));
    }

    #[test]
    fn scopes_and_employers() {
        assert_eq!(Employer::of_remote("github.com/Acme/app").unwrap().as_str(), "acme");
        assert_eq!(Employer::of_remote("github.com/acme"), None);
        assert_eq!(MemFile::People(Employer::parse("acme").unwrap()).scope().key(), "org/acme");
        assert_eq!(MemFile::Work(key("app-3f9a1c"), "x".into()).scope().key(), "projects/app-3f9a1c");
        assert_eq!(MemFile::Index.scope(), Scope::Glasscode);
        assert_eq!(topic("Deploy & Rollback!").as_deref(), Some("deploy-rollback"));
        assert_eq!(topic("  ---  "), None);
        assert_eq!(month_of(1_763_560_000_000), "2025-11");
    }
}
