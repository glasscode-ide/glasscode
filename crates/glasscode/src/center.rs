//! What the central pane shows: a local session, or a session running on a remote server
//! (reached over SSH, Tailscale or a self-hosted relay).

/// The central-pane selection.
#[derive(Clone, PartialEq)]
pub enum Center {
    Session,
    /// A session running on the developer's server.
    Server(String),
}

/// Open a URL in the person's default browser. It starts from a task of its own, after the caller
/// (a click handler) returns: on Windows the browser starts through `ShellExecuteW`, which can run
/// the window's messages, and a GPUI task that ran inside them while the click held the app
/// panicked ("RefCell already borrowed"; seen on the status bar's Open, U17). GPUI defers its own
/// restart in the same way.
pub fn open_url(url: &str, cx: &gpui::App) {
    let url = url.to_string();
    cx.spawn(async move |_| {
        if let Err(error) = glasscode_core::sys::open_url(&url) {
            log::warn!("could not open the link: {error}");
        }
    })
    .detach();
}

/// Start something outside Glasscode (a folder or a file in the system's program for it, a file
/// shown in its folder, an editor) from a task of its own, as `open_url` does and for the same
/// reason: a click handler, and an agent's command, hold the app while `ShellExecuteW` or a new
/// process runs the window's messages. `done` gets the result in the workspace: a toast, or the
/// reply to the command.
pub fn start_outside(
    start: impl FnOnce() -> std::io::Result<()> + 'static,
    done: impl FnOnce(&mut crate::workspace::Workspace, std::io::Result<()>, &mut gpui::Context<crate::workspace::Workspace>) + 'static,
    cx: &mut gpui::Context<crate::workspace::Workspace>,
) {
    cx.spawn(async move |this, cx| {
        let result = start();
        let _ = this.update(cx, |ws, cx| done(ws, result, cx));
    })
    .detach();
}

/// [`start_outside`] whose failure is a toast that starts with `failed` ("Could not open the
/// editor").
pub fn start_or_toast(failed: impl Into<String>, start: impl FnOnce() -> std::io::Result<()> + 'static, cx: &mut gpui::Context<crate::workspace::Workspace>) {
    let failed = failed.into();
    start_outside(
        start,
        move |ws, result, cx| {
            if let Err(error) = result {
                log::warn!("{failed}: {error}");
                ws.show_toast(format!("{failed}: {error}"), cx);
            }
        },
        cx,
    );
}
