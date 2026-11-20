//! Who is asking Glasscode to do something: decided by the channel a request came in on, and by
//! what the app itself saw, never by what the request says about itself (design D2).
//!
//! - `Ui`: the app's own window, in-process only. Nothing that arrives on the control socket, in a
//!   request file or from the server is `Ui`: [`by_channel`] cannot return it.
//! - `Session`: a session Glasscode started, proven by the signature the app gave it.
//! - `ShellTab`: the person, typing a `glasscode` command in a Glasscode shell tab. The app's own
//!   terminal view saw the Enter key on that line ([`note_typed_command`]); the request's words do
//!   not count. This is attribution, not a wall: a program the person started in that tab within
//!   [`SHELL_WINDOW_MS`] of the key is taken for them too.
//! - `Manager`: the manager's brain. Until it has its own MCP server instance (M34), v1's rule
//!   stays: a socket caller that runs under the brain's process, which is attribution as well.
//! - `Phone`: a paired device, and `Machine`: another Glasscode machine's sync connection. glasscoded
//!   assigns both per connection (M13.2); the app never derives them.
//! - `Terminal`: everything else, the person at a terminal outside Glasscode included, as any
//!   program can say it is that terminal. It may read; anything else waits on a card in the app
//!   (owner decision O13).
//!
//! The request's `interactive` flag ([`crate::control::Request`]) is still sent, for older apps; no
//! caller is decided by it here.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::control::{AgentSettings, Kind, Request, Rule};
use crate::paths::{MachineId, ProjectKey};

/// The manager's two lanes (M51): the chat with the person, and wakes with nobody in the loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lane {
    Chat,
    Wake,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Caller {
    /// The app's own window (in-process only).
    Ui,
    /// An agent, or a program, in a session Glasscode started.
    Session { id: String, project: Option<ProjectKey> },
    /// The person, typing in that session's Glasscode shell tab (the app saw the keys).
    ShellTab { session: String, project: Option<ProjectKey> },
    Manager { lane: Lane, wake: Option<String>, project: Option<ProjectKey> },
    /// A paired phone or glasses, signed (glasscoded).
    Phone { device: String },
    /// Another Glasscode machine's sync connection (the Mac, seen from glasscoded).
    Machine { id: MachineId },
    /// Any other program running as the person: a terminal outside Glasscode, a script.
    Terminal { pid: Option<u32> },
}

impl Caller {
    /// The person themselves: the app's window, or a command they typed in a Glasscode shell tab.
    pub fn is_person(&self) -> bool {
        matches!(self, Caller::Ui | Caller::ShellTab { .. })
    }
}

/// How a request reached the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Channel {
    /// The control socket (a named pipe on Windows), and the process on the other end when the
    /// system says which.
    Socket { pid: Option<u32> },
    /// A request file left in a worktree: anyone who can write there can leave one.
    RequestFile,
    /// A `glasscode` command on the server, relayed by glasscoded, which checked the signature of
    /// the session it names (none: a program on the server).
    Server { session: Option<String> },
}

/// What the app knows of the session a request names, once the signature checked out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Signed {
    /// The session runs a shell, not an agent.
    pub shell: bool,
}

/// The caller of `request`, which came in on `channel`. The app answers three questions, from what
/// it knows and never from the request:
/// - `signed`: the session a request names and its signature, when the signature is the app's for
///   a session it has;
/// - `under_brain`: whether a process runs under the manager's brain (v1's attribution);
/// - `typed_in_tab`: whether the person just typed a `glasscode` line in that session's tab.
///
/// `request.interactive` is not read: a request that says it was typed at a terminal is the same
/// caller as one that does not.
pub fn by_channel(request: &Request, channel: &Channel, signed: impl Fn(&str, &str) -> Option<Signed>, under_brain: impl Fn(u32) -> bool, typed_in_tab: impl Fn(&str) -> bool) -> Caller {
    if let Channel::Server { session } = channel {
        return match session {
            Some(id) => Caller::Session { id: format!("server:{id}"), project: None },
            None => Caller::Terminal { pid: None },
        };
    }
    if let (Some(id), Some(sig)) = (&request.session, &request.sig) {
        if let Some(session) = signed(id, sig) {
            // The keys of a shell tab are typed in the app's own window, so only a request on the
            // socket (a command the shell ran) can be them; a request file is anyone's.
            if session.shell && matches!(channel, Channel::Socket { .. }) && typed_in_tab(id) {
                return Caller::ShellTab { session: id.clone(), project: None };
            }
            return Caller::Session { id: id.clone(), project: None };
        }
    }
    match channel {
        Channel::Socket { pid: Some(pid) } if under_brain(*pid) => Caller::Manager { lane: Lane::Wake, wake: None, project: None },
        Channel::Socket { pid } => Caller::Terminal { pid: *pid },
        _ => Caller::Terminal { pid: None },
    }
}

// ----- what each caller may do (v1's rules) ---------------------------------------------------

/// What Settings › Permissions says for `kind` from `caller`, until the v2 gate (M21.1) decides:
/// the person may do everything; a session or the manager follows the rules of Settings; anyone
/// else (a terminal, a phone, another machine) gets [`outside_rule`]. The app adds "Allow for this
/// session" on top, and with the engine at v1 still treats a terminal that says it is typed at as
/// the person (O13).
pub fn rule(caller: &Caller, kind: Kind, settings: &AgentSettings) -> Rule {
    match caller {
        Caller::Ui | Caller::ShellTab { .. } => Rule::Allow,
        Caller::Session { .. } | Caller::Manager { .. } => settings.rule(kind),
        Caller::Phone { .. } | Caller::Machine { .. } | Caller::Terminal { .. } => outside_rule(kind, settings),
    }
}

/// Today's rule for a program Glasscode did not start: it may read (as Settings allows), "never"
/// stays never, and everything else waits on a card, unless the person let such programs follow
/// the same rules as agents (`outside_allowed`).
pub fn outside_rule(kind: Kind, settings: &AgentSettings) -> Rule {
    if settings.outside_allowed {
        return settings.rule(kind);
    }
    match (kind, settings.rule(kind)) {
        (Kind::Read, rule) => rule,
        (_, Rule::Never) => Rule::Never,
        _ => Rule::Ask,
    }
}

// ----- keys typed in a shell tab ---------------------------------------------------------------

/// How long after the person pressed Enter on a `glasscode` line in a shell tab a request from
/// that tab's session is theirs. The command connects within a second; a line of several commands
/// (`glasscode a && glasscode b`) takes longer.
pub const SHELL_WINDOW_MS: u64 = 15_000;

/// When the person last pressed Enter on a line holding `glasscode`, per shell session. Only the
/// time is kept, never the line: people type passwords in shells.
static TYPED: Mutex<Option<HashMap<String, u64>>> = Mutex::new(None);

/// The terminal view saw the person press Enter on a `glasscode` line in this session's tab.
pub fn note_typed_command(session: &str) {
    let now = crate::clock::now_ms();
    let mut typed = TYPED.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let typed = typed.get_or_insert_with(HashMap::new);
    typed.retain(|_, at| within_window(*at, now));
    typed.insert(session.to_string(), now);
}

/// Whether the person typed a `glasscode` line in this session's tab within [`SHELL_WINDOW_MS`].
pub fn typed_command_recently(session: &str) -> bool {
    let now = crate::clock::now_ms();
    let typed = TYPED.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    typed.as_ref().and_then(|typed| typed.get(session)).is_some_and(|at| within_window(*at, now))
}

fn within_window(at: u64, now: u64) -> bool {
    now >= at && now - at <= SHELL_WINDOW_MS
}

/// Whether a line on the screen, where the person pressed Enter, names the `glasscode` command
/// (`glasscode`, `glasscode.exe`, a path to it) as a word. A word that only contains it does not.
/// The line starts with the shell's prompt, which differs per person, so any word counts (a
/// `cd` into a folder named glasscode too: attribution, for the short window).
pub fn runs_glasscode(line: &str) -> bool {
    line.split(|c: char| c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | ')' | '`' | '$' | '\'' | '"'))
        .any(|word| matches!(word.rsplit(['/', '\\']).next(), Some("glasscode" | "glasscode.exe")))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::clock::Clock;

    fn request(session: Option<(&str, &str)>, interactive: bool) -> Request {
        Request { v: crate::control::VERSION, args: vec!["board".into(), "add".into(), "x".into()], session: session.map(|(s, _)| s.to_string()), sig: session.map(|(_, sig)| sig.to_string()), interactive, cwd: None }
    }

    /// The app's answers in the tests: session "s-agent" (an agent) and "s-shell" (a shell), each
    /// signed "good"; process 77 under the brain; the person typed in "s-shell"'s tab when `typed`.
    fn derive(request: &Request, channel: &Channel, typed: bool) -> Caller {
        let signed = |id: &str, sig: &str| match (id, sig) {
            ("s-agent", "good") => Some(Signed { shell: false }),
            ("s-shell", "good") => Some(Signed { shell: true }),
            _ => None,
        };
        by_channel(request, channel, signed, |pid| pid == 77, |id| typed && id == "s-shell")
    }

    #[test]
    fn interactive_flag_from_a_socket_is_terminal() {
        for pid in [Some(4242), None] {
            let channel = Channel::Socket { pid };
            assert_eq!(derive(&request(None, true), &channel, false), Caller::Terminal { pid });
            // The flag changes nothing: with it or without it, the same caller.
            assert_eq!(derive(&request(None, true), &channel, true), derive(&request(None, false), &channel, true));
        }
        // A signature that is not the app's is no session, whatever else the request says.
        assert_eq!(derive(&request(Some(("s-agent", "forged")), true), &Channel::Socket { pid: Some(9) }, false), Caller::Terminal { pid: Some(9) });
    }

    #[test]
    fn signed_shell_session_with_interactive_is_not_you() {
        // A script in a shell session sends a signed request that says it is typed at a terminal:
        // the person did not press Enter in that tab, so it is the session, not the person.
        let script = derive(&request(Some(("s-shell", "good")), true), &Channel::Socket { pid: Some(5) }, false);
        assert_eq!(script, Caller::Session { id: "s-shell".into(), project: None });
        assert!(!script.is_person());
        // The person's own Enter in that tab is what makes it them, not the flag.
        let typed = derive(&request(Some(("s-shell", "good")), false), &Channel::Socket { pid: Some(5) }, true);
        assert_eq!(typed, Caller::ShellTab { session: "s-shell".into(), project: None });
        assert!(typed.is_person());
        // An agent's session never is, keys or not; and a request file never is.
        assert_eq!(derive(&request(Some(("s-agent", "good")), true), &Channel::Socket { pid: Some(5) }, true), Caller::Session { id: "s-agent".into(), project: None });
        assert_eq!(derive(&request(Some(("s-shell", "good")), true), &Channel::RequestFile, true), Caller::Session { id: "s-shell".into(), project: None });
    }

    #[test]
    fn no_socket_or_request_file_yields_ui() {
        let channels = [Channel::Socket { pid: Some(77) }, Channel::Socket { pid: Some(5) }, Channel::Socket { pid: None }, Channel::RequestFile, Channel::Server { session: Some("s-agent".into()) }, Channel::Server { session: None }];
        let sessions = [None, Some(("s-agent", "good")), Some(("s-shell", "good")), Some(("s-shell", "forged")), Some(("nobody", "good"))];
        let mut seen = std::collections::BTreeSet::new();
        for channel in &channels {
            for session in sessions {
                for interactive in [false, true] {
                    for typed in [false, true] {
                        let caller = derive(&request(session, interactive), channel, typed);
                        assert_ne!(caller, Caller::Ui, "{channel:?} {session:?} {interactive} {typed}");
                        seen.insert(format!("{caller:?}").split([' ', '{']).next().unwrap_or_default().to_string());
                    }
                }
            }
        }
        // Every caller a channel can give came up, and none of them is Ui.
        assert_eq!(seen.into_iter().collect::<Vec<_>>(), ["Manager", "Session", "ShellTab", "Terminal"]);
        // The server's relay: its session, never the person; a program there is a terminal.
        assert_eq!(derive(&request(None, true), &Channel::Server { session: Some("abc".into()) }, true), Caller::Session { id: "server:abc".into(), project: None });
        assert_eq!(derive(&request(None, true), &Channel::Server { session: None }, true), Caller::Terminal { pid: None });
        // The brain's process: the manager, by v1's rule (attribution).
        assert_eq!(derive(&request(None, true), &Channel::Socket { pid: Some(77) }, false), Caller::Manager { lane: Lane::Wake, wake: None, project: None });
    }

    struct Fixed(AtomicU64);

    impl Clock for Fixed {
        fn now_ms(&self) -> u64 {
            self.0.load(Ordering::Relaxed)
        }

        fn local_minutes(&self, at_ms: u64) -> Option<i64> {
            Some((at_ms / 60_000) as i64)
        }
    }

    #[test]
    fn a_typed_command_counts_for_a_short_while() {
        let clock = Arc::new(Fixed(AtomicU64::new(1_000_000)));
        let _guard = crate::clock::use_test_clock(clock.clone());
        assert!(!typed_command_recently("tab-1"));
        note_typed_command("tab-1");
        assert!(typed_command_recently("tab-1") && !typed_command_recently("tab-2"));
        clock.0.store(1_000_000 + SHELL_WINDOW_MS, Ordering::Relaxed);
        assert!(typed_command_recently("tab-1"));
        clock.0.store(1_000_001 + SHELL_WINDOW_MS, Ordering::Relaxed);
        assert!(!typed_command_recently("tab-1"));
        // A clock that went back does not make an old key new.
        clock.0.store(10, Ordering::Relaxed);
        assert!(!typed_command_recently("tab-1"));
    }

    #[test]
    fn sees_the_glasscode_command_on_a_line() {
        for line in ["alex@mac app % glasscode board add x", "$ ./bin/glasscode status", "PS C:\\app> glasscode.exe session list", "make && glasscode notify done", "(glasscode whoami)"] {
            assert!(runs_glasscode(line), "{line}");
        }
        for line in ["alex@mac app % ls glasscode-notes", "git log --grep glasscode-ide", "vim mglasscode.rs", ""] {
            assert!(!runs_glasscode(line), "{line}");
        }
    }

    #[test]
    fn a_terminal_reads_and_asks_for_the_rest() {
        let settings = AgentSettings::default();
        let terminal = Caller::Terminal { pid: Some(4242) };
        assert_eq!(rule(&terminal, Kind::Read, &settings), Rule::Allow);
        // Agents may start sessions and add tasks by default; a terminal asks.
        for kind in [Kind::Board, Kind::Start, Kind::Close, Kind::Remove] {
            assert_eq!(rule(&terminal, kind, &settings), Rule::Ask, "{kind:?}");
        }
        assert_eq!(rule(&terminal, Kind::Settings, &settings), Rule::Never);
        assert_eq!(rule(&Caller::Session { id: "s".into(), project: None }, Kind::Start, &settings), Rule::Allow);
        assert_eq!(rule(&Caller::Session { id: "s".into(), project: None }, Kind::Remove, &settings), Rule::Ask);
        for person in [Caller::Ui, Caller::ShellTab { session: "s".into(), project: None }] {
            assert!(Kind::ALL.iter().all(|kind| rule(&person, *kind, &settings) == Rule::Allow));
        }
        // The person let programs follow the agents' rules.
        let open = AgentSettings { outside_allowed: true, ..AgentSettings::default() };
        assert_eq!(rule(&terminal, Kind::Start, &open), Rule::Allow);
    }

    #[test]
    fn a_caller_round_trips_as_json() {
        let caller = Caller::Session { id: "s1".into(), project: ProjectKey::parse("app-3f9a1c") };
        let json = serde_json::to_string(&caller).unwrap();
        assert_eq!(json, r#"{"kind":"session","id":"s1","project":"app-3f9a1c"}"#);
        assert_eq!(serde_json::from_str::<Caller>(&json).unwrap(), caller);
    }
}
