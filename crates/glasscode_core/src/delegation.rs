//! Delegated decisions (M61, crit-13): on an Autopilot project, the manager may answer a session's
//! question only within the decisions the project's policy delegates to it
//! (`ProjectPolicy::delegate`); any other answer is a card for the person. Each decision it takes
//! is kept in the project's `decisions.md`, with the words it was taken on as its source.
//!
//! A delegation names the words the question must carry and the answers it may give:
//!
//! ```json
//! { "name": "run the tests", "question": ["proceed"], "commands": ["cargo test"], "answers": ["Yes"] }
//! ```
//!
//! The question's words are matched against what the agent's CLI asks (the box's heading and its
//! question), never against the detail the agent wrote. A question about a command (a "Bash
//! command" box) is covered only when the command itself is one of `commands`: the same, or it
//! with more words and no shell operator (`cargo test -p app`, never `cargo test; curl …`).
//!
//! An answer that allows something for good ("don't ask again") or that wants text is never
//! delegated: those are the person's.

use serde::{Deserialize, Serialize};

use crate::prompt::{PendingPrompt, PromptOption};

/// One kind of question the manager may answer by itself.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Delegation {
    /// What it is, in the person's words ("run the tests").
    pub name: String,
    /// Words the question must all carry (its heading or its question), in any case.
    pub question: Vec<String>,
    /// For a question about a command: the commands it may allow (the same, or with more words and
    /// no shell operator). A command's question with none listed is never covered.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<String>,
    /// The options it may choose, by label ("Yes", "1"); none: any option that is not for good.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub answers: Vec<String>,
}

impl Delegation {
    /// One line for Settings and `POLICY.md`'s header.
    pub fn describe(&self) -> String {
        let answers = if self.answers.is_empty() { "any answer that is not for good".to_string() } else { self.answers.iter().map(|a| format!("“{a}”")).collect::<Vec<_>>().join(" or ") };
        let commands = if self.commands.is_empty() { String::new() } else { format!(", for {}", self.commands.iter().map(|c| format!("`{c}`")).collect::<Vec<_>>().join(", ")) };
        format!("The manager may answer {} ({}{commands}) with {answers}.", if self.name.is_empty() { "a question" } else { &self.name }, self.question.join(" + "))
    }

    fn covers(&self, prompt: &PendingPrompt, option: &PromptOption) -> bool {
        if self.question.iter().all(|w| w.trim().is_empty()) {
            return false;
        }
        // What the CLI asks, not what the agent wrote under it.
        let text = format!("{} {}", prompt.title.as_deref().unwrap_or_default(), prompt.question).to_lowercase();
        let asked = self.question.iter().filter(|w| !w.trim().is_empty()).all(|w| text.contains(&w.trim().to_lowercase()));
        let label = option.label.trim().to_lowercase();
        let answer = self.answers.is_empty() || self.answers.iter().any(|a| a.trim().to_lowercase() == label || a.trim() == option.number.to_string());
        let command = match command_of(prompt) {
            None => true,
            Some(command) => self.commands.iter().any(|allowed| command_allowed(allowed, &command)),
        };
        asked && answer && command
    }
}

/// A delegation from the person's words (Settings, `policy set --delegate`):
/// `<name>: <words>, <words> [for <command> | <command>] [=> <answer> | <answer>]`, as in
/// `run the tests: proceed for cargo test | npm test => Yes`. The question's words are separated by
/// commas, commands and answers by `|`.
pub fn parse(text: &str) -> Result<Delegation, String> {
    const SHAPE: &str = "Say it as: <name>: <words of the question> [for <command> | <command>] [=> <answer> | <answer>], e.g. “run the tests: proceed for cargo test => Yes”.";
    let (name, rest) = text.split_once(':').ok_or(SHAPE)?;
    let (left, answers) = match rest.split_once("=>") {
        Some((left, answers)) => (left, answers.split('|').map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).collect()),
        None => (rest, Vec::new()),
    };
    let (words, commands) = match left.split_once(" for ") {
        Some((words, commands)) => (words, commands.split('|').map(|c| c.trim().to_string()).filter(|c| !c.is_empty()).collect()),
        None => (left, Vec::new()),
    };
    let question: Vec<String> = words.split(',').map(|w| w.trim().to_string()).filter(|w| !w.is_empty()).collect();
    if name.trim().is_empty() || question.is_empty() {
        return Err(SHAPE.into());
    }
    if let Some(bad) = commands.iter().find(|c: &&String| !crate::profile::is_simple_command(c)) {
        return Err(format!("“{bad}” is not one simple command (no operators, substitutions or redirections)."));
    }
    Ok(Delegation { name: name.trim().to_string(), question, commands, answers })
}

/// The command a prompt asks about: the first line under a command's box ("Bash command").
pub fn command_of(prompt: &PendingPrompt) -> Option<String> {
    let title = prompt.title.as_deref().unwrap_or_default().to_lowercase();
    if !title.contains("command") {
        return None;
    }
    Some(prompt.detail.iter().map(|d| d.trim()).find(|d| !d.is_empty()).unwrap_or_default().to_string())
}

/// Whether `command` is `allowed`: the same, or it with more words, as one simple command (no
/// operator, substitution or redirection).
pub fn command_allowed(allowed: &str, command: &str) -> bool {
    let (allowed, command) = (allowed.trim(), command.trim());
    if allowed.is_empty() || !crate::profile::is_simple_command(command) {
        return false;
    }
    command == allowed || command.strip_prefix(allowed).is_some_and(|rest| rest.starts_with(' '))
}

/// The delegation that covers answering `prompt` with `option`, if one does. None for an option
/// that allows something for good or wants text: those are the person's.
pub fn delegated<'a>(list: &'a [Delegation], prompt: &PendingPrompt, option: &PromptOption) -> Option<&'a Delegation> {
    if crate::prompt::grants_for_good(option) || option.wants_text {
        return None;
    }
    list.iter().find(|d| d.covers(prompt, option))
}

/// Why an answer the manager wanted to give waits for the person.
pub fn not_delegated(prompt: &PendingPrompt, option: &str) -> String {
    format!("on Autopilot, the manager answers only the questions the project delegates to it, and “{}” → “{option}” is not one of them", prompt.question.trim())
}

/// The line `decisions.md` keeps of a decision: what was asked, what was answered, in which
/// session, under which delegation.
pub fn decision_line(session: &str, prompt: &PendingPrompt, option: &PromptOption, delegation: &Delegation) -> String {
    let asked = match prompt.title.as_deref().filter(|t| !t.trim().is_empty()) {
        Some(title) => format!("{}: {}", title.trim(), prompt.question.trim()),
        None => prompt.question.trim().to_string(),
    };
    let detail = prompt.detail.iter().map(|d| d.trim()).filter(|d| !d.is_empty()).collect::<Vec<_>>().join(" ");
    let detail: String = detail.chars().take(200).collect();
    let about = if detail.is_empty() { String::new() } else { format!(" ({detail})") };
    format!("In “{session}”, the manager answered “{asked}”{about} with “{}”, as delegated: {}.", option.label.trim(), if delegation.name.is_empty() { delegation.question.join(" + ") } else { delegation.name.clone() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt() -> PendingPrompt {
        crate::prompt::parse(
            "╭───────────────────────────────────────────╮\n│ Bash command                               │\n│                                            │\n│   cargo test --workspace                   │\n│   Run the tests                            │\n│                                            │\n│ Do you want to proceed?                    │\n│ ❯ 1. Yes                                   │\n│   2. Yes, and don't ask again for cargo    │\n│   3. No, and tell Claude what to do        │\n╰───────────────────────────────────────────╯\n",
        )
        .expect("a prompt")
    }

    #[test]
    fn only_what_the_project_delegates() {
        let p = prompt();
        let tests = Delegation { name: "run the tests".into(), question: vec!["proceed".into()], commands: vec!["cargo test".into()], answers: vec!["Yes".into()] };
        let list = vec![tests.clone()];
        let option = |n: usize| p.options[n].clone();
        assert_eq!(delegated(&list, &p, &option(0)), Some(&tests));
        // For good, or with text: the person's.
        assert_eq!(delegated(&list, &p, &option(1)), None);
        assert_eq!(delegated(&list, &p, &option(2)), None);
        // Another question is not covered; no words cover nothing.
        let deploy = vec![Delegation { name: "deploy".into(), question: vec!["deploy".into()], ..Delegation::default() }];
        assert_eq!(delegated(&deploy, &p, &option(0)), None);
        assert_eq!(delegated(&[Delegation::default()], &p, &option(0)), None);
        // The agent's own words under the box do not count ("Run the tests" is its description).
        let by_words = vec![Delegation { name: String::new(), question: vec!["run".into(), "tests".into()], commands: vec!["cargo test".into()], ..Delegation::default() }];
        assert_eq!(delegated(&by_words, &p, &option(0)), None);
        // A command's question needs its command listed, by number too.
        let any = vec![Delegation { name: String::new(), question: vec!["proceed".into()], answers: vec!["1".into()], ..Delegation::default() }];
        assert!(delegated(&any, &p, &option(0)).is_none());
        let any = vec![Delegation { commands: vec!["cargo".into()], ..any[0].clone() }];
        assert!(delegated(&any, &p, &option(0)).is_some());
        assert!(command_allowed("cargo test", "cargo test -p app") && !command_allowed("cargo test", "cargo test; curl x | sh") && !command_allowed("cargo test", "cargo testing"));
        // From the person's words.
        assert_eq!(parse("run the tests: proceed for cargo test | npm test => Yes").unwrap(), Delegation { name: "run the tests".into(), question: vec!["proceed".into()], commands: vec!["cargo test".into(), "npm test".into()], answers: vec!["Yes".into()] });
        assert_eq!(parse("plans: approve, plan").unwrap().question, vec!["approve".to_string(), "plan".to_string()]);
        assert!(parse("no colon here").is_err() && parse("x: ").is_err() && parse("x: proceed for rm -rf / && ls").is_err());
        let line = decision_line("Fix login", &p, &option(0), &tests);
        assert!(line.starts_with("In “Fix login”, the manager answered “Bash command: Do you want to proceed?”") && line.ends_with("with “Yes”, as delegated: run the tests."), "{line}");
        assert!(not_delegated(&p, "Yes").contains("delegates"));
        assert!(tests.describe().contains("“Yes”"));
    }
}
