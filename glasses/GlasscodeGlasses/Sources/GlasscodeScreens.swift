import MWDATDisplay

// Builds the root FlexBox for each screen of the browse flow (the manager first, then projects). One `send()`
// per state change; the selected row is the one the neural band is pointing at.
//
// This file imports only MWDATDisplay (no SwiftUI) so the DSL names `Text`/`Button`
// are unambiguous.
enum GlasscodeScreens {
    private static let maxRows = 5

    // MARK: list screens

    /// Home: the manager first (talk with it, today's update), then the projects. `notice`
    /// says why the manager is not there when it is not ("Manager is off — …"); `banner` is a
    /// note about the last thing done ("Hung up").
    static func home(crumb title: String, notice: String, banner: String, rows: [GlasscodeRow]) -> FlexBox {
        let selected = rows.firstIndex { $0.selected } ?? 0
        return FlexBox(direction: .column, spacing: 10) {
            crumb(title)
            if !notice.isEmpty { note(notice) }
            if !banner.isEmpty { hint(banner) }
            if rows.isEmpty {
                hint("No projects yet")
            } else {
                let window = windowed(rows.count, selected)
                for i in window {
                    row(prefix: rows[i].selected,
                        title: rows[i].title,
                        subtitle: rows[i].subtitle,
                        selected: rows[i].selected)
                }
                if rows.count > window.count { hint("\(rows.count) rows · swipe to scroll") }
            }
            footer("swipe ▲▼ move · ▶ open")
        }
    }

    static func sessions(project: String, items: [GlasscodeSessionItem], selected: Int) -> FlexBox {
        FlexBox(direction: .column, spacing: 10) {
            crumb("Projects ▸ \(project)")
            if items.isEmpty {
                hint("No sessions in \(project)")
            } else {
                let window = windowed(items.count, selected)
                for i in window {
                    row(prefix: i == selected,
                        title: items[i].title,
                        subtitle: items[i].branch,
                        selected: i == selected)
                }
                if items.count > window.count { hint("\(items.count) sessions · swipe to scroll") }
            }
            footer("swipe ▲▼ move · ▶ open · ◀ back")
        }
    }

    // MARK: session actions (Voice / Talk)

    static func sessionActions(title: String, context: String, talkDetail: String, banner: String, selected: Int) -> FlexBox {
        FlexBox(direction: .column, spacing: 10) {
            crumb(title)
            FlexBox(direction: .column, spacing: 4) {
                Text("Latest in this session", style: .meta, color: .secondary)
                Text(context, style: .body)
            }
            .padding(14)
            .background(.card)

            if !banner.isEmpty { hint(banner) }
            action(icon: "🎙", name: "Voice — free", detail: "Speak, it goes to the thread", selected: selected == 0)
            action(icon: "💬", name: "Talk — live", detail: talkDetail, selected: selected == 1)
            footer("swipe ▲▼ choose · ▶ start · ◀ back")
        }
    }

    // MARK: live panes

    static func voice(session: String, listening: Bool, text: String) -> FlexBox {
        FlexBox(direction: .column, spacing: 10) {
            crumb("\(session) ▸ Voice")
            FlexBox(direction: .column, spacing: 6) {
                Text(listening ? "● Listening — free" : "Voice", style: .heading)
                Text(text.isEmpty ? "Speak, then swipe ▶ to send to the thread." : text, style: .body)
            }
            .padding(16)
            .background(.card)
            footer(listening ? "swipe ▶ send · ◀ cancel" : "swipe ▶ start · ◀ back")
        }
    }

    /// A talk with the manager: what the room is doing (listening / thinking / speaking), what
    /// it heard, and the caption of what the manager says.
    static func managerTalk(crumb title: String, state: String, heard: String, caption: String, foot: String) -> FlexBox {
        FlexBox(direction: .column, spacing: 10) {
            crumb(title)
            FlexBox(direction: .column, spacing: 6) {
                Text(state, style: .heading)
                if !heard.isEmpty { Text(clip(heard, 140), style: .meta, color: .secondary) }
                if !caption.isEmpty { Text(clip(caption, 280), style: .body) }
            }
            .padding(16)
            .background(.card)
            footer(foot)
        }
    }

    /// Today's update: one line at a time ("2 of 7"), with the voice's state under the title.
    static func brief(crumb title: String, title heading: String, state: String, text: String, foot: String) -> FlexBox {
        FlexBox(direction: .column, spacing: 10) {
            crumb(title)
            FlexBox(direction: .column, spacing: 6) {
                Text(heading, style: .heading)
                if !state.isEmpty { Text(state, style: .meta, color: .secondary) }
                Text(clip(text, 280), style: .body)
            }
            .padding(16)
            .background(.card)
            footer(foot)
        }
    }

    // MARK: status

    /// Near-blank "lens off" (desk-mode) screen. Keeps the Display capability attached — so
    /// the band still routes to us and drives the IDE mirror — while the lens shows nothing.
    static func blank() -> FlexBox {
        FlexBox(direction: .column) { Text(" ", style: .meta, color: .secondary) }
    }

    static func message(_ title: String, _ body: String) -> FlexBox {
        FlexBox(direction: .column, spacing: 6) {
            Text(title, style: .heading)
            Text(body, style: .body, color: .secondary)
        }
        .padding(20)
        .background(.card)
    }

    // MARK: building blocks

    private static func crumb(_ text: String) -> FlexBox {
        FlexBox(direction: .column) { Text(text, style: .meta, color: .secondary) }
    }

    private static func footer(_ text: String) -> FlexBox {
        FlexBox(direction: .column) { Text(text, style: .meta, color: .secondary) }
    }

    private static func hint(_ text: String) -> FlexBox {
        FlexBox(direction: .column) { Text(text, style: .meta, color: .secondary) }.padding(6)
    }

    /// A plain sentence in a card of its own (why the manager is not there).
    private static func note(_ text: String) -> FlexBox {
        FlexBox(direction: .column) { Text(text, style: .meta, color: .secondary) }
            .padding(12)
            .background(.card)
    }

    /// The lens has room for a few lines: keep the start of a long text.
    private static func clip(_ text: String, _ limit: Int) -> String {
        text.count <= limit ? text : String(text.prefix(limit - 1)) + "…"
    }

    private static func row(prefix selected: Bool, title: String, subtitle: String?, selected rowSelected: Bool) -> FlexBox {
        FlexBox(direction: .column, spacing: 2) {
            Text((rowSelected ? "▸ " : "  ") + title, style: .body)
            if let subtitle, !subtitle.isEmpty {
                Text("   " + subtitle, style: .meta, color: .secondary)
            }
        }
        .padding(12)
        .background(rowSelected ? .card : .card)   // selected rows read brighter via the ▸ marker
    }

    private static func action(icon: String, name: String, detail: String, selected: Bool) -> FlexBox {
        FlexBox(direction: .column, spacing: 2) {
            Text((selected ? "▸ " : "  ") + icon + "  " + name, style: .body)
            Text("   " + detail, style: .meta, color: .secondary)
        }
        .padding(14)
        .background(.card)
    }

    // Keep the selected index inside a window of at most `maxRows`.
    private static func windowed(_ count: Int, _ selected: Int) -> [Int] {
        if count <= maxRows { return Array(0..<count) }
        var start = selected - maxRows / 2
        if start < 0 { start = 0 }
        if start + maxRows > count { start = count - maxRows }
        return Array(start..<(start + maxRows))
    }
}
