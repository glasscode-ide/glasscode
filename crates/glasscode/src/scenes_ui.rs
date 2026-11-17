//! Drawing a `Scene`: the picture the manager explains with, one step lit at a time. It is
//! built from plain boxes and text in the theme's colours, so it reads in every theme. A flow
//! is boxes in rows with the arrows between neighbours drawn between them (any other arrow is
//! listed under the boxes); a sequence is a list of "A → B: label" rows; code is a file with
//! lines lit; a comparison is two cards side by side. Under it: the step's sentence.

use gpui::{AnyElement, Div, FontWeight, Hsla, SharedString, div, prelude::*, px};
use glasscode_core::manager::{Scene, SceneEdge, SceneKind, SceneStep};

use crate::theme;

// ----- the manager's own colour (its avatar, its orb, what it points at) -------------------

/// The warm colour that marks the manager, from the theme.
pub(crate) fn gold() -> Hsla {
    theme::modified()
}

/// A quiet fill in that colour.
pub(crate) fn gold_bg() -> Hsla {
    theme::warn_bg()
}

/// A thin line in that colour.
pub(crate) fn gold_line() -> Hsla {
    let mut color = theme::modified();
    color.a = 0.45;
    color
}

/// Text that may have line breaks: one row per line, an empty line a small gap.
pub(crate) fn text_lines(text: &str) -> Div {
    div().flex().flex_col().min_w_0().children(text.trim_end().split('\n').map(|line| {
        if line.trim().is_empty() { div().h(px(8.)) } else { div().min_w_0().child(line.replace('\t', "    ")) }
    }))
}

// ----- what a step shows -------------------------------------------------------------------

/// What a step shows: the nodes and edges revealed so far (indexes into the scene, in its
/// order), which of them came with this step, the node it is about, and the lines lit.
#[derive(Debug, Default, PartialEq)]
struct Shown {
    nodes: Vec<usize>,
    edges: Vec<usize>,
    new_nodes: Vec<usize>,
    new_edges: Vec<usize>,
    focus: Option<usize>,
    lines: Vec<usize>,
    say: String,
    /// The step drawn, and how many there are.
    at: usize,
    steps: usize,
}

/// The nodes and edges a step has revealed. A node an edge touches is shown with it. No
/// step at all (a scene without steps): everything.
fn revealed(scene: &Scene, step: Option<&SceneStep>) -> (Vec<usize>, Vec<usize>) {
    let Some(step) = step else {
        return ((0..scene.nodes.len()).collect(), (0..scene.edges.len()).collect());
    };
    let mut edges: Vec<usize> = step.edges.iter().copied().filter(|i| *i < scene.edges.len()).collect();
    edges.sort_unstable();
    edges.dedup();
    let nodes = scene
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| step.nodes.contains(&n.id) || edges.iter().any(|e| scene.edges[*e].from == n.id || scene.edges[*e].to == n.id))
        .map(|(i, _)| i)
        .collect();
    (nodes, edges)
}

fn shown(scene: &Scene, step: usize) -> Shown {
    if scene.steps.is_empty() {
        let (nodes, edges) = revealed(scene, None);
        return Shown { nodes, edges, ..Shown::default() };
    }
    let at = step.min(scene.steps.len() - 1);
    let now = &scene.steps[at];
    let (nodes, edges) = revealed(scene, Some(now));
    let (new_nodes, new_edges) = if at == 0 {
        (Vec::new(), Vec::new())
    } else {
        let (before_nodes, before_edges) = revealed(scene, Some(&scene.steps[at - 1]));
        (nodes.iter().copied().filter(|n| !before_nodes.contains(n)).collect(), edges.iter().copied().filter(|e| !before_edges.contains(e)).collect())
    };
    Shown {
        focus: now.focus.as_ref().and_then(|id| scene.nodes.iter().position(|n| &n.id == id)),
        lines: now.lines.clone(),
        say: now.say.clone(),
        nodes,
        edges,
        new_nodes,
        new_edges,
        at,
        steps: scene.steps.len(),
    }
}

/// For a flow: the edges that run between two boxes drawn next to each other (per gap, with
/// whether they point forward), and the edges that go anywhere else.
fn between(scene: &Scene, shown: &Shown) -> (Vec<Vec<(usize, bool)>>, Vec<usize>) {
    let mut gaps: Vec<Vec<(usize, bool)>> = vec![Vec::new(); shown.nodes.len().saturating_sub(1)];
    let mut rest = Vec::new();
    for &e in &shown.edges {
        let edge = &scene.edges[e];
        let from = shown.nodes.iter().position(|n| scene.nodes[*n].id == edge.from);
        let to = shown.nodes.iter().position(|n| scene.nodes[*n].id == edge.to);
        match (from, to) {
            (Some(a), Some(b)) if b == a + 1 => gaps[a].push((e, true)),
            (Some(a), Some(b)) if a == b + 1 => gaps[b].push((e, false)),
            _ => rest.push(e),
        }
    }
    (gaps, rest)
}

/// A node's name as the person reads it.
fn name(scene: &Scene, id: &str) -> String {
    scene.nodes.iter().find(|n| n.id == id).map(|n| n.label.clone()).filter(|l| !l.trim().is_empty()).unwrap_or_else(|| id.to_string())
}

// ----- drawing -----------------------------------------------------------------------------

/// The colour of an edge: red when it is what goes wrong, strong when it is new.
fn edge_color(edge: &SceneEdge, fresh: bool) -> Hsla {
    if edge.bad {
        theme::danger()
    } else if fresh {
        theme::text_strong()
    } else {
        theme::muted()
    }
}

/// An arrow of a given length: a line (dashed when the edge is broken) and its head.
fn arrow(forward: bool, bad: bool, color: Hsla, length: f32) -> Div {
    let line = if bad {
        div().flex_1().h(px(0.)).border_t_2().border_dashed().border_color(color)
    } else {
        div().flex_1().h(px(1.5)).bg(color)
    };
    let head = div().flex_none().text_size(px(11.)).line_height(px(11.)).text_color(color).child(if forward { "▶" } else { "◀" });
    let row = div().flex().flex_none().items_center().w(px(length));
    if forward { row.child(line).child(head) } else { row.child(head).child(line) }
}

fn frame() -> Div {
    div().flex().flex_col().min_w_0().rounded(px(12.)).border_1().border_color(theme::border()).bg(theme::raised())
}

fn node_box(label: &str, note: &str, focus: bool, fresh: bool) -> Div {
    div()
        .flex()
        .flex_col()
        .flex_none()
        .items_center()
        .justify_center()
        .gap_0p5()
        .min_w(px(112.))
        .max_w(px(180.))
        .min_h(px(54.))
        .px_3()
        .py_2()
        .rounded(px(10.))
        .border_1()
        .border_color(if focus || fresh { gold() } else { theme::border_strong() })
        .when(focus, |d| d.border_2().bg(gold_bg()))
        .when(!focus, |d| d.bg(theme::panel()))
        .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).text_center().child(label.to_string()))
        .when(!note.trim().is_empty(), |d| d.child(div().text_size(px(11.)).text_color(theme::muted()).text_center().child(note.to_string())))
}

fn flow(scene: &Scene, shown: &Shown) -> AnyElement {
    let (gaps, rest) = between(scene, shown);
    let mut cells: Vec<AnyElement> = Vec::new();
    for (k, &n) in shown.nodes.iter().enumerate() {
        let node = &scene.nodes[n];
        cells.push(node_box(if node.label.trim().is_empty() { &node.id } else { &node.label }, &node.note, shown.focus == Some(n), shown.new_nodes.contains(&n)).into_any_element());
        let Some(edges) = gaps.get(k) else { continue };
        if edges.is_empty() {
            cells.push(div().flex_none().w(px(18.)).into_any_element());
            continue;
        }
        cells.push(
            div()
                .flex()
                .flex_col()
                .flex_none()
                .items_center()
                .gap_1p5()
                .px_1p5()
                .children(edges.iter().map(|&(e, forward)| {
                    let edge = &scene.edges[e];
                    let color = edge_color(edge, shown.new_edges.contains(&e));
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_0p5()
                        .when(!edge.label.trim().is_empty(), |d| {
                            d.child(div().max_w(px(150.)).font_family(theme::MONO_FONT).text_size(px(11.)).font_weight(FontWeight::MEDIUM).text_color(if edge.bad { color } else { theme::text_soft() }).text_center().child(edge.label.clone()))
                        })
                        .child(arrow(forward, edge.bad, color, 64.))
                }))
                .into_any_element(),
        );
    }
    frame()
        .child(div().flex().flex_wrap().items_center().gap_y_4().min_h(px(96.)).p_4().children(cells))
        .when(!rest.is_empty(), |d| {
            d.child(div().flex().flex_col().gap_1p5().px_4().py_3().border_t_1().border_color(theme::border()).children(rest.iter().map(|&e| edge_row(scene, e, None, shown.new_edges.contains(&e)))))
        })
        .into_any_element()
}

/// One edge as words: "A → B  label".
fn edge_row(scene: &Scene, e: usize, number: Option<usize>, fresh: bool) -> Div {
    let edge = &scene.edges[e];
    let color = edge_color(edge, fresh);
    let who = |id: &str| div().flex_none().max_w(px(200.)).truncate().font_weight(FontWeight::MEDIUM).text_color(theme::text_strong()).child(name(scene, id));
    div()
        .flex()
        .items_center()
        .gap_2()
        .min_w_0()
        .text_size(px(13.))
        .when_some(number, |d, n| d.child(div().flex_none().w(px(20.)).font_family(theme::MONO_FONT).text_size(px(11.5)).text_color(theme::faint()).child(n.to_string())))
        .child(who(&edge.from))
        .child(arrow(true, edge.bad, color, 34.))
        .child(who(&edge.to))
        .when(!edge.label.trim().is_empty(), |d| d.child(div().flex_1().min_w_0().text_color(if edge.bad { color } else { theme::text_soft() }).child(edge.label.clone())))
}

fn sequence(scene: &Scene, shown: &Shown) -> AnyElement {
    let people = div().flex().flex_wrap().gap_1p5().px_4().py_3().children(shown.nodes.iter().map(|&n| {
        let node = &scene.nodes[n];
        let focus = shown.focus == Some(n);
        div()
            .flex_none()
            .px_2p5()
            .py_0p5()
            .rounded_full()
            .border_1()
            .border_color(if focus || shown.new_nodes.contains(&n) { gold() } else { theme::border_strong() })
            .when(focus, |d| d.bg(gold_bg()))
            .text_size(px(12.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme::text_strong())
            .child(if node.label.trim().is_empty() { node.id.clone() } else { node.label.clone() })
    }));
    frame()
        .min_h(px(96.))
        .when(!shown.nodes.is_empty(), |d| d.child(people))
        .children(shown.edges.iter().enumerate().map(|(i, &e)| {
            let fresh = shown.new_edges.contains(&e);
            edge_row(scene, e, Some(i + 1), fresh).px_4().py_2().border_t_1().border_color(theme::border()).when(fresh, |d| d.bg(gold_bg()))
        }))
        .into_any_element()
}

fn code(scene: &Scene, shown: &Shown) -> AnyElement {
    let digits = scene.code.len().to_string().len().max(2) as f32;
    frame()
        .overflow_hidden()
        .when(!scene.file.trim().is_empty(), |d| {
            d.child(div().px_4().py_2().border_b_1().border_color(theme::border()).font_family(theme::MONO_FONT).text_size(px(12.)).text_color(theme::muted()).child(scene.file.clone()))
        })
        .child(div().id("scene-code").flex().flex_col().py_2().overflow_x_scroll().font_family(theme::MONO_FONT).text_size(px(12.5)).line_height(px(22.)).children(scene.code.iter().enumerate().map(
            |(i, line)| {
                let lit = shown.lines.contains(&i);
                div()
                    .flex()
                    .flex_none()
                    .h(px(22.))
                    .pr_4()
                    .border_l_3()
                    .border_color(if lit { gold() } else { gpui::transparent_black() })
                    .when(lit, |d| d.bg(gold_bg()))
                    .child(div().flex().flex_none().justify_end().w(px(14. + digits * 8.)).pr_3().text_color(theme::faint()).child((i + 1).to_string()))
                    .child(div().flex_none().whitespace_nowrap().text_color(if lit { theme::text_strong() } else { theme::muted() }).child(SharedString::from(line.replace('\t', "    "))))
            },
        )))
        .into_any_element()
}

fn compare(scene: &Scene, shown: &Shown) -> AnyElement {
    let side = |title: &'static str, text: &str, lit: bool| {
        let several = text.trim_end().contains('\n');
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(220.))
            .gap_2()
            .p_4()
            .rounded(px(12.))
            .border_1()
            .border_color(if lit { gold() } else { theme::border() })
            .bg(theme::raised())
            .child(div().font_family(theme::MONO_FONT).text_size(px(11.)).font_weight(FontWeight::MEDIUM).text_color(if lit { gold() } else { theme::muted() }).child(title))
            .child(text_lines(text).text_color(theme::text()).when(several, |d| d.font_family(theme::MONO_FONT).text_size(px(12.5)).line_height(px(20.))).when(!several, |d| d.text_size(px(14.)).line_height(px(21.))))
    };
    let last = shown.steps > 1 && shown.at + 1 == shown.steps;
    div()
        .flex()
        .flex_wrap()
        .items_start()
        .gap_3()
        .child(side("BEFORE", &scene.before, shown.steps > 1 && shown.at == 0))
        .child(side("AFTER", &scene.after, last))
        .into_any_element()
}

/// The scene at `step` (everything up to that step shown, the step's own parts lit).
pub fn render_scene(scene: &Scene, step: usize) -> AnyElement {
    let shown = shown(scene, step);
    let picture = match scene.kind {
        SceneKind::Flow => flow(scene, &shown),
        SceneKind::Sequence => sequence(scene, &shown),
        SceneKind::Code => code(scene, &shown),
        SceneKind::Compare => compare(scene, &shown),
    };
    div()
        .flex()
        .flex_col()
        .min_w_0()
        .gap_3p5()
        .child(picture)
        .when(!shown.say.trim().is_empty(), |d| d.child(div().max_w(px(620.)).text_size(px(16.)).line_height(px(23.)).text_color(theme::text()).child(shown.say.clone())))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use glasscode_core::manager::SceneNode;

    fn node(id: &str) -> SceneNode {
        SceneNode { id: id.into(), label: id.to_uppercase(), note: String::new() }
    }

    fn edge(from: &str, to: &str) -> SceneEdge {
        SceneEdge { from: from.into(), to: to.into(), label: String::new(), bad: false }
    }

    fn step(nodes: &[&str], edges: &[usize]) -> SceneStep {
        SceneStep { nodes: nodes.iter().map(|n| n.to_string()).collect(), edges: edges.to_vec(), ..SceneStep::default() }
    }

    fn relay() -> Scene {
        Scene {
            nodes: vec![node("phone"), node("relay"), node("mac")],
            edges: vec![edge("phone", "relay"), edge("relay", "mac"), edge("mac", "phone"), edge("mac", "relay")],
            steps: vec![step(&["phone"], &[]), step(&["phone", "relay"], &[0]), step(&["phone", "relay"], &[0, 1, 2, 3, 9])],
            ..Scene::default()
        }
    }

    #[test]
    fn a_step_shows_what_was_revealed_so_far_and_marks_what_is_new() {
        let scene = relay();
        let first = shown(&scene, 0);
        assert_eq!((first.nodes.clone(), first.edges.clone()), (vec![0], vec![]));
        assert!(first.new_nodes.is_empty(), "nothing is new on the first step");

        let second = shown(&scene, 1);
        assert_eq!((second.nodes.clone(), second.edges.clone()), (vec![0, 1], vec![0]));
        assert_eq!((second.new_nodes.clone(), second.new_edges.clone()), (vec![1], vec![0]));

        // An edge brings its nodes along, an index out of range is ignored, and a step past
        // the end is the last one.
        let last = shown(&scene, 7);
        assert_eq!(last.nodes, vec![0, 1, 2]);
        assert_eq!(last.edges, vec![0, 1, 2, 3]);
        assert_eq!(last.new_nodes, vec![2]);
        assert_eq!((last.at, last.steps), (2, 3));
    }

    #[test]
    fn a_scene_without_steps_shows_everything() {
        let mut scene = relay();
        scene.steps.clear();
        let all = shown(&scene, 3);
        assert_eq!((all.nodes.len(), all.edges.len()), (3, 4));
        assert!(all.new_nodes.is_empty() && all.new_edges.is_empty() && all.focus.is_none());
    }

    #[test]
    fn the_focus_is_the_node_the_step_names() {
        let mut scene = relay();
        scene.steps[1].focus = Some("relay".into());
        assert_eq!(shown(&scene, 1).focus, Some(1));
        scene.steps[1].focus = Some("nobody".into());
        assert_eq!(shown(&scene, 1).focus, None);
    }

    #[test]
    fn arrows_between_neighbours_are_drawn_between_them_and_the_rest_listed() {
        let scene = relay();
        let (gaps, rest) = between(&scene, &shown(&scene, 2));
        assert_eq!(gaps, vec![vec![(0, true)], vec![(1, true), (3, false)]]);
        assert_eq!(rest, vec![2], "mac → phone skips a box");
    }

    #[test]
    fn a_node_without_a_label_goes_by_its_id() {
        let mut scene = relay();
        scene.nodes[0].label = " ".into();
        assert_eq!(name(&scene, "phone"), "phone");
        assert_eq!(name(&scene, "relay"), "RELAY");
        assert_eq!(name(&scene, "ghost"), "ghost");
    }
}
