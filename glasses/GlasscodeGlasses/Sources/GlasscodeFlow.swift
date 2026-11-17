import Foundation
import Combine
import Network
import MWDATDisplay
import MWDATSpeech

struct GlasscodeProject: Equatable {
    let name: String
    let repo: String?
    let sessionCount: Int
}

struct GlasscodeSessionItem: Equatable {
    let id: String
    let title: String
    let branch: String?
    let project: String?
}

/// A flattened row for the phone mirror (the lens draws itself via `GlasscodeScreens`).
struct GlasscodeRow: Identifiable, Equatable {
    let id: String
    let title: String
    let subtitle: String?
    let selected: Bool
}

/// The browse brain. Neural-band (or phone) events drive a little state machine:
/// Home ▸ Sessions ▸ Session. Home is the manager first (talk with it, today's update), then
/// the projects. A session offers Voice (free, Meta transcription into the thread) and Talk
/// (the manager, told which session is open).
///
/// The manager replaces the earlier voice copilot: Talk joins the manager's voice room
/// (`ManagerVoice`); nothing is routed to the old ElevenLabs agent (`GlasscodeVoice`) any more. With the manager off, or its
/// voice not set up, the lens says so plainly and the project/session browsing works as before.
///
/// Every state change redraws the lens and republishes the phone mirror from the same data.
@MainActor
final class GlasscodeFlow: ObservableObject {
    enum Screen: Equatable { case projects, sessionList, actions, voice, talk, brief, loading, error }

    // Phone-mirror surface (ContentView renders these; the lens is drawn separately).
    @Published private(set) var screen: Screen = .loading
    @Published private(set) var crumb: String = "Glasscode"
    @Published private(set) var rows: [GlasscodeRow] = []
    @Published private(set) var banner: String = ""
    @Published private(set) var detail: String = ""
    @Published private(set) var listening = false

    // The manager, for the phone mirror: who it is, why it is not there (when it is not), and
    // the live pane of a talk or of today's update (the same three lines the lens shows).
    @Published private(set) var manager: ManagerStatus?
    @Published private(set) var managerNotice = ""
    @Published private(set) var paneTitle = ""
    @Published private(set) var paneMeta = ""
    @Published private(set) var paneBody = ""
    /// "listening", "thinking", "speaking"… while in the manager's voice room; empty otherwise.
    @Published private(set) var voiceState = ""

    private let glasses: GlassesSession
    private let voice: ManagerVoice
    private let control = GlasscodeControl()
    private var cancellables = Set<AnyCancellable>()

    /// One row of the home list: the manager's two rows, then the projects.
    private enum HomeItem: Equatable {
        case manager, update, project(GlasscodeProject)

        var id: String {
            switch self {
            case .manager: return "@manager"
            case .update: return "@update"
            case .project(let p): return p.name
            }
        }
    }

    private var projects: [GlasscodeProject] = []
    private var sessions: [GlasscodeSessionItem] = []
    private var index = 0
    private var project: GlasscodeProject?
    private var session: GlasscodeSessionItem?

    // Latest thread slice for the open session — the context box on the session screen.
    private var contextText = ""

    // Voice accumulation.
    private var voiceFinal = ""
    private var voicePartial = ""

    // A talk with the manager: the session it was opened from (nil: from home), and today's
    // update while it is on the lens. `visit` goes up whenever a talk or an update starts or
    // ends, so work begun for an earlier one drops its result.
    private var talkSession: GlasscodeSessionItem?
    private var brief: ManagerBrief?
    private var briefIndex = 0
    private var briefTrouble = ""
    private var visit = 0

    // IDE mirror: the last view we published (to dedupe) + a heartbeat so the IDE knows the
    // glasses are still live even when the screen hasn't changed.
    private var lastPostedView = ""
    private var mirrorTimer: Timer?
    private var beats = 0

    // Connection recovery: retry the control plane while unreachable, and re-probe the route
    // when the network changes (new Wi-Fi/cellular, Mac back on the LAN, resume from sleep).
    private var retryTimer: Timer?
    private let pathMonitor = NWPathMonitor()
    private var monitoring = false

    init(glasses: GlassesSession, voice: ManagerVoice) {
        self.glasses = glasses
        self.voice = voice
        glasses.onInput = { [weak self] input in self?.handle(input) }
        glasses.onTranscript = { [weak self] result in self?.onTranscript(result) }
        glasses.onSpeechError = { [weak self] msg in self?.onSpeechError(msg) }
        glasses.onDisplayChange = { [weak self] in self?.publishView() }
        voice.onEnded = { [weak self] why in self?.onVoiceEnded(why) }
        voice.onQuestion = { [weak self] _ in self?.onQuestion() }
        // Reflect the voice room on the Talk and update screens: connecting → listening →
        // thinking → speaking, what it heard and what the manager says. It also tells you at
        // a glance whether your mic is getting through.
        voice.objectWillChange
            .receive(on: RunLoop.main)
            .sink { [weak self] _ in self?.onVoiceChanged() }
            .store(in: &cancellables)
    }

    private var managerOn: Bool { manager?.enabled == true }
    private var managerName: String { manager?.name ?? "Manager" }

    // MARK: - entry

    func start() {
        screen = .loading
        render()
        // Heartbeat: re-post the current view every few seconds so the IDE mirror knows the
        // glasses are still live even when nothing changed (otherwise it would age out). Once
        // a minute the manager's line on the home screen is brought up to date.
        mirrorTimer?.invalidate()
        mirrorTimer = Timer.scheduledTimer(withTimeInterval: 2.5, repeats: true) { [weak self] _ in
            Task { @MainActor in
                guard let self else { return }
                self.publishView(force: true)
                self.beats += 1
                if self.beats % 24 == 0, self.screen == .projects { await self.loadManager() }
            }
        }
        startNetworkMonitor()
        Task { await loadProjects() }
    }

    // MARK: - connection recovery

    /// Watch for network changes (Wi-Fi↔cellular, moving networks, resume) and, when the path
    /// comes back, reset the transport and recover immediately instead of waiting minutes.
    private func startNetworkMonitor() {
        guard !monitoring else { return }
        monitoring = true
        pathMonitor.pathUpdateHandler = { [weak self] path in
            let up = path.status == .satisfied
            Task { @MainActor in await self?.onNetworkChange(up: up) }
        }
        pathMonitor.start(queue: DispatchQueue.global(qos: .utility))
    }

    private func onNetworkChange(up: Bool) async {
        // The old LAN route / relay tunnel is stale after a network flip — rebuild from scratch.
        await GlasscodeLink.shared.resetTransport()
        if up { await recover() }
    }

    /// Called on foreground + network return: if we're stuck not showing projects, reload now.
    func recover() async {
        if screen == .error || (screen == .projects && projects.isEmpty) {
            await loadProjects()
        }
    }

    /// Retry the control plane every few seconds while it's unreachable, re-probing the route
    /// each time, so a transient drop (sleep, network change) heals in seconds, not minutes.
    private func scheduleRetry() {
        guard retryTimer == nil else { return }
        retryTimer = Timer.scheduledTimer(withTimeInterval: 3, repeats: true) { [weak self] _ in
            Task { @MainActor in
                guard let self else { return }
                guard self.screen == .error else { self.stopRetry(); return }
                await GlasscodeLink.shared.invalidateRoute()
                await self.loadProjects()
            }
        }
    }

    private func stopRetry() { retryTimer?.invalidate(); retryTimer = nil }

    private func loadProjects() async {
        // The manager's status is asked for at the same time, so home appears in one step.
        async let status: Void = loadManager(redraw: false)
        guard let obj = (try? await control.getJSON("/api/projects")) as? [String: Any] else {
            // Unreachable (Mac asleep, network changed, relay dropped). Show the error but keep
            // retrying in the background so it heals on its own in seconds.
            screen = .error; render(); scheduleRetry(); return
        }
        let raw = (obj["projects"] as? [[String: Any]]) ?? []
        projects = raw.map {
            GlasscodeProject(name: ($0["name"] as? String) ?? "—",
                         repo: $0["repo"] as? String,
                         sessionCount: intOf($0["sessions"]) ?? 0)
        }
        await status
        // Only the loading and error screens are replaced: a reload must not pull the person
        // out of a session or a talk.
        guard screen == .loading || screen == .error || screen == .projects else { return }
        index = 0
        banner = ""
        if home.isEmpty {
            screen = .error; scheduleRetry()
        } else {
            screen = .projects; stopRetry()
        }
        render()
    }

    // MARK: - the manager on the home screen

    /// Ask the Mac whether the manager is on, its name and what waits for the person. When it
    /// is off or not there, `managerNotice` says so and the home screen is the projects alone.
    private func loadManager(redraw: Bool = true) async {
        let before = home.map(\.id)
        let selected = index < before.count ? before[index] : nil
        do {
            let status = try await control.managerStatus()
            manager = status
            managerNotice = status.enabled ? "" : "Manager is off — switch it on in Glasscode › Settings › Manager"
        } catch let error as ManagerError {
            manager = nil
            // An older control server answers "not found"; the app itself answers in a sentence.
            managerNotice = error.text == "not found" ? "The manager is not on this Mac yet" : error.text
        } catch {
            // The Mac did not answer at all. Keep what we knew; the projects call shows that.
            return
        }
        guard redraw, screen == .projects else { return }
        // The manager's rows came or went: stay on the row the band was pointing at.
        if let selected, let i = home.firstIndex(where: { $0.id == selected }) { index = i }
        else { index = min(index, max(home.count - 1, 0)) }
        render()
    }

    private var home: [HomeItem] {
        (managerOn ? [.manager, .update] : []) + projects.map { .project($0) }
    }

    private func homeRows() -> [GlasscodeRow] {
        home.enumerated().map { i, item in
            switch item {
            case .manager:
                return GlasscodeRow(id: item.id, title: "\(manager?.initials ?? "M") · \(managerName)",
                                subtitle: manager?.line, selected: i == index)
            case .update:
                return GlasscodeRow(id: item.id, title: "Today's update",
                                subtitle: "\(managerName) reads it out", selected: i == index)
            case .project(let p):
                return GlasscodeRow(id: item.id, title: p.name,
                                subtitle: (p.repo.map { $0 + " · " } ?? "") + "\(p.sessionCount) session\(p.sessionCount == 1 ? "" : "s")",
                                selected: i == index)
            }
        }
    }

    // MARK: - navigation

    func handle(_ input: GlassesInput) {
        // Hardware reality (Meta docs): real glasses don't deliver `.back`, and the pinch is a
        // system gesture that opens Meta AI. So we drive everything from directional swipes,
        // which the band does deliver: up/down = move, right = open/select, left = back.
        switch input {
        case .up:              nav(-1)
        case .down:            nav(1)
        case .right, .select, .capture: select()
        case .left, .back:     back()
        }
    }

    /// Phone-mirror convenience: tap a row to select it and act, same as pointing the
    /// band at it and pinching.
    func tapRow(_ row: GlasscodeRow) {
        switch screen {
        case .projects:
            if let i = home.firstIndex(where: { $0.id == row.id }) { index = i; select() }
        case .sessionList:
            if let i = sessions.firstIndex(where: { $0.id == row.id }) { index = i; select() }
        case .actions:
            index = (row.id == "voice") ? 0 : 1; select()
        default: break
        }
    }

    private func listLen() -> Int {
        switch screen {
        case .projects: return home.count
        case .sessionList: return sessions.count
        case .actions: return 2
        default: return 0
        }
    }

    private func nav(_ delta: Int) {
        if screen == .brief {
            // Today's update: one line at a time, no wrap-around.
            let count = brief?.lines.count ?? 0
            guard count > 0 else { return }
            briefIndex = min(max(briefIndex + delta, 0), count - 1)
            render()
            return
        }
        let n = listLen()
        guard n > 0 else { return }
        index = ((index + delta) % n + n) % n
        banner = ""   // a note about the last thing done ("Hung up", "Sent to…") goes when you move on
        render()
    }

    private func select() {
        switch screen {
        case .projects:
            let items = home
            guard index < items.count else { return }
            switch items[index] {
            case .manager: startTalk(about: nil)
            case .update: startBrief()
            case .project(let p): openProject(p)
            }
        case .sessionList:
            guard index < sessions.count else { return }
            openSession(sessions[index])
        case .actions:
            index == 0 ? startVoice() : startTalk(about: session)
        case .voice:
            listening ? sendVoice() : beginListening()
        case .talk:
            // The one thing to "do" in a talk: cut in while the manager speaks.
            if voice.state == .speaking { Task { await voice.interrupt() } }
        case .brief:
            // Cut in while it reads; otherwise read the line on the lens again.
            if voice.state == .speaking {
                Task { await voice.interrupt() }
            } else if voice.live, let line = currentBriefLine() {
                Task { await voice.say(line.text) }
            }
        default: break
        }
    }

    private func back() {
        switch screen {
        case .sessionList: screen = .projects; index = indexOfProject(); banner = ""; render(); Task { await loadManager() }
        case .actions:     screen = .sessionList; index = indexOfSession(); render()
        case .voice:       glasses.stopSpeech(); screen = .actions; index = 0; render()
        case .talk, .brief: endTalk(reason: "Hung up")
        case .error:       start()
        default: break
        }
    }

    // MARK: - projects / sessions

    private func openProject(_ p: GlasscodeProject) {
        project = p
        screen = .loading
        render()
        Task { await loadSessions(p) }
    }

    private func loadSessions(_ p: GlasscodeProject) async {
        let (list, _) = (try? await control.sessions(project: p.name)) ?? ([], nil)
        sessions = list.map {
            GlasscodeSessionItem(id: ($0["id"] as? String) ?? "",
                             title: ($0["title"] as? String) ?? "Session",
                             branch: $0["branch"] as? String,
                             project: $0["project"] as? String)
        }
        index = 0
        banner = ""
        screen = .sessionList
        render()
    }

    private func openSession(_ s: GlasscodeSessionItem) {
        session = s
        index = 0
        detail = s.branch ?? "—"
        contextText = ""
        banner = ""
        screen = .actions
        // Tell the IDE to open this session too (desk mode: lens off, looking at the IDE).
        Task { try? await control.setCurrent(s.id) }
        render()
        // Fill the context box with a slice of the live thread.
        Task {
            if let text = try? await control.transcript(s.id, last: 4), !text.isEmpty {
                self.contextText = String(text.suffix(600)).trimmingCharacters(in: .whitespacesAndNewlines)
                let oneLine = String(self.contextText.suffix(160)).replacingOccurrences(of: "\n", with: " ")
                self.detail = oneLine
                if self.screen == .actions { self.render() }
            }
        }
    }

    // MARK: - Voice (free)

    private func startVoice() {
        voiceFinal = ""; voicePartial = ""
        screen = .voice
        render()
    }

    private func beginListening() {
        voiceFinal = ""; voicePartial = ""
        glasses.startSpeech()
        render()
    }

    private func onTranscript(_ result: TranscriptionResult) {
        guard screen == .voice else { return }
        if result.isFinal {
            voiceFinal = (voiceFinal + " " + result.text).trimmingCharacters(in: .whitespaces)
            voicePartial = ""
        } else {
            voicePartial = result.text
        }
        render()
    }

    private func sendVoice() {
        glasses.stopSpeech()
        let text = (voiceFinal + " " + voicePartial).trimmingCharacters(in: .whitespaces)
        guard let s = session, !text.isEmpty else { screen = .actions; index = 0; render(); return }
        Task { try? await control.send(s.id, text) }
        voiceFinal = ""; voicePartial = ""
        screen = .actions; index = 0
        banner = "Sent to \(s.title)"
        render()
    }

    private func onSpeechError(_ msg: String) {
        guard screen == .voice else { return }
        voicePartial = ""
        banner = msg
        listening = false
        render()
    }

    // MARK: - Talk (the manager's voice room)

    /// Open a talk with the manager: from home (`nil`), or from a session, which the manager
    /// is told about in front of the first question.
    private func startTalk(about s: GlasscodeSessionItem?) {
        guard managerOn else {
            // No manager: say why and stay where the person is (browsing still works).
            banner = managerNotice.isEmpty ? "The manager is not available right now" : managerNotice
            render()
            return
        }
        visit += 1
        talkSession = s
        brief = nil
        screen = .talk
        render()
        let name = managerName
        Task { await voice.start(name: name, context: s.map(context(for:))) }
    }

    /// Which session the person has open, for the manager (it looks the rest up itself).
    private func context(for s: GlasscodeSessionItem) -> String {
        let project = (s.project ?? self.project?.name).map { " in the project \($0)" } ?? ""
        let branch = s.branch.map { " on the branch \($0)" } ?? ""
        return "(I am on my glasses with the Glasscode session \"\(s.title)\"\(project)\(branch) open, id \(s.id). "
            + "What I say is about this session unless I name another one.)"
    }

    /// Today's update: its lines on the lens one at a time (swipe up/down), read out by the
    /// manager's voice. Without voice the lines are still there to read.
    private func startBrief() {
        guard managerOn else { return }
        visit += 1
        let mine = visit
        talkSession = nil
        brief = nil; briefIndex = 0; briefTrouble = ""
        screen = .brief
        render()
        let name = managerName
        Task {
            do {
                let loaded = try await control.managerBrief()
                guard mine == visit else { return }
                brief = loaded
                render()
                if await voice.start(name: name), mine == visit { await voice.say(loaded.text) }
            } catch {
                guard mine == visit else { return }
                briefTrouble = (error as? ManagerError)?.text ?? "Can't reach Glasscode right now."
                render()
            }
        }
    }

    private func currentBriefLine() -> ManagerBrief.Line? {
        guard let lines = brief?.lines, briefIndex < lines.count else { return nil }
        return lines[briefIndex]
    }

    /// The person asked something while the update was on the lens: carry on as a talk.
    private func onQuestion() {
        guard screen == .brief else { return }
        screen = .talk
        render()
    }

    private func onVoiceChanged() {
        guard screen == .talk || screen == .brief else { return }
        render()
    }

    /// The room ended by itself (dropped, or quiet for too long).
    private func onVoiceEnded(_ why: String) {
        guard screen == .talk || screen == .brief else { return }
        leaveTalk(reason: why)
    }

    private func endTalk(reason: String) {
        Task { await voice.hangUp() }
        leaveTalk(reason: reason)
    }

    /// Back to where the talk was opened from: the session's screen, or home.
    private func leaveTalk(reason: String) {
        visit += 1
        let wasBrief = screen == .brief
        brief = nil
        if talkSession != nil {
            screen = .actions; index = 1
        } else {
            screen = .projects
            index = home.firstIndex(of: wasBrief ? .update : .manager) ?? 0
            Task { await loadManager() }
        }
        talkSession = nil
        banner = reason
        render()
    }

    /// The talk's lines: what the room is doing, what it heard, what the manager says.
    private func talkPane() -> (title: String, meta: String, body: String) {
        let name = managerName
        if let problem = voice.problem, !voice.live { return (problem.title, "", problem.detail) }
        let heard = voice.heard.isEmpty ? "" : "You: \(voice.heard)"
        let title: String
        switch voice.state {
        case .off:        title = "Connecting…"
        case .connecting: title = "Connecting to \(name)…"
        case .thinking:   title = "Thinking…"
        case .speaking:   title = "\(name) is speaking"
        case .listening:  title = "● Listening"
        case .idle:
            if !voice.workerHere { title = "Waiting for \(name)'s voice…" }
            else if !voice.micOn { title = "Mic didn't open — hang up & retry" }
            else { title = "Listening — just talk" }
        }
        return (title, heard, voice.caption)
    }

    // MARK: - render (lens + phone mirror)

    private func render() {
        listening = glasses.isListening
        voiceState = voice.live ? voice.state.rawValue : ""
        switch screen {
        case .loading:
            crumb = "Glasscode"; rows = []; banner = "Loading…"; detail = ""
            glasses.show(GlasscodeScreens.message("Glasscode", "Loading your projects…"))

        case .error:
            crumb = "Glasscode"; rows = []; banner = "Can't reach Glasscode"; detail = "Reconnecting automatically…"
            glasses.show(GlasscodeScreens.message("Can't reach Glasscode", "Reconnecting…"))

        case .projects:
            crumb = managerOn ? managerName : "Projects"
            rows = homeRows()
            glasses.show(GlasscodeScreens.home(crumb: crumb, notice: managerNotice, banner: banner, rows: rows))

        case .sessionList:
            crumb = "Projects ▸ \(project?.name ?? "")"
            rows = sessions.enumerated().map { i, s in
                GlasscodeRow(id: s.id, title: s.title, subtitle: s.branch, selected: i == index)
            }
            glasses.show(GlasscodeScreens.sessions(project: project?.name ?? "", items: sessions, selected: index))

        case .actions:
            crumb = session?.title ?? ""
            let talkDetail = managerOn ? "Talk it over with \(managerName)" : "The manager is off"
            rows = [
                GlasscodeRow(id: "voice", title: "🎙  Voice — free", subtitle: "Speak → thread", selected: index == 0),
                GlasscodeRow(id: "talk",  title: "💬  Talk — live",  subtitle: talkDetail, selected: index == 1),
            ]
            glasses.show(GlasscodeScreens.sessionActions(title: session?.title ?? "", context: detail,
                                                     talkDetail: talkDetail, banner: banner, selected: index))

        case .voice:
            crumb = "\(session?.title ?? "") ▸ Voice"
            rows = []
            let text = (voiceFinal + " " + voicePartial).trimmingCharacters(in: .whitespaces)
            detail = text
            glasses.show(GlasscodeScreens.voice(session: session?.title ?? "", listening: listening, text: text))

        case .talk:
            crumb = talkSession.map { "\($0.title) ▸ \(managerName)" } ?? "\(managerName) ▸ Talk"
            rows = []
            let pane = talkPane()
            paneTitle = pane.title; paneMeta = pane.meta; paneBody = pane.body
            // One line for the IDE mirror, which shows a talk as its banner.
            banner = pane.body.isEmpty ? pane.title : "\(pane.title) — \(pane.body)"
            let foot = voice.state == .speaking ? "swipe ▶ to cut in · ◀ to hang up" : "swipe ◀ to hang up"
            glasses.show(GlasscodeScreens.managerTalk(crumb: crumb, state: pane.title, heard: pane.meta,
                                                  caption: pane.body, foot: voice.live ? foot : "swipe ◀ back"))

        case .brief:
            crumb = "\(managerName) ▸ Today's update"
            let lines = brief?.lines ?? []
            rows = lines.enumerated().map { i, line in
                GlasscodeRow(id: "line-\(i)", title: line.text, subtitle: nil, selected: i == briefIndex)
            }
            let pane = talkPane()
            if !briefTrouble.isEmpty {
                paneTitle = "Today's update"; paneMeta = ""; paneBody = briefTrouble
            } else if brief == nil {
                paneTitle = "Today's update"; paneMeta = ""; paneBody = "Getting it…"
            } else if lines.isEmpty {
                paneTitle = "Today's update"; paneMeta = ""; paneBody = "Nothing to report yet."
            } else {
                paneTitle = "Today's update · \(briefIndex + 1) of \(lines.count)"
                // The voice's state under the title; a voice problem is said once, in short.
                paneMeta = voice.live ? pane.title : (voice.problem.map { "\($0.title) — read it here" } ?? "")
                paneBody = lines[briefIndex].text
            }
            banner = paneMeta
            let foot: String
            if lines.count > 1 {
                foot = voice.state == .speaking ? "swipe ▲▼ lines · ▶ cut in · ◀ back" : "swipe ▲▼ lines · ◀ back"
            } else {
                foot = "swipe ◀ back"
            }
            glasses.show(GlasscodeScreens.brief(crumb: crumb, title: paneTitle, state: paneMeta, text: paneBody, foot: foot))
        }
        publishView()
    }

    // MARK: - IDE mirror (publish the lens view to the control plane)

    /// Post the current view to `/api/voice/state` so the IDE renders a live replica.
    /// Deduped unless `force` (the heartbeat) keeps the IDE's freshness alive.
    private func publishView(force: Bool = false) {
        var view: [String: Any] = [
            "screen": screenName(screen),
            "crumb": crumb,
            "banner": banner,
            "detail": detail,
            "listening": listening,
            "billing": false,
            "cost": "",
            "lensOn": glasses.displayOn,
            "connected": glasses.isConnected,
        ]
        view["rows"] = rows.map { ["id": $0.id, "title": $0.title, "subtitle": $0.subtitle ?? "", "selected": $0.selected] }
        if screen == .talk || screen == .brief {
            view["manager"] = ["name": managerName, "state": voiceState, "heard": voice.heard, "caption": voice.caption]
        }
        let sig = signature(view)
        if !force && sig == lastPostedView { return }
        lastPostedView = sig
        Task { await control.reportVoice(["view": view]) }
    }

    private func screenName(_ s: Screen) -> String {
        switch s {
        case .projects: return "projects"
        case .sessionList: return "sessionList"
        case .actions: return "actions"
        case .voice: return "voice"
        case .talk: return "talk"
        case .brief: return "brief"
        case .loading: return "loading"
        case .error: return "error"
        }
    }

    private func signature(_ view: [String: Any]) -> String {
        let data = (try? JSONSerialization.data(withJSONObject: view, options: [.sortedKeys])) ?? Data()
        return String(data: data, encoding: .utf8) ?? ""
    }

    // MARK: - helpers

    private func indexOfProject() -> Int {
        guard let project else { return 0 }
        return home.firstIndex(of: .project(project)) ?? 0
    }
    private func indexOfSession() -> Int { sessions.firstIndex { $0 == session } ?? 0 }
    private func intOf(_ v: Any?) -> Int? {
        if let i = v as? Int { return i }; if let d = v as? Double { return Int(d) }
        if let n = v as? NSNumber { return n.intValue }; if let s = v as? String { return Int(s) }
        return nil
    }
}
