import MWDATDisplay

/// The lens's cards (G12, the plan's section 5), drawn with the toolkit's FlexBox: 600 by 600,
/// redrawn whole on each change. Short words, one thing at a time; the focused row or button has
/// the card background. This file imports only MWDATDisplay, so `Text` is the toolkit's.
enum LensCards {
    static func render(_ model: LensModel, voice: String) -> FlexBox {
        let focus = model.focus
        switch model.card {
        case .glance: return glance(model.glance, focus: focus, voice: voice)
        case .listening(let heard): return listening(heard)
        case .working(let lines): return working(lines)
        case .answer(let text, let chips): return answer(text, chips: chips, focus: focus, voice: voice)
        case .session(let s): return session(s, chips: model.sessionChips(s), focus: focus)
        case .pr(let pr): return pullRequest(pr, focus: focus)
        case .points(let title, let points, let at): return explanation(title, points: points, at: at)
        case .approval(let a): return approval(a, focus: focus)
        case .question(let q): return question(q, focus: focus)
        case .note(let title, let body, let tone, let open): return note(title, body, tone: tone, open: open != nil, focused: focus == 0)
        case .message(let title, let body): return message(title, body)
        }
    }

    // MARK: cards

    static func glance(_ g: LensModel.Glance, focus: Int, voice: String) -> FlexBox {
        FlexBox(direction: .column, spacing: 12) {
            head(name: g.name, place: g.place)
            Text(g.line, style: .body)
            for (i, row) in g.rows.prefix(3).enumerated() {
                item(title: row.title, detail: row.detail, icon: icon(row.tone), focused: focus == i)
            }
            hint(voice == "speaking" ? "Pinch to stop" : "Pinch to talk · swipe ▼ to a row")
        }
    }

    static func listening(_ heard: String) -> FlexBox {
        FlexBox(direction: .column, spacing: 14) {
            FlexBox(direction: .row, spacing: 10, crossAlignment: .center) {
                Icon(name: .threeDotSpeechBubble)
                Text("Listening", style: .heading)
            }
            Text(heard.isEmpty ? "Say what you want…" : clip(heard, 220), style: .body, color: heard.isEmpty ? .secondary : .primary)
            hint("A pause sends it · pinch sends now · swipe ◀ cancels")
        }
    }

    static func working(_ lines: [String]) -> FlexBox {
        FlexBox(direction: .column, spacing: 10) {
            FlexBox(direction: .row, spacing: 10, crossAlignment: .center) {
                Icon(name: .threeDotsHorizontal)
                Text("Thinking", style: .heading)
            }
            for (i, line) in lines.suffix(4).enumerated() {
                FlexBox(direction: .row, spacing: 8, crossAlignment: .center) {
                    Icon(name: i == min(lines.count, 4) - 1 ? .clock : .checkmark, style: .outline)
                    Text(clip(line, 70), style: .meta, color: .secondary)
                }
            }
            hint("Pinch to stop and talk")
        }
    }

    static func answer(_ text: String, chips: [LensModel.Chip], focus: Int, voice: String) -> FlexBox {
        FlexBox(direction: .column, spacing: 12) {
            FlexBox(direction: .row, spacing: 10, crossAlignment: .center) {
                Icon(name: voice == "speaking" ? .speakerWithThreeArcs : .speechBubble)
                Text(voice == "speaking" ? "Speaking" : "Answer", style: .meta, color: .secondary)
            }
            Text(clip(text, 300), style: .body)
            if !chips.isEmpty {
                FlexBox(direction: .row, spacing: 8, wrap: true) {
                    for (i, chip) in chips.prefix(3).enumerated() {
                        pill(chip.label, focused: focus == i)
                    }
                }
            }
            hint(chips.isEmpty ? "Pinch to talk" : "Swipe ▼ to the links · pinch to talk")
        }
    }

    static func session(_ s: LensModel.SessionCard, chips: [LensModel.Chip], focus: Int) -> FlexBox {
        FlexBox(direction: .column, spacing: 10) {
            Text("Session", style: .meta, color: .secondary)
            Text(clip(s.title, 40), style: .heading)
            Text("\(s.branch) · \(s.agent)", style: .meta, color: .secondary)
            FlexBox(direction: .row, spacing: 8, crossAlignment: .center) {
                Icon(name: icon(s.tone), style: .outline)
                Text(s.state, style: .meta)
            }
            if let question = s.question {
                Text("Asks: " + clip(question, 120), style: .body)
            } else if !s.lastReply.isEmpty {
                Text(clip(s.lastReply, 160), style: .body)
            }
            if !chips.isEmpty {
                FlexBox(direction: .row, spacing: 8, wrap: true) {
                    for (i, chip) in chips.enumerated() { pill(chip.label, focused: focus == i) }
                }
            }
            hint("Pinch to talk about it · swipe ◀ back")
        }
    }

    static func pullRequest(_ pr: LensModel.PRCard, focus: Int) -> FlexBox {
        FlexBox(direction: .column, spacing: 10) {
            Text("Pull request #\(pr.number)", style: .meta, color: .secondary)
            Text(clip(pr.title, 70), style: .heading)
            for check in pr.checks.prefix(5) {
                FlexBox(direction: .row, spacing: 8, crossAlignment: .center) {
                    Icon(name: check.passed == true ? .checkmark : check.passed == false ? .x : .clock, style: .outline)
                    Text(clip(check.name, 40), style: .meta)
                }
            }
            if !pr.state.isEmpty { Text(pr.state, style: .meta, color: .secondary) }
            FlexBox(direction: .row, spacing: 8) {
                pill("Explain", focused: focus == 0)
                if pr.session != nil { pill("Its session", focused: focus == 1) }
            }
            hint("Pinch to talk about it · swipe ◀ back")
        }
    }

    static func explanation(_ title: String, points: [String], at: Int) -> FlexBox {
        FlexBox(direction: .column, spacing: 12) {
            Text(title, style: .meta, color: .secondary)
            Text("Point \(at + 1) of \(points.count)", style: .meta, color: .secondary)
            Text(points.indices.contains(at) ? clip(points[at], 260) : "", style: .body)
            hint(at + 1 < points.count ? "Swipe ▼ next · ▲ back · more on the phone" : "Swipe ▲ back · pinch to ask about it")
        }
    }

    static func approval(_ a: LensModel.ApprovalCard, focus: Int) -> FlexBox {
        FlexBox(direction: .column, spacing: 10) {
            Text("Needs your OK · \(a.who)", style: .meta, color: .secondary)
            Text(clip(a.what, 80), style: .heading)
            if a.fits {
                FlexBox(direction: .column) { Text(a.payload, style: .body) }.padding(10).background(.card)
            } else {
                Text("Too long for the lens: read it on the phone to allow it.", style: .meta, color: .secondary)
            }
            FlexBox(direction: .row, spacing: 10) {
                if a.fits { pill("Allow", focused: focus == 0) }
                pill("Deny", focused: focus == 1)
            }
            hint("Signed by this iPhone. Saying “yes” does nothing.")
        }
    }

    static func question(_ q: LensModel.QuestionCard, focus: Int) -> FlexBox {
        FlexBox(direction: .column, spacing: 10) {
            Text("\(clip(q.title, 40)) asks", style: .meta, color: .secondary)
            Text(clip(q.question, 120), style: .heading)
            for (i, option) in q.options.prefix(4).enumerated() {
                item(title: "\(option.number) · \(clip(option.label, 50))", detail: "", icon: nil, focused: focus == i)
            }
            hint("“Allow for good” is only on your Mac")
        }
    }

    static func note(_ title: String, _ body: String, tone: LensModel.Tone, open: Bool, focused: Bool) -> FlexBox {
        FlexBox(direction: .column, spacing: 12) {
            FlexBox(direction: .row, spacing: 10, crossAlignment: .center) {
                Icon(name: icon(tone))
                Text(clip(title, 50), style: .heading)
            }
            Text(clip(body, 200), style: .body, color: .secondary)
            if open { pill("Open", focused: focused) }
            hint(open ? "Swipe ▼ and pinch to open · swipe ◀ to let it go" : "Swipe ◀ to let it go")
        }
    }

    static func message(_ title: String, _ body: String) -> FlexBox {
        FlexBox(direction: .column, spacing: 8) {
            Text(title, style: .heading)
            Text(body, style: .body, color: .secondary)
        }
        .padding(20)
        .background(.card)
    }

    /// Near blank: the lens stays ours, showing nothing.
    static func blank() -> FlexBox {
        FlexBox(direction: .column) { Text(" ", style: .meta, color: .secondary) }
    }

    // MARK: parts

    private static func head(name: String, place: String) -> FlexBox {
        FlexBox(direction: .row, spacing: 10, crossAlignment: .center) {
            Icon(name: .smartGlasses)
            Text(name, style: .heading)
            Text("· " + place, style: .meta, color: .secondary)
        }
    }

    private static func item(title: String, detail: String, icon: IconName?, focused: Bool) -> FlexBox {
        FlexBox(direction: .row, spacing: 10, crossAlignment: .center) {
            if let icon { Icon(name: icon, style: .outline) }
            FlexBox(direction: .column, spacing: 2) {
                Text(clip(title, 46), style: .body)
                if !detail.isEmpty { Text(clip(detail, 60), style: .meta, color: .secondary) }
            }
        }
        .padding(10)
        .background(focused ? .card : .none)
    }

    private static func pill(_ label: String, focused: Bool) -> FlexBox {
        FlexBox(direction: .row) { Text((focused ? "▸ " : "") + label, style: .meta) }
            .padding(10)
            .background(focused ? .card : .none)
    }

    private static func hint(_ text: String) -> FlexBox {
        FlexBox(direction: .column) { Text(text, style: .meta, color: .secondary) }
    }

    private static func icon(_ tone: LensModel.Tone) -> IconName {
        switch tone {
        case .ok: return .checkmarkCircle
        case .warn: return .exclamationCircle
        case .bad: return .exclamationTriangle
        case .plain: return .bell
        }
    }

    static func clip(_ text: String, _ limit: Int) -> String {
        let flat = text.replacingOccurrences(of: "\n", with: " ")
        return flat.count <= limit ? flat : String(flat.prefix(limit - 1)) + "…"
    }
}
