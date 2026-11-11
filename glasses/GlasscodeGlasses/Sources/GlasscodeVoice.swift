import Foundation
import Combine
import AVFoundation
import ElevenLabs

/// A compact view of what Glasscode is doing, pushed to the glasses lens.
struct GlasscodeHUD {
    var live: Bool = false
    var status: String = "Tap to wake Glasscode"
    var currentTitle: String? = nil
    var needs: Int = 0
    var suggestion: String? = nil
    var lastLine: String? = nil
}

/// The glasses voice copilot: same ElevenLabs core + control plane as the IDE helper.
/// Audio runs over the glasses' Bluetooth mic/speaker; tool calls drive the Mac's sessions.
@MainActor
final class GlasscodeVoice: ObservableObject {
    @Published var live = false
    @Published var mode = "idle"          // idle | connecting | listening | speaking
    @Published var caption: String? = nil
    @Published var errorText: String? = nil
    /// Whether the mic track is actually publishing — so the UI can show "listening" honestly
    /// (the SDK silently "continues without microphone" if the mic fails to enable at connect).
    @Published var micOn = false

    /// Set by the glasses layer to mirror Glasscode onto the lens whenever state changes.
    var onHUD: ((GlasscodeHUD) -> Void)?

    /// Fired when Glasscode opens a session mid-conversation. In the glasses browse flow this
    /// is the "jump there and end the call" signal; GlasscodeFlow wires it up during Talk.
    var onOpenSession: ((String, String) -> Void)?

    private let control = GlasscodeControl()
    private var conversation: Conversation?
    private var cancellables = Set<AnyCancellable>()
    private var handledCalls = Set<String>()
    private var sessionCache: [[String: Any]] = []
    private var current: String?
    private var needsCount = 0
    private var suggestionTitle: String?

    // Idle auto-sleep + "Hey Glasscode" wake: when you go quiet, end the paid ElevenLabs session;
    // a free on-device listener brings Glasscode back when you say his name (no tap needed).
    private let wakeWord = WakeWord()
    private var idleTimer: Timer?
    private var lastActivity = Date()
    private let idleTimeout: TimeInterval = 45
    private var sleeping = false
    private var wakeReady = false

    // MARK: - lifecycle

    func toggle() { Task { live ? await fullStop() : await start() } }

    /// One tap: asleep → wake, awake → turn off, off → start.
    func tap() {
        Task {
            if sleeping { await wake() }
            else if live { await fullStop() }
            else { await start() }
        }
    }

    /// User turns Glasscode on (tap / app open).
    func start() async {
        guard conversation == nil else { return }
        live = true; sleeping = false; errorText = nil
        // Pre-authorize the wake listener in the background so sleep→wake is seamless.
        Task { self.wakeReady = await WakeWord.authorize() }
        await connect()
    }

    /// Open (or re-open) the ElevenLabs conversation, injecting memory so Glasscode continues.
    private func connect() async {
        guard conversation == nil else { return }
        mode = "connecting"; caption = nil; pushHUD()
        // Talk records through the *phone's* mic (ElevenLabs/LiveKit), not Meta's on-device
        // Speech — so iOS must have granted mic access before the audio session comes up, or
        // capture is silent and Glasscode never hears a word.
        guard await ensureMicPermission() else {
            fail("Turn on Microphone for Glasscode in iOS Settings › Glasscode."); return
        }
        configureAudioForGlasses()
        _ = await refreshSessions()
        // refreshSessions() just overwrote `current` with the server's value — if Talk was
        // pointed at a specific session, keep it locked to that one.
        if let fid = focusedSession { current = fid }

        let cfg = (try? await control.config()) ?? [:]
        guard let agentId = cfg["agentId"] as? String, !agentId.isEmpty else {
            fail("Glasscode isn't set up on the Mac yet."); return
        }
        // Load memory so Glasscode continues from last time instead of starting cold.
        var vars: [String: String] = [:]
        if let (memory, opener) = try? await control.memoryContext() {
            if !memory.isEmpty { vars["memory"] = memory }
            if !opener.isEmpty { vars["opener"] = opener }
        }
        // Talk-from-a-session: give Glasscode the session as *context* (folded into memory, not a
        // transcript dump) and leave the opener EMPTY so he waits for the user to speak first
        // instead of monologuing.
        if let briefing = focusedBriefing, !briefing.isEmpty {
            focusedBriefing = nil   // one-shot
            let mem = vars["memory"].map { $0 + "\n\n" } ?? ""
            vars["memory"] = mem + briefing
            vars["opener"] = ""     // user talks first
        }
        let vc = ConversationConfig(dynamicVariables: vars.isEmpty ? nil : vars)
        do {
            if let token = try? await control.conversationToken(), !token.isEmpty {
                conversation = try await ElevenLabs.startConversation(tokenProvider: { token }, config: vc)
            } else {
                conversation = try await ElevenLabs.startConversation(agentId: agentId, config: vc)
            }
        } catch {
            fail("Couldn't reach Glasscode: \(error.localizedDescription)"); return
        }
        guard let conversation else { fail("Couldn't start."); return }
        sleeping = false; mode = "listening"; markActivity(); pushHUD()
        await control.reportVoice(["live": true, "mode": "listening", "caption": ""])
        wire(conversation)
        // The SDK enables the mic once at connect and *silently continues without it* if that
        // fails (WebRTCConnectionManager: "Continuing without microphone"). So explicitly
        // (re-)enable it now that the room + audio session are up, and publish whether it
        // actually came on — otherwise Glasscode talks but never hears you.
        try? await conversation.setMicrophoneMuted(false)
        micOn = !conversation.isMuted
        startIdleTimer()
    }

    /// Idle too long → drop the paid session but stay awake for "Hey Glasscode".
    private func sleep() async {
        guard conversation != nil, !sleeping else { return }
        sleeping = true
        stopIdleTimer()
        let c = conversation; conversation = nil
        cancellables.removeAll(); handledCalls.removeAll()
        if let c, c.state.isActive { await c.endConversation() }
        try? AVAudioSession.sharedInstance().setActive(false, options: [.notifyOthersOnDeactivation])
        if wakeReady || wakeWord.available {
            wakeWord.onWake = { [weak self] in Task { @MainActor in await self?.wake() } }
            wakeWord.start()
            caption = "Say “Hey Glasscode” to wake me"
        } else {
            caption = "Tap to wake me"
        }
        mode = "asleep"; pushHUD()
        await control.reportVoice(["mode": "asleep", "caption": caption ?? ""])
    }

    /// "Hey Glasscode" (or a tap) while asleep → re-open the conversation.
    func wake() async {
        guard sleeping else { return }
        wakeWord.stop()
        sleeping = false
        await connect()
    }

    /// User turns Glasscode fully off.
    func fullStop() async {
        stopIdleTimer()
        wakeWord.stop()
        let c = conversation; conversation = nil
        cancellables.removeAll(); handledCalls.removeAll()
        if let c, c.state.isActive { await c.endConversation() }
        live = false; sleeping = false; mode = "idle"; caption = nil; suggestionTitle = nil; micOn = false
        await control.reportVoice(["intentLive": false, "live": false, "mode": "idle", "caption": "", "suggest": ""])
        pushHUD()
        try? AVAudioSession.sharedInstance().setActive(false, options: [.notifyOthersOnDeactivation])
    }

    private func fail(_ msg: String) {
        errorText = msg; live = false; sleeping = false; mode = "idle"; conversation = nil; pushHUD()
    }

    // Talk-from-a-session: the session Glasscode must stay locked to, plus a one-shot briefing
    // injected as the opener so he greets with that session's latest status.
    private var focusedSession: String?
    private var focusedBriefing: String?

    /// Point Talk at a specific session before `start()`, so Glasscode's tools resolve to it,
    /// the Mac's current session matches the lens, and he opens by summarizing its status.
    func focus(session id: String, briefing: String) {
        focusedSession = id
        focusedBriefing = briefing
        current = id
        Task { try? await control.setCurrent(id) }
    }

    func focusSession(_ id: String) { focus(session: id, briefing: "") }

    // MARK: - idle tracking

    private func markActivity() { lastActivity = Date() }

    private func startIdleTimer() {
        stopIdleTimer()
        idleTimer = Timer.scheduledTimer(withTimeInterval: 3, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.checkIdle() }
        }
    }
    private func stopIdleTimer() { idleTimer?.invalidate(); idleTimer = nil }

    private func checkIdle() {
        guard conversation != nil, !sleeping else { return }
        if mode == "speaking" || mode == "thinking" { markActivity(); return }  // mid-reply isn't idle
        if Date().timeIntervalSince(lastActivity) >= idleTimeout { Task { await sleep() } }
    }

    /// Make sure iOS has granted the app mic access (the ElevenLabs/LiveKit capture needs it).
    private func ensureMicPermission() async -> Bool {
        switch AVAudioApplication.shared.recordPermission {
        case .granted: return true
        case .denied: return false
        default:
            return await withCheckedContinuation { c in
                AVAudioApplication.requestRecordPermission { c.resume(returning: $0) }
            }
        }
    }

    private func configureAudioForGlasses() {
        let s = AVAudioSession.sharedInstance()
        // Route through the glasses when they're the Bluetooth audio device.
        // Standard VoIP over Bluetooth: .voiceChat + .allowBluetooth routes BOTH mic and
        // speaker to a headset (HFP). .allowBluetoothA2DP is output-only and can leave the
        // call with no usable input route, which silently breaks two-way audio.
        try? s.setCategory(.playAndRecord, mode: .voiceChat, options: [.allowBluetooth])
        try? s.setActive(true, options: [])
    }

    private func wire(_ conversation: Conversation) {
        conversation.$pendingToolCalls
            .sink { [weak self] calls in
                guard let self else { return }
                if !calls.isEmpty { self.markActivity() }
                for call in calls where !self.handledCalls.contains(call.toolCallId) {
                    self.handledCalls.insert(call.toolCallId)
                    let name = call.toolName, id = call.toolCallId
                    let params = (try? call.getParameters()) ?? [:]
                    Task { @MainActor in
                        let (result, isError) = await self.dispatch(tool: name, params: params)
                        try? await conversation.sendToolResult(for: id, result: result, isError: isError)
                    }
                }
            }
            .store(in: &cancellables)

        conversation.$isMuted
            .sink { [weak self] muted in Task { @MainActor in self?.micOn = !muted } }
            .store(in: &cancellables)

        conversation.$agentState
            .sink { [weak self] st in
                guard let self else { return }
                let s = String(describing: st).lowercased()
                self.mode = s.contains("speak") ? "speaking" : (s.contains("think") ? "thinking" : "listening")
                self.markActivity()   // user/agent turn-taking keeps the session awake
                self.pushHUD()
                Task { await self.control.reportVoice(["mode": self.mode]) }
            }
            .store(in: &cancellables)

        conversation.$messages
            .sink { [weak self] _ in self?.markActivity(); self?.pushHUD() }
            .store(in: &cancellables)

        conversation.$state
            .sink { [weak self] st in
                guard let self else { return }
                let s = String(describing: st).lowercased()
                // An unintended end (not our idle-sleep) → turn fully off.
                if (s.contains("ended") || s.contains("error")) && !self.sleeping { Task { await self.fullStop() } }
            }
            .store(in: &cancellables)
    }

    private func pushHUD() {
        let status: String
        switch (live, mode) {
        case (false, _): status = "Tap to wake Glasscode"
        case (true, "asleep"): status = caption ?? "Asleep — say “Hey Glasscode”"
        case (true, "connecting"): status = "Waking Glasscode…"
        case (true, "speaking"): status = caption ?? "Speaking…"
        default: status = "Listening…"
        }
        let curTitle = sessionCache.first { ($0["id"] as? String) == current }?["title"] as? String
        onHUD?(GlasscodeHUD(live: live, status: status, currentTitle: curTitle, needs: needsCount,
                       suggestion: suggestionTitle, lastLine: caption))
    }

    // MARK: - sessions + resolve (same shape as the IDE helper)

    @discardableResult
    private func refreshSessions(needs: Bool = false, project: String? = nil) async -> [[String: Any]] {
        if let (list, cur) = try? await control.sessions(needs: needs, project: project) {
            if !needs && project == nil { sessionCache = list; current = cur }
            needsCount = (try? await control.sessions(needs: true).list.count) ?? needsCount
            return list
        }
        return needs ? [] : sessionCache
    }
    private func resolve(_ ref: String?) -> [String: Any]? {
        let all = sessionCache
        func id(_ s: [String: Any]) -> String { (s["id"] as? String) ?? "" }
        func title(_ s: [String: Any]) -> String { (s["title"] as? String) ?? "" }
        guard let ref, !ref.trimmingCharacters(in: .whitespaces).isEmpty else { return all.first { id($0) == current } }
        let r = ref.lowercased().trimmingCharacters(in: .whitespaces)
        return all.first { id($0) == ref }
            ?? all.first { id($0).lowercased() == r }
            ?? all.first { title($0).lowercased() == r }
            ?? all.first { title($0).lowercased().contains(r) }
            ?? all.first { r.contains(title($0).lowercased()) && title($0).count > 3 }
    }
    private func compact(_ s: [String: Any]) -> [String: Any] {
        var o: [String: Any] = ["title": s["title"] ?? "", "state": s["state"] ?? ""]
        if let p = s["project"] { o["project"] = p }
        if let b = s["branch"] { o["branch"] = b }
        if let acct = s["account"] { o["account"] = acct }
        if let pr = s["pull_request"] as? [String: Any] {
            var po: [String: Any] = [:]
            if let n = pr["number"] { po["number"] = n }
            if let t = pr["title"] { po["title"] = t }
            if let a = pr["author"] { po["author"] = a }
            o["pr"] = po
        }
        if let r = s["review"], !(r is NSNull) { o["review"] = r }
        return o
    }
    private func jsonString(_ obj: Any) -> String {
        guard let d = try? JSONSerialization.data(withJSONObject: obj) else { return "{}" }
        return String(data: d, encoding: .utf8) ?? "{}"
    }
    private func intOf(_ v: Any?) -> Int? {
        if let i = v as? Int { return i }; if let d = v as? Double { return Int(d) }
        if let n = v as? NSNumber { return n.intValue }; if let s = v as? String { return Int(s) }
        return nil
    }

    // MARK: - the 16 tools (same contract as the IDE)

    private func dispatch(tool: String, params: [String: Any]) async -> (result: String, isError: Bool) {
        switch tool {
        case "list_sessions":
            let needs = (params["needs"] as? Bool) ?? false
            let proj = params["project"] as? String
            let list = await refreshSessions(needs: needs, project: proj)
            let shown = (needs || proj != nil) ? list : sessionCache
            let curTitle = sessionCache.first { ($0["id"] as? String) == current }?["title"] ?? NSNull()
            pushHUD()
            return (jsonString(["total": sessionCache.count, "needs_you": needsCount, "current": curTitle,
                                "sessions": shown.prefix(25).map { compact($0) }]), false)

        case "list_projects":
            let obj = (try? await control.getJSON("/api/projects")) as? [String: Any]
            let projects = (obj?["projects"] as? [[String: Any]]) ?? []
            return (jsonString(["projects": projects.map { ["name": $0["name"] ?? "", "repo": $0["repo"] ?? NSNull(), "sessions": $0["sessions"] ?? 0, "worktrees": $0["worktrees"] ?? 0] }]), false)

        case "list_worktrees":
            var path = "/api/worktrees"
            if let p = params["project"] as? String, !p.isEmpty { path += "?project=\(p.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) ?? p)" }
            let obj = (try? await control.getJSON(path)) as? [String: Any]
            let wts = (obj?["worktrees"] as? [[String: Any]]) ?? []
            return (jsonString(["worktrees": wts.prefix(40).map { ["project": $0["project"] ?? "", "branch": $0["branch"] ?? "", "main": $0["main"] ?? false, "sessions": $0["sessions"] ?? 0] }]), false)

        case "session_status":
            if sessionCache.isEmpty { _ = await refreshSessions() }
            guard let s = resolve(params["session"] as? String) else { return ("No session matches that.", false) }
            return (jsonString(compact(s)), false)

        case "read_transcript":
            if sessionCache.isEmpty { _ = await refreshSessions() }
            guard let s = resolve(params["session"] as? String), let id = s["id"] as? String else { return ("No session matches that.", false) }
            let text = (try? await control.transcript(id, last: intOf(params["last"]) ?? 6)) ?? ""
            let t = String(text.prefix(1600)).trimmingCharacters(in: .whitespacesAndNewlines)
            return (t.isEmpty ? "Nothing in it yet." : t, false)

        case "suggest_session":
            if sessionCache.isEmpty { _ = await refreshSessions() }
            guard let s = resolve(params["session"] as? String), let id = s["id"] as? String else { return ("No session matches that.", false) }
            suggestionTitle = s["title"] as? String; current = id
            await control.reportVoice(["suggest": id]); try? await control.setCurrent(id); pushHUD()
            return (jsonString(["ok": true, "highlighted": s["title"] ?? ""]), false)

        case "open_session":
            if sessionCache.isEmpty { _ = await refreshSessions() }
            guard let s = resolve(params["session"] as? String), let id = s["id"] as? String else { return ("No session matches that.", false) }
            do { try await control.post("/api/sessions/\(id)/open", [:]) } catch { return ("I couldn't open that.", true) }
            current = id; pushHUD()
            onOpenSession?(id, (s["title"] as? String) ?? "")
            return (jsonString(["ok": true, "opened": s["title"] ?? ""]), false)

        case "rename_session":
            if sessionCache.isEmpty { _ = await refreshSessions() }
            guard let s = resolve(params["session"] as? String), let id = s["id"] as? String else { return ("No session matches that.", false) }
            let name = (params["name"] as? String) ?? ""
            if name.trimmingCharacters(in: .whitespaces).isEmpty { return ("What should I call it?", false) }
            do { try await control.post("/api/sessions/\(id)/rename", ["name": name]) } catch { return ("Rename failed.", true) }
            _ = await refreshSessions()
            return (jsonString(["ok": true, "title": name]), false)

        case "new_session":
            var body: [String: Any] = [:]
            for k in ["project", "agent", "title", "branch", "worktree", "ticket", "from", "model", "mode", "message"] {
                if let v = params[k] as? String, !v.isEmpty { body[k] = v }
            }
            if let pr = intOf(params["pr"]) { body["pr"] = pr }
            if let rv = intOf(params["review"]) { body["review"] = rv }
            if let open = params["open"] as? Bool { body["open"] = open }
            let data = (try? await control.post("/api/sessions/new", body)) ?? Data()
            _ = await refreshSessions()
            return (jsonString((try? JSONSerialization.jsonObject(with: data)) ?? ["ok": true]), false)

        case "send_prompt":
            if sessionCache.isEmpty { _ = await refreshSessions() }
            guard let s = resolve(params["session"] as? String), let id = s["id"] as? String else { return ("Which session should I write to?", false) }
            let message = (params["message"] as? String) ?? ""
            if message.trimmingCharacters(in: .whitespaces).isEmpty { return ("There is no message to send.", false) }
            do { try await control.send(id, message) } catch { return ("That failed to send.", true) }
            caption = "→ sent to \(s["title"] ?? "")"; pushHUD()
            return (jsonString(["ok": true, "title": s["title"] ?? ""]), false)

        case "list_reviews":
            let obj = (try? await control.getJSON("/api/reviews")) as? [String: Any]
            let reviews = (obj?["reviews"] as? [[String: Any]]) ?? []
            return (jsonString(["reviews": reviews.map { ["number": $0["number"] ?? 0, "title": $0["title"] ?? "", "author": $0["author"] ?? "", "repo": $0["repo"] ?? ""] }]), false)

        case "start_review":
            guard let pr = intOf(params["pr"]) else { return ("Which PR number?", false) }
            var body: [String: Any] = [:]
            if let p = params["project"] as? String { body["project"] = p }
            if let a = params["agent"] as? String { body["agent"] = a }
            let data = (try? await control.post("/api/pr/\(pr)/review", body)) ?? Data()
            _ = await refreshSessions()
            return (jsonString((try? JSONSerialization.jsonObject(with: data)) ?? ["ok": true]), false)

        case "build_pr":
            guard let pr = intOf(params["pr"]) else { return ("Which PR number?", false) }
            var body: [String: Any] = [:]
            if let p = params["project"] as? String { body["project"] = p }
            if let m = params["message"] as? String { body["message"] = m }
            let data = (try? await control.post("/api/pr/\(pr)/build", body)) ?? Data()
            _ = await refreshSessions()
            return (jsonString((try? JSONSerialization.jsonObject(with: data)) ?? ["ok": true]), false)

        case "pr_status":
            guard let pr = intOf(params["pr"]) else { return ("Which PR number?", false) }
            var path = "/api/pr/\(pr)/status"
            if let p = params["project"] as? String, !p.isEmpty { path += "?project=\(p.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) ?? p)" }
            return (jsonString((try? await control.getJSON(path)) ?? ["ok": true]), false)

        case "today_board":
            let all = (params["all"] as? Bool) ?? false
            return (jsonString((try? await control.getJSON("/api/board" + (all ? "?all=1" : ""))) ?? [:]), false)

        case "add_task":
            let text = (params["text"] as? String) ?? ""
            if text.trimmingCharacters(in: .whitespaces).isEmpty { return ("What should I add?", false) }
            var body: [String: Any] = ["text": text]
            for k in ["due", "asked_by", "waiting_on", "list"] { if let v = params[k] as? String, !v.isEmpty { body[k] = v } }
            do { try await control.post("/api/board", body) } catch { return ("Couldn't add that.", true) }
            return (jsonString(["ok": true]), false)

        case "recall_memory":
            let q = (params["query"] as? String) ?? ""
            if q.trimmingCharacters(in: .whitespaces).isEmpty { return ("What should I look for?", false) }
            let text = (try? await control.recall(q)) ?? "{}"
            return (text, false)

        case "remember_this":
            let text = (params["text"] as? String) ?? ""
            if text.trimmingCharacters(in: .whitespaces).isEmpty { return ("What should I remember?", false) }
            do { try await control.note(text) } catch { return ("I couldn't save that.", true) }
            return (jsonString(["ok": true]), false)

        default:
            return ("Unknown tool.", true)
        }
    }
}
