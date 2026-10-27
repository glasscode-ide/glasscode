//! The question an agent's terminal is showing right now — Claude's "Do you want to
//! proceed?", Codex's "Would you like to run the following command?" — read from the screen
//! so a phone can show it as buttons and answer it with the same keys a person would press.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PendingPrompt {
    /// The box's heading ("Bash command", "Edit file"), when it has one.
    #[serde(default)]
    pub title: Option<String>,
    /// "Do you want to proceed?"
    pub question: String,
    /// What it is about, as shown: the command, the file, its description.
    #[serde(default)]
    pub detail: Vec<String>,
    pub options: Vec<PromptOption>,
    /// The highlighted option (0-based); answering moves from here with the arrow keys.
    pub selected: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PromptOption {
    pub number: u32,
    pub label: String,
    /// Choosing it means "no, do this instead": the answer carries text for the agent.
    #[serde(default)]
    pub wants_text: bool,
}

/// Box drawing and the frame characters agents put around a question.
fn strip_frame(line: &str) -> &str {
    let frame = |c: char| matches!(c, '│' | '┃' | '║' | '╭' | '╮' | '╰' | '╯' | '─' | '━' | '┌' | '┐' | '└' | '┘' | '▌' | '▐') || c.is_whitespace();
    line.trim_matches(frame)
}

fn is_rule(line: &str) -> bool {
    // A horizontal line (a box edge or a separator), not an empty row inside a box.
    let t = line.trim();
    let horizontal = |c: char| matches!(c, '─' | '━' | '═' | '-' | '╌' | '┄');
    t.chars().filter(|&c| horizontal(c)).count() >= 3 && t.chars().all(|c| horizontal(c) || matches!(c, '│' | '╭' | '╮' | '╰' | '╯' | '┌' | '┐' | '└' | '┘' | ' '))
}

/// `❯ 1. Yes` / `  2. Yes, and don't ask again` / `› 1. Yes, proceed (y)`:
/// (highlighted, number, label).
fn option_line(line: &str) -> Option<(bool, u32, String)> {
    let t = strip_frame(line);
    let (marked, rest) = match t.chars().next()? {
        '❯' | '›' | '▶' | '>' | '→' => (true, t[t.chars().next()?.len_utf8()..].trim_start()),
        _ => (false, t),
    };
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || digits.len() > 2 {
        return None;
    }
    let after = &rest[digits.len()..];
    let label = after.strip_prefix('.').or_else(|| after.strip_prefix(')'))?;
    if !label.starts_with(' ') {
        return None;
    }
    let label = label.trim();
    if label.is_empty() {
        return None;
    }
    Some((marked, digits.parse().ok()?, label.to_string()))
}

fn wants_text(label: &str) -> bool {
    let l = label.to_lowercase();
    l.contains("what to do differently") || l.contains("tell claude") || l.contains("tell codex") || l.contains("provide feedback") || l.contains("give feedback")
}

/// Footer lines agents print under a question ("Esc to cancel · Tab to amend").
fn is_footer(line: &str) -> bool {
    let l = line.to_lowercase();
    l.contains("esc to") || l.contains("to cancel") || l.contains("tab to") || l.contains("enter to") || l.contains("press enter") || l.contains("ctrl+") || l.contains("shift+tab") || l.starts_with('⏵')
}

/// The open question at the bottom of the screen, if there is one.
///
/// Only a numbered list with exactly one highlighted entry counts (that is a menu waiting for a
/// key); a numbered list in the agent's own reply has no highlight and is left alone.
pub fn parse(screen: &str) -> Option<PendingPrompt> {
    let lines: Vec<&str> = screen.lines().collect();
    // The last option line, near the bottom (only footers may follow it).
    let last = lines.iter().rposition(|l| option_line(l).is_some())?;
    // An open menu is the last thing on the screen: only its footer ("Esc to cancel") may follow.
    // Anything else below it (the agent's next reply, an input box) means it was answered.
    let after: Vec<&str> = lines[last + 1..].iter().map(|l| strip_frame(l)).filter(|l| !l.is_empty()).collect();
    if after.len() > 3 || after.iter().any(|l| !is_footer(l)) {
        return None;
    }
    // Walk up to the first option, folding wrapped label lines into the option above them.
    let mut first = last;
    let mut i = last;
    while i > 0 {
        i -= 1;
        let t = strip_frame(lines[i]);
        if option_line(lines[i]).is_some() {
            first = i;
            continue;
        }
        if t.is_empty() || is_rule(lines[i]) || t.ends_with('?') || i + 6 < first {
            break;
        }
        // An indented continuation line of the option above (wrapped label): keep going.
        if lines[i].starts_with("  ") || lines[i].starts_with("│ ") {
            continue;
        }
        break;
    }
    let mut options: Vec<PromptOption> = Vec::new();
    let mut selected = None;
    for line in &lines[first..=last] {
        match option_line(line) {
            Some((marked, number, label)) => {
                if marked {
                    if selected.is_some() {
                        return None;
                    }
                    selected = Some(options.len());
                }
                options.push(PromptOption { number, wants_text: false, label });
            }
            None => {
                let t = strip_frame(line);
                if let (Some(option), false) = (options.last_mut(), t.is_empty()) {
                    option.label.push(' ');
                    option.label.push_str(t);
                }
            }
        }
    }
    let selected = selected?;
    if options.len() < 2 || options.iter().enumerate().any(|(i, o)| o.number as usize != i + 1) {
        return None;
    }
    for option in &mut options {
        option.wants_text = wants_text(&option.label);
    }
    // The question: the nearest line above the options ending in '?'. Trust-this-folder,
    // onboarding and theme selectors phrase their headline as a statement, not a question,
    // so when there is no '?' line we fall back to the nearest real line above the options.
    // A numbered reply can't reach here: it has no highlighted option (selected is None above).
    let q = match (first.saturating_sub(14)..first).rev().find(|&i| strip_frame(lines[i]).ends_with('?')) {
        Some(q) => q,
        None => (first.saturating_sub(14)..first).rev().find(|&i| {
            let t = strip_frame(lines[i]);
            !t.is_empty() && !is_rule(lines[i]) && !is_footer(t) && option_line(lines[i]).is_none()
        })?,
    };
    let question = strip_frame(lines[q]).to_string();
    // The box: from its top edge down to the question; without a box, from the blank line above.
    let near = q.saturating_sub(12)..q;
    let edge = near.clone().rev().find(|&i| lines[i].trim_start().starts_with('╭') || lines[i].trim_start().starts_with('┌') || is_rule(lines[i]));
    let blank = near.rev().find(|&i| strip_frame(lines[i]).is_empty());
    let top = edge.or(blank).map(|i| i + 1).unwrap_or(q.saturating_sub(8));
    let body: Vec<String> = lines[top.max(q.saturating_sub(10))..q].iter().map(|l| strip_frame(l).to_string()).filter(|l| !l.is_empty() && !is_rule(l)).collect();
    // A short first line over more detail is the box's heading ("Bash command").
    let (title, detail) = match body.split_first() {
        Some((head, rest)) if !rest.is_empty() && head.split_whitespace().count() <= 4 && !head.starts_with('$') => (Some(head.clone()), rest.to_vec()),
        _ => (None, body),
    };
    Some(PendingPrompt { title, question, detail: detail.into_iter().take(8).collect(), options, selected })
}

/// A selection menu is on screen right now (a highlighted `❯`/`›` option near the bottom),
/// even if `parse` could not turn it into buttons. A numbered reply never has a highlight,
/// so this stays false for the agent's own lists — it means "the agent is waiting on a key".
pub fn has_live_menu(screen: &str) -> bool {
    screen.lines().rev().take(40).any(|l| matches!(option_line(l), Some((true, _, _))))
}

/// The keys that pick `number` from `prompt`: arrows from the highlighted option, then Enter.
pub fn keys_for(prompt: &PendingPrompt, number: u32) -> Option<Vec<&'static str>> {
    let target = prompt.options.iter().position(|o| o.number == number)?;
    let mut keys = Vec::new();
    if target > prompt.selected {
        keys.extend(std::iter::repeat_n("Down", target - prompt.selected));
    } else {
        keys.extend(std::iter::repeat_n("Up", prompt.selected - target));
    }
    keys.push("Enter");
    Some(keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLAUDE: &str = "\
⏺ Nested groups now inherit their parent's role.

╭──────────────────────────────────────────────────────────────╮
│ Bash command                                                 │
│                                                              │
│   git push origin feat/group-roles                           │
│   Push the fix to the PR branch                              │
│                                                              │
│ Do you want to proceed?                                      │
│ ❯ 1. Yes                                                     │
│   2. Yes, and don't ask again for git push commands in       │
│   /home/dev/app                                         │
│   3. No, and tell Claude what to do differently (esc)        │
╰──────────────────────────────────────────────────────────────╯
";

    const CODEX: &str = "\
• Ran bun test
  └ 41 pass

  Would you like to run the following command?

  $ git push origin fix/pwa-app-icons

› 1. Yes, proceed (y)
  2. Yes, and don't ask again for this command (a)
  3. No, and tell Codex what to do differently (esc)

  Press enter to confirm or esc to cancel
";

    #[test]
    fn reads_claudes_permission_box() {
        let p = parse(CLAUDE).expect("prompt");
        assert_eq!(p.title.as_deref(), Some("Bash command"));
        assert_eq!(p.question, "Do you want to proceed?");
        assert_eq!(p.detail, vec!["git push origin feat/group-roles", "Push the fix to the PR branch"]);
        assert_eq!(p.options.len(), 3);
        assert_eq!(p.options[1].label, "Yes, and don't ask again for git push commands in /home/dev/app");
        assert!(p.options[2].wants_text && !p.options[0].wants_text);
        assert_eq!(p.selected, 0);
        assert_eq!(keys_for(&p, 3), Some(vec!["Down", "Down", "Enter"]));
        assert_eq!(keys_for(&p, 1), Some(vec!["Enter"]));
    }

    #[test]
    fn reads_codexs_approval() {
        let p = parse(CODEX).expect("prompt");
        assert_eq!(p.question, "Would you like to run the following command?");
        assert_eq!(p.options.iter().map(|o| o.number).collect::<Vec<_>>(), vec![1, 2, 3]);
        assert_eq!(p.options[0].label, "Yes, proceed (y)");
        assert!(p.options[2].wants_text);
    }

    #[test]
    fn an_answered_menu_is_not_open() {
        let answered = format!("{CLAUDE}\n⏺ Pushed feat/group-roles.\n\n> \n");
        assert_eq!(parse(&answered), None);
        let with_status = format!("{CLAUDE}  ⏵⏵ accept edits on (shift+tab to cycle)\n");
        assert!(parse(&with_status).is_some());
    }

    #[test]
    fn leaves_numbered_replies_alone() {
        let reply = "⏺ Two ways to fix it — which do you prefer?\n  1. Inherit the parent's role\n  2. Copy roles at sync time\n\n> \n  ? for shortcuts\n";
        assert_eq!(parse(reply), None);
        assert_eq!(parse("plain shell\n$ ls\n"), None);
    }

    // Codex's first-run folder-trust selector: the headline is a statement, not a "?" question.
    const CODEX_TRUST: &str = "\
> You are running Codex in /Users/dev/app

  Since this folder is version controlled, you may wish to allow
  Codex to work in this folder without asking for approval.

› 1. Yes, allow Codex to work in this folder without asking for approval
  2. No, ask me to approve edits and commands

  Press enter to confirm
";

    // Claude's onboarding theme selector: also no "?" headline.
    const CLAUDE_THEME: &str = "\
 Let's get started.

 Choose the text style that looks best with your terminal:

❯ 1. Dark mode
  2. Light mode
  3. Dark mode (colorblind-friendly)
";

    #[test]
    fn reads_statement_headed_selectors() {
        let trust = parse(CODEX_TRUST).expect("codex trust prompt");
        // No "?" headline: the question is the nearest statement line; the rest is in detail.
        assert_eq!(trust.question, "Codex to work in this folder without asking for approval.");
        assert!(trust.detail.iter().any(|d| d.contains("version controlled")));
        assert_eq!(trust.options.len(), 2);
        assert_eq!(trust.selected, 0);
        assert_eq!(keys_for(&trust, 2), Some(vec!["Down", "Enter"]));

        let theme = parse(CLAUDE_THEME).expect("claude theme prompt");
        assert_eq!(theme.question, "Choose the text style that looks best with your terminal:");
        assert_eq!(theme.options.len(), 3);
        assert_eq!(theme.selected, 0);
    }
}

/// The agent on this screen has lost its login ("Login expired · Please run /login", "Not logged
/// in · Run /login", Codex asking to sign in): it cannot work until someone signs it in again.
pub fn signed_out(screen: &str) -> bool {
    screen.lines().rev().take(80).any(|line| {
        let l = line.to_lowercase();
        l.contains("run /login") || l.contains("login expired") || l.contains("not logged in") || l.contains("oauth token has expired") || l.contains("invalid api key") || l.contains("run `codex login`") || l.contains("please sign in to codex")
    })
}

#[cfg(test)]
mod signed_out_tests {
    #[test]
    fn spots_a_lost_login_only() {
        assert!(super::signed_out("● Login expired · Please run /login\n❯ "));
        assert!(super::signed_out("  Not logged in · Run /login"));
        // The slash menu lists /login; that is not a lost login.
        assert!(!super::signed_out("  /login     Sign in with your Anthropic account\n❯ /lo"));
        assert!(!super::signed_out("All tests pass."));
    }
}
