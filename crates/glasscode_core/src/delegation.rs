//! Delegated decisions (M61, crit-13): on an Autopilot project, the manager may answer a session's
//! question only within the decisions the project's policy delegates to it
//! (`ProjectPolicy::delegate`); any other answer is a card for the person. Each decision it takes
//! is kept in the project's `decisions.md`, with the words it was taken on as its source.
//!
//! A delegation names the words the question must carry and the answers it may give:
//!
//! ```json
//! { "name": "run the tests", "question": ["run", "test"], "answers": ["Yes"] }
//! ```
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
    /// Words the question must all carry (its heading, question or detail), in any case.
    pub question: Vec<String>,
    /// The options it may choose, by label ("Yes", "1"); none: any option that is not for good.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub answers: Vec<String>,
}

impl Delegation {
    /// One line for Settings and `POLICY.md`'s header.
    pub fn describe(&self) -> String {
        let answers = if self.answers.is_empty() { "any answer that is not for good".to_string() } else { self.answers.iter().map(|a| format!("“{a}”")).collect::<Vec<_>>().join(" or ") };
        format!("The manager may answer {} ({}) with {answers}.", if self.name.is_empty() { "a question" } else { &self.name }, self.question.join(" + "))
    }

    fn covers(&self, prompt: &PendingPrompt, option: &PromptOption) -> bool {
        if self.question.iter().all(|w| w.trim().is_empty()) {
            return false;
        }
        let text = format!("{} {} {}", prompt.title.as_deref().unwrap_or_default(), prompt.question, prompt.detail.join(" ")).to_lowercase();
        let asked = self.question.iter().filter(|w| !w.trim().is_empty()).all(|w| text.contains(&w.trim().to_lowercase()));
        let label = option.label.trim().to_lowercase();
        let answer = self.answers.is_empty() || self.answers.iter().any(|a| a.trim().to_lowercase() == label || a.trim() == option.number.to_string());
        asked && answer
    }
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
        let tests = Delegation { name: "run the tests".into(), question: vec!["cargo test".into()], answers: vec!["Yes".into()] };
        let list = vec![tests.clone()];
        let option = |n: usize| p.options[n].clone();
        assert_eq!(delegated(&list, &p, &option(0)), Some(&tests));
        // For good, or with text: the person's.
        assert_eq!(delegated(&list, &p, &option(1)), None);
        assert_eq!(delegated(&list, &p, &option(2)), None);
        // Another question is not covered; no words cover nothing.
        let deploy = vec![Delegation { name: "deploy".into(), question: vec!["deploy".into()], answers: Vec::new() }];
        assert_eq!(delegated(&deploy, &p, &option(0)), None);
        assert_eq!(delegated(&[Delegation::default()], &p, &option(0)), None);
        // Any answer that is not for good, by number too.
        let any = vec![Delegation { name: String::new(), question: vec!["proceed".into()], answers: vec!["1".into()] }];
        assert!(delegated(&any, &p, &option(0)).is_some());
        let line = decision_line("Fix login", &p, &option(0), &tests);
        assert!(line.starts_with("In “Fix login”, the manager answered “Bash command: Do you want to proceed?”") && line.ends_with("with “Yes”, as delegated: run the tests."), "{line}");
        assert!(not_delegated(&p, "Yes").contains("delegates"));
        assert!(tests.describe().contains("“Yes”"));
    }
}
