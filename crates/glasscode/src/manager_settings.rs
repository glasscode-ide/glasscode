//! Settings › Manager: its name and brain, which projects it manages, where it runs, what it
//! may send and through which messenger, the teammates it knows, its voice (the server's address,
//! and its key and secret, which go to the Keychain) and its daily brief. Switches and choices save
//! at once; a text field saves when the typing stops (or on Enter).

use std::cell::{Cell, RefCell};
use std::time::Duration;

use gpui::{AnyElement, ClickEvent, Context, Div, Entity, FontWeight, SharedString, Window, div, prelude::*, px};
use glasscode_core::manager::{Home, Listen, Person, SendPolicy, Settings, voice};
use glasscode_core::model::AgentKind;

use crate::scenes_ui::gold;
use crate::settings_ui::{group, row};
use crate::text_input::{InputEvent, TextInput};
use crate::theme;
use crate::ui;
use crate::ui::switch;
use crate::workspace::Workspace;

/// How long after the last key a text field is saved.
const SAVE_AFTER: Duration = Duration::from_millis(900);

/// The longest name it can have.
const NAME_MAX: usize = 24;

/// A text field of Settings › Manager that is a setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Name,
    Owner,
    Model,
    Messenger,
    Livekit,
    Brief,
}

/// The text fields of Settings › Manager.
pub struct ManagerForm {
    pub name: Entity<TextInput>,
    pub owner: Entity<TextInput>,
    pub model: Entity<TextInput>,
    pub messenger: Entity<TextInput>,
    pub livekit: Entity<TextInput>,
    pub brief: Entity<TextInput>,
    pub person_name: Entity<TextInput>,
    pub person_github: Entity<TextInput>,
    pub person_contact: Entity<TextInput>,
    /// The LiveKit key and secret as typed. They are no setting: Save puts them in the
    /// Keychain and empties the fields, so what is kept is never shown.
    pub voice_key: Entity<TextInput>,
    pub voice_secret: Entity<TextInput>,
    /// Why the last Save kept nothing (None: it did, or there was none).
    pub voice_keys_note: RefCell<Option<String>>,
    /// Counts the edits, so a burst of typing is saved once, and which of them is saved.
    pub edits: Cell<u64>,
    pub saved: Cell<u64>,
}

impl ManagerForm {
    fn new(settings: &Settings, cx: &mut Context<Workspace>) -> Self {
        let field = |which: Field, placeholder: &'static str, text: String, mono: bool, cx: &mut Context<Workspace>| {
            let input = cx.new(|cx| {
                let input = TextInput::new(placeholder, cx).with_text(text);
                if mono { input.mono() } else { input }
            });
            cx.subscribe(&input, move |this: &mut Workspace, _, event: &InputEvent, cx| match event {
                InputEvent::Changed => this.manager_field_edited(which, false, cx),
                InputEvent::Submit => this.manager_field_edited(which, true, cx),
                InputEvent::Cancel => {}
            })
            .detach();
            input
        };
        let person = |placeholder: &'static str, mono: bool, cx: &mut Context<Workspace>| {
            let input = cx.new(|cx| {
                let input = TextInput::new(placeholder, cx);
                if mono { input.mono() } else { input }
            });
            cx.subscribe(&input, |this: &mut Workspace, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Submit) {
                    this.manager_add_person(cx);
                }
            })
            .detach();
            input
        };
        ManagerForm {
            name: field(Field::Name, "Manager", settings.name.clone(), false, cx),
            owner: field(Field::Owner, "Your first name", settings.owner.clone(), false, cx),
            model: field(Field::Model, "The agent's own default", settings.model.clone().unwrap_or_default(), true, cx),
            messenger: field(Field::Messenger, "A program, e.g. my-messenger", settings.messenger.clone(), true, cx),
            livekit: field(Field::Livekit, "wss://…", settings.voice.livekit_url.clone(), true, cx),
            brief: field(Field::Brief, "09:00", settings.brief_at.clone().unwrap_or_default(), true, cx),
            person_name: person("Name, e.g. Alex", false, cx),
            person_github: person("GitHub login", true, cx),
            person_contact: person("Contact (optional)", true, cx),
            voice_key: cx.new(|cx| TextInput::new("LiveKit key", cx).mono()),
            voice_secret: cx.new(|cx| TextInput::new("LiveKit secret", cx).mono().masked()),
            voice_keys_note: RefCell::new(None),
            edits: Cell::new(0),
            saved: Cell::new(0),
        }
    }

    fn input(&self, field: Field) -> &Entity<TextInput> {
        match field {
            Field::Name => &self.name,
            Field::Owner => &self.owner,
            Field::Model => &self.model,
            Field::Messenger => &self.messenger,
            Field::Livekit => &self.livekit,
            Field::Brief => &self.brief,
        }
    }
}

// ----- the rules of the fields (no UI) -----------------------------------------------------

/// A time of day as the person types it ("9", "9:5", "0930", "17.45") → "09:00". None when
/// it is not one.
fn parse_time(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() || !text.chars().all(|c| c.is_ascii_digit() || c == ':' || c == '.') {
        return None;
    }
    let (hours, minutes) = match text.split_once([':', '.']) {
        Some((h, m)) => (h, m),
        None if text.len() > 2 => text.split_at(text.len() - 2),
        None => (text, "0"),
    };
    if hours.is_empty() || hours.len() > 2 || minutes.is_empty() || minutes.len() > 2 {
        return None;
    }
    let (h, m): (u32, u32) = (hours.parse().ok()?, minutes.parse().ok()?);
    (h < 24 && m < 60).then(|| format!("{h:02}:{m:02}"))
}

/// Put what a field holds into the settings. Returns whether that changed anything.
fn apply(settings: &mut Settings, field: Field, text: &str) -> bool {
    let text = text.trim();
    let before = settings.clone();
    match field {
        Field::Name => settings.name = if text.is_empty() { Settings::default().name } else { text.chars().take(NAME_MAX).collect::<String>().trim_end().to_string() },
        Field::Owner => settings.owner = text.chars().take(40).collect::<String>().trim_end().to_string(),
        Field::Model => settings.model = (!text.is_empty()).then(|| text.to_string()),
        Field::Messenger => settings.messenger = text.to_string(),
        Field::Livekit => settings.voice.livekit_url = text.to_string(),
        // A time that does not read as one changes nothing: the brief stays where it was. The
        // field never switches the brief on; its switch does.
        Field::Brief => {
            if let (true, Some(time)) = (settings.brief_at.is_some(), parse_time(text)) {
                settings.brief_at = Some(time);
            }
        }
    }
    *settings != before
}

/// Add a teammate, or give one already known (by name, whatever the case) a GitHub login or
/// a contact (how the messenger knows them). An empty field leaves what is known as it is.
/// Returns whether anything changed.
fn add_person(settings: &mut Settings, name: &str, github: &str, contact: &str) -> bool {
    let name = name.trim();
    let github = github.trim().trim_start_matches('@');
    let contact = contact.trim();
    if name.is_empty() {
        return false;
    }
    match settings.people.iter_mut().find(|p| p.name.to_lowercase() == name.to_lowercase()) {
        Some(known) => {
            let before = known.clone();
            if !github.is_empty() {
                known.github = github.to_string();
            }
            if !contact.is_empty() {
                known.contact = Some(contact.to_string());
            }
            *known != before
        }
        None => {
            settings.people.push(Person { name: name.to_string(), github: github.to_string(), contact: (!contact.is_empty()).then(|| contact.to_string()), ..Person::default() });
            true
        }
    }
}

/// What a teammate's row says under the name: their GitHub login, and whom a message goes to.
fn person_detail(person: &Person) -> String {
    let github = if person.github.trim().is_empty() { "No GitHub login".to_string() } else { format!("@{} on GitHub", person.github.trim()) };
    match person.contact.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
        Some(contact) => format!("{github} · messages go to {contact}"),
        None => format!("{github} · messages go to their name"),
    }
}

/// Why the key and secret were not kept, as a sentence for the row.
fn keys_problem(why: &str) -> String {
    let why = why.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("the Keychain did not take them");
    let mut chars = why.chars();
    let why: String = match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    };
    format!("Not saved. {}{}", why, if why.ends_with(['.', '!', '?']) { "" } else { "." })
}

// ----- controls ----------------------------------------------------------------------------

/// The frame around the segments of a choice, as in the other settings.
fn segments() -> Div {
    div().flex().flex_wrap().gap_0p5().p_0p5().rounded(px(8.)).border_1().border_color(theme::border()).bg(theme::bg())
}

fn segment(id: impl Into<SharedString>, on: bool, label: impl Into<SharedString>) -> gpui::Stateful<Div> {
    div()
        .id(gpui::ElementId::Name(id.into()))
        .flex()
        .items_center()
        .h(px(30.))
        .px_3()
        .rounded(px(6.))
        .cursor_pointer()
        .when(on, |d| d.bg(theme::selected()).text_color(theme::text_strong()).font_weight(FontWeight::SEMIBOLD))
        .when(!on, |d| d.text_color(theme::muted()).hover(|s| s.text_color(theme::text())))
        .child(label.into())
}

/// A text field in a row, at a width that fits what goes in it.
fn field_box(input: &Entity<TextInput>, width: f32) -> Div {
    div().flex_none().w(px(width)).child(input.clone())
}

impl Workspace {
    // ----- changing settings ---------------------------------------------------------------

    /// Save the manager's settings and let its parts follow them.
    fn manager_save_settings(&mut self, cx: &mut Context<Self>) {
        if let Some(form) = self.manager.page.form.get() {
            form.saved.set(form.edits.get());
        }
        self.save();
        self.manager_settings_changed(cx);
        cx.notify();
    }

    /// Change a setting with a switch or a choice: saved at once.
    fn manager_set(&mut self, change: impl FnOnce(&mut Settings), cx: &mut Context<Self>) {
        change(&mut self.state.manager);
        self.manager_save_settings(cx);
    }

    /// A text field changed (or Enter was pressed in it): the setting follows at once, and it
    /// is saved when the typing stops, or now on Enter.
    fn manager_field_edited(&mut self, field: Field, submit: bool, cx: &mut Context<Self>) {
        let Some(form) = self.manager.page.form.get() else { return };
        let text = form.input(field).read(cx).text();
        let changed = apply(&mut self.state.manager, field, &text);
        if changed {
            form.edits.set(form.edits.get() + 1);
            cx.notify();
        }
        let edit = form.edits.get();
        if edit == form.saved.get() {
            return;
        }
        if submit {
            self.manager_save_settings(cx);
        } else if changed {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(SAVE_AFTER).await;
                let _ = this.update(cx, |this, cx| {
                    let still = this.manager.page.form.get().is_some_and(|form| form.edits.get() == edit && form.saved.get() != edit);
                    if still {
                        this.manager_save_settings(cx);
                    }
                });
            })
            .detach();
        }
    }

    /// The other agent thinks for it: its accounts and models are not the same, so both go
    /// back to the default.
    fn manager_set_brain(&mut self, agent: AgentKind, cx: &mut Context<Self>) {
        if self.state.manager.brain == agent {
            return;
        }
        if let Some(form) = self.manager.page.form.get() {
            form.model.update(cx, |input, cx| input.set_text("", cx));
        }
        self.manager_set(
            |m| {
                m.brain = agent;
                m.account = None;
                m.model = None;
            },
            cx,
        );
    }

    /// Add the teammate typed into the fields of Teammates.
    fn manager_add_person(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.manager.page.form.get() else { return };
        let inputs = [form.person_name.clone(), form.person_github.clone(), form.person_contact.clone()];
        let [name, github, contact] = inputs.clone().map(|input| input.read(cx).text());
        if name.trim().is_empty() {
            return;
        }
        let changed = add_person(&mut self.state.manager, &name, &github, &contact);
        for input in inputs {
            input.update(cx, |input, cx| input.set_text("", cx));
        }
        if changed {
            self.manager_save_settings(cx);
        }
        cx.notify();
    }

    /// Look whether the Keychain holds the LiveKit key and secret, off the main thread.
    /// Called when Settings › Manager opens; drawing the section never asks the Keychain.
    pub fn manager_voice_keys_check(&mut self, cx: &mut Context<Self>) {
        let looked = cx.background_spawn(async { voice::keys().is_some() });
        cx.spawn(async move |this, cx| {
            let kept = looked.await;
            let _ = this.update(cx, |this, cx| {
                this.manager.voice.keys_kept = Some(kept);
                cx.notify();
            });
        })
        .detach();
    }

    /// Save: keep the LiveKit key and secret typed into Voice in the Keychain. Remove
    /// (`remove`): delete the ones kept. Save with a field empty changes nothing: only Remove
    /// deletes. Afterwards the fields are empty and the Keychain is asked what it holds now.
    fn manager_save_voice_keys(&mut self, remove: bool, cx: &mut Context<Self>) {
        let Some(form) = self.manager.page.form.get() else { return };
        let (key, secret) = if remove { (String::new(), String::new()) } else { (form.voice_key.read(cx).text(), form.voice_secret.read(cx).text()) };
        if !remove && (key.trim().is_empty() || secret.trim().is_empty()) {
            *form.voice_keys_note.borrow_mut() = Some("Not saved. Enter both the key and the secret.".into());
            cx.notify();
            return;
        }
        let task = cx.background_spawn(async move { (voice::set_keys(&key, &secret).map_err(|e| format!("{e:#}")), voice::keys().is_some()) });
        cx.spawn(async move |this, cx| {
            let (result, kept) = task.await;
            let _ = this.update(cx, |this, cx| {
                this.manager.voice.keys_kept = Some(kept);
                let Some(form) = this.manager.page.form.get() else { return };
                match result {
                    Ok(()) => {
                        *form.voice_keys_note.borrow_mut() = None;
                        for input in [form.voice_key.clone(), form.voice_secret.clone()] {
                            input.update(cx, |input, cx| input.set_text("", cx));
                        }
                        // Whatever voice said was missing may be there now.
                        this.manager.voice.problem = None;
                    }
                    Err(why) => *form.voice_keys_note.borrow_mut() = Some(keys_problem(&why)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ----- the section ---------------------------------------------------------------------

    /// A choice between a few values of one setting.
    fn manager_choice<T: PartialEq + Copy + 'static>(&self, id: &'static str, options: &[(T, &'static str)], current: T, set: fn(&mut Settings, T), cx: &mut Context<Self>) -> AnyElement {
        segments()
            .children(options.iter().enumerate().map(|(i, (value, label))| {
                let value = *value;
                segment(format!("{id}-{i}"), value == current, *label).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.manager_set(|m| set(m, value), cx)))
            }))
            .into_any_element()
    }

    pub fn settings_manager(&self, _window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let settings = &self.state.manager;
        let form = self.manager.page.form.get_or_init(|| ManagerForm::new(settings, cx));
        let mut out: Vec<AnyElement> = Vec::new();

        // Manager.
        out.push(group(
            "Manager",
            "",
            vec![
                row(
                    "Run the manager",
                    "One agent above all your sessions. Off: nothing of him runs and nothing of him shows.",
                    Some(switch("manager-enabled", settings.enabled).on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.manager_set(|m| m.enabled = !m.enabled, cx))).into_any_element()),
                ),
                row("Name", "Used everywhere, in his signature and as the wake word", Some(field_box(&form.name, 240.).into_any_element())),
                row("Your first name", "For the signature", Some(field_box(&form.owner, 240.).into_any_element())),
                row(
                    "Signature",
                    "Under every message he sends",
                    Some(div().font_family(theme::MONO_FONT).text_size(px(12.)).text_color(gold()).child(settings.signature()).into_any_element()),
                ),
            ],
        ));

        // Moving in (M35): with the new manager's memory only.
        if self.control.engine_v2 {
            out.push(group(
                "Move in",
                "",
                vec![row(
                    "Move in what your agents already know",
                    "Reads only what you tick, then proposes it for memory; you accept what is right. A run can be undone.",
                    Some(crate::ui::button("manager-move-in", "Move in…").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.open_move_in(cx))).into_any_element()),
                )],
            ));
        }

        // Brain.
        let brain = segments()
            .children([AgentKind::Claude, AgentKind::Codex].into_iter().map(|agent| {
                segment(format!("manager-brain-{}", agent.name()), settings.brain == agent, agent.name()).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.manager_set_brain(agent, cx)))
            }))
            .into_any_element();
        let mut accounts: Vec<(Option<String>, String)> = vec![(None, "Default".to_string())];
        accounts.extend(self.state.accounts.iter().filter(|a| a.agent == settings.brain).map(|a| (Some(a.id.clone()), a.display())));
        let account = segments()
            .children(accounts.into_iter().enumerate().map(|(i, (id, label))| {
                segment(format!("manager-account-{i}"), settings.account == id, label).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    let id = id.clone();
                    this.manager_set(|m| m.account = id, cx)
                }))
            }))
            .into_any_element();
        out.push(group(
            "Brain",
            "The agent that thinks for him: Claude or Codex. A CLI you added yourself cannot be his brain.",
            vec![
                row("Agent", "", Some(brain)),
                row("Account", format!("The {} account he runs as", settings.brain.name()), Some(account)),
                row("Model", "As the agent's own command takes it. Empty: its default.", Some(field_box(&form.model, 240.).into_any_element())),
            ],
        ));

        // Projects.
        let mut projects: Vec<Div> = self
            .state
            .projects
            .iter()
            .map(|project| {
                let id = project.id.clone();
                let on = !settings.off_projects.contains(&project.id);
                row(
                    project.name.clone(),
                    if self.state.is_hidden(&project.id) { "Hidden in Glasscode" } else { "" },
                    Some(
                        switch(SharedString::from(format!("manager-project-{}", project.id)), on)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                let id = id.clone();
                                this.manager_set(
                                    |m| {
                                        if !m.off_projects.remove(&id) {
                                            m.off_projects.insert(id);
                                        }
                                    },
                                    cx,
                                )
                            }))
                            .into_any_element(),
                    ),
                )
            })
            .collect();
        if projects.is_empty() {
            projects.push(row("No projects yet", "Add one with ⌘O. He manages every project unless you switch it off here.", None));
        }
        out.push(group("Projects", "What he looks at. Off: he does not see that project at all.", projects));

        // Where he runs.
        let has_server = self.server.config.is_some();
        let home_detail = match settings.home {
            Home::Mac => "On this Mac he stops when it sleeps.".to_string(),
            Home::Server if !has_server => "No server is set up yet (Settings › Server & devices). Until then he runs on this Mac.".to_string(),
            Home::Server if self.manager_on_server() => "On the server he keeps working when this Mac is off. If the server cannot be reached, this Mac takes over.".to_string(),
            Home::Server => "The server does not run him right now, so this Mac does. Test the server to see why.".to_string(),
        };
        // The v2 manager moves between this Mac and the server by itself (D5, M57 step 5).
        let home_row = if self.control.engine_v2 {
            use glasscode_core::home::HomeChoice;
            let detail = match settings.home_choice {
                HomeChoice::Automatic => format!("He works on this {} while you use it, and on the server when it sleeps or you switch to Server mode. {}", crate::platform::computer(), self.home_label().unwrap_or_default()),
                HomeChoice::AlwaysServer => format!("He works on the server; this {} shows the same conversation.", crate::platform::computer()),
                HomeChoice::AlwaysThisMac => format!("He works only on this {}, and waits while it sleeps.", crate::platform::computer()),
            };
            row(
                "Home",
                detail,
                Some(self.manager_choice("manager-home-v2", &[(HomeChoice::Automatic, "Automatic"), (HomeChoice::AlwaysServer, "Always on the server"), (HomeChoice::AlwaysThisMac, "Always this Mac")], settings.home_choice, |m, choice| m.home_choice = choice, cx)),
            )
        } else {
            row("Home", home_detail, Some(self.manager_choice("manager-home", &[(Home::Mac, "This Mac"), (Home::Server, "Server")], settings.home, |m, home| m.home = home, cx)))
        };
        let mut home_rows = vec![
            home_row,
            row(
                "Test the server",
                "Whether he can reach it, whether it is the same version, and whether he can work there",
                Some(if self.manager.testing {
                    ui::disabled_button("manager-test", "Testing…").into_any_element()
                } else {
                    ui::button("manager-test", "Test the server").on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.manager_test_server(cx);
                        cx.notify();
                    }))
                    .into_any_element()
                }),
            ),
        ];
        if let Some(lines) = self.manager.test.as_ref().filter(|lines| !lines.is_empty()) {
            home_rows.push(div().flex().flex_col().gap_1().px_4().py_3().bg(theme::subtle()).font_family(theme::MONO_FONT).text_size(px(12.5)).children(lines.iter().map(|line| {
                let detail = if line.detail.trim().is_empty() { String::new() } else { format!(" · {}", line.detail.trim()) };
                div().text_color(if line.ok { theme::success_text() } else { theme::error_text() }).child(format!("{} {}{detail}", if line.ok { "✓" } else { "✕" }, line.what))
            })));
        }
        home_rows.push(row("Memory", "Kept on the server and on this Mac", None));
        out.push(group("Where he runs", "", home_rows));

        // Messages.
        out.push(group(
            "Messages",
            "To teammates, in your name",
            vec![
                row(
                    "Messenger",
                    if settings.messenger.trim().is_empty() {
                        "The program that sends his messages and reads the ones for you: its name, or its full path. Empty: no message goes out and none is read. docs/manager.md says what the program must do."
                    } else {
                        "The program that sends his messages and reads the ones for you: its name, or its full path. docs/manager.md says what the program must do."
                    },
                    Some(field_box(&form.messenger, 300.).into_any_element()),
                ),
                row(
                    "What he may send",
                    "Ask me every time: every message waits for your OK. Send routine pings by himself: a plain fact (“the fix is pushed”) goes out; anything with a promise, a date or an opinion still waits.",
                    Some(self.manager_choice("manager-send", &[(SendPolicy::AskAlways, "Ask me every time"), (SendPolicy::RoutineAuto, "Send routine pings by himself")], settings.send, |m, send| m.send = send, cx)),
                ),
                row("What he never does", "He never approves, merges or closes a pull request, and never posts on GitHub.", None),
            ],
        ));

        // Teammates.
        let mut people: Vec<Div> = settings
            .people
            .iter()
            .enumerate()
            .map(|(i, person)| {
                let (urgent_name, remove_name) = (person.name.clone(), person.name.clone());
                let controls = div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_2p5()
                    .child(div().text_size(px(12.5)).text_color(theme::muted()).child("May send urgent alerts"))
                    .child(switch(SharedString::from(format!("manager-person-urgent-{i}")), person.urgent).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        let name = urgent_name.clone();
                        this.manager_set(
                            |m| {
                                if let Some(p) = m.people.iter_mut().find(|p| p.name == name) {
                                    p.urgent = !p.urgent;
                                }
                            },
                            cx,
                        )
                    })))
                    .child(ui::button(SharedString::from(format!("manager-person-remove-{i}")), "Remove").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        let name = remove_name.clone();
                        this.manager_set(|m| m.people.retain(|p| p.name != name), cx)
                    })))
                    .into_any_element();
                row(person.name.clone(), person_detail(person), Some(controls))
            })
            .collect();
        people.push(row(
            "Add a teammate",
            "Their name as you say it, their GitHub login so he can match reviews, and how the messenger knows them (empty: by their name)",
            Some(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(field_box(&form.person_name, 170.))
                    .child(field_box(&form.person_github, 170.))
                    .child(field_box(&form.person_contact, 170.))
                    .child(ui::button("manager-person-add", "Add").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.manager_add_person(cx))))
                    .into_any_element(),
            ),
        ));
        out.push(group("Teammates", "Who he may talk about and write to. An urgent alert reaches you at once, with a holding reply to them.", people));

        // Voice. The key and secret go to the Keychain; what is kept there is never shown,
        // only whether there is one (looked up when the section opens, not while drawing).
        let kept = self.manager.voice.keys_kept;
        let keys_detail = match (form.voice_keys_note.borrow().clone(), kept) {
            (Some(note), _) => note,
            (None, Some(true)) => "Saved".to_string(),
            (None, Some(false)) => "Not set".to_string(),
            (None, None) => String::new(),
        };
        let keys_control = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(field_box(&form.voice_secret, 300.))
            .child(ui::button("manager-voice-keys-save", "Save").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.manager_save_voice_keys(false, cx))))
            .when(kept == Some(true), |d| d.child(ui::button("manager-voice-keys-remove", "Remove").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.manager_save_voice_keys(true, cx)))))
            .into_any_element();
        out.push(group(
            "Voice",
            "",
            vec![
                row(
                    "He listens",
                    "In Glasscode, on the phone and on the glasses",
                    Some(self.manager_choice("manager-listen", &[(Listen::Wake, "On the wake word"), (Listen::Hold, "While you hold"), (Listen::Never, "Never")], settings.listen, |m, listen| m.listen = listen, cx)),
                ),
                row("LiveKit URL", "LiveKit on your server: speech to text, his brain, text to speech", Some(field_box(&form.livekit, 300.).into_any_element())),
                row("LiveKit key", "Kept in your Keychain, not in the settings file.", Some(field_box(&form.voice_key, 300.).into_any_element())),
                row("LiveKit secret", keys_detail, Some(keys_control)),
            ],
        ));

        // Daily brief.
        let brief_on = settings.brief_at.is_some();
        let typed = form.brief.read(cx).text();
        let unreadable = brief_on && !typed.trim().is_empty() && parse_time(&typed).is_none();
        let brief_detail = match (&settings.brief_at, unreadable) {
            (Some(at), true) => format!("That is not a time of day. It stays at {at}."),
            (Some(at), false) => format!("Every day at {at} he tells you what needs you, what came in and what he does next"),
            (None, _) => "Off: only when you ask (“give me today's update”)".to_string(),
        };
        let brief_control = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_2p5()
            .when(brief_on, |d| d.child(field_box(&form.brief, 90.)))
            .child(switch("manager-brief", brief_on).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                let typed = this.manager.page.form.get().map(|form| form.brief.read(cx).text()).unwrap_or_default();
                let was_on = this.state.manager.brief_at.is_some();
                let at = (!was_on).then(|| parse_time(&typed).unwrap_or_else(|| "09:00".to_string()));
                if let (Some(at), Some(form)) = (at.clone(), this.manager.page.form.get()) {
                    form.brief.update(cx, |input, cx| input.set_text(at, cx));
                }
                this.manager_set(|m| m.brief_at = at, cx)
            })))
            .into_any_element();
        out.push(group("Daily brief", "", vec![row("Daily brief", brief_detail, Some(brief_control))]));
        // Trust (M53): levels, conditions, playbooks and schedules, with the v2 engine.
        out.extend(self.trust_settings(cx));

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_time_of_day_is_read_however_it_is_typed() {
        assert_eq!(parse_time("9").as_deref(), Some("09:00"));
        assert_eq!(parse_time("09:00").as_deref(), Some("09:00"));
        assert_eq!(parse_time(" 9:5 ").as_deref(), Some("09:05"));
        assert_eq!(parse_time("17.45").as_deref(), Some("17:45"));
        assert_eq!(parse_time("0930").as_deref(), Some("09:30"));
        assert_eq!(parse_time("930").as_deref(), Some("09:30"));
        assert_eq!(parse_time("23:59").as_deref(), Some("23:59"));
        for bad in ["", "24:00", "9:60", "nine", "9:", ":30", "12:345", "1:2:3", "-1", "09:00am"] {
            assert_eq!(parse_time(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_field_changes_its_setting_and_says_whether_it_did() {
        let mut s = Settings::default();
        assert!(apply(&mut s, Field::Name, "  Mira the Manager  "));
        assert_eq!(s.name, "Mira the Manager");
        assert!(!apply(&mut s, Field::Name, "Mira the Manager"), "the same again changes nothing");
        assert!(apply(&mut s, Field::Name, "A name that is far too long to be a wake word"));
        assert_eq!(s.name.chars().count(), NAME_MAX);
        assert!(apply(&mut s, Field::Name, "   "));
        assert_eq!(s.name, "Manager", "an empty name goes back to the default");

        assert!(apply(&mut s, Field::Owner, " Alex "));
        assert_eq!(s.signature(), "Manager, on behalf of Alex");

        assert!(apply(&mut s, Field::Model, "opus"));
        assert_eq!(s.model.as_deref(), Some("opus"));
        assert!(apply(&mut s, Field::Model, " "));
        assert_eq!(s.model, None);

        assert!(apply(&mut s, Field::Messenger, " /usr/local/bin/my-messenger "));
        assert_eq!(s.messenger, "/usr/local/bin/my-messenger");
        assert!(apply(&mut s, Field::Messenger, ""), "empty: no messenger");
        assert_eq!(s.messenger, "");

        assert!(apply(&mut s, Field::Livekit, " wss://voice.example.test "));
        assert_eq!(s.voice.livekit_url, "wss://voice.example.test");
    }

    #[test]
    fn a_brief_time_that_is_not_one_leaves_the_brief_where_it_was() {
        let mut s = Settings::default();
        assert_eq!(s.brief_at.as_deref(), Some("09:00"));
        assert!(apply(&mut s, Field::Brief, "8:30"));
        assert_eq!(s.brief_at.as_deref(), Some("08:30"));
        assert!(!apply(&mut s, Field::Brief, "8:3x"));
        assert!(!apply(&mut s, Field::Brief, ""));
        assert_eq!(s.brief_at.as_deref(), Some("08:30"));
        s.brief_at = None;
        assert!(!apply(&mut s, Field::Brief, "10:00"), "the field does not switch the brief on");
        assert_eq!(s.brief_at, None);
    }

    #[test]
    fn a_teammate_is_added_once_and_can_get_a_login_later() {
        let mut s = Settings::default();
        assert!(!add_person(&mut s, "  ", "sam-dev", ""), "no name, no teammate");
        assert!(add_person(&mut s, " Sam ", "@sam-dev", ""));
        assert_eq!(s.people, vec![Person { name: "Sam".into(), github: "sam-dev".into(), ..Person::default() }]);
        assert!(!add_person(&mut s, "sam", "", ""), "known already");
        assert!(!add_person(&mut s, "SAM", "sam-dev", ""));
        assert!(add_person(&mut s, "sam", "sam-works", ""));
        assert_eq!((s.people.len(), s.people[0].github.as_str()), (1, "sam-works"));
        assert!(add_person(&mut s, "Mira", "", ""));
        assert_eq!(s.people.len(), 2);
        assert!(!s.people[1].urgent, "urgent alerts are off until the person allows them");
    }

    #[test]
    fn why_a_key_was_not_kept_reads_as_a_sentence() {
        assert_eq!(keys_problem("the LiveKit key and its secret are both needed"), "Not saved. The LiveKit key and its secret are both needed.");
        assert_eq!(keys_problem("  \n"), "Not saved. The Keychain did not take them.");
    }

    #[test]
    fn a_teammate_can_have_a_contact_for_the_messenger() {
        let mut s = Settings::default();
        assert!(add_person(&mut s, "Sam", "sam-dev", " sam@example.test "));
        assert_eq!(s.people[0].contact.as_deref(), Some("sam@example.test"));
        assert_eq!(person_detail(&s.people[0]), "@sam-dev on GitHub · messages go to sam@example.test");
        // An empty field keeps what is known; a new contact replaces the old one.
        assert!(!add_person(&mut s, "sam", "", ""));
        assert!(add_person(&mut s, "sam", "", "sam.example"));
        assert_eq!((s.people[0].github.as_str(), s.people[0].contact.as_deref()), ("sam-dev", Some("sam.example")));
        assert!(add_person(&mut s, "Mira", "", ""));
        assert_eq!(s.people[1].contact, None);
        assert_eq!(person_detail(&s.people[1]), "No GitHub login · messages go to their name");
    }
}
