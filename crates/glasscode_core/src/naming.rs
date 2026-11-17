//! Names for sessions and the worktrees Glasscode makes. A session the person did not name
//! is called after its pull request's title, else Claude's own title for the conversation
//! (`ai-title` in its transcript), else its first message. A worktree Glasscode made with a
//! placeholder branch (`glasscode/session-9ade`) gets a branch from that name, but only before the
//! branch is ever pushed.

use crate::model::AgentKind;

/// A title fit for the sidebar: no Conventional Commits prefix (`feat(scope)!:`), a capital
/// first letter, no trailing full stop, at most `max` characters, cut at a word.
pub fn tidy_title(title: &str, max: usize) -> String {
    let mut t = title.trim().lines().next().unwrap_or_default().trim().to_string();
    if let Some((head, rest)) = t.split_once(':') {
        let head = head.trim_end_matches('!');
        let kind = head.split('(').next().unwrap_or_default();
        let scoped_ok = head.find('(').is_none_or(|i| head.ends_with(')') && i > 0);
        if !kind.is_empty() && kind.len() <= 12 && kind.chars().all(|c| c.is_ascii_lowercase()) && scoped_ok && !rest.trim().is_empty() {
            t = rest.trim().to_string();
        }
    }
    let t = t.trim_end_matches('.').trim().to_string();
    let mut chars = t.chars();
    let mut out = match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    };
    if out.chars().count() > max {
        let cut: String = out.chars().take(max).collect();
        // The cut already ends a word when what follows is a space or punctuation.
        let ends_word = out.chars().nth(max).is_some_and(|c| c == ' ' || c.is_ascii_punctuation());
        out = match cut.rfind(' ') {
            _ if ends_word => format!("{}…", cut.trim_end_matches([',', ';', ' '])),
            Some(i) if i > max / 2 => format!("{}…", cut[..i].trim_end_matches([',', ';', ' '])),
            _ => format!("{cut}…"),
        };
    }
    out
}

/// A branch Glasscode made as a placeholder (`glasscode/session-` and four hex digits).
pub fn is_placeholder_branch(branch: &str) -> bool {
    branch.strip_prefix("glasscode/session-").is_some_and(|rest| rest.len() == 4 && rest.chars().all(|c| c.is_ascii_hexdigit()))
}

/// A branch named after a title: `glasscode/` and up to five words, lower case, ASCII.
pub fn branch_for(title: &str) -> Option<String> {
    slug(title).map(|slug| format!("glasscode/{slug}"))
}

/// Up to five words of a title, lower case, ASCII, joined by hyphens, at most 40 characters
/// (`{slug}` of a project's branch template).
pub fn slug(title: &str) -> Option<String> {
    let words: Vec<String> = title
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_ascii_lowercase())
        .filter(|w| !matches!(w.as_str(), "a" | "an" | "the" | "and" | "of" | "to" | "for" | "in" | "on" | "with"))
        .take(5)
        .collect();
    if words.is_empty() {
        return None;
    }
    let mut slug = words.join("-");
    slug.truncate(40);
    Some(slug.trim_end_matches('-').to_string())
}

/// The kind of work (`{type}` of a project's branch template): from a ticket's labels ("Bug"),
/// else the first word of its title ("Fix the login", "docs: …"), else feat.
pub fn work_type(title: &str, labels: &[String]) -> &'static str {
    let kind = |word: &str| match word {
        "bug" | "bugs" | "fix" | "fixes" | "hotfix" | "defect" => Some("fix"),
        "docs" | "doc" | "documentation" => Some("docs"),
        "refactor" | "refactoring" => Some("refactor"),
        "test" | "tests" | "testing" => Some("test"),
        "chore" | "chores" => Some("chore"),
        _ => None,
    };
    let from_labels = labels.iter().find_map(|l| kind(l.trim().to_lowercase().as_str()));
    let first = title.split(|c: char| !c.is_ascii_alphanumeric()).find(|w| !w.is_empty()).unwrap_or_default().to_ascii_lowercase();
    from_labels.or_else(|| kind(&first)).unwrap_or("feat")
}

/// Whether a session's title is one Glasscode gave it (not the person's own), so a better
/// one may replace it: `Claude · <branch>`, a review's `#958 …`, `Task · T-…`, or the
/// start of its pull request's title.
pub fn is_generated_title(title: &str, agent: AgentKind, branch: &str, pr_title: Option<&str>) -> bool {
    let title = title.trim();
    // "Claude · <its branch then>": the branch may have changed since.
    let agent_prefix = format!("{} · ", agent.name());
    if title == format!("{agent_prefix}{branch}") || title.starts_with("Task · ") || title.strip_prefix(&agent_prefix).is_some_and(|rest| !rest.contains(' ')) {
        return true;
    }
    if let Some(rest) = title.strip_prefix('#') {
        if rest.split(' ').next().is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())) {
            return true;
        }
    }
    pr_title.is_some_and(|p| !title.is_empty() && p.starts_with(title.trim_end_matches('…')))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_lose_commit_prefixes() {
        assert_eq!(tidy_title("feat(browser-extension): add Acme for Chrome with a side panel", 60), "Add Acme for Chrome with a side panel");
        assert_eq!(tidy_title("feat!: add passwordless login, MFA, invitations, and sessions", 40), "Add passwordless login, MFA, invitations…");
        assert_eq!(tidy_title("fix(core): configure the sandbox git gateway without git.", 80), "Configure the sandbox git gateway without git");
        assert_eq!(tidy_title("build a slimmer sandbox image", 60), "Build a slimmer sandbox image");
        assert_eq!(tidy_title("Note: keep this", 60), "Note: keep this");
    }

    #[test]
    fn placeholder_branches_and_their_names() {
        assert!(is_placeholder_branch("glasscode/session-9ade"));
        assert!(!is_placeholder_branch("glasscode/session-9adex"));
        assert!(!is_placeholder_branch("feat/ocr-provider"));
        assert_eq!(branch_for("Build a slimmer sandbox image for agent runs").as_deref(), Some("glasscode/build-slimmer-sandbox-image-agent"));
        assert_eq!(branch_for("!!!"), None);
        assert_eq!(slug("Build a slimmer sandbox image for agent runs").as_deref(), Some("build-slimmer-sandbox-image-agent"));
        assert_eq!(slug("Ünïcode only: äöü"), Some("n-code-only".to_string()));
    }

    #[test]
    fn the_kind_of_work_from_labels_and_title() {
        assert_eq!(work_type("Calendar sync", &[]), "feat");
        assert_eq!(work_type("Fix the flaky login test", &[]), "fix");
        assert_eq!(work_type("docs: the settings file", &[]), "docs");
        assert_eq!(work_type("Refactor(core): split git.rs", &[]), "refactor");
        assert_eq!(work_type("Calendar sync", &["Feature".into(), " Bug ".into()]), "fix");
        assert_eq!(work_type("", &[]), "feat");
    }

    #[test]
    fn generated_titles() {
        assert!(is_generated_title("Claude · glasscode/session-9ade", AgentKind::Claude, "glasscode/session-9ade", None));
        assert!(is_generated_title("#960 fix(core): configure", AgentKind::Claude, "review/960", None));
        assert!(is_generated_title("feat: add reporting and data pipeline connectors and fix S", AgentKind::Claude, "x", Some("feat: add reporting and data pipeline connectors and fix SSO")));
        assert!(is_generated_title("Claude · glasscode/session-9ade", AgentKind::Claude, "glasscode/munich-session-search", None));
        assert!(!is_generated_title("Claude · my own words here", AgentKind::Claude, "x", None));
        assert!(!is_generated_title("chat in seperate pod", AgentKind::Claude, "pr-835-new", None));
    }
}
