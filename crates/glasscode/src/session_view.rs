//! The centre of the Sessions view: the active session (its header, the chat or the terminal)
//! or the empty state, and the header's menus.

use gpui::{AnyElement, ClickEvent, Context, FontWeight, SharedString, Window, deferred, div, prelude::*, px};
use glasscode_core::agents::AgentActivity;
use glasscode_core::model::{AgentKind, Session};

use crate::theme;
use crate::ui;
use crate::workspace::Workspace;

impl Workspace {
    pub(crate) fn render_center(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Sessions are switched and closed from the sidebar; the center shows just the active one.
        let active = self.active_session().cloned();
        let body: AnyElement = match active {
            Some(session) => self.render_session(session, window, cx).into_any_element(),
            None => self.render_empty_state(cx).into_any_element(),
        };

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(theme::bg())
            .child(body)
    }

    fn render_empty_state(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let has_projects = !self.state.projects.is_empty();
        div()
            .flex()
            .flex_col()
            .flex_1()
            .items_center()
            .justify_center()
            .gap_3()
            .child(
                div()
                    .text_size(theme::sz(18.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(if has_projects { "No open sessions" } else { "Welcome to Glasscode" }),
            )
            .child(div().max_w(theme::sz(380.)).text_color(theme::muted()).child(if has_projects {
                "Start a session on any project. New sessions open Claude with your default account."
            } else {
                "Add a git repository. Each session gets its own worktree, agent and account."
            }))
            .child(if has_projects {
                ui::primary_button("empty-new", "New session")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.open_new_session(None, None, window, cx)
                    }))
            } else {
                ui::primary_button("empty-add", "Add project…")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.prompt_add_project(window, cx)))
            })
    }

    fn render_session(&mut self, session: Session, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.terminals.contains_key(&session.id) && session.worktree.is_dir() {
            // A restored session that has not been resumed yet.
            let id = session.id.clone();
            return div()
                .flex()
                .flex_col()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_3()
                .child(div().text_size(theme::sz(16.)).font_weight(FontWeight::SEMIBOLD).child(session.title.clone()))
                .child(div().text_color(theme::muted()).child(match &session.agent_session_id {
                    Some(conversation) => format!(
                        "Saved {} conversation {} — resume it with the same account.",
                        session.agent.name(),
                        &conversation[..8.min(conversation.len())]
                    ),
                    None => format!("{} session in {}", session.agent.name(), session.branch),
                }))
                .child(ui::primary_button("resume", "Resume session").on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| this.restart_session(&id, window, cx),
                )))
                .into_any_element();
        }
        let terminal = self.terminals.get(&session.id).cloned();
        let exited = terminal
            .as_ref()
            .and_then(|t| t.read(cx).exit_code)
            .map(|code| code.map(|c| c.to_string()).unwrap_or_else(|| "signal".into()));
        let account = session
            .account_id
            .as_deref()
            .and_then(|id| self.state.account(id))
            .cloned();
        let tabs = self.worktree_tabs(&session, cx);
        // Claude and Codex read as a chat, or show their terminal (the header's switch).
        let chat = if self.session_view(&session) == glasscode_core::model::SessionView::Chat { Some(self.render_chat(&session, window, cx)) } else { None };
        let chat_view = chat.is_some();
        let header = self.render_session_header(&session, account.as_ref(), window, cx);
        let restart_id = session.id.clone();
        let close_id = session.id.clone();

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(header)
            .children(tabs)
            .when(session.lineage.len() > 1, |d| {
                d.child(
                    div()
                        .px_3p5()
                        .py_1p5()
                        .bg(theme::header())
                        .border_b_1()
                        .border_color(theme::border())
                        .text_size(theme::sz(11.5))
                        .text_color(theme::muted())
                        .child(format!("Session history: {}", session.lineage.join("  →  "))),
                )
            })
            .child(match chat {
                Some(chat) => div().flex().flex_col().flex_1().min_h_0().child(chat),
                None => div().flex_1().min_h_0().when_some(terminal, |d, t| d.child(t)),
            })
            // In the Terminal view, what the agent asks Glasscode to do shows under it.
            .when(!chat_view, |d| {
                let asks: Vec<AnyElement> = self.control.pending_for(&session.id).into_iter().map(|a| crate::today_ui::render_approval(a, true, cx)).collect();
                d.when(!asks.is_empty(), |d| d.child(div().flex().flex_col().gap_2().p_3().bg(theme::header()).border_t_1().border_color(theme::border()).children(asks)))
            })
            .when_some(exited, |d, code| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .px_3p5()
                        .py_2()
                        .bg(theme::header())
                        .border_t_1()
                        .border_color(theme::border())
                        .child(
                            div()
                                .flex_1()
                                .text_color(theme::muted())
                                .child(format!("{} exited ({code}).", session.agent.name())),
                        )
                        .child(ui::primary_button("restart", "Resume").on_click(cx.listener(
                            move |this, _: &ClickEvent, window, cx| this.restart_session(&restart_id, window, cx),
                        )))
                        .child(ui::button("close-ended", "Close tab").on_click(cx.listener(
                            move |this, _: &ClickEvent, window, cx| this.close_session(&close_id, window, cx),
                        ))),
                )
            })
            .into_any_element()
    }

    fn render_session_header(
        &self,
        session: &Session,
        account: Option<&glasscode_core::model::Account>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let status = self.activity.get(&session.id).cloned();
        let menu = if self.menu_open && account.is_some() {
            Some(self.render_account_menu(session, cx))
        } else {
            None
        };
        let other = session.agent.other();
        div()
            .flex()
            .flex_none()
            .flex_wrap()
            .items_center()
            .gap_x_3()
            .gap_y_2()
            .px_3p5()
            .py_2()
            .bg(theme::header())
            .border_b_1()
            .border_color(theme::border())
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(ui::dot(theme::agent_color(session.agent)))
                    .child(session.agent.name()),
            )
            .when_some(account, |d, account| {
                d.child(
                    div()
                        .relative()
                        .flex_none()
                        .child(
                            ui::button("account-menu", format!("{}  ▾", account.display())).on_click(
                                cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.menu_open = !this.menu_open;
                                    cx.notify();
                                }),
                            ),
                        )
                        .when_some(menu, |d, menu| d.child(menu)),
                )
            })
            .child(
                div()
                    .min_w(theme::sz(60.))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .font_family(theme::MONO_FONT)
                    .text_size(theme::sz(12.))
                    .text_color(theme::text_soft())
                    .child(format!("⎇ {}", session.branch)),
            )
            // The Linear ticket this session works on: opens it in Tickets.
            .when_some(self.ticket_for_session(session).map(|t| (t.identifier.clone(), t.status.name.clone())), |d, (id, status)| {
                let key = crate::tickets_ui::Key::Ticket(id.clone());
                d.child(
                    div()
                        .id("session-ticket")
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap_1p5()
                        .h(theme::sz(24.))
                        .px_2()
                        .rounded_md()
                        .border_1()
                        .border_color(theme::border_strong())
                        .text_size(theme::sz(12.))
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open_ticket(key.clone(), cx)))
                        .child(div().font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child(id))
                        .child(div().text_color(theme::muted()).child(status)),
                )
            })
            .when(session.review.is_some(), |d| {
                d.child(div().flex_none().px_1p5().py_0p5().rounded(theme::sz(5.)).bg(theme::tag_bg()).text_size(theme::sz(11.5)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text_soft()).child("Review"))
            })
            // Read it as a chat (the Claude app's look) or see the agent's own terminal.
            .when(session.agent != AgentKind::Shell, |d| {
                let view = self.session_view(session);
                let (chat_id, term_id) = (session.id.clone(), session.id.clone());
                d.child(
                    ui::segmented()
                        .flex_none()
                        .child(ui::segment("view-chat", view == glasscode_core::model::SessionView::Chat, "Chat").h(theme::sz(24.)).px_2p5().on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.set_session_view(&chat_id, glasscode_core::model::SessionView::Chat, window, cx)
                        })))
                        .child(ui::segment("view-terminal", view == glasscode_core::model::SessionView::Terminal, "Terminal").h(theme::sz(24.)).px_2p5().on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.set_session_view(&term_id, glasscode_core::model::SessionView::Terminal, window, cx)
                        }))),
                )
            })
            .when_some(status, |d, status| {
                d.child(
                    div()
                        .flex_none()
                        .whitespace_nowrap()
                        .text_size(theme::sz(11.5))
                        .text_color(match status {
                            AgentActivity::Working => theme::added(),
                            AgentActivity::NeedsInput => theme::attention(),
                            AgentActivity::Idle => theme::muted(),
                        })
                        .child(match status {
                            AgentActivity::Working => "● working",
                            AgentActivity::NeedsInput => "● needs your input",
                            AgentActivity::Idle => "● idle",
                        }),
                )
            })
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    // The session's tab in the Glasscode Browser, brought to the front for the person.
                    .when(self.browser.session_tabs.contains_key(&session.id), |d| {
                        let id = session.id.clone();
                        d.child(
                            ui::button("session-browser", "◍ Browser")
                                .flex_none()
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.show_session_tab(&id, cx))),
                        )
                    })
                    .child(
                        ui::button("handoff", "⇄  Hand off…").flex_none().on_click(
                            cx.listener(move |this, _: &ClickEvent, window, cx| this.open_handoff(Some(other), None, window, cx)),
                        ),
                    )
                    .child(
                        div()
                            .relative()
                            .flex_none()
                            .child(ui::button("session-more", "⋯").w(theme::sz(34.)).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.session_more = !this.session_more;
                                cx.notify();
                            })))
                            .when(self.session_more, |d| d.child(self.render_session_more(session, cx))),
                    )
                    .child(
                        ui::button("toggle-right", if self.show_right { "▥" } else { "▢" })
                            .w(theme::sz(34.))
                            .flex_none()
                            .when(self.show_right, |b| b.bg(theme::selected()))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_right(cx))),
                    ),
            )
    }

    /// The header's ⋯ menu: what is done with a session less often.
    fn render_session_more(&self, session: &Session, cx: &mut Context<Self>) -> AnyElement {
        let item = |id: &'static str, label: String| {
            div()
                .id(id)
                .px_2p5()
                .py_2()
                .rounded_md()
                .text_size(theme::sz(13.))
                .cursor_pointer()
                .hover(|s| s.bg(theme::hover()))
                .child(label)
        };
        let (move_id, close_id) = (session.id.clone(), session.id.clone());
        let conversation = session.agent_session_id.clone();
        deferred(
            div()
                .id("session-more-menu")
                .occlude()
                .absolute()
                .top(theme::sz(36.))
                .right_0()
                .w(theme::sz(240.))
                .p_1()
                .flex()
                .flex_col()
                .rounded_lg()
                .border_1()
                .border_color(theme::border_strong())
                .bg(theme::dialog())
                .shadow_lg()
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.session_more = false;
                    cx.notify();
                }))
                .when(self.server.connected(), |d| {
                    d.child(item("more-move", format!("⇡ Move to {}", self.server.name())).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.session_more = false;
                        this.move_one(crate::switch_ui::Direction::ToServer, move_id.clone(), cx)
                    })))
                })
                .child(item("more-editor", format!("Open in {}", self.preferred_editor().map(|(name, _)| name).unwrap_or(crate::platform::file_manager()))).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.session_more = false;
                    this.open_in_editor(None, cx)
                })))
                .when_some(conversation, |d, id| {
                    d.child(item("more-copy-id", format!("Copy conversation id ({})", &id[..8.min(id.len())])).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.session_more = false;
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(id.clone()));
                        this.show_toast("Conversation id copied".to_string(), cx);
                    })))
                })
                .child(div().h(px(1.)).my_1().bg(theme::border()))
                .child(item("more-close", "Close session…".to_string()).text_color(theme::removed()).on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.session_more = false;
                    this.close_session(&close_id, window, cx)
                }))),
        )
        .with_priority(5)
        .into_any_element()
    }

    fn render_account_menu(&self, session: &Session, cx: &mut Context<Self>) -> AnyElement {
        let mut items: Vec<AnyElement> = Vec::new();
        for agent in [AgentKind::Claude, AgentKind::Codex] {
            items.push(
                div()
                    .px_2()
                    .pt_2()
                    .pb_1()
                    .child(ui::caps_label(format!("{} ACCOUNTS", agent.name().to_uppercase())))
                    .into_any_element(),
            );
            let accounts: Vec<_> = self.state.accounts_for(agent).cloned().collect();
            if accounts.is_empty() {
                items.push(
                    div()
                        .px_2()
                        .py_1()
                        .text_size(theme::sz(12.))
                        .text_color(theme::faint())
                        .child("None yet")
                        .into_any_element(),
                );
            }
            for account in accounts {
                let current = session.account_id.as_deref() == Some(account.id.as_str());
                let same_agent = agent == session.agent;
                let hint = if current {
                    "Current"
                } else if same_agent {
                    "Switch"
                } else {
                    "Hand off →"
                };
                let account_id = account.id.clone();
                items.push(
                    div()
                        .id(SharedString::from(format!("menu-{}", account.id)))
                        .flex()
                        .items_center()
                        .gap_2()
                        .h(theme::sz(32.))
                        .px_2()
                        .rounded_md()
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.menu_open = false;
                            if current {
                                cx.notify();
                            } else if same_agent {
                                this.switch_account(&account_id, window, cx);
                            } else {
                                this.open_handoff(Some(agent), Some(account_id.clone()), window, cx);
                            }
                        }))
                        .child(ui::dot(theme::agent_color(agent)))
                        .child(account.label.clone())
                        .child(
                            div()
                                .text_size(theme::sz(11.))
                                .text_color(theme::muted())
                                .child(account.plan.clone().unwrap_or_default()),
                        )
                        .child(
                            div()
                                .ml_auto()
                                .text_size(theme::sz(11.))
                                .text_color(if current { theme::accent() } else { theme::muted() })
                                .child(hint),
                        )
                        .into_any_element(),
                );
            }
        }
        items.push(div().h(px(1.)).my_1p5().bg(theme::border_strong()).into_any_element());
        items.push(
            div()
                .id("menu-manage")
                .flex()
                .items_center()
                .h(theme::sz(32.))
                .px_2()
                .rounded_md()
                .cursor_pointer()
                .hover(|s| s.bg(theme::hover()))
                .text_color(theme::text_soft())
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_accounts(window, cx)))
                .child("Manage accounts…")
                .into_any_element(),
        );
        deferred(
            div()
                .id("account-menu-popover")
                .occlude()
                .absolute()
                .top(theme::sz(36.))
                .left_0()
                .w(theme::sz(320.))
                .p_1p5()
                .rounded_lg()
                .border_1()
                .border_color(theme::border_strong())
                .bg(theme::subtle())
                .shadow_lg()
                .children(items),
        )
        .with_priority(2)
        .into_any_element()
    }
}
