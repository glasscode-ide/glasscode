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
    if let Err(error) = glasscode_core::sys::open_url(url) {
        log::warn!("could not open the link: {error}");
    }
}
