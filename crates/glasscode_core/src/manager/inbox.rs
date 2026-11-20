//! v2's inbox (M56.2): the teammates' messages the messenger brought, on v2's own store, with v1's
//! rule (`Harness::on_inbox` was its home): each message is taken once; an urgent one from a
//! teammate the person allows to alarm them (`Person::urgent`), and not read yet, notifies the
//! person at once and drafts a holding reply in the outbox, which waits for the person like every
//! message; nothing else is answered. What it took is kept in `manager/inbox.json`.

use serde::{Deserialize, Serialize};

use super::{InMessage, Person, Settings};

/// Messages kept: the newest this many.
const KEEP: usize = 500;

/// `manager/inbox.json`. Damaged, it starts empty: the next read takes the last hour again, and
/// a message already said is said once more at most.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct InboxFile {
    pub messages: Vec<InMessage>,
}

impl crate::store::Store for InboxFile {
    const NAME: &'static str = "manager/inbox";
    const VERSION: u32 = 1;
    const ON_DAMAGE: crate::store::OnDamage = crate::store::OnDamage::Empty;

    fn path() -> std::path::PathBuf {
        super::store_dir().join("inbox.json")
    }
}

/// An urgent message to tell the person about, and the holding reply it drafts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alarm {
    pub title: String,
    pub body: String,
    /// Who the holding reply goes to, and the message it answers.
    pub to: String,
    pub about: String,
}

impl InboxFile {
    /// From when the next read asks the messenger: the newest message it has, else the last hour.
    pub fn since(&self, now: u64) -> u64 {
        self.messages.iter().map(|m| m.at).max().unwrap_or_else(|| now.saturating_sub(3600))
    }

    /// Take new messages in: the alarms they raise (each message once; one the person read in
    /// their messenger since is marked so, and raises nothing).
    pub fn take(&mut self, settings: &Settings, messages: Vec<InMessage>) -> Vec<Alarm> {
        if !settings.enabled {
            return Vec::new();
        }
        let mut alarms = Vec::new();
        for msg in messages.into_iter().filter(|m| !m.id.is_empty()) {
            if let Some(known) = self.messages.iter_mut().find(|m| m.id == msg.id) {
                if msg.handled && !known.handled {
                    known.handled = true;
                }
                continue;
            }
            let sender = sender_of(settings, &msg).filter(|p| p.urgent && msg.urgent && !msg.handled).cloned();
            self.messages.push(msg.clone());
            let Some(sender) = sender else { continue };
            let who = if sender.name.trim().is_empty() { msg.from.trim().to_string() } else { sender.name.trim().to_string() };
            alarms.push(Alarm { title: format!("Urgent from {who}"), body: super::gh::one_line(&msg.text, 160), to: who, about: msg.id.clone() });
        }
        if self.messages.len() > KEEP {
            let over = self.messages.len() - KEEP;
            self.messages.drain(..over);
        }
        alarms
    }
}

/// The teammate a message is from: by name, else by how the messenger knows them.
fn sender_of<'a>(settings: &'a Settings, msg: &InMessage) -> Option<&'a Person> {
    let from = msg.from.trim();
    let by_name = if from.is_empty() { None } else { settings.person(from) };
    let contact = |p: &Person, what: &str| p.contact.as_deref().map(str::trim).is_some_and(|c| !c.is_empty() && c.eq_ignore_ascii_case(what.trim()));
    by_name.or_else(|| settings.people.iter().find(|p| (!from.is_empty() && contact(p, from)) || (!msg.chat.is_empty() && contact(p, &msg.chat))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        Settings { enabled: true, name: "AI Robin".into(), owner: "Robin".into(), people: vec![Person { name: "Sam".into(), urgent: true, contact: Some("+15550100".into()), ..Person::default() }, Person { name: "Alex".into(), ..Person::default() }], ..Settings::default() }
    }

    fn msg(id: &str, from: &str, urgent: bool, handled: bool) -> InMessage {
        InMessage { id: id.into(), from: from.into(), text: "The release is broken, call me.".into(), at: 100, urgent, handled, ..InMessage::default() }
    }

    /// v1's rule on v2's store: an urgent message from the right teammate alarms once (and its
    /// holding reply waits in v2's outbox); anyone else's, or one already read, alarms nobody.
    #[test]
    fn inbox_urgent_once() {
        let s = settings();
        let mut file = InboxFile::default();
        let alarms = file.take(&s, vec![msg("m1", "+15550100", true, false), msg("m2", "Alex", true, false), msg("m3", "Sam", true, true), msg("", "Sam", true, false)]);
        assert_eq!(alarms.len(), 1, "{alarms:?}");
        assert_eq!((alarms[0].title.as_str(), alarms[0].to.as_str(), alarms[0].about.as_str()), ("Urgent from Sam", "Sam", "m1"));
        assert!(file.take(&s, vec![msg("m1", "+15550100", true, false)]).is_empty(), "once");
        file.take(&s, vec![msg("m2", "Alex", true, true)]);
        assert!(file.messages.iter().find(|m| m.id == "m2").unwrap().handled);
        assert_eq!(file.since(10_000), 100);
        let mut outbox = crate::manager::messages::Outbox::default();
        let reply = outbox.hold(&s, &alarms[0].to, Some(&alarms[0].about), 200);
        assert_eq!((reply.to.as_str(), reply.urgent, outbox.waiting().len()), ("Sam", true, 1));
        assert!(InboxFile::default().take(&Settings { enabled: false, ..s }, vec![msg("m9", "Sam", true, false)]).is_empty(), "the manager is off");
    }
}
