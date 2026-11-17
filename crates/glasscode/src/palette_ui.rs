//! The command palette (its shortcut, or the search in the title bar): type a few letters of a session,
//! a ticket, a review request or a command, and Enter (or a click) goes there. ↑ and ↓ move.

use gpui::{AnyElement, AppContext as _, ClickEvent, Context, Entity, FontWeight, SharedString, Window, deferred, div, prelude::*, px};

use crate::keymap;
use crate::text_input::{InputEvent, TextInput};
use crate::theme;
use crate::shell::View;
use crate::tickets_ui::Key;
use crate::workspace::{AddProject, GoBack, GoForward, Handoff, NewSession, OpenSettings, ToggleLeftPanel, ToggleRightPanel, Workspace};

pub struct Palette {
    pub input: Entity<TextInput>,
    pub selected: usize,
}

type Run = Box<dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>)>;

/// One line of the palette: what it says, a quiet hint on the right, and what it does.
struct Item {
    label: String,
    hint: String,
    group: &'static str,
    run: Run,
}

const SHOWN: usize = 14;

impl Workspace {
    pub fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| TextInput::new("Type a session, a ticket, a pull request or a command…", cx));
        cx.subscribe_in(&input, window, |this: &mut Workspace, _, event: &InputEvent, window, cx| match event {
            InputEvent::Submit => this.palette_run(None, window, cx),
            InputEvent::Cancel => this.close_palette(window, cx),
            InputEvent::Changed => {
                if let Some(p) = this.palette.as_mut() {
                    p.selected = 0;
                }
                cx.notify();
            }
        })
        .detach();
        crate::text_input::focus(&input, window, cx);
        self.menu_open = false;
        self.palette = Some(Palette { input, selected: 0 });
        cx.notify();
    }

    pub fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.palette.take().map(|p| p.input.read(cx).focus_handle_clone());
        self.palette_gone(input, window, cx);
        cx.notify();
    }

    /// The palette's box had the keyboard and is gone: it goes to the session on screen, or to
    /// the window, so the window's keys (back, forward, the panels) keep working.
    fn palette_gone(&mut self, input: Option<gpui::FocusHandle>, window: &mut Window, cx: &mut Context<Self>) {
        if !input.is_some_and(|input| input.is_focused(window)) {
            return;
        }
        if self.view == View::Sessions {
            self.focus_active(window, cx);
        } else {
            window.focus(self.focus_handle_ref(), cx);
        }
    }

    pub fn palette_move(&mut self, down: bool, cx: &mut Context<Self>) {
        let count = self.palette_items(cx).len().min(SHOWN);
        if let Some(p) = self.palette.as_mut() {
            if count > 0 {
                p.selected = if down { (p.selected + 1) % count } else { (p.selected + count - 1) % count };
            }
        }
        cx.notify();
    }

    fn palette_run(&mut self, index: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(selected) = self.palette.as_ref().map(|p| index.unwrap_or(p.selected)) else { return };
        let mut items = self.palette_items(cx);
        if selected >= items.len() {
            return;
        }
        let item = items.swap_remove(selected);
        let input = self.palette.take().map(|p| p.input.read(cx).focus_handle_clone());
        (item.run)(self, window, cx);
        // Unless the command gave the keyboard to something else.
        self.palette_gone(input, window, cx);
        cx.notify();
    }

    /// Everything the palette can go to, filtered by what is typed.
    fn palette_items(&self, cx: &Context<Self>) -> Vec<Item> {
        let query = self.palette.as_ref().map(|p| p.input.read(cx).text()).unwrap_or_default().to_lowercase();
        let words: Vec<&str> = query.split_whitespace().collect();
        let mut items: Vec<Item> = Vec::new();
        let command = |label: &str, hint: &str, run: Run| Item { label: label.to_string(), hint: hint.to_string(), group: "Commands", run };
        items.push(command("New session…", &keymap::label(&NewSession), Box::new(|ws, window, cx| ws.open_new_session(None, None, window, cx))));
        items.push(command("Go to Today", "Your day", Box::new(|ws, window, cx| ws.open_today(window, cx))));
        items.push(command("Go to the Board", "Tickets from Linear", Box::new(|ws, _, cx| ws.show_tickets(cx))));
        items.push(command("Go to Usage", "Claude and Codex", Box::new(|ws, _, cx| ws.show_view(View::Usage, cx))));
        items.push(command("Go back", &keymap::label(&GoBack), Box::new(|ws, window, cx| ws.go_back(window, cx))));
        items.push(command("Go forward", &keymap::label(&GoForward), Box::new(|ws, window, cx| ws.go_forward(window, cx))));
        items.push(command("Open Settings", &keymap::label(&OpenSettings), Box::new(|ws, window, cx| ws.go(View::Settings, window, cx))));
        items.push(command("Change the theme", "Themes", Box::new(|ws, _, cx| ws.open_appearance(cx))));
        items.push(command("Accounts", "Settings", Box::new(|ws, window, cx| ws.open_settings(crate::settings_ui::Section::Accounts, window, cx))));
        items.push(command("Linear settings", "Who you are, your reviewer label", Box::new(|ws, window, cx| ws.open_linear_settings(window, cx))));
        items.push(command("GitHub account for a project", "Projects & Git", Box::new(|ws, window, cx| ws.open_settings(crate::settings_ui::Section::Projects, window, cx))));
        items.push(command("Add a project…", &keymap::label(&AddProject), Box::new(|ws, window, cx| ws.prompt_add_project(window, cx))));
        items.push(command("Hand off this session…", &keymap::label(&Handoff), Box::new(|ws, window, cx| ws.open_handoff(None, None, window, cx))));
        items.push(command("Show or hide the left sidebar", &keymap::label(&ToggleLeftPanel), Box::new(|ws, _, cx| ws.toggle_left(cx))));
        items.push(command("Show or hide the side panel", &keymap::label(&ToggleRightPanel), Box::new(|ws, _, cx| ws.toggle_right(cx))));
        items.push(command("Browser", "Settings", Box::new(|ws, window, cx| ws.open_settings(crate::settings_ui::Section::Browser, window, cx))));
        items.push(command("Server…", "Your always-on machine", Box::new(|ws, window, cx| ws.open_server_dialog(window, cx))));
        items.push(command("Check for CLI updates", "Updates & CLIs", Box::new(|ws, window, cx| ws.open_settings(crate::settings_ui::Section::Updates, window, cx))));
        for s in &self.state.sessions {
            let id = s.id.clone();
            items.push(Item {
                label: self.session_title(s),
                hint: format!("{} · ⎇ {}", s.agent.name(), s.branch),
                group: "Sessions",
                run: Box::new(move |ws, window, cx| {
                    ws.center = crate::center::Center::Session;
                    ws.open_session(&id, window, cx);
                }),
            });
        }
        if let Some(snapshot) = self.tickets.snapshot.as_ref() {
            for t in snapshot.mine.iter().chain(snapshot.reviewing.iter()) {
                let key = Key::Ticket(t.identifier.clone());
                items.push(Item { label: format!("{} · {}", t.identifier, t.title), hint: t.status.name.clone(), group: "Tickets", run: Box::new(move |ws, _, cx| ws.open_ticket(key.clone(), cx)) });
            }
        }
        for r in &self.tickets.reviews {
            let key = Key::Pr(r.repo.clone(), r.hit.number);
            items.push(Item { label: format!("#{} · {}", r.hit.number, crate::tickets_view::tidy_pr_title(&r.hit.title)), hint: format!("{} · asks for your review", r.repo), group: "Reviews", run: Box::new(move |ws, _, cx| ws.open_ticket(key.clone(), cx)) });
        }
        if words.is_empty() {
            // Nothing typed: the commands, then the sessions.
            items.retain(|i| i.group == "Commands" || i.group == "Sessions");
            return items;
        }
        let mut scored: Vec<(usize, Item)> = items
            .into_iter()
            .filter_map(|item| {
                let text = format!("{} {} {}", item.label, item.hint, item.group).to_lowercase();
                if !words.iter().all(|w| text.contains(w)) {
                    return None;
                }
                // Starts of the label first, then the label, then the hint.
                let label = item.label.to_lowercase();
                let score = if label.starts_with(words[0]) { 0 } else if label.contains(words[0]) { 1 } else { 2 };
                Some((score, item))
            })
            .collect();
        scored.sort_by_key(|(score, _)| *score);
        scored.into_iter().map(|(_, item)| item).collect()
    }

    pub fn render_palette(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let palette = self.palette.as_ref()?;
        let items = self.palette_items(cx);
        let total = items.len();
        let mut last_group = "";
        let mut rows: Vec<AnyElement> = Vec::new();
        for (i, item) in items.iter().take(SHOWN).enumerate() {
            if item.group != last_group {
                last_group = item.group;
                rows.push(div().px_3().pt_2().pb_1().text_size(px(11.)).font_weight(FontWeight::MEDIUM).text_color(theme::faint()).child(item.group).into_any_element());
            }
            let on = i == palette.selected;
            rows.push(
                div()
                    .id(SharedString::from(format!("palette-{i}")))
                    .flex()
                    .items_center()
                    .gap_3()
                    .h(px(36.))
                    .px_3()
                    .rounded(px(8.))
                    .cursor_pointer()
                    .when(on, |d| d.bg(theme::selected()))
                    .hover(|s| s.bg(theme::hover()))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.palette_run(Some(i), window, cx)))
                    .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(theme::text_strong()).child(item.label.clone()))
                    .child(div().flex_none().max_w(px(220.)).overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(px(12.)).text_color(theme::faint()).child(item.hint.clone()))
                    .into_any_element(),
            );
        }
        let more = total.saturating_sub(SHOWN);
        Some(
            deferred(
                div()
                    .id("palette-overlay")
                    .occlude()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .justify_center()
                    .items_start()
                    .pt(px(64.))
                    .bg(theme::overlay())
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_palette(window, cx)))
                    .child(
                        div()
                            .id("palette")
                            .key_context("Palette")
                            .on_action(cx.listener(|this, _: &crate::workspace::PaletteUp, _, cx| this.palette_move(false, cx)))
                            .on_action(cx.listener(|this, _: &crate::workspace::PaletteDown, _, cx| this.palette_move(true, cx)))
                            .on_click(|_: &ClickEvent, _, cx| cx.stop_propagation())
                            .flex()
                            .flex_col()
                            .w(px(600.))
                            .max_h(px(520.))
                            .rounded(px(14.))
                            .border_1()
                            .border_color(theme::border_strong())
                            .bg(theme::dialog())
                            .shadow_lg()
                            .overflow_hidden()
                            .child(div().p_2().border_b_1().border_color(theme::border()).child(palette.input.clone()))
                            .child(
                                div()
                                    .id("palette-list")
                                    .flex()
                                    .flex_col()
                                    .p_1p5()
                                    .overflow_y_scroll()
                                    .children(rows)
                                    .when(total == 0, |d| d.child(div().px_3().py_4().text_color(theme::muted()).child("Nothing matches. Try other words.")))
                                    .when(more > 0, |d| d.child(div().px_3().py_2().text_size(px(12.)).text_color(theme::faint()).child(format!("{more} more: type a little more to narrow it down")))),
                            ),
                    ),
            )
            .with_priority(6)
            .into_any_element(),
        )
    }
}
