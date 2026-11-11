import MWDATDisplay

// Builds the root FlexBox for each screen of the Glasscode browse flow. One `send()`
// per state change; the selected row is the one the neural band is pointing at.
//
// This file imports only MWDATDisplay (no SwiftUI) so the DSL names `Text`/`Button`
// are unambiguous.
enum GlasscodeScreens {
    private static let maxRows = 5

    // MARK: list screens

    static func projects(_ items: [GlasscodeProject], selected: Int) -> FlexBox {
        FlexBox(direction: .column, spacing: 10) {
            crumb("Projects")
            if items.isEmpty {
                hint("No projects yet")
            } else {
                let window = windowed(items.count, selected)
                for i in window {
                    row(prefix: i == selected,
                        title: items[i].name,
                        subtitle: subtitleFor(items[i]),
                        selected: i == selected)
                }
                if items.count > window.count { hint("\(items.count) projects · swipe to scroll") }
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

    static func sessionActions(title: String, context: String, selected: Int) -> FlexBox {
        FlexBox(direction: .column, spacing: 10) {
            crumb(title)
            FlexBox(direction: .column, spacing: 4) {
                Text("Glasscode has this session's context", style: .meta, color: .secondary)
                Text(context, style: .body)
            }
            .padding(14)
            .background(.card)

            action(icon: "🎙", name: "Voice — free", detail: "Speak, it goes to the thread", selected: selected == 0)
            action(icon: "💬", name: "Talk — live", detail: "Discuss with Glasscode", selected: selected == 1)
            footer("swipe ▲▼ choose · ▶ start · ◀ back")
        }
    }

    // MARK: live panes

    static func voice(session: String, listening: Bool, text: String) -> FlexBox {
        FlexBox(direction: .column, spacing: 10) {
            crumb("\(session) ▸ Voice")
            FlexBox(direction: .column, spacing: 6) {
                Text(listening ? "● Listening — free" : "Voice", style: .heading)
                Text(text.isEmpty ? "Speak, then pinch to send to the thread." : text, style: .body)
            }
            .padding(16)
            .background(.card)
            footer(listening ? "swipe ▶ send · ◀ cancel" : "swipe ▶ start · ◀ back")
        }
    }

    static func talk(session: String, line: String, billing: String) -> FlexBox {
        FlexBox(direction: .column, spacing: 10) {
            crumb("\(session) ▸ Talk · \(billing)")
            FlexBox(direction: .column, spacing: 6) {
                Text("Live with Glasscode", style: .heading)
                Text(line.isEmpty ? "Ask about this session, or say \u{201C}jump to…\u{201D}." : line, style: .body)
            }
            .padding(16)
            .background(.card)
            footer("swipe ◀ to hang up")
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

    private static func subtitleFor(_ p: GlasscodeProject) -> String? {
        var parts: [String] = []
        if let repo = p.repo, !repo.isEmpty { parts.append(repo) }
        parts.append("\(p.sessionCount) session\(p.sessionCount == 1 ? "" : "s")")
        return parts.joined(separator: " · ")
    }
}
