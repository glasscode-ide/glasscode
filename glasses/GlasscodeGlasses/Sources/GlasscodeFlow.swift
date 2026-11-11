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
/// Projects ▸ Sessions ▸ Session, where a session offers Voice (free, Meta transcription
/// into the thread) and Talk (live ElevenLabs with the session's context). Asking Glasscode
/// to "jump" during Talk ends the call and drops you back in the UI at the target.
///
/// Every state change redraws the lens and republishes the phone mirror from the same data.
@MainActor
final class GlasscodeFlow: ObservableObject {
    enum Screen: Equatable { case projects, sessionList, actions, voice, talk, loading, error }

    // Phone-mirror surface (ContentView renders these; the lens is drawn separately).
    @Published private(set) var screen: Screen = .loading
    @Published private(set) var crumb: String = "Glasscode"
    @Published private(set) var rows: [GlasscodeRow] = []
    @Published private(set) var banner: String = ""
    @Published private(set) var detail: String = ""
    @Published private(set) var listening = false
    @Published private(set) var billing = false
    @Published private(set) var costText = "$0.00"

    private let glasses: GlassesSession
    private let glasscode: GlasscodeVoice
    private let control = GlasscodeControl()
    private var cancellables = Set<AnyCancellable>()

    private var projects: [GlasscodeProject] = []
    private var sessions: [GlasscodeSessionItem] = []
    private var index = 0
    private var project: GlasscodeProject?
    private var session: GlasscodeSessionItem?

    // Latest thread slice for the open session — context box + Talk briefing.
    private var contextText = ""

    // Voice accumulation.
    private var voiceFinal = ""
    private var voicePartial = ""

    // Talk billing (illustrative, matches the prototype's placeholder rate).
    private let ratePerMinute = 0.12
    private var billingSeconds = 0
    private var billingTimer: Timer?

    // IDE mirror: the last view we published (to dedupe) + a heartbeat so the IDE knows the
    // glasses are still live even when the screen hasn't changed.
    private var lastPostedView = ""
    private var mirrorTimer: Timer?

    // Connection recovery: retry the control plane while unreachable, and re-probe the route
    // when the network changes (new Wi-Fi/cellular, Mac back on the LAN, resume from sleep).
    private var retryTimer: Timer?
    private let pathMonitor = NWPathMonitor()
    private var monitoring = false

    init(glasses: GlassesSession, glasscode: GlasscodeVoice) {
        self.glasses = glasses
        self.glasscode = glasscode
        glasses.onInput = { [weak self] input in self?.handle(input) }
        glasses.onTranscript = { [weak self] result in self?.onTranscript(result) }
        glasses.onSpeechError = { [weak self] msg in self?.onSpeechError(msg) }
        glasses.onDisplayChange = { [weak self] in self?.publishView() }
        glasscode.onOpenSession = { [weak self] id, title in self?.onGlasscodeJump(id: id, title: title) }
        // Reflect the live voice state in the Talk screen so it isn't stuck on "Connecting…":
        // Connecting → Listening → Speaking, with Glasscode's caption. Also tells you at a glance
        // whether your mic is getting through (it flips to Listening when it's your turn).
        glasscode.$mode.combineLatest(glasscode.$caption, glasscode.$micOn)
            .receive(on: RunLoop.main)
            .sink { [weak self] _, _, _ in self?.onGlasscodeChanged() }
            .store(in: &cancellables)
    }

    /// Mirror GlasscodeVoice's live state onto the Talk screen (lens + phone + IDE mirror).
    private func onGlasscodeChanged() {
        guard screen == .talk else { return }
        switch glasscode.mode {
        case "connecting": banner = "Connecting…"
        case "speaking":   banner = glasscode.caption ?? "Glasscode is speaking…"
        case "thinking":   banner = "Thinking…"
        case "asleep":     banner = glasscode.caption ?? "Paused — say \u{201C}Hey Glasscode\u{201D}"
        default:           banner = glasscode.micOn ? "Listening — just talk" : "Mic didn’t open — hang up & retry"
        }
        render()
    }

    // MARK: - entry

    func start() {
        screen = .loading
        render()
        // Heartbeat: re-post the current view every few seconds so the IDE mirror knows the
        // glasses are still live even when nothing changed (otherwise it would age out).
        mirrorTimer?.invalidate()
        mirrorTimer = Timer.scheduledTimer(withTimeInterval: 2.5, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.publishView(force: true) }
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
        index = 0
        if projects.isEmpty {
            screen = .error; scheduleRetry()
        } else {
            screen = .projects; stopRetry()
        }
        render()
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
            if let i = projects.firstIndex(where: { $0.name == row.id }) { index = i; select() }
        case .sessionList:
            if let i = sessions.firstIndex(where: { $0.id == row.id }) { index = i; select() }
        case .actions:
            index = (row.id == "voice") ? 0 : 1; select()
        default: break
        }
    }

    private func listLen() -> Int {
        switch screen {
        case .projects: return projects.count
        case .sessionList: return sessions.count
        case .actions: return 2
        default: return 0
        }
    }

    private func nav(_ delta: Int) {
        let n = listLen()
        guard n > 0 else { return }
        index = ((index + delta) % n + n) % n
        render()
    }

    private func select() {
        switch screen {
        case .projects:
            guard index < projects.count else { return }
            openProject(projects[index])
        case .sessionList:
            guard index < sessions.count else { return }
            openSession(sessions[index])
        case .actions:
            index == 0 ? startVoice() : startTalk()
        case .voice:
            listening ? sendVoice() : beginListening()
        default: break
        }
    }

    private func back() {
        switch screen {
        case .sessionList: screen = .projects; index = indexOfProject(); render()
        case .actions:     screen = .sessionList; index = indexOfSession(); render()
        case .voice:       glasses.stopSpeech(); screen = .actions; index = 0; render()
        case .talk:        endTalk(reason: "Hung up")
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
        screen = .sessionList
        render()
    }

    private func openSession(_ s: GlasscodeSessionItem) {
        session = s
        index = 0
        detail = s.branch ?? "—"
        contextText = ""
        screen = .actions
        // Tell the IDE to open this session too (desk mode: lens off, looking at the IDE).
        Task { try? await control.setCurrent(s.id) }
        render()
        // Fill the context box (and prime Glasscode's Talk briefing) with a slice of the live thread.
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

    // MARK: - Talk (live, billed)

    private func startTalk() {
        guard let s = session else { return }
        screen = .talk
        banner = "Connecting Glasscode to \(s.title)…"
        startBilling()
        glasscode.focus(session: s.id, briefing: briefing(for: s))
        Task { await glasscode.start() }
        render()
    }

    /// Lightweight Talk context: tell Glasscode WHICH session is focused and to let the user lead.
    /// No transcript dump and no forced opener — he looks up details with his tools if asked.
    private func briefing(for s: GlasscodeSessionItem) -> String {
        let branch = s.branch.map { " on branch \($0)" } ?? ""
        return "The user just opened the Glasscode session \"\(s.title)\"\(branch) and wants to talk about it. "
            + "Let them speak first — do NOT greet or summarize unprompted. "
            + "If they ask about its status, recent activity or history, use your tools "
            + "(session_status, read_transcript) to look it up instead of guessing. "
            + "Stay on this session unless they ask to jump elsewhere."
    }

    /// Glasscode called `open_session` mid-Talk → treat it as "jump there", end the call,
    /// and leave the user in that session's UI.
    private func onGlasscodeJump(id: String, title: String) {
        guard screen == .talk else { return }
        banner = "Taking you to \(title)…"
        render()
        Task {
            await glasscode.fullStop()
            stopBilling()
            // Find the target in our loaded list, or synthesize a stub so the UI can show it.
            let target = sessions.first { $0.id == id }
                ?? GlasscodeSessionItem(id: id, title: title, branch: nil, project: project?.name)
            self.session = target
            self.detail = target.branch ?? "—"
            self.screen = .actions
            self.index = 0
            self.banner = "You're in \(title) — pick Voice or Talk"
            try? await self.control.setCurrent(target.id)
            self.render()
        }
    }

    private func endTalk(reason: String) {
        Task { await glasscode.fullStop() }
        stopBilling()
        screen = .actions; index = 0
        banner = reason
        render()
    }

    private func startBilling() {
        billingSeconds = 0; billing = true; paintCost()
        billingTimer?.invalidate()
        billingTimer = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.tickCost() }
        }
    }
    private func stopBilling() {
        billing = false
        billingTimer?.invalidate(); billingTimer = nil
    }
    private func tickCost() { billingSeconds += 1; paintCost() }
    private func paintCost() {
        costText = String(format: "$%.2f", Double(billingSeconds) / 60.0 * ratePerMinute)
    }

    // MARK: - render (lens + phone mirror)

    private func render() {
        listening = glasses.isListening
        switch screen {
        case .loading:
            crumb = "Glasscode"; rows = []; banner = "Loading…"; detail = ""
            glasses.show(GlasscodeScreens.message("Glasscode", "Loading your projects…"))

        case .error:
            crumb = "Glasscode"; rows = []; banner = "Can't reach Glasscode"; detail = "Reconnecting automatically…"
            glasses.show(GlasscodeScreens.message("Can't reach Glasscode", "Reconnecting…"))

        case .projects:
            crumb = "Projects"
            rows = projects.enumerated().map { i, p in
                GlasscodeRow(id: p.name, title: p.name,
                         subtitle: (p.repo.map { $0 + " · " } ?? "") + "\(p.sessionCount) sessions",
                         selected: i == index)
            }
            glasses.show(GlasscodeScreens.projects(projects, selected: index))

        case .sessionList:
            crumb = "Projects ▸ \(project?.name ?? "")"
            rows = sessions.enumerated().map { i, s in
                GlasscodeRow(id: s.id, title: s.title, subtitle: s.branch, selected: i == index)
            }
            glasses.show(GlasscodeScreens.sessions(project: project?.name ?? "", items: sessions, selected: index))

        case .actions:
            crumb = session?.title ?? ""
            rows = [
                GlasscodeRow(id: "voice", title: "🎙  Voice — free", subtitle: "Speak → thread", selected: index == 0),
                GlasscodeRow(id: "talk",  title: "💬  Talk — live",  subtitle: "Discuss with Glasscode", selected: index == 1),
            ]
            glasses.show(GlasscodeScreens.sessionActions(title: session?.title ?? "", context: detail, selected: index))

        case .voice:
            crumb = "\(session?.title ?? "") ▸ Voice"
            rows = []
            let text = (voiceFinal + " " + voicePartial).trimmingCharacters(in: .whitespaces)
            detail = text
            glasses.show(GlasscodeScreens.voice(session: session?.title ?? "", listening: listening, text: text))

        case .talk:
            crumb = "\(session?.title ?? "") ▸ Talk"
            rows = []
            glasses.show(GlasscodeScreens.talk(session: session?.title ?? "", line: banner, billing: costText))
        }
        publishView()
    }

    // MARK: - IDE mirror (publish the lens view to the control plane)

    /// Post the current view to `/api/voice/state` so the IDE's Glasscode tab renders a live
    /// replica. Deduped unless `force` (the heartbeat) keeps the IDE's freshness alive.
    private func publishView(force: Bool = false) {
        var view: [String: Any] = [
            "screen": screenName(screen),
            "crumb": crumb,
            "banner": banner,
            "detail": detail,
            "listening": listening,
            "billing": billing,
            "cost": costText,
            "lensOn": glasses.displayOn,
            "connected": glasses.isConnected,
        ]
        view["rows"] = rows.map { ["id": $0.id, "title": $0.title, "subtitle": $0.subtitle ?? "", "selected": $0.selected] }
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
        case .loading: return "loading"
        case .error: return "error"
        }
    }

    private func signature(_ view: [String: Any]) -> String {
        let data = (try? JSONSerialization.data(withJSONObject: view, options: [.sortedKeys])) ?? Data()
        return String(data: data, encoding: .utf8) ?? ""
    }

    // MARK: - helpers

    private func indexOfProject() -> Int { projects.firstIndex { $0 == project } ?? 0 }
    private func indexOfSession() -> Int { sessions.firstIndex { $0 == session } ?? 0 }
    private func intOf(_ v: Any?) -> Int? {
        if let i = v as? Int { return i }; if let d = v as? Double { return Int(d) }
        if let n = v as? NSNumber { return n.intValue }; if let s = v as? String { return Int(s) }
        return nil
    }
}
