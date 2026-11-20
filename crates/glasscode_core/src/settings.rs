//! The settings a caller may read or change by name (`settings.get`, `settings.set`; M33.2,
//! ops-3): one table of key, type and allowed values, a validator that runs at write time, and a
//! tier that is code, not policy. The Settings page's cards name these keys
//! (`capabilities::card_key` in the app), and a test holds the two together.
//!
//! The tiers:
//! - **Free**: how the app looks and sounds; anyone the gate lets change a setting may.
//! - **Ask**: notices, how sessions behave, the autofix's tries and waits; a caller that is not the
//!   person asks first (the gate's `Settings` rule decides; this tier says it is not free).
//! - **PersonOnly**: the gate itself (the 13 rules, programs outside Glasscode, the skill, the log
//!   of what agents did), the manager's switch, what runs a program (the editor), and what acts
//!   outward by itself (autofix, auto-review, moving tickets, the update channel). Refused for
//!   every caller but the person in the app, with the section that has it.
//! - **Secret**: never returned and never set (the Linear key; account credentials have no key).
//!
//! This binds callers confined to the op surface (the manager's brain, the phone, voice). A
//! session with a shell can still edit `state.json` itself; for sessions the walls are the agent's
//! own sandbox and the deny rules on the Glasscode folder.

use serde_json::{Value, json};

use crate::control::{Kind, Rule};
use crate::model::State;

/// Who may change a setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Free,
    Ask,
    PersonOnly,
    Secret,
}

/// What a setting holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Type {
    Switch,
    /// One of these words.
    Choice(&'static [&'static str]),
    /// A whole number in this range.
    Number { min: u64, max: u64 },
    /// Any text, or null for the default.
    Text,
    /// A list of words.
    Words,
}

/// One setting.
#[derive(Clone, Copy)]
pub struct Setting {
    /// "appearance.theme".
    pub key: &'static str,
    pub kind: Type,
    pub tier: Tier,
    /// What it does, in the Settings page's words.
    pub help: &'static str,
    /// The section of Settings it is in (its menu label).
    pub section: &'static str,
    get: fn(&State) -> Value,
    set: fn(&mut State, &Value),
}

impl std::fmt::Debug for Setting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Setting").field("key", &self.key).field("kind", &self.kind).field("tier", &self.tier).finish()
    }
}

impl Setting {
    /// Its value now; a secret's is never given.
    pub fn get(&self, state: &State) -> Value {
        match self.tier {
            Tier::Secret => json!("(secret)"),
            _ => (self.get)(state),
        }
    }

    /// Why `value` is not one this setting takes, if it is not.
    pub fn check(&self, value: &Value) -> Result<(), String> {
        if self.tier == Tier::Secret {
            return Err(format!("{} is a secret: only you set it, in Settings › {}.", self.key, self.section));
        }
        let ok = match self.kind {
            Type::Switch => value.is_boolean(),
            Type::Choice(words) => value.as_str().is_some_and(|w| words.contains(&w)),
            Type::Number { min, max } => value.as_u64().is_some_and(|n| (min..=max).contains(&n)),
            Type::Text => value.is_string() || value.is_null(),
            Type::Words => value.as_array().is_some_and(|items| items.iter().all(|i| i.as_str().is_some_and(|s| !s.trim().is_empty()))),
        };
        if ok { Ok(()) } else { Err(format!("{} takes {}, not {value}.", self.key, self.takes())) }
    }

    /// What it takes, in words.
    pub fn takes(&self) -> String {
        match self.kind {
            Type::Switch => "true or false".into(),
            Type::Choice(words) => format!("one of {}", words.join(", ")),
            Type::Number { min, max } => format!("a number from {min} to {max}"),
            Type::Text => "a text, or null for the default".into(),
            Type::Words => "a list of words".into(),
        }
    }

    /// Change it, after [`Setting::check`]; the value before, for the journal and an undo.
    pub fn set(&self, state: &mut State, value: &Value) -> Result<Value, String> {
        self.check(value)?;
        let before = (self.get)(state);
        (self.set)(state, value);
        Ok(before)
    }
}

/// Whether a caller may change `setting` (`person`: the person, by the channel): a secret never,
/// the person's own keys only for them; the rest goes to the gate's `Settings` rule.
pub fn may_set(setting: &Setting, person: bool) -> Result<(), String> {
    match setting.tier {
        Tier::Secret => Err(format!("{} is a secret: only you set it, in Settings › {}.", setting.key, setting.section)),
        Tier::PersonOnly if !person => Err(format!("Only you change {} ({}), in Settings › {}.", setting.key, setting.help, setting.section)),
        _ => Ok(()),
    }
}

/// A setting by its key.
pub fn find(key: &str) -> Option<&'static Setting> {
    SETTINGS.iter().find(|s| s.key == key.trim())
}

/// Every setting and its value, secrets redacted: what `settings.get` with no key gives.
pub fn all(state: &State) -> Value {
    Value::Object(SETTINGS.iter().map(|s| (s.key.to_string(), s.get(state))).collect())
}

fn switch(value: &Value) -> bool {
    value.as_bool().unwrap_or_default()
}

fn word(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}

/// A field of an enum by its serde word (checked against the choice first).
fn from<T: serde::de::DeserializeOwned + Default>(value: &Value) -> T {
    serde_json::from_value(value.clone()).unwrap_or_default()
}

fn to(value: impl serde::Serialize) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

const RULES: &[&str] = &["allow", "ask", "never"];

fn rule_of(value: &Value) -> Rule {
    match word(value) {
        "allow" => Rule::Allow,
        "never" => Rule::Never,
        _ => Rule::Ask,
    }
}

/// The 13 rules of Settings › Permissions, one key each.
macro_rules! rule {
    ($key:literal, $kind:expr) => {
        Setting {
            key: $key,
            kind: Type::Choice(RULES),
            tier: Tier::PersonOnly,
            help: "what agents may do without asking (Settings › Permissions)",
            section: "Permissions",
            get: |s| to(s.agents.rule($kind)),
            set: |s, v| s.agents.set($kind, rule_of(v)),
        }
    };
}

const fn setting(key: &'static str, kind: Type, tier: Tier, section: &'static str, help: &'static str, get: fn(&State) -> Value, set: fn(&mut State, &Value)) -> Setting {
    Setting { key, kind, tier, help, section, get, set }
}

use Tier::{Ask, Free, PersonOnly, Secret};

/// Every setting, in the order of the Settings page.
pub static SETTINGS: &[Setting] = &[
    // General
    setting("general.startup", Type::Choice(&["restore", "restore_and_new_claude"]), Ask, "General", "what comes back when Glasscode opens", |s| to(s.startup), |s, v| s.startup = from(v)),
    setting("notify.needs", Type::Switch, Ask, "General", "a notice when a session needs you", |s| json!(!s.notify.needs_input_off), |s, v| s.notify.needs_input_off = !switch(v)),
    setting("notify.finished", Type::Switch, Ask, "General", "a notice when a session finishes its turn", |s| json!(!s.notify.finished_off), |s, v| s.notify.finished_off = !switch(v)),
    setting("notify.ci", Type::Switch, Ask, "General", "a notice when CI fails on one of your pull requests", |s| json!(!s.prefs.notify_ci_off), |s, v| s.prefs.notify_ci_off = !switch(v)),
    setting("notify.tickets", Type::Switch, Ask, "General", "a notice when a ticket or a review asks for you", |s| json!(!s.linear.notify_off), |s, v| s.linear.notify_off = !switch(v)),
    setting("notify.sound", Type::Switch, Free, "General", "a sound with each notice", |s| json!(!s.notify.sound_off), |s, v| s.notify.sound_off = !switch(v)),
    setting("sessions.auto_names", Type::Switch, Ask, "General", "name sessions and new worktrees by themselves", |s| json!(!s.auto_names_off), |s, v| s.auto_names_off = !switch(v)),
    setting("sessions.confirm_close", Type::Switch, Ask, "General", "ask before closing a session that is working", |s| json!(!s.prefs.confirm_close_off), |s, v| s.prefs.confirm_close_off = !switch(v)),
    // It names a program Glasscode runs.
    setting("general.editor", Type::Text, PersonOnly, "General", "the editor files open in", |s| to(&s.prefs.editor), |s, v| s.prefs.editor = v.as_str().map(str::to_string).filter(|e| !e.trim().is_empty())),
    // Manager
    setting("manager.enabled", Type::Switch, PersonOnly, "Manager", "the manager is on", |s| json!(s.manager.enabled), |s, v| s.manager.enabled = switch(v)),
    // Appearance
    setting("appearance.theme", Type::Choice(&["auto", "light", "dark", "classic", "contrast"]), Free, "Appearance", "the theme", |s| to(s.appearance.theme), |s, v| s.appearance.theme = from(v)),
    setting("appearance.accent", Type::Choice(&["theme", "indigo", "amber"]), Free, "Appearance", "the accent colour", |s| to(s.appearance.accent), |s, v| s.appearance.accent = from(v)),
    setting("appearance.backdrop", Type::Choice(&["prism", "aurora", "dusk", "none"]), Free, "Appearance", "the moving backdrop", |s| to(s.appearance.backdrop), |s, v| s.appearance.backdrop = from(v)),
    setting("appearance.finish", Type::Choice(&["smooth", "dither"]), Free, "Appearance", "how the backdrop is drawn", |s| to(s.appearance.finish), |s, v| s.appearance.finish = from(v)),
    setting("appearance.backdrop_in_sessions", Type::Switch, Free, "Appearance", "the backdrop behind sessions too", |s| json!(s.appearance.backdrop_in_sessions), |s, v| s.appearance.backdrop_in_sessions = switch(v)),
    setting("appearance.scale", Type::Choice(&["p90", "p100", "p115", "p130"]), Free, "Appearance", "the size of the whole interface", |s| to(s.appearance.scale), |s, v| s.appearance.scale = from(v)),
    setting("appearance.text_size", Type::Choice(&["small", "medium", "large"]), Free, "Appearance", "the text size", |s| to(s.appearance.text_size), |s, v| s.appearance.text_size = from(v)),
    setting("appearance.density", Type::Choice(&["comfortable", "compact"]), Free, "Appearance", "the rows of the sidebar", |s| to(s.appearance.density), |s, v| s.appearance.density = from(v)),
    setting("appearance.ui_font", Type::Choice(&["geist", "inter", "plex", "system"]), Free, "Appearance", "the interface font", |s| to(s.appearance.ui_font), |s, v| s.appearance.ui_font = from(v)),
    setting("appearance.code_font", Type::Choice(&["geist", "jet-brains", "plex", "system"]), Free, "Appearance", "the code font", |s| to(s.appearance.code_font), |s, v| s.appearance.code_font = from(v)),
    setting("appearance.chat_font", Type::Choice(&["ui", "serif", "system"]), Free, "Appearance", "the conversation font", |s| to(s.appearance.chat_font), |s, v| s.appearance.chat_font = from(v)),
    setting("appearance.session_view", Type::Choice(&["chat", "terminal"]), Free, "Appearance", "how sessions open", |s| to(s.appearance.session_view), |s, v| s.appearance.session_view = from(v)),
    setting("appearance.cursor", Type::Choice(&["block", "bar", "underline"]), Free, "Appearance", "the terminal cursor", |s| to(s.appearance.cursor), |s, v| s.appearance.cursor = from(v)),
    // Permissions: the gate.
    rule!("permissions.read", Kind::Read),
    rule!("permissions.board", Kind::Board),
    rule!("permissions.start", Kind::Start),
    rule!("permissions.rename", Kind::Rename),
    rule!("permissions.send", Kind::Send),
    rule!("permissions.import", Kind::Import),
    rule!("permissions.close", Kind::Close),
    rule!("permissions.remove", Kind::Remove),
    rule!("permissions.move", Kind::Move),
    rule!("permissions.tickets", Kind::Tickets),
    rule!("permissions.github", Kind::Github),
    rule!("permissions.show", Kind::Show),
    rule!("permissions.settings", Kind::Settings),
    setting("agents.skill", Type::Switch, PersonOnly, "Permissions", "tell new sessions about the glasscode command", |s| json!(!s.agents.skill_off), |s, v| s.agents.skill_off = !switch(v)),
    setting("agents.outside_allowed", Type::Switch, PersonOnly, "Permissions", "programs Glasscode did not start follow these rules too", |s| json!(s.agents.outside_allowed), |s, v| s.agents.outside_allowed = switch(v)),
    // Git and projects
    setting("projects.fresh_main", Type::Switch, Ask, "Git and projects", "start new work from the latest main", |s| json!(!s.prefs.fresh_main_off), |s, v| s.prefs.fresh_main_off = !switch(v)),
    setting("projects.delete_branch", Type::Switch, Ask, "Git and projects", "also delete the local branch when removing a worktree", |s| json!(!s.prefs.keep_branch_on_remove), |s, v| s.prefs.keep_branch_on_remove = !switch(v)),
    // Integrations
    setting("integrations.linear_key", Type::Text, Secret, "Integrations", "the Linear API key", |_| Value::Null, |_, _| {}),
    // Automation: what acts outward by itself is the person's.
    setting("automation.fix_ci", Type::Switch, PersonOnly, "Automation", "ask the agent to fix failing CI", |s| json!(!s.ci_autofix_off), |s, v| s.ci_autofix_off = !switch(v)),
    setting("automation.fix_conflicts", Type::Switch, PersonOnly, "Automation", "ask the agent to fix merge conflicts", |s| json!(!s.conflict_autofix_off), |s, v| s.conflict_autofix_off = !switch(v)),
    setting("automation.fix_tries", Type::Number { min: 1, max: 10 }, Ask, "Automation", "how often the agent tries a fix", |s| json!(s.prefs.fix_tries()), |s, v| s.prefs.fix_tries = v.as_u64().map(|n| n as u32)),
    setting("automation.ci_wait", Type::Number { min: 1, max: 120 }, Ask, "Automation", "minutes to wait for CI before asking", |s| json!(s.prefs.ci_wait().as_secs() / 60), |s, v| s.prefs.ci_wait_minutes = v.as_u64().map(|n| n as u32)),
    setting("automation.auto_review", Type::Words, PersonOnly, "Automation", "the repositories whose pull requests get a review by themselves", |s| to(&s.auto_review_repos), |s, v| s.auto_review_repos = v.as_array().map(|items| items.iter().filter_map(|i| i.as_str().map(|r| r.trim().to_string())).collect()).unwrap_or_default()),
    setting("automation.move_ticket", Type::Switch, PersonOnly, "Automation", "move a ticket to started when a session starts on it", |s| json!(!s.linear.move_on_start_off), |s, v| s.linear.move_on_start_off = !switch(v)),
    setting("automation.agents_log", Type::Switch, PersonOnly, "Automation", "keep the log of what agents did", |s| json!(!s.agents.log_off), |s, v| s.agents.log_off = !switch(v)),
    setting("automation.morning", Type::Switch, Ask, "Automation", "the morning update", |s| json!(!s.agents.morning_off), |s, v| s.agents.morning_off = !switch(v)),
    // Updates
    setting("updates.channel", Type::Choice(&["stable", "beta"]), PersonOnly, "Updates", "the update channel", |s| json!(if s.prefs.update_channel() == "beta" { "beta" } else { "stable" }), |s, v| s.prefs.update_channel = (word(v) == "beta").then(|| "beta".to_string())),
    setting("updates.install_on_quit", Type::Switch, PersonOnly, "Updates", "install an update when Glasscode quits", |s| json!(!s.prefs.install_on_quit_off), |s, v| s.prefs.install_on_quit_off = !switch(v)),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_unique_and_well_formed() {
        let mut keys: Vec<&str> = SETTINGS.iter().map(|s| s.key).collect();
        let n = keys.len();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), n);
        for key in keys {
            assert!(key.contains('.') && key.bytes().all(|b| b.is_ascii_lowercase() || b == b'.' || b == b'_'), "{key}");
        }
    }

    /// Each value a setting takes is kept and read back as given, and its value now is one it
    /// takes: the choices are the words of the state's own enums.
    #[test]
    fn every_value_round_trips() {
        for setting in SETTINGS.iter().filter(|s| s.tier != Tier::Secret) {
            let mut state = State::default();
            let now = setting.get(&state);
            assert!(setting.check(&now).is_ok(), "{}: its value now, {now}, is not one it takes", setting.key);
            let values: Vec<Value> = match setting.kind {
                Type::Switch => vec![json!(true), json!(false)],
                Type::Choice(words) => words.iter().map(|w| json!(w)).collect(),
                Type::Number { min, max } => vec![json!(min), json!(max)],
                Type::Text => vec![json!("zed")],
                Type::Words => vec![json!(["acme/app", "acme/web"])],
            };
            for value in values {
                setting.set(&mut state, &value).unwrap();
                assert_eq!(setting.get(&state), value, "{}", setting.key);
            }
        }
    }

    #[test]
    fn values_are_checked_when_written() {
        let theme = find("appearance.theme").unwrap();
        let mut state = State::default();
        assert!(theme.set(&mut state, &json!("neon")).unwrap_err().contains("one of auto, light"));
        assert!(find("automation.fix_tries").unwrap().check(&json!(11)).is_err());
        assert!(find("notify.sound").unwrap().check(&json!("yes")).is_err());
        assert_eq!(theme.set(&mut state, &json!("dark")).unwrap(), json!("auto"), "the value before");
    }

    #[test]
    fn secrets_are_never_given_or_set() {
        let key = find("integrations.linear_key").unwrap();
        assert_eq!(key.get(&State::default()), json!("(secret)"));
        assert!(key.set(&mut State::default(), &json!("lin_api_x")).is_err());
        assert_eq!(all(&State::default())["integrations.linear_key"], json!("(secret)"));
    }

    /// The manager (any caller that is not the person) sets no key of the gate, of what runs a
    /// program or acts outward by itself, and no secret; the person sets all but secrets.
    #[test]
    fn settings_set_refuses_hooks_for_manager() {
        for setting in SETTINGS {
            let manager = may_set(setting, false);
            match setting.tier {
                Tier::Free | Tier::Ask => assert!(manager.is_ok(), "{}", setting.key),
                Tier::PersonOnly | Tier::Secret => assert!(manager.is_err(), "{}", setting.key),
            }
            assert_eq!(may_set(setting, true).is_ok(), setting.tier != Tier::Secret, "{}", setting.key);
        }
        assert!(may_set(find("agents.skill").unwrap(), false).unwrap_err().contains("Settings › Permissions"));
    }

    /// The gate is the person's: its rules, outside programs, the skill and the log.
    #[test]
    fn the_gate_is_person_only() {
        for key in ["permissions.read", "permissions.settings", "agents.outside_allowed", "agents.skill", "automation.agents_log", "manager.enabled", "general.editor", "automation.auto_review", "updates.channel"] {
            assert_eq!(find(key).unwrap().tier, Tier::PersonOnly, "{key}");
        }
        let rules = SETTINGS.iter().filter(|s| s.key.starts_with("permissions.")).count();
        assert_eq!(rules, Kind::ALL.len());
    }
}
