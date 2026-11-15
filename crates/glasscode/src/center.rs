//! What the central pane shows: a local session, or a session running on a remote server
//! (reached over SSH, Tailscale or a self-hosted relay).

/// The central-pane selection.
#[derive(Clone, PartialEq)]
pub enum Center {
    Session,
    /// A session running on the developer's server.
    Server(String),
}

/// Open a URL in the person's default browser.
pub fn open_url(url: &str) {
    let _ = glasscode_core::sys::command("open").arg(url).spawn();
}
