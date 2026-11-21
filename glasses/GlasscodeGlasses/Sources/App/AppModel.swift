import Foundation
import SwiftUI

/// The app (G11 to G14): the glasses, the band, speech on the phone, the lens, and the manager
/// over the device link, wired together. There is no menu: the manager is the home screen.
///
/// A turn: a pinch → listening (the words on the lens as they are heard) → a pause → the question
/// goes to the manager's home with what the lens shows → the answer streams back, spoken sentence
/// by sentence and shown on the lens → places the manager shows (`device.open`) open there.
@MainActor
final class AppModel: ObservableObject {
    let link = ManagerLink()
    /// Made when the app starts, never in the test host: the tests' simulated glasses take one
    /// session at a time.
    private(set) lazy var glasses: GlassesSession = {
        let glasses = GlassesSession()
        glasses.onInput = { [weak self] input in self?.band(input) }
        return glasses
    }()
    private let listener = Listener()
    private let voice = Voice()

    @Published private(set) var lens = LensModel()
    @Published private(set) var turn = TurnTaking()
    @Published private(set) var conversation: [JSON] = []
    @Published private(set) var sessions: [JSON] = []
    @Published private(set) var today: JSON?
    @Published private(set) var problem: String?
    /// The answer being written, for the phone's screen.
    @Published private(set) var writing = ""

    private var clock: Task<Void, Never>?
    private var sentences = Sentences()
    private var seenApprovals = Set<Int>()
    private var seenQuestions = Set<String>()
    private var presenceSent = ""
    private var refreshing = false
    private var started = false

    init() {
        listener.onWords = { [weak self] words in self?.heard(words) }
        listener.onError = { [weak self] why in self?.problem = why }
        voice.onFinished = { [weak self] in self?.voiceDone() }
        link.onEvent = { [weak self] name, data in self?.event(name, data) }
        clock = Task { [weak self] in
            var beat = 0
            while !Task.isCancelled {
                try? await Task.sleep(for: .milliseconds(100))
                guard let self else { return }
                self.tick()
                beat += 1
                if beat % 50 == 0 { await self.refresh() }
            }
        }
    }

    /// The phone's push token (G15), for the server's notes while the app is away.
    var pushToken: (token: String, sandbox: Bool)? {
        didSet { if let pushToken { Task { await link.pushToken(pushToken.token, sandbox: pushToken.sandbox) } } }
    }

    func start() {
        // In the background the app stays awake for as long as the glasses should be on Glasscode.
        BackgroundKeepAlive.shared.wanted = { [weak self] in self?.glasses.stayOn ?? false }
        started = true
        link.start()
        glasses.connect()
        Task { await refresh() }
        redraw()
    }

    // MARK: - the band and the phone's own taps

    /// A gesture from the band (or the phone's pad, for the same moves).
    func band(_ input: GlassesInput) {
        let gesture: LensModel.Gesture
        switch input {
        case .select: gesture = .pinch
        case .up: gesture = .up
        case .down: gesture = .down
        case .left, .back: gesture = .left
        case .right: gesture = .right
        case .capture: return
        }
        var model = lens
        let effect = model.handle(gesture, speaking: voice.isSpeaking)
        lens = model
        perform(effect)
        redraw()
    }

    private func perform(_ effect: LensModel.Effect) {
        switch effect {
        case .none, .back: break
        case .talk: talk()
        case .sendNow: send(force: true)
        case .cancelListening:
            listener.stop()
            turn.cancel()
        case .stopSpeaking:
            voice.stop()
            turn.done()
            Task { await link.stop() }
            talk()
        case .open(let target): Task { await open(target) }
        case .ask(let question): ask(question)
        case .allow(let card): Task { await decide(card, allow: true) }
        case .deny(let card): Task { await decide(card, allow: false) }
        case .answer(let session, let option): Task { await answer(session, option) }
        }
    }

    // MARK: - talking

    func talk() {
        guard link.status == .connected else {
            showMessage("Not connected", problemText)
            return
        }
        voice.stop()
        Task {
            guard await Listener.permission() else {
                showMessage("The microphone is off", "Allow the microphone and speech recognition for Glasscode in Settings.")
                return
            }
            BackgroundKeepAlive.shared.yield()
            turn.talk(now: Date())
            var model = lens
            model.show(.listening(""))
            lens = model
            listener.start()
            reportPresence()
            redraw()
        }
    }

    private func heard(_ words: String) {
        turn.heard(words, now: Date())
        if case .listening = lens.card {
            var model = lens
            model.replaceTop(.listening(words))
            lens = model
            redraw()
        }
    }

    private func tick() {
        if let asked = turn.tick(now: Date()) {
            listener.stop()
            ask(asked)
        }
    }

    /// A pinch while listening: send what was heard now.
    private func send(force: Bool) {
        guard case .listening(let heard) = turn.state else { return }
        let words = heard.trimmingCharacters(in: .whitespacesAndNewlines)
        listener.stop()
        if words.isEmpty { turn.cancel(); var m = lens; m.back(); lens = m; redraw(); return }
        turn.heard(words + " ", now: Date.distantPast)
        if let asked = turn.tick(now: Date()) { ask(asked) }
    }

    /// Ask the manager: the answer streams to the lens and the voice.
    func ask(_ question: String) {
        if !turn.isListening, case .idle = turn.state { turn.talk(now: Date()); turn.heard(question, now: .distantPast); _ = turn.tick(now: Date()) }
        BackgroundKeepAlive.shared.yield()
        AudioRoute.configureForTalk()
        sentences = Sentences()
        writing = ""
        var model = lens
        model.show(.working(["“\(LensCards.clip(question, 60))”"]))
        lens = model
        reportPresence()
        redraw()
        let focus = focusNow()
        Task {
            do {
                let said = try await link.ask(question, focus: focus) { [weak self] update in self?.answerUpdate(update) }
                finishAnswer(said)
            } catch {
                turn.done()
                showMessage("No answer", error.localizedDescription)
                BackgroundKeepAlive.shared.resume()
            }
            await refresh()
        }
    }

    private func answerUpdate(_ update: AnswerUpdate) {
        if let line = update.activity {
            if case .working(let lines) = lens.card {
                var model = lens
                model.replaceTop(.working(lines + [line]))
                lens = model
                redraw()
            }
            return
        }
        guard !update.text.isEmpty else { return }
        writing = update.text
        for sentence in sentences.next(update.text, final: update.done) { speak(sentence) }
        var model = lens
        if case .answer = model.card { model.replaceTop(.answer(update.text, [])) } else { model.show(.answer(update.text, [])) }
        lens = model
        redraw()
    }

    private func finishAnswer(_ said: String) {
        writing = ""
        for sentence in sentences.next(said, final: true) { speak(sentence) }
        let chips = chips(for: said)
        var model = lens
        // A place the manager showed meanwhile stays on top; the answer is under it.
        if case .answer = model.card { model.replaceTop(.answer(said, chips)) }
        else if case .working = model.card { model.show(.answer(said, chips)) }
        lens = model
        if !voice.isSpeaking { turn.done(); BackgroundKeepAlive.shared.resume() }
        reportPresence()
        redraw()
    }

    private func speak(_ sentence: String) {
        turn.speaking()
        voice.say(sentence)
    }

    private func voiceDone() {
        if case .speaking = turn.state { turn.done() }
        BackgroundKeepAlive.shared.resume()
        reportPresence()
        redraw()
    }

    /// The links an answer names: pull requests (#42) and the sessions the phone knows by title.
    private func chips(for text: String) -> [LensModel.Chip] {
        var chips: [LensModel.Chip] = []
        let pattern = try? NSRegularExpression(pattern: "#(\\d{1,6})\\b")
        let range = NSRange(text.startIndex..., in: text)
        let numbers = (pattern?.matches(in: text, range: range) ?? []).compactMap { match in Range(match.range(at: 1), in: text).flatMap { Int(text[$0]) } }
        for number in Array(Set(numbers)).sorted().prefix(2) {
            chips.append(.init(label: "#\(number)", target: .pr(number, session: sessionForPR(number))))
        }
        for session in sessions {
            guard chips.count < 3, let id = session.object("session")?.string("id"), let title = session.object("session")?.string("title"), title.count > 3,
                  text.localizedCaseInsensitiveContains(title) else { continue }
            chips.append(.init(label: LensCards.clip(title, 24), target: .session(id)))
        }
        return chips
    }

    /// What the lens shows now, for "this" and "that".
    private func focusNow() -> JSON {
        for card in lens.stack.reversed() {
            switch card {
            case .session(let s): return ["view": "session", "session": s.id] + (s.pr.map { ["pr": $0] } ?? [:])
            case .pr(let pr): return ["view": "pr", "pr": pr.number] + (pr.session.map { ["session": $0] } ?? [:])
            case .question(let q): return ["view": "card", "session": q.session]
            default: continue
            }
        }
        return ["view": "glance"]
    }

    // MARK: - places

    func open(_ target: LensModel.Target) async {
        await refresh()
        var model = lens
        switch target {
        case .today, .conversation:
            model.show(.glance)
        case .project(let name):
            model.show(.note(title: name, body: "\(sessions.filter { $0.object("session")?.string("project_id") == name }.count) sessions", tone: .plain, open: nil))
        case .session(let id):
            if let card = sessionCard(id) {
                if let question = questionCard(id) { model.show(.session(card)); model.show(.question(question)) }
                else { model.show(.session(card)) }
            } else {
                model.show(.message(title: "Not found", body: "That session is not on your Mac or your server now."))
            }
        case .pr(let number, let session):
            model.show(.pr(prCard(number, session: session)))
        }
        lens = model
        redraw()
    }

    func sessionCard(_ id: String) -> LensModel.SessionCard? {
        guard let entry = sessions.first(where: { $0.object("session")?.string("id") == id }), let session = entry.object("session") else { return nil }
        let activity = entry.string("activity") ?? ""
        let prompt = entry.object("prompt")
        let (state, tone): (String, LensModel.Tone) = prompt != nil ? ("Waiting for you", .warn) : activity == "working" ? ("Working", .plain) : entry.bool("running") == true ? ("Idle", .ok) : ("Not running", .plain)
        return LensModel.SessionCard(
            id: id,
            title: session.string("title") ?? "Session",
            branch: session.string("branch") ?? "",
            agent: (session.string("agent") ?? "").capitalized,
            state: state,
            tone: tone,
            lastReply: entry.string("last_reply") ?? "",
            question: prompt?.string("question"),
            pr: entry.object("pr")?.int("number")
        )
    }

    private func questionCard(_ id: String) -> LensModel.QuestionCard? {
        guard let entry = sessions.first(where: { $0.object("session")?.string("id") == id }), let prompt = entry.object("prompt") else { return nil }
        let good = ["don't ask again", "do not ask again", "always", "this session", "all edits", "allow all", "remember", "trust this folder"]
        let options = prompt.array("options").compactMap { o -> (Int, String)? in
            guard let n = o.int("number"), let label = o.string("label") else { return nil }
            let lower = label.lowercased().replacingOccurrences(of: "’", with: "'")
            return good.contains(where: { lower.contains($0) }) ? nil : (n, label)
        }
        return LensModel.QuestionCard(session: id, title: entry.object("session")?.string("title") ?? "A session", question: [prompt.string("title"), prompt.string("question")].compactMap { $0 }.joined(separator: ": "), options: options.map { (number: $0.0, label: $0.1) })
    }

    private func prCard(_ number: Int, session: String?, checks givenChecks: [JSON]? = nil, title givenTitle: String? = nil) -> LensModel.PRCard {
        let entry = sessions.first { $0.object("pr")?.int("number") == number }
        let pr = entry?.object("pr")
        let checks = (givenChecks ?? pr?.array("checks") ?? []).map { c -> (name: String, passed: Bool?) in
            let conclusion = (c.string("conclusion") ?? "").lowercased()
            let passed: Bool? = conclusion.isEmpty ? nil : ["success", "neutral", "skipped"].contains(conclusion)
            return (name: c.string("name") ?? "check", passed: passed)
        }
        let failed = checks.filter { $0.passed == false }.count
        let state = failed > 0 ? "\(failed) check\(failed == 1 ? "" : "s") failing" : checks.isEmpty ? (pr?.string("state") ?? "") : "Checks pass"
        return LensModel.PRCard(number: number, title: givenTitle ?? pr?.string("title") ?? "Pull request #\(number)", checks: checks, state: state, session: session ?? entry?.object("session")?.string("id"))
    }

    private func sessionForPR(_ number: Int) -> String? {
        sessions.first { $0.object("pr")?.int("number") == number }?.object("session")?.string("id")
    }

    // MARK: - what the link tells

    private func event(_ name: String, _ data: Any) {
        let json = data as? JSON ?? [:]
        switch name {
        case "device.open":
            switch json.string("kind") {
            case "session": if let id = json.string("id") { Task { await open(.session(id)) } }
            case "pr":
                if let number = json.int("number") {
                    Task {
                        await refresh()
                        var model = lens
                        model.show(.pr(prCard(number, session: json.string("session"), checks: json.array("checks").isEmpty ? nil : json.array("checks"), title: json.string("title").flatMap { $0.isEmpty ? nil : $0 })))
                        lens = model
                        redraw()
                    }
                }
            case "project": if let title = json.string("title") { Task { await open(.project(title)) } }
            default: Task { await open(.today) }
            }
        case "manager.item":
            conversation.append(json)
            if conversation.count > 200 { conversation.removeFirst(conversation.count - 200) }
        case "today":
            takeToday(json)
        default:
            break
        }
    }

    private func takeToday(_ snapshot: JSON) {
        today = snapshot
        var model = lens
        let needs = snapshot.array("needs")
        let approvals = snapshot.array("approvals")
        model.glance.line = snapshot.string("summary").flatMap { $0.isEmpty ? nil : $0 } ?? (needs.isEmpty ? "Nothing needs you" : "\(needs.count) need you")
        var rows: [LensModel.Row] = approvals.prefix(2).map { a in
            LensModel.Row(title: "Needs your OK", detail: a.string("what") ?? "", tone: .warn, target: nil)
        }
        rows += needs.prefix(3).map { n in
            LensModel.Row(title: n.string("title") ?? "", detail: n.string("what") ?? "", tone: .warn, target: n.string("session").map { $0.hasPrefix("server:") ? .session(String($0.dropFirst(7))) : .session($0) })
        }
        model.glance.rows = Array(rows.prefix(3))
        // A new card or a session's question: a note on the lens, unless the person is busy there.
        let launch = snapshot.string("launch") ?? ""
        for a in approvals {
            guard let id = a.int("id"), !seenApprovals.contains(id) else { continue }
            seenApprovals.insert(id)
            let payload = a.string("command") ?? ""
            let card = LensModel.ApprovalCard(id: id, launch: launch, who: a.string("who") ?? "", what: a.string("what") ?? "", payload: payload, fits: payload.count <= LensModel.lensFits)
            if case .listening = model.card {} else if case .working = model.card {} else { model.show(.approval(card)) }
        }
        lens = model
        redraw()
    }

    // MARK: - acting

    private func decide(_ card: LensModel.ApprovalCard, allow: Bool) async {
        do {
            try await link.decide(approval: card.id, allow: allow, launch: card.launch, shown: "lens")
            var model = lens
            model.show(.note(title: allow ? "Allowed" : "Denied", body: card.what, tone: allow ? .ok : .plain, open: nil))
            lens = model
        } catch {
            showMessage("Not done", error.localizedDescription)
        }
        redraw()
    }

    private func answer(_ session: String, _ option: Int) async {
        do {
            try await link.answer(session: session, option: option)
            var model = lens
            model.show(.note(title: "Answered", body: "Option \(option)", tone: .ok, open: .session(session)))
            lens = model
        } catch {
            showMessage("Not answered", error.localizedDescription)
        }
        redraw()
    }

    // MARK: - keeping things current

    func refresh() async {
        guard started, !refreshing, link.status == .connected else { return }
        refreshing = true
        defer { refreshing = false }
        await link.refreshHome()
        let found = await link.sessions()
        if !found.isEmpty || sessions.isEmpty { sessions = found }
        if let snapshot = await link.today() { takeToday(snapshot) }
        if conversation.isEmpty { conversation = await link.conversation() }
        if let home = link.home {
            var model = lens
            model.glance.name = home.name
            model.glance.initials = home.initials
            model.glance.place = home.place == .mac ? "On your Mac" : "On the server"
            if !home.enabled { model.glance.line = "The manager is off: switch it on in Settings › Manager on your Mac." }
            lens = model
        }
        reportPresence()
        redraw()
    }

    /// What the glasses do, for the Mac's title bar.
    private func reportPresence() {
        guard started else { return }
        let presence: JSON = ["glasses": glasses.isConnected, "worn": glasses.isConnected, "lens": glasses.isConnected && glasses.displayOn, "voice": turn.word, "via": link.home?.place.rawValue ?? "mac"]
        let text = "\(presence)"
        guard text != presenceSent else { return }
        presenceSent = text
        link.presence(presence)
    }

    private var problemText: String {
        switch link.status {
        case .unpaired: return "Pair this phone with your Mac first: Settings › Glasses on the Mac shows a code."
        case .connecting: return "Connecting to your Mac…"
        case .unreachable(let why): return why
        case .connected: return ""
        }
    }

    private func showMessage(_ title: String, _ body: String) {
        var model = lens
        model.show(.message(title: title, body: body))
        lens = model
        redraw()
    }

    /// Draw the lens again.
    func redraw() {
        guard started else { return }
        if link.status != .connected, case .glance = lens.card {
            var model = lens
            model.glance.line = problemText
            lens = model
        }
        glasses.show(LensCards.render(lens, voice: turn.word))
    }
}

private func + (a: JSON, b: JSON) -> JSON {
    a.merging(b) { _, new in new }
}
