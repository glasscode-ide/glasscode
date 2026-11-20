//! The server test in Settings › Manager: whether the server can be the manager's home
//! (`manager.test`, glasscoded's `manager.rs`).
//!
//! v1's copy of the manager on the server, which this Mac kept in step through `manager.sync`,
//! went with M56 (row 6 on the server, row 4 here). The v2 manager moves between this Mac and the
//! server by the home's lease (D5, M57; `home_ui.rs`).

use gpui::{AppContext as _, Context};
use glasscode_core::manager::TestLine;
use glasscode_core::remote::Hello;
use serde_json::json;

use crate::workspace::Workspace;

fn line(what: &str, ok: bool, detail: impl Into<String>) -> TestLine {
    TestLine { what: what.into(), ok, detail: detail.into() }
}

/// Whether the server's daemon is the build this app brings.
fn version_line(hello: &Hello, name: &str) -> TestLine {
    let what = "Same version";
    let mine = glasscode_core::update::BUILD;
    let theirs = glasscode_core::update::describe(&hello.version, hello.build);
    if mine == 0 || hello.build == 0 {
        line(what, true, format!("glasscoded {theirs} on {name}; a development build, so not compared"))
    } else if hello.build == mine {
        line(what, true, format!("glasscoded {theirs}, the same as this app"))
    } else if hello.build < mine {
        line(what, false, format!("glasscoded {theirs} on {name} is older than this app ({mine}). Update it in the Server dialog."))
    } else {
        line(what, true, format!("glasscoded {theirs} on {name} is newer than this app ({mine}). Update Glasscode when you can."))
    }
}

impl Workspace {
    /// Run the server test (Settings › Manager): the lines land in `ManagerUi::test`.
    pub fn manager_test_server(&mut self, cx: &mut Context<Self>) {
        if self.manager.testing {
            return;
        }
        let name = self.server.name();
        let Some(client) = self.server.client.clone().filter(|_| self.server.connected()) else {
            let detail = if self.server.config.is_some() { format!("{name} cannot be reached right now.") } else { "No server is set up yet (Settings › Server & devices).".to_string() };
            self.manager.test = Some(vec![line("Server", false, detail)]);
            cx.notify();
            return;
        };
        let mut lines = vec![line("Server", true, format!("Connected to {name}")), version_line(&client.hello, &name)];
        if !client.hello.features.iter().any(|f| f == "manager") {
            lines.push(line("Manager", false, format!("glasscoded on {name} cannot run the manager yet. Update it in the Server dialog.")));
            self.manager.test = Some(lines);
            cx.notify();
            return;
        }
        self.manager.testing = true;
        let settings = self.state.manager.clone();
        let task = cx.background_spawn(async move { client.call::<Vec<TestLine>>("manager.test", json!({ "settings": settings })) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(more) => lines.extend(more),
                    Err(error) => lines.push(line("Manager", false, format!("{error:#}"))),
                }
                this.manager.testing = false;
                this.manager.test = Some(lines);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}
