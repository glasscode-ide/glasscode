//! How much the manager, and the sessions it drives, may do in a project: three levels, and the
//! column of the default table ([`super::defaults`]) each caller reads.

use serde::{Deserialize, Serialize};

use crate::control::Rule;

/// A project's trust level. A project the policy does not name, and every project while the
/// policy cannot be read or was changed outside the app, is [`Level::Careful`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// The manager asks before anything but reading, the board and its own notes.
    #[default]
    Careful,
    /// The manager starts and drives sessions; what leaves the Mac or removes work asks.
    Standard,
    /// Fully managed: it also acts on the forge where the forge's own rules hold (D29).
    Autopilot,
}

impl Level {
    pub const ALL: [Level; 3] = [Level::Careful, Level::Standard, Level::Autopilot];

    pub fn key(self) -> &'static str {
        match self {
            Level::Careful => "careful",
            Level::Standard => "standard",
            Level::Autopilot => "autopilot",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Level::Careful => "Careful",
            Level::Standard => "Standard",
            Level::Autopilot => "Autopilot",
        }
    }

    pub fn parse(text: &str) -> Option<Level> {
        Level::ALL.into_iter().find(|level| level.key().eq_ignore_ascii_case(text.trim()))
    }

    /// One line for Settings and the generated header of `POLICY.md`.
    pub fn describe(self) -> &'static str {
        match self {
            Level::Careful => "the manager asks before it does anything but read, keep the board and take notes",
            Level::Standard => "the manager starts and drives sessions; removing work and anything that leaves this Mac asks",
            Level::Autopilot => "the manager also comments, labels and opens draft pull requests where the forge's own rules guard merges",
        }
    }
}

/// Which column of the default table a non-person caller reads. Careful has one for each; the
/// other levels have one for both.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Column {
    /// The manager's brain, in either lane.
    Manager,
    /// A session a playbook or the manager started, or sent text to (M22's automation and
    /// quarantine origins).
    Automation,
}

/// How strict a rule is: Allow < Ask < Never.
pub fn strictness(rule: Rule) -> u8 {
    match rule {
        Rule::Allow => 0,
        Rule::Ask => 1,
        Rule::Never => 2,
    }
}

/// The stricter of two rules.
pub fn stricter(a: Rule, b: Rule) -> Rule {
    if strictness(b) > strictness(a) { b } else { a }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_read_back_and_order() {
        for level in Level::ALL {
            assert_eq!(Level::parse(level.key()), Some(level));
            assert_eq!(Level::parse(&level.label().to_uppercase()), Some(level));
            assert_eq!(serde_json::to_value(level).unwrap(), serde_json::json!(level.key()));
        }
        assert_eq!(Level::parse("yolo"), None);
        assert_eq!(Level::default(), Level::Careful);
        assert_eq!(stricter(Rule::Allow, Rule::Ask), Rule::Ask);
        assert_eq!(stricter(Rule::Never, Rule::Ask), Rule::Never);
        assert_eq!(stricter(Rule::Allow, Rule::Allow), Rule::Allow);
    }
}
