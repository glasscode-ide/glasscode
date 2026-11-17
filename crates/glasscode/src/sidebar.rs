//! The sidebar's rows, for this Mac's sessions and the server's. One row per worktree (the
//! piece of work): its agent's icon (with a count when several sessions share the worktree),
//! its name, then a quiet second line with where it is, and two aligned columns, Review /
//! Team and the pull request's number with its review state. On the right one icon says what
//! needs attention: green while working, orange when it needs you. A selected worktree with
//! several sessions lists them underneath.

use std::time::Duration;

use gpui::{
    Animation, AnimationExt, AnyElement, App, ClickEvent, Div, FontWeight, Hsla, MouseButton, MouseDownEvent, SharedString, Window, div,
    prelude::*, pulsating_between,
};
use glasscode_core::model::AgentKind;

use crate::icons;
use crate::theme;

/// What a row says about its sessions, most urgent first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RowState {
    NeedsYou,
    Working,
    Idle,
}

pub struct Row {
    /// Unique among rows (element ids).
    pub key: String,
    pub title: String,
    pub agent: AgentKind,
    /// Its program is running (else the icon is faded: it starts when opened).
    pub running: bool,
    pub selected: bool,
    pub state: RowState,
    /// The second line: branch, repository or task.
    pub sub: String,
    /// "Review" or "Team".
    pub tag: Option<&'static str>,
    /// "#958 ✓" and its colour.
    pub pr: Option<(String, Hsla)>,
    /// Sessions in this worktree (a badge when more than one).
    pub count: usize,
    /// Pinned to the top of the sidebar (shows a small pin by the name).
    pub pinned: bool,
    /// Shown under the row (the worktree's sessions).
    pub kids: Vec<Kid>,
}

/// One session of a worktree that has several, under its row.
pub struct Kid {
    pub key: String,
    pub agent: AgentKind,
    pub label: String,
    pub state: RowState,
    pub selected: bool,
    pub running: bool,
    pub on_click: OnClick,
}

pub type OnClick = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;
pub type OnRightClick = Box<dyn Fn(&MouseDownEvent, &mut Window, &mut App)>;

/// The orange that means "needs you".
pub fn attention() -> Hsla {
    theme::attention()
}

/// An agent's mark (Claude, Codex, a terminal for a shell), faded while its program is not
/// running.
pub fn agent_icon(agent: AgentKind, size: f32, running: bool) -> Div {
    div()
        .flex_none()
        .size(theme::sz(size))
        .flex()
        .items_center()
        .justify_center()
        .when(!running, |d| d.opacity(0.6))
        // `brand` takes pixels on the screen, so the scale goes in here.
        .child(icons::brand(icons::agent_brand(agent), (size * 0.82 * theme::scale()).round()))
}

/// Green while working (a slow pulse), orange when it needs you, nothing when idle.
pub fn status_icon(state: RowState, key: &str) -> Option<AnyElement> {
    match state {
        RowState::Working => Some(
            div()
                .size(theme::sz(8.))
                .rounded_full()
                .bg(theme::added())
                .with_animation(SharedString::from(format!("working-{key}")), Animation::new(Duration::from_millis(1400)).repeat().with_easing(pulsating_between(0.35, 1.0)), |d, t| d.opacity(t))
                .into_any_element(),
        ),
        RowState::NeedsYou => Some(
            div()
                .size(theme::sz(15.))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(attention())
                .text_color(theme::on_attention())
                .text_size(theme::sz(10.5))
                .font_weight(FontWeight::BOLD)
                .child("!")
                .into_any_element(),
        ),
        RowState::Idle => None,
    }
}

fn hover_action(id: SharedString, label: &'static str, on_click: OnClick) -> AnyElement {
    div()
        .id(id)
        .size(theme::sz(24.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .text_color(theme::muted())
        .text_size(theme::sz(14.))
        .cursor_pointer()
        .hover(|s| s.bg(theme::selected()).text_color(theme::text_strong()))
        .on_click(move |event, window, cx| {
            cx.stop_propagation();
            on_click(event, window, cx);
        })
        .child(label)
        .into_any_element()
}

/// A worktree's row. `close` adds the × on hover; `menu` the ⋯ and right-click.
pub fn session_row(row: Row, on_click: OnClick, close: Option<OnClick>, menu: Option<(OnClick, OnRightClick)>) -> AnyElement {
    let group = SharedString::from(format!("side-row-{}", row.key));
    let (menu_click, menu_right) = match menu {
        Some((click, right)) => (Some(click), Some(right)),
        None => (None, None),
    };
    let mut actions: Vec<AnyElement> = Vec::new();
    if let Some(click) = menu_click {
        actions.push(hover_action(SharedString::from(format!("side-menu-{}", row.key)), "⋯", click));
    }
    if let Some(close) = close {
        actions.push(hover_action(SharedString::from(format!("side-close-{}", row.key)), "×", close));
    }
    let has_actions = !actions.is_empty();
    let status = status_icon(row.state, &row.key);

    let icon = div()
        .relative()
        .flex_none()
        .child(agent_icon(row.agent, 22., row.running))
        .when(row.count > 1, |d| {
            d.child(
                div()
                    .absolute()
                    .right(theme::sz(-6.))
                    .bottom(theme::sz(-6.))
                    .min_w(theme::sz(15.))
                    .h(theme::sz(15.))
                    .px(theme::sz(3.))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(theme::text_strong())
                    .text_color(theme::bg())
                    .text_size(theme::sz(9.5))
                    .font_weight(FontWeight::BOLD)
                    .border_2()
                    .border_color(theme::panel())
                    .child(row.count.to_string()),
            )
        });

    let second = div()
        .flex()
        .items_center()
        .gap_2()
        .min_w_0()
        .text_size(theme::sz(11.))
        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(theme::muted()).child(row.sub))
        .when_some(row.tag, |d, tag| {
            d.child(
                div().flex_none().w(theme::sz(50.)).child(
                    div()
                        .flex()
                        .items_center()
                        .h(theme::sz(17.))
                        .px_1p5()
                        .rounded_full()
                        .border_1()
                        .border_color(theme::border_strong())
                        .text_color(theme::text_soft())
                        .text_size(theme::sz(10.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(tag),
                ),
            )
        })
        .child({
            let (text, color) = row.pr.clone().unwrap_or_default();
            div().flex_none().w(theme::sz(54.)).flex().justify_end().whitespace_nowrap().text_color(color).child(text)
        });

    let mut out = div()
        .id(SharedString::from(format!("side-{}", row.key)))
        .group(group.clone())
        .relative()
        .flex()
        .items_center()
        .gap_2p5()
        // Density (Themes): compact rows are closer together.
        .min_h(theme::sz(if theme::compact() { 36. } else { 44. }))
        .py(theme::sz(if theme::compact() { 3. } else { 6. }))
        .pl_2()
        .pr_1p5()
        .rounded(theme::sz(8.))
        .cursor_pointer()
        .when(row.selected, |d| d.bg(theme::selected()))
        .hover(|s| s.bg(theme::hover()))
        .on_click(move |event, window, cx| on_click(event, window, cx))
        .when_some(menu_right, |d, right| d.on_mouse_down(MouseButton::Right, move |event, window, cx| right(event, window, cx)))
        .child(icon)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(theme::sz(2.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .min_w_0()
                        .when(row.pinned, |d| {
                            d.child(div().flex_none().text_size(theme::sz(10.)).text_color(theme::faint()).child("📌"))
                        })
                        .child(
                            div()
                                .flex_1()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_size(theme::sz(12.5))
                                .text_color(if row.selected { theme::text_strong() } else { theme::text_soft() })
                                .when(row.state == RowState::NeedsYou, |d| d.font_weight(FontWeight::SEMIBOLD))
                                .child(row.title),
                        ),
                )
                .child(second),
        )
        .child(div().flex_none().w(theme::sz(16.)).flex().justify_center().children(status));
    if has_actions {
        out = out.child(
            div()
                .absolute()
                .right(theme::sz(4.))
                .top(theme::sz(10.))
                .flex()
                .gap_0p5()
                .p_0p5()
                .rounded_md()
                .bg(theme::hover())
                .invisible()
                .group_hover(group, |s| s.visible())
                .children(actions),
        );
    }
    let kids: Vec<AnyElement> = row.kids.into_iter().map(kid_row).collect();
    div().flex().flex_col().child(out).children(kids).into_any_element()
}

fn kid_row(kid: Kid) -> AnyElement {
    let on_click = kid.on_click;
    div()
        .id(SharedString::from(format!("side-kid-{}", kid.key)))
        .flex()
        .items_center()
        .gap_2()
        .h(theme::sz(if theme::compact() { 26. } else { 30. }))
        .pl(theme::sz(40.))
        .pr_1p5()
        .rounded(theme::sz(8.))
        .cursor_pointer()
        .when(kid.selected, |d| d.bg(theme::selected()))
        .hover(|s| s.bg(theme::hover()))
        .text_size(theme::sz(12.))
        .text_color(if kid.selected { theme::text_strong() } else { theme::text_soft() })
        .on_click(move |event, window, cx| on_click(event, window, cx))
        .child(agent_icon(kid.agent, 17., kid.running))
        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(kid.label))
        .child(div().flex_none().w(theme::sz(16.)).flex().justify_center().children(status_icon(kid.state, &kid.key)))
        .into_any_element()
}

/// A review's "#958 fix: …" without the number, which the row shows in its own column.
pub fn without_number(title: &str, number: Option<u64>) -> String {
    if let Some(n) = number {
        if let Some(rest) = title.strip_prefix(&format!("#{n} ")) {
            return rest.to_string();
        }
    }
    title.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_titles_lose_their_number() {
        assert_eq!(without_number("#960 fix(core): configure the gateway", Some(960)), "fix(core): configure the gateway");
        assert_eq!(without_number("#960 fix", Some(961)), "#960 fix");
        assert_eq!(without_number("OCR", None), "OCR");
    }

    #[test]
    fn needs_you_is_most_urgent() {
        let mut states = vec![RowState::Idle, RowState::Working, RowState::NeedsYou];
        states.sort();
        assert_eq!(states[0], RowState::NeedsYou);
    }
}
