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
