//! Signing an account in without a terminal.
//!
//! Glasscode never sees a password or a token: the official CLI signs the account in itself, in
//! the account's own folder. What changes here is how it is started. Instead of a terminal with
//! the CLI in it, the CLI's own login command runs out of sight (`claude auth login`,
//! `codex login`): it opens the browser, prints the address in case the browser did not open, and
//! ends when the person has signed in. The app shows that as a state of the account's row.
//!
//! Seen with Claude Code 2.1 and Codex 0.15x, started without a terminal:
//!
//! ```text
//! Opening browser to sign in…
//! If the browser didn't open, visit: https://claude.com/cai/oauth/authorize?…
//! Paste code here if prompted >
//! ```
//!
//! ```text
//! Starting local login server on http://localhost:1455.
//! If your browser did not open, navigate to this URL to authenticate:
//!
//! https://auth.openai.com/oauth/authorize?…
//! ```
//!
//! Claude Code can also take a code that the browser page shows, on its standard input
//! ([`Attempt::send_code`]). An older CLI without these commands ends at once with an error: the
//! app then offers the terminal, as before.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc::{Receiver, channel};

use crate::model::{Account, AgentKind};

/// What a running sign-in told us since the last look.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The address the browser was sent to, for "Open the link again" and "Copy link".
    Url(String),
    /// The CLI takes a code from the browser page.
    WantsCode,
    /// The CLI ended: signed in, or what it said last.
    Ended(Result<(), String>),
}

/// A sign-in under way: the CLI's login command, out of sight.
pub struct Attempt {
    child: Child,
    stdin: Option<ChildStdin>,
    output: Receiver<Vec<u8>>,
    /// What the CLI wrote and no event has used yet.
    pending: String,
    /// Its last lines, for a failure.
    tail: Vec<String>,
    url_seen: bool,
    code_seen: bool,
    ended: bool,
}

/// The CLI's login command for an account: its program, arguments and environment.
pub fn command(account: &Account, base_env: &HashMap<String, String>, email: Option<&str>) -> crate::agents::Launch {
    // The same program and environment as the terminal's login (`agents::login_launch`): the
    // account's own folder, and no self-update.
    let mut launch = crate::agents::login_launch(account, base_env);
    launch.args = match account.agent {
        AgentKind::Codex => vec!["login".into()],
        _ => {
            let mut args = vec!["auth".to_string(), "login".to_string()];
            if let Some(email) = email.map(str::trim).filter(|e| plain_email(e)) {
                args.extend(["--email".to_string(), email.to_string()]);
            }
            args
        }
    };
    launch
}

/// An address that is safe as an argument: nothing a command line could take for an option.
fn plain_email(text: &str) -> bool {
    !text.starts_with('-') && text.contains('@') && text.len() <= 254 && text.chars().all(|c| c.is_ascii_alphanumeric() || "@._+-".contains(c))
}

/// Start signing `account` in. The browser opens by the CLI's own doing.
pub fn start(account: &Account, base_env: &HashMap<String, String>, email: Option<&str>) -> anyhow::Result<Attempt> {
    let launch = command(account, base_env, email);
    let mut cmd = crate::sys::command(&launch.program);
    cmd.args(&launch.args).env_clear().envs(&launch.env).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    // No colours and no cursor moves in what we read.
    cmd.env("NO_COLOR", "1").env("TERM", "dumb");
    let mut child = cmd.spawn().map_err(|err| anyhow::anyhow!("could not start {}: {err}", launch.program))?;
    let (send, output) = channel();
    for mut stream in [child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>), child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>)].into_iter().flatten() {
        let send = send.clone();
        std::thread::spawn(move || {
            // By the piece, not by the line: the question for a code has no line end.
            let mut buf = [0u8; 2048];
            while let Ok(n) = stream.read(&mut buf) {
                if n == 0 || send.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
    }
    Ok(Attempt { stdin: child.stdin.take(), child, output, pending: String::new(), tail: Vec::new(), url_seen: false, code_seen: false, ended: false })
}

impl Attempt {
    /// What happened since the last call. After [`Event::Ended`] nothing more comes.
    pub fn poll(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        if self.ended {
            return events;
        }
        while let Ok(bytes) = self.output.try_recv() {
            self.pending.push_str(&clean(&String::from_utf8_lossy(&bytes)));
        }
        if !self.url_seen
            && let Some(url) = first_url(&self.pending)
        {
            self.url_seen = true;
            events.push(Event::Url(url));
        }
        if !self.code_seen && asks_for_code(&self.pending) {
            self.code_seen = true;
            events.push(Event::WantsCode);
        }
        // Whole lines go to the tail; the rest waits for its end (an address can come in two pieces).
        while let Some(end) = self.pending.find('\n') {
            let line: String = self.pending.drain(..=end).collect();
            let line = line.trim();
            if !line.is_empty() {
                self.tail.push(line.to_string());
                if self.tail.len() > 6 {
                    self.tail.remove(0);
                }
            }
        }
        if let Ok(Some(status)) = self.child.try_wait() {
            self.ended = true;
            self.stdin = None;
            events.push(Event::Ended(if status.success() { Ok(()) } else { Err(self.said_last()) }));
        }
        events
    }

    /// The code the browser page shows, for a CLI that asked for one.
    pub fn send_code(&mut self, code: &str) -> std::io::Result<()> {
        let code = code.trim();
        if code.is_empty() || code.contains(['\n', '\r']) {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "no code"));
        }
        let stdin = self.stdin.as_mut().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "the sign-in has ended"))?;
        stdin.write_all(code.as_bytes())?;
        stdin.write_all(b"\n")?;
        stdin.flush()
    }

    /// Stop: ends the CLI this attempt started (and nothing else).
    pub fn cancel(&mut self) {
        if !self.ended {
            self.ended = true;
            self.stdin = None;
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    pub fn ended(&self) -> bool {
        self.ended
    }

    /// The CLI's last line, without an address (it is long, and of no use in a message) and
    /// without its own question for a code, which has no line end and so stands before the answer.
    fn said_last(&self) -> String {
        let pending = self.pending.trim();
        let last = self.tail.iter().map(String::as_str).chain((!pending.is_empty()).then_some(pending)).filter(|l| !l.contains("https://")).last().unwrap_or_default();
        let words = match last.rsplit_once('>') {
            Some((question, answer)) if asks_for_code(question) => answer.trim(),
            _ => last,
        };
        if words.is_empty() { "the sign-in ended before it was done".to_string() } else { words.to_string() }
    }
}

impl Drop for Attempt {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Text without terminal sequences: colours, and the hyperlink wrapper (`ESC ] 8 ; ; url BEL`)
/// that Claude Code puts around its address.
fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                // CSI: to its final letter.
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                // OSC: to BEL or ESC \. A hyperlink's address is in it; keep it, with a space
                // before, so that it is found even when the visible words are not the address.
                Some(']') => {
                    let mut body = String::new();
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' {
                            break;
                        }
                        if c == '\u{1b}' {
                            chars.next_if_eq(&'\\');
                            break;
                        }
                        body.push(c);
                    }
                    if let Some(address) = body.strip_prefix("8;").and_then(|rest| rest.split_once(';')).map(|(_, address)| address).filter(|a| !a.is_empty()) {
                        out.push(' ');
                        out.push_str(address);
                        out.push(' ');
                    }
                }
                _ => {}
            },
            '\r' => {}
            c if c.is_control() && c != '\n' && c != '\t' => {}
            c => out.push(c),
        }
    }
    out
}

/// The first web address in the CLI's words that is whole: one that something follows. (The
/// local server's own `http://localhost` line of Codex is not it.)
fn first_url(text: &str) -> Option<String> {
    let mut rest = text;
    while let Some(at) = rest.find("https://") {
        let from = &rest[at..];
        let end = from.find(|c: char| c.is_whitespace())?;
        let url = &from[..end];
        if url.len() > "https://".len() + 3 && !url.contains(['"', '<', '>']) {
            return Some(url.trim_end_matches(['.', ',', ')']).to_string());
        }
        rest = &from[end..];
    }
    None
}

fn asks_for_code(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    text.contains("paste code") || text.contains("paste the code") || text.contains("enter the code")
}

/// Whether an account is signed in, and as whom, as its CLI says.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub signed_in: bool,
    /// The address or name the CLI gives, when it gives one.
    pub who: Option<String>,
    /// "Claude Max", "ChatGPT", "an API key": what the login is, when the CLI says.
    pub kind: Option<String>,
}

/// Ask the CLI (`claude auth status --json`, `codex login status`). It starts a program: call it
/// from a background task. None: the CLI did not answer in a form we know (an old CLI).
pub fn status(account: &Account, base_env: &HashMap<String, String>) -> Option<Status> {
    let mut launch = crate::agents::login_launch(account, base_env);
    launch.args = match account.agent {
        AgentKind::Codex => vec!["login".into(), "status".into()],
        _ => vec!["auth".into(), "status".into(), "--json".into()],
    };
    let output = crate::sys::command(&launch.program).args(&launch.args).env_clear().envs(&launch.env).stdin(Stdio::null()).output().ok()?;
    let (out, err) = (String::from_utf8_lossy(&output.stdout).to_string(), String::from_utf8_lossy(&output.stderr).to_string());
    match account.agent {
        AgentKind::Codex => codex_status(&format!("{out}\n{err}")),
        _ => claude_status(&out),
    }
}

fn claude_status(json: &str) -> Option<Status> {
    let value: serde_json::Value = serde_json::from_str(json.trim()).ok()?;
    let signed_in = value.get("loggedIn")?.as_bool()?;
    let word = |key: &str| value.get(key).and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    let kind = match (word("subscriptionType"), word("authMethod")) {
        (Some(plan), _) => Some(format!("Claude {}", upper_first(&plan))),
        (None, Some(method)) if method != "none" => Some(method),
        _ => None,
    };
    Some(Status { signed_in, who: word("email").or_else(|| word("orgName")), kind: kind.filter(|_| signed_in) })
}

fn codex_status(text: &str) -> Option<Status> {
    let line = text.lines().map(str::trim).find(|l| l.to_ascii_lowercase().starts_with("logged in") || l.to_ascii_lowercase().starts_with("not logged in"))?;
    let lower = line.to_ascii_lowercase();
    if lower.starts_with("not logged in") {
        return Some(Status::default());
    }
    // "Logged in using ChatGPT", "Logged in using an API key - sk-…".
    let kind = line.split_once(" using ").map(|(_, how)| how.split(" - ").next().unwrap_or(how).trim().trim_end_matches('.').to_string()).filter(|k| !k.is_empty());
    Some(Status { signed_in: true, who: None, kind })
}

fn upper_first(word: &str) -> String {
    let mut letters = word.chars();
    letters.next().map(|first| first.to_uppercase().chain(letters).collect()).unwrap_or_default()
}

/// The CLI's sign-out command for an account, to run in the background.
pub fn sign_out(account: &Account, base_env: &HashMap<String, String>) -> anyhow::Result<()> {
    let mut launch = crate::agents::login_launch(account, base_env);
    launch.args = match account.agent {
        AgentKind::Codex => vec!["logout".into()],
        _ => vec!["auth".into(), "logout".into()],
    };
    let output = crate::sys::command(&launch.program).args(&launch.args).env_clear().envs(&launch.env).stdin(Stdio::null()).output()?;
    if output.status.success() { Ok(()) } else { anyhow::bail!("{}", String::from_utf8_lossy(&output.stderr).trim().to_string()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_address_is_found_in_what_each_cli_prints() {
        // Claude Code wraps its address in a hyperlink sequence and ends with a question.
        let claude = "Opening browser to sign in…\nIf the browser didn't open, visit: \u{1b}]8;;https://claude.example/oauth/authorize?code=true&client_id=abc&state=xyz\u{7}https://claude.example/oauth/authorize?code=true&client_id=abc&state=xyz\u{1b}]8;;\u{7}\nPaste code here if prompted > ";
        let text = clean(claude);
        assert_eq!(first_url(&text).as_deref(), Some("https://claude.example/oauth/authorize?code=true&client_id=abc&state=xyz"));
        assert!(asks_for_code(&text));
        // Codex says its local server first, then the address on a line of its own.
        let codex = "Starting local login server on http://localhost:1455.\nIf your browser did not open, navigate to this URL to authenticate:\n\nhttps://auth.example/oauth/authorize?response_type=code&state=s\n\nOn a remote or headless machine? Use `codex login --device-auth` instead.\n";
        assert_eq!(first_url(&clean(codex)).as_deref(), Some("https://auth.example/oauth/authorize?response_type=code&state=s"));
        assert!(!asks_for_code(codex));
        // An address that is still coming is not taken in half.
        assert_eq!(first_url("visit: https://auth.example/oauth/auth"), None);
        assert_eq!(first_url("nothing here\n"), None);
        // Colours go.
        assert_eq!(clean("\u{1b}[1;32mok\u{1b}[0m\r\n"), "ok\n");
    }

    #[test]
    fn each_clis_status_is_read() {
        let out = claude_status(r#"{"loggedIn":false,"authMethod":"none","apiProvider":"firstParty"}"#).unwrap();
        assert_eq!(out, Status::default());
        let inn = claude_status(r#"{"loggedIn":true,"authMethod":"claude.ai","email":"sam@example.com","orgName":"Acme","subscriptionType":"max"}"#).unwrap();
        assert_eq!(inn, Status { signed_in: true, who: Some("sam@example.com".into()), kind: Some("Claude Max".into()) });
        assert_eq!(claude_status("not json"), None);
        assert_eq!(codex_status("WARNING: something\nNot logged in\n").unwrap(), Status::default());
        assert_eq!(codex_status("Logged in using ChatGPT\n").unwrap(), Status { signed_in: true, who: None, kind: Some("ChatGPT".into()) });
        assert_eq!(codex_status("Logged in using an API key - sk-***abc\n").unwrap().kind.as_deref(), Some("an API key"));
        assert_eq!(codex_status("error: unknown subcommand"), None);
    }

    #[test]
    fn the_login_command_of_each_agent() {
        let env = HashMap::from([("PATH".to_string(), String::new())]);
        let claude = Account { id: "claude-test".into(), agent: AgentKind::Claude, label: "Test".into(), plan: None, system_default: false };
        assert_eq!(command(&claude, &env, None).args, ["auth", "login"]);
        assert_eq!(command(&claude, &env, Some(" sam@example.com ")).args, ["auth", "login", "--email", "sam@example.com"]);
        // What is not an address is never an argument.
        assert_eq!(command(&claude, &env, Some("--console")).args, ["auth", "login"]);
        assert_eq!(command(&claude, &env, Some("a b@example.com")).args, ["auth", "login"]);
        let codex = Account { agent: AgentKind::Codex, id: "codex-test".into(), ..claude.clone() };
        assert_eq!(command(&codex, &env, Some("sam@example.com")).args, ["login"]);
        // Each signs in in the account's own folder.
        assert!(command(&claude, &env, None).env.contains_key("CLAUDE_CONFIG_DIR"));
        assert!(command(&codex, &env, None).env.contains_key("CODEX_HOME"));
    }

    /// A stand-in CLI: prints an address, asks for a code, and ends well when it gets "good".
    #[cfg(windows)]
    #[test]
    fn an_attempt_reports_the_address_takes_a_code_and_ends() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("login.cmd");
        std::fs::write(&script, "@echo off\r\necho Opening browser\r\necho visit: https://auth.example/authorize?state=s \r\nset /p code=Paste code here if prompted ^> \r\nif \"%code%\"==\"good\" (echo done& exit /b 0)\r\necho the code was refused 1>&2\r\nexit /b 3\r\n").unwrap();
        let spawn = || {
            let mut child = crate::sys::command(&script).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
            let (send, output) = channel();
            for mut stream in [Box::new(child.stdout.take().unwrap()) as Box<dyn Read + Send>, Box::new(child.stderr.take().unwrap())] {
                let send = send.clone();
                std::thread::spawn(move || {
                    let mut buf = [0u8; 512];
                    while let Ok(n) = stream.read(&mut buf) {
                        if n == 0 || send.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                });
            }
            Attempt { stdin: child.stdin.take(), child, output, pending: String::new(), tail: Vec::new(), url_seen: false, code_seen: false, ended: false }
        };
        let wait = |attempt: &mut Attempt, until: &dyn Fn(&[Event]) -> bool| {
            let mut all = Vec::new();
            for _ in 0..200 {
                all.extend(attempt.poll());
                if until(&all) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            all
        };
        let mut good = spawn();
        let seen = wait(&mut good, &|e| e.contains(&Event::WantsCode));
        assert_eq!(seen, [Event::Url("https://auth.example/authorize?state=s".into()), Event::WantsCode]);
        assert!(good.send_code("").is_err());
        good.send_code(" good ").unwrap();
        assert_eq!(wait(&mut good, &|e| !e.is_empty()), [Event::Ended(Ok(()))]);
        assert!(good.poll().is_empty() && good.ended());

        let mut bad = spawn();
        wait(&mut bad, &|e| e.contains(&Event::WantsCode));
        bad.send_code("wrong").unwrap();
        assert_eq!(wait(&mut bad, &|e| !e.is_empty()), [Event::Ended(Err("the code was refused".into()))]);

        // Cancel ends the stand-in while it waits.
        let mut gone = spawn();
        wait(&mut gone, &|e| e.contains(&Event::WantsCode));
        gone.cancel();
        assert!(gone.ended() && gone.poll().is_empty());
    }
}
