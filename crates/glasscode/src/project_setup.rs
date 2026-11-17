//! A project's `.glasscode/settings.json` in the app: each project's file as last read (in the
//! background), and the setup script of a new worktree.
//!
//! The setup comes with the repository, so it never runs before the person approved that exact
//! command: the first session in a new worktree shows it and asks Run, Skip this time, or Never
//! for this project, and its agent waits. An approved command (its hash in
//! `State.setup_approvals`) runs without asking; a changed one asks again. It runs in its own
//! terminal in the worktree, as the person's shell's command line, and the agent starts when it
//! ends, whatever its exit code. A setup that does not end can be stopped from the session.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use glasscode_core::model::{AgentKind, Session};
use glasscode_core::project_settings::{self, Decision, Loaded, Source};
use gpui::{
    AnyElement, ClickEvent, Context, Entity, FontWeight, SharedString, Subscription, Window, div,
    prelude::*,
};

use crate::backdrop;
use crate::terminal::{SpawnSpec, TerminalEvent, TerminalView};
use crate::theme;
use crate::ui;
use crate::workspace::Workspace;

/// The last lines of a setup's output that the session keeps.
const OUTPUT_LINES: usize = 40;

/// Each project's file, the warnings shown, and the setup of each session that has one.
#[derive(Default)]
pub struct ProjectFiles {
    /// By project folder, as last read.
    pub files: HashMap<PathBuf, Loaded>,
    loading: bool,
    /// The warnings shown (folder, text): each one once while Glasscode runs.
    warned: HashSet<(PathBuf, String)>,
    /// By session.
    setups: HashMap<String, Setup>,
}

enum Setup {
    /// The question; the agent waits.
    Asking {
        root: PathBuf,
        command: String,
        changed: bool,
    },
    /// The command runs in its own terminal; the agent starts when it ends.
    Running {
        command: String,
        terminal: Entity<TerminalView>,
        _exit: Subscription,
        stopping: bool,
    },
    /// It ended, or did not run: a line in the session says so.
    Ended {
        command: String,
        outcome: Outcome,
        output: String,
        open: bool,
    },
}

#[derive(Clone, Debug, PartialEq)]
enum Outcome {
    Exit(Option<i32>),
    Stopped,
    Skipped,
    Never,
    NotStarted(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Run,
    Skip,
    Never,
}

/// The last `n` lines with text in them.
fn last_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let end = lines
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map_or(0, |i| i + 1);
    lines[end.saturating_sub(n)..end].join("\n")
}

/// The name of the shell a setup runs with ("pwsh", "powershell", "zsh").
pub fn shell_name() -> String {
    let program = glasscode_core::sys::default_shell().program;
    Path::new(&program)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or(program)
}

/// How a source reads in Settings.
pub fn source_words(source: Source) -> &'static str {
    match source {
        Source::Project => "from this project's file",
        Source::Personal => "your setting",
        Source::Glasscode => "Glasscode's default",
    }
}

impl Workspace {
    // ----- the file ----------------------------------------------------------------------------

    /// Read each project's file in the background: at launch, when Settings opens its projects,
    /// never in a render or a tick. A change shows; the same values draw nothing again.
    pub(crate) fn project_files_refresh(&mut self, cx: &mut Context<Self>) {
        if self.project_files.loading {
            return;
        }
        self.project_files.loading = true;
        let roots: Vec<PathBuf> = self.state.projects.iter().map(|p| p.root.clone()).collect();
        let task = cx.background_spawn(async move {
            roots
                .into_iter()
                .map(|root| (project_settings::load(&root), root))
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let read = task.await;
            let _ = this.update(cx, |this, cx| {
                this.project_files.loading = false;
                let read: HashMap<PathBuf, Loaded> = read
                    .into_iter()
                    .map(|(loaded, root)| (root, loaded))
                    .collect();
                if read != this.project_files.files {
                    this.project_files.files = read;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// A file read for a new session: kept, and what is wrong in it said once, in a toast.
    pub(crate) fn project_file_read(
        &mut self,
        root: &Path,
        loaded: Loaded,
        cx: &mut Context<Self>,
    ) {
        let new: Vec<String> = loaded
            .warnings
            .iter()
            .filter(|w| {
                self.project_files
                    .warned
                    .insert((root.to_path_buf(), (*w).clone()))
            })
            .cloned()
            .collect();
        if !new.is_empty() {
            let name = self
                .state
                .projects
                .iter()
                .find(|p| p.root == root)
                .map(|p| p.name.clone())
                .unwrap_or_else(|| crate::settings_ui::tilde(root));
            self.show_toast(
                format!("{name}: .glasscode/settings.json {}.", new.join("; ")),
                cx,
            );
        }
        if self.project_files.files.get(root) != Some(&loaded) {
            self.project_files.files.insert(root.to_path_buf(), loaded);
            cx.notify();
        }
    }

    /// A project's file as last read, if it has one.
    pub(crate) fn project_file(&self, project_id: &str) -> Option<&Loaded> {
        let root = &self.state.project(project_id)?.root;
        self.project_files.files.get(root)
    }

    /// The agent of a new session when none is chosen: the project's `default_agent`, else for a
    /// ticket the agent Settings › Integrations › Linear names, else Claude.
    pub(crate) fn default_agent(&self, project_id: &str, for_ticket: bool) -> (AgentKind, Source) {
        let project = self
            .project_file(project_id)
            .and_then(|l| l.settings.default_agent);
        let personal = if for_ticket {
            self.state.linear.agent
        } else {
            None
        };
        project_settings::in_effect(project, personal, AgentKind::Claude)
    }

    // ----- the setup ---------------------------------------------------------------------------

    /// Whether the agent of this session waits for a setup (`ensure_terminal` asks first). The
    /// first session in a worktree that `git::add_worktree` just made takes its setup: it runs
    /// when this exact command was approved, else the session asks.
    pub(crate) fn setup_first(
        &mut self,
        session: &Session,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if matches!(
            self.project_files.setups.get(&session.id),
            Some(Setup::Asking { .. } | Setup::Running { .. })
        ) {
            return true;
        }
        let Some(new) = project_settings::take_new_worktree(&session.worktree) else {
            return false;
        };
        self.project_file_read(&new.root, new.loaded.clone(), cx);
        // The command as this one read gave it: the same text is shown, hashed and run.
        let Some(command) = new.loaded.settings.setup else {
            return false;
        };
        let key = project_settings::approval_key(&new.root);
        match project_settings::decide(self.state.setup_approvals.get(&key), &command) {
            Decision::Run => self.setup_run(&session.id, command, window, cx),
            Decision::Ask { changed } => {
                // Started in the background: the question waits in the session.
                if self.state.active_session.as_deref() != Some(session.id.as_str()) {
                    self.show_toast(
                        format!(
                            "“{}” waits for you: its project's setup script needs an answer.",
                            session.title
                        ),
                        cx,
                    );
                }
                self.project_files.setups.insert(
                    session.id.clone(),
                    Setup::Asking {
                        root: new.root,
                        command,
                        changed,
                    },
                );
                cx.notify();
                true
            }
            Decision::Never => {
                self.project_files.setups.insert(
                    session.id.clone(),
                    Setup::Ended {
                        command,
                        outcome: Outcome::Never,
                        output: String::new(),
                        open: false,
                    },
                );
                false
            }
        }
    }

    /// The person's answer to the question; then the setup runs, or the agent starts.
    pub(crate) fn setup_answer(
        &mut self,
        session_id: &str,
        answer: Answer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(Setup::Asking { root, command, .. }) = self.project_files.setups.get(session_id)
        else {
            return;
        };
        let (key, command) = (project_settings::approval_key(root), command.clone());
        match answer {
            Answer::Run => {
                self.state
                    .setup_approvals
                    .insert(key, project_settings::approved(&command));
                self.save();
                if self.setup_run(session_id, command, window, cx) {
                    cx.notify();
                    return;
                }
            }
            Answer::Skip => {
                self.project_files.setups.insert(
                    session_id.to_string(),
                    Setup::Ended {
                        command,
                        outcome: Outcome::Skipped,
                        output: String::new(),
                        open: false,
                    },
                );
            }
            Answer::Never => {
                self.state
                    .setup_approvals
                    .insert(key, project_settings::never());
                self.save();
                self.project_files.setups.insert(
                    session_id.to_string(),
                    Setup::Ended {
                        command,
                        outcome: Outcome::Never,
                        output: String::new(),
                        open: false,
                    },
                );
            }
        }
        self.ensure_terminal(session_id, window, cx);
        cx.notify();
    }

    /// Start the command in a terminal of its own in the session's worktree: the person's shell
    /// with the command as its command line, nothing hidden and nothing elevated. False when it
    /// could not start (the agent then starts at once).
    fn setup_run(
        &mut self,
        session_id: &str,
        command: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(session) = self.state.session(session_id).cloned() else {
            return false;
        };
        let shell = glasscode_core::sys::default_shell();
        let mut args = shell.command_args.clone();
        args.push(command.clone());
        // The person's environment, not the session's: the setup is not the agent and does not
        // act as it with the glasscode command.
        let mut env: HashMap<String, String> = (*self.env).clone();
        env.insert(
            "GLASSCODE_WORKTREE".into(),
            session.worktree.to_string_lossy().into_owned(),
        );
        env.insert(
            "COLORFGBG".into(),
            if theme::is_light() { "0;15" } else { "15;0" }.into(),
        );
        let spec = SpawnSpec {
            program: shell.program,
            args,
            cwd: session.worktree.clone(),
            env,
        };
        match TerminalView::spawn(spec, cx) {
            Ok(terminal) => {
                let id = session_id.to_string();
                let exit = cx.subscribe_in(
                    &terminal,
                    window,
                    move |this, _, event: &TerminalEvent, window, cx| {
                        if let TerminalEvent::Exited(code) = event {
                            this.setup_ended(&id, Some(*code), window, cx);
                        }
                    },
                );
                self.project_files.setups.insert(
                    session_id.to_string(),
                    Setup::Running {
                        command,
                        terminal,
                        _exit: exit,
                        stopping: false,
                    },
                );
                true
            }
            Err(err) => {
                self.project_files.setups.insert(
                    session_id.to_string(),
                    Setup::Ended {
                        command,
                        outcome: Outcome::NotStarted(err.to_string()),
                        output: String::new(),
                        open: true,
                    },
                );
                false
            }
        }
    }

    /// The setup ended (`code`), or was stopped (None): keep its last lines, start the agent.
    fn setup_ended(
        &mut self,
        session_id: &str,
        code: Option<Option<i32>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(Setup::Running {
            command,
            terminal,
            stopping,
            ..
        }) = self.project_files.setups.remove(session_id)
        else {
            return;
        };
        let output = last_lines(&terminal.read(cx).text_tail(400), OUTPUT_LINES);
        let outcome = match code {
            Some(code) if !stopping => Outcome::Exit(code),
            _ => Outcome::Stopped,
        };
        let open = outcome != Outcome::Exit(Some(0));
        self.project_files.setups.insert(
            session_id.to_string(),
            Setup::Ended {
                command,
                outcome,
                output,
                open,
            },
        );
        // Dropping it ends what still runs there, as closing a session does.
        drop(terminal);
        self.ensure_terminal(session_id, window, cx);
        cx.notify();
    }

    /// Stop a setup that runs: Ctrl+C, as at the keyboard; if it has not ended after three
    /// seconds, its terminal is closed. Then the agent starts.
    pub(crate) fn setup_stop(
        &mut self,
        session_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(Setup::Running {
            terminal, stopping, ..
        }) = self.project_files.setups.get_mut(session_id)
        else {
            return;
        };
        if *stopping {
            return;
        }
        *stopping = true;
        terminal.update(cx, |t, cx| t.write(b"\x03".to_vec(), cx));
        let id = session_id.to_string();
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(3)).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.setup_ended(&id, None, window, cx)
            });
        })
        .detach();
        cx.notify();
    }

    /// A session closes: a question or a run of its setup goes with it.
    pub(crate) fn setup_forget(&mut self, session_id: &str) {
        self.project_files.setups.remove(session_id);
    }

    // ----- in the session ----------------------------------------------------------------------

    /// The session's column while its setup asks or runs (in place of the agent's); a line about
    /// a setup that ended goes into `top`, under the header.
    pub(crate) fn render_setup(
        &mut self,
        session: &Session,
        top: &mut Vec<AnyElement>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let agent = session.agent.name();
        let column = |top: Vec<AnyElement>| {
            backdrop::glass_strong(theme::sz(ui::CARD_RADIUS))
                .when(theme::is_light(), |d| d.shadow_md())
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .children(top)
        };
        match self.project_files.setups.get(&session.id)? {
            Setup::Asking {
                root,
                command,
                changed,
            } => {
                let project = self
                    .state
                    .project(&session.project_id)
                    .map(|p| p.name.clone())
                    .unwrap_or_default();
                let file = crate::settings_ui::tilde(&project_settings::file(root));
                let (run, skip, never) =
                    (session.id.clone(), session.id.clone(), session.id.clone());
                let card = div()
                    .flex()
                    .flex_col()
                    .gap(theme::sz(12.))
                    .m(theme::sz(16.))
                    .child(div().text_size(theme::sz(16.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(if *changed {
                        format!("{project}'s setup script changed")
                    } else {
                        format!("{project} has a setup script")
                    }))
                    .child(div().text_color(theme::card_soft_text()).child(format!(
                        "It comes with the repository, in {file}, and runs in this new worktree before {agent} starts. Nothing runs until you choose."
                    )))
                    .child(command_box(command))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(theme::sz(8.))
                            .child(ui::primary_button("setup-run", "Run").on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.setup_answer(&run, Answer::Run, window, cx))))
                            .child(ui::button("setup-skip", "Skip this time").on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.setup_answer(&skip, Answer::Skip, window, cx))))
                            .child(ui::button("setup-never", "Never for this project").on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.setup_answer(&never, Answer::Never, window, cx)))),
                    )
                    .child(div().text_size(theme::sz(12.5)).text_color(theme::card_soft_text()).child(format!(
                        "Run asks again only when the command changes. It runs with {} in {}; its output shows here.",
                        shell_name(),
                        crate::settings_ui::tilde(&session.worktree)
                    )));
                Some(column(std::mem::take(top)).child(card).into_any_element())
            }
            Setup::Running {
                command,
                terminal,
                stopping,
                ..
            } => {
                let stop = session.id.clone();
                let strip = div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(theme::sz(10.))
                    .px(theme::sz(14.))
                    .py(theme::sz(8.))
                    .border_b_1()
                    .border_color(theme::glass_border())
                    .child(
                        div()
                            .flex_none()
                            .text_color(theme::text())
                            .font_weight(FontWeight::MEDIUM)
                            .child(if *stopping {
                                "Stopping the setup…"
                            } else {
                                "Setup is running"
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(theme::sz(120.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .font_family(theme::MONO_FONT)
                            .text_size(theme::sz(12.))
                            .text_color(theme::text_soft())
                            .child(command.clone()),
                    )
                    .when(!*stopping, |d| {
                        d.child(
                            ui::button("setup-stop", format!("Stop and start {agent}")).on_click(
                                cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    this.setup_stop(&stop, window, cx)
                                }),
                            ),
                        )
                    });
                let output = div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(theme::sz(80.))
                    .m(theme::sz(8.))
                    .p(theme::sz(6.))
                    .rounded(theme::sz(10.))
                    .border_1()
                    .border_color(theme::border())
                    .bg(theme::c(theme::term().background))
                    .child(div().flex_1().min_h_0().child(terminal.clone()));
                Some(
                    column(std::mem::take(top))
                        .child(strip)
                        .child(output)
                        .into_any_element(),
                )
            }
            Setup::Ended {
                command,
                outcome,
                output,
                open,
            } => {
                let (said, failed) = match outcome {
                    Outcome::Exit(Some(0)) => ("Setup ran (exit 0).".to_string(), false),
                    Outcome::Exit(Some(code)) => (format!("Setup failed (exit {code}); {agent} started anyway."), true),
                    Outcome::Exit(None) => (format!("Setup ended without an exit code; {agent} started anyway."), true),
                    Outcome::Stopped => (format!("Setup was stopped; {agent} started."), true),
                    Outcome::Skipped => ("Setup skipped this time.".to_string(), false),
                    Outcome::Never => ("Setup not run: Never for this project (Settings › Git and projects can ask again).".to_string(), false),
                    Outcome::NotStarted(why) => (format!("Setup could not start: {why}"), true),
                };
                let (toggle, close) = (session.id.clone(), session.id.clone());
                let has_output = !output.is_empty();
                let line = div()
                    .flex()
                    .flex_col()
                    .gap(theme::sz(6.))
                    .px(theme::sz(14.))
                    .py(theme::sz(7.))
                    .border_b_1()
                    .border_color(theme::glass_border())
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(theme::sz(10.))
                            .child(ui::dot(if failed {
                                theme::removed()
                            } else {
                                theme::added()
                            }))
                            .child(
                                div()
                                    .text_size(theme::sz(12.5))
                                    .text_color(theme::text())
                                    .child(said),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(theme::sz(80.))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .font_family(theme::MONO_FONT)
                                    .text_size(theme::sz(12.))
                                    .text_color(theme::card_soft_text())
                                    .child(command.clone()),
                            )
                            .when(has_output, |d| {
                                d.child(
                                    ui::button(
                                        "setup-output",
                                        if *open { "Hide output" } else { "Show output" },
                                    )
                                    .on_click(cx.listener(
                                        move |this, _: &ClickEvent, _, cx| {
                                            if let Some(Setup::Ended { open, .. }) =
                                                this.project_files.setups.get_mut(&toggle)
                                            {
                                                *open = !*open;
                                            }
                                            cx.notify();
                                        },
                                    )),
                                )
                            })
                            .child(ui::button("setup-dismiss", "Dismiss").on_click(cx.listener(
                                move |this, _: &ClickEvent, _, cx| {
                                    this.project_files.setups.remove(&close);
                                    cx.notify();
                                },
                            ))),
                    )
                    .when(*open && has_output, |d| d.child(command_box(output)));
                top.push(line.into_any_element());
                None
            }
        }
    }
}

/// Text as it is, in the code font, on an opaque ground: a command, or a setup's output.
fn command_box(text: &str) -> AnyElement {
    div()
        .px(theme::sz(12.))
        .py(theme::sz(9.))
        .rounded(theme::sz(8.))
        .border_1()
        .border_color(theme::border())
        .bg(theme::input())
        .font_family(theme::MONO_FONT)
        .text_size(theme::sz(12.5))
        .text_color(theme::text())
        .child(SharedString::from(text.to_string()))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_lines_with_text() {
        assert_eq!(last_lines("a\nb\nc\n\n  \n", 2), "b\nc");
        assert_eq!(last_lines("", 5), "");
        assert_eq!(last_lines("\n\n", 5), "");
        assert_eq!(last_lines("only", 5), "only");
    }
}
