import Foundation

/// What the lens shows and how the Neural Band moves through it (G12, the plan's section 5).
/// Pure: the app feeds it what it knows and the band's gestures, and does the effects it returns.
///
/// - **Pinch**: talk, from the glance and from any card; on a focused row or button, that one;
///   while the manager speaks, stop and listen; while listening, send now.
/// - **Swipe up / down**: move between rows and buttons; in an explanation, the next or the
///   previous point.
/// - **Swipe right**: open the focused row.
/// - **Swipe left**: back one card, or cancel what is being said. On the glance, nothing.
///
/// A place's page (`LensPage`, the Mac's window's place) follows the Mac: it comes when the Mac's
/// window goes somewhere. Down and up move through its rows (on a page without rows, to the next
/// or the previous one of its list); right or a pinch on a row opens it, on the lens and the Mac.
struct LensModel: Equatable {
    enum Gesture { case pinch, up, down, left, right }

    /// A place the lens can open.
    enum Target: Equatable {
        case session(String)
        case pr(Int, session: String?)
        case project(String)
        case today
        case conversation
    }

    struct Chip: Equatable {
        var label: String
        var target: Target
    }

    /// A row of the glance: something that needs the person.
    struct Row: Equatable {
        var title: String
        var detail: String
        var tone: Tone
        var target: Target?
    }

    enum Tone: Equatable { case ok, busy, warn, bad, plain }

    struct SessionCard: Equatable {
        var id: String
        var title: String
        var branch: String
        var agent: String
        var state: String
        var tone: Tone
        var lastReply: String
        var question: String?
        var pr: Int?
    }

    struct PRCard: Equatable {
        var number: Int
        var title: String
        var checks: [(name: String, passed: Bool?)]
        var state: String
        var session: String?

        static func == (a: PRCard, b: PRCard) -> Bool {
            a.number == b.number && a.title == b.title && a.state == b.state && a.session == b.session && a.checks.map(\.name) == b.checks.map(\.name) && a.checks.map(\.passed) == b.checks.map(\.passed)
        }
    }

    struct ApprovalCard: Equatable {
        var id: Int
        var launch: String
        var who: String
        var what: String
        var payload: String
        /// The whole payload fits on the lens: only then may it be allowed here.
        var fits: Bool
    }

    struct QuestionCard: Equatable {
        var session: String
        var title: String
        var question: String
        /// Number and label; never an option that allows for good (left out by the app).
        var options: [(number: Int, label: String)]

        static func == (a: QuestionCard, b: QuestionCard) -> Bool {
            a.session == b.session && a.question == b.question && a.options.map(\.number) == b.options.map(\.number) && a.options.map(\.label) == b.options.map(\.label)
        }
    }

    enum Card: Equatable {
        case glance
        case listening(String)
        case working([String])
        case answer(String, [Chip])
        case session(SessionCard)
        case pr(PRCard)
        case points(title: String, points: [String], at: Int)
        case approval(ApprovalCard)
        case question(QuestionCard)
        case note(title: String, body: String, tone: Tone, open: Target?)
        case message(title: String, body: String)
        /// A place's page: what the Mac's window shows, the lens's version of it.
        case page(LensPage)
    }

    enum Effect: Equatable {
        case none
        case talk
        case sendNow
        case cancelListening
        case stopSpeaking
        case open(Target)
        case ask(String)
        case allow(ApprovalCard)
        case deny(ApprovalCard)
        case answer(session: String, option: Int)
        case back
        /// Open a place (a page's row, the next ticket): on the Mac, and so on the lens.
        case place(String)
    }

    /// The glance's data.
    struct Glance: Equatable {
        var name = "Manager"
        var initials = "M"
        var place = "On your Mac"
        var line = "Connecting…"
        var rows: [Row] = []
    }

    var glance = Glance()
    private(set) var stack: [Card] = [.glance]
    /// The focused row or button of the card on top; -1: none (a pinch talks).
    private(set) var focus = -1

    var card: Card { stack.last ?? .glance }

    /// Put a card on top (what the manager showed, an answer, a note).
    mutating func show(_ card: Card) {
        // The listening and working cards give way to what follows them.
        if let top = stack.last {
            switch top {
            case .listening, .working: stack.removeLast()
            default: break
            }
        }
        if card == .glance {
            stack = [.glance]
        } else if stack.last != card {
            stack.append(card)
        }
        if stack.count > 8 { stack.removeFirst(stack.count - 8) }
        focus = defaultFocus(card)
    }

    /// Change the card on top in place (an answer as it is written, the words as they are heard).
    mutating func replaceTop(_ card: Card) {
        if stack.count > 1 { stack[stack.count - 1] = card } else { show(card) }
    }

    mutating func back() {
        if stack.count > 1 { stack.removeLast() }
        focus = defaultFocus(card)
    }

    private func defaultFocus(_ card: Card) -> Int {
        switch card {
        case .approval(let a): return a.fits ? 0 : 1
        case .question: return 0
        default: return -1
        }
    }

    /// How many rows or buttons the card has.
    var focusables: Int {
        switch card {
        case .glance: return min(glance.rows.count, 3)
        case .answer(_, let chips): return chips.count
        case .session(let s): return sessionChips(s).count
        case .pr: return 2
        case .approval: return 2
        case .question(let q): return q.options.count
        case .note(_, _, _, let open): return open == nil ? 0 : 1
        case .page(let page): return page.items.count
        case .listening, .working, .points, .message: return 0
        }
    }

    /// The Mac's window shows a place: its page on the lens. `opened`: the person opened it from
    /// here (it goes on top, and back returns to where they were); else the Mac went there, and
    /// the page takes the place of the page shown. While the person talks it waits under that card.
    mutating func follow(_ page: LensPage, opened: Bool) {
        if case .page(let shown) = card, shown.place == page.place {
            stack[stack.count - 1] = .page(page)
            focus = min(focus, page.items.count - 1)
            return
        }
        // One copy of a place in the stack: the one it goes to now.
        let top = stack.count - 1
        stack = stack.enumerated().filter { i, card in
            if i == top { return true }
            if case .page(let p) = card, p.place == page.place { return false }
            return true
        }.map(\.element)
        switch card {
        case .listening, .approval, .question:
            stack.insert(.page(page), at: stack.count - 1)
            return
        case .working:
            // What the person asked for (opened by the voice): shown now.
            stack[stack.count - 1] = .page(page)
        case .page where !opened:
            stack[stack.count - 1] = .page(page)
        default:
            stack.append(.page(page))
        }
        if stack.count > 8 { stack.removeFirst(stack.count - 8) }
        focus = -1
    }

    /// What the Mac shows there changed, not where it is: the page is updated wherever it is in
    /// the stack, and nothing moves (an answer being read, a swipe back, a card stay as they are).
    mutating func refresh(_ page: LensPage) {
        for i in stack.indices {
            if case .page(let p) = stack[i], p.place == page.place { stack[i] = .page(page) }
        }
        if case .page(let p) = card, p.place == page.place { focus = min(focus, page.items.count - 1) }
    }

    /// The place on the lens now (for "this" and for the voice), if a page is shown.
    var page: LensPage? {
        if case .page(let page) = card { return page }
        return nil
    }

    func sessionChips(_ s: SessionCard) -> [Chip] {
        var chips: [Chip] = []
        if let pr = s.pr { chips.append(Chip(label: "Pull request #\(pr)", target: .pr(pr, session: s.id))) }
        if s.question != nil { chips.append(Chip(label: "Answer", target: .session(s.id))) }
        return chips
    }

    /// What a gesture does now. `speaking`: the manager's voice is playing.
    mutating func handle(_ gesture: Gesture, speaking: Bool) -> Effect {
        if case .listening = card {
            switch gesture {
            case .pinch: return .sendNow
            case .left:
                back()
                return .cancelListening
            default: return .none
            }
        }
        switch gesture {
        case .pinch:
            if speaking { return .stopSpeaking }
            return activate() ?? .talk
        case .up, .down:
            // A page without rows: the next or the previous one of its list (the tickets one by one).
            if case .page(let page) = card, page.items.isEmpty {
                let beside = gesture == .down ? page.next : page.prev
                return beside.map(Effect.place) ?? .none
            }
            if case .points(let title, let points, let at) = card {
                let next = max(0, min(points.count - 1, at + (gesture == .down ? 1 : -1)))
                replaceTop(.points(title: title, points: points, at: next))
                return .none
            }
            let n = focusables
            guard n > 0 else { return .none }
            // From "nothing" (-1) a swipe down goes to the first; up from the first goes back to
            // nothing, where a pinch talks.
            focus = gesture == .down ? min(n - 1, focus + 1) : max(-1, focus - 1)
            return .none
        case .right:
            guard focus >= 0, let effect = activate() else { return .none }
            switch effect {
            case .open, .place: return effect
            default: return .none
            }
        case .left:
            guard stack.count > 1 else { return .none }
            back()
            return .back
        }
    }

    /// What the focused row or button does; nil when nothing is focused.
    private func activate() -> Effect? {
        guard focus >= 0 else { return nil }
        switch card {
        case .glance:
            guard focus < glance.rows.count, let target = glance.rows[focus].target else { return nil }
            return .open(target)
        case .answer(_, let chips):
            return focus < chips.count ? .open(chips[focus].target) : nil
        case .session(let s):
            let chips = sessionChips(s)
            return focus < chips.count ? .open(chips[focus].target) : nil
        case .pr(let pr):
            if focus == 0 { return .ask("Explain what pull request #\(pr.number) changes, and why its checks fail if they do.") }
            if let session = pr.session { return .open(.session(session)) }
            return nil
        case .approval(let a):
            if focus == 0 { return a.fits ? Effect.allow(a) : Effect.none }
            return .deny(a)
        case .question(let q):
            return focus < q.options.count ? .answer(session: q.session, option: q.options[focus].number) : nil
        case .note(_, _, _, let open):
            return open.map(Effect.open)
        case .page(let page):
            return focus < page.items.count ? page.items[focus].open.map(Effect.place) : nil
        default:
            return nil
        }
    }

    /// The number of characters of a payload the lens shows whole (the home checks it too).
    static let lensFits = 280
}
