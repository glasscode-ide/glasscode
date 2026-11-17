import Foundation
import Combine
import ElevenLabs

// glasscode-voice — headless. Launched by Glasscode when the moon goes live; killed when it goes
// idle. It opens one ElevenLabs conversation, lets the user talk, and turns Glasscode's tool calls
// into control-plane requests against the user's real sessions. State is reported back so the
// IDE moon mirrors what she's doing.

@MainActor
final class GlasscodeHelper {
    let control = Control()
    var conversation: Conversation?
    var cancellables = Set<AnyCancellable>()
    var handledCalls = Set<String>()

    // a small cache of sessions so tool calls can resolve a title to an id
    var sessionCache: [[String: Any]] = []
    var current: String?

    // Idle auto-sleep + "Hey Glasscode" wake: go quiet → drop the paid ElevenLabs session but keep
    // the helper alive, listening on-device for "Hey Glasscode" to re-open it. Saves cost without
    // the IDE losing the moon.
    let wakeWord = WakeWord()
    var idleTimer: DispatchSourceTimer?
    var lastActivity = Date()
    let idleTimeout: TimeInterval = Double(ProcessInfo.processInfo.environment["GLASSCODE_IDLE_SECS"] ?? "") ?? 45
    var sleeping = false
    var wakeReady = false
    var mode = "idle"
    var agentId: String?

    nonisolated init() {}

    func log(_ s: String) { FileHandle.standardError.write(Data(("[glasscode-voice] " + s + "\n").utf8)) }

    // ---- session resolution (mirror of the web app's fuzzy resolve) ----
    func refreshSessions(needs: Bool = false) async -> [[String: Any]] {
        if let (list, cur) = try? await control.sessions(needs: needs) {
            if !needs { sessionCache = list; current = cur }
            return list
        }
        return needs ? [] : sessionCache
    }
    func resolve(_ ref: String?) -> [String: Any]? {
        let all = sessionCache
        func id(_ s: [String: Any]) -> String { (s["id"] as? String) ?? "" }
        func title(_ s: [String: Any]) -> String { (s["title"] as? String) ?? "" }
        guard let ref, !ref.trimmingCharacters(in: .whitespaces).isEmpty else {
            return all.first { id($0) == current } ?? nil
        }
        let r = ref.lowercased().trimmingCharacters(in: .whitespaces)
        return all.first { id($0) == ref }
            ?? all.first { id($0).lowercased() == r }
            ?? all.first { title($0).lowercased() == r }
            ?? all.first { title($0).lowercased().contains(r) }
            ?? all.first { r.contains(title($0).lowercased()) && title($0).count > 3 }
    }
    func compact(_ s: [String: Any]) -> [String: Any] {
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
    func jsonString(_ obj: Any) -> String {
        guard let d = try? JSONSerialization.data(withJSONObject: obj) else { return "{}" }
        return String(data: d, encoding: .utf8) ?? "{}"
    }
    func intOf(_ v: Any?) -> Int? {
        if let i = v as? Int { return i }
        if let d = v as? Double { return Int(d) }
        if let n = v as? NSNumber { return n.intValue }
        if let s = v as? String { return Int(s) }
        return nil
    }

    func dispatch(tool: String, params: [String: Any]) async -> (result: String, isError: Bool) {
        switch tool {
        case "list_sessions":
            let needs = (params["needs"] as? Bool) ?? false
            let proj = (params["project"] as? String)
            let list = try? await control.sessions(needs: needs, project: proj)
            let shown = list?.list ?? (needs ? [] : sessionCache)
            if !needs && proj == nil, let cur = list?.current { current = cur }
            if !needs && proj == nil, let l = list?.list { sessionCache = l }
            let needCount = sessionCache.filter { (($0["state"] as? String) ?? "").lowercased().contains("needs") }.count
            let curTitle = sessionCache.first { ($0["id"] as? String) == current }?["title"] ?? NSNull()
            return (jsonString([
                "total": sessionCache.count,
                "needs_you": needCount,
                "current": curTitle,
                "sessions": shown.prefix(25).map { compact($0) },
            ]), false)

        case "list_projects":
            let obj = (try? await control.getJSON("/api/projects")) as? [String: Any]
            let projects = (obj?["projects"] as? [[String: Any]]) ?? []
            let out = projects.map { p -> [String: Any] in
                ["name": p["name"] ?? "", "repo": p["repo"] ?? NSNull(), "sessions": p["sessions"] ?? 0, "worktrees": p["worktrees"] ?? 0]
            }
            return (jsonString(["projects": out]), false)

        case "list_worktrees":
            var path = "/api/worktrees"
            if let p = params["project"] as? String, !p.isEmpty { path += "?project=\(p.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) ?? p)" }
            let obj = (try? await control.getJSON(path)) as? [String: Any]
            let wts = (obj?["worktrees"] as? [[String: Any]]) ?? []
            let out = wts.prefix(40).map { w -> [String: Any] in
                ["project": w["project"] ?? "", "branch": w["branch"] ?? "", "main": w["main"] ?? false, "sessions": w["sessions"] ?? 0]
            }
            return (jsonString(["worktrees": out]), false)

        case "session_status":
            if sessionCache.isEmpty { _ = await refreshSessions() }
            guard let s = resolve(params["session"] as? String) else {
                return ("No session matches that — want me to list them?", false)
            }
            return (jsonString(compact(s)), false)

        case "read_transcript":
            if sessionCache.isEmpty { _ = await refreshSessions() }
            guard let s = resolve(params["session"] as? String), let id = s["id"] as? String else {
                return ("No session matches that.", false)
            }
            let last = intOf(params["last"]) ?? 6
            let text = (try? await control.transcript(id, last: last)) ?? ""
            let trimmed = String(text.prefix(1600)).trimmingCharacters(in: .whitespacesAndNewlines)
            return (trimmed.isEmpty ? "Nothing in it yet." : trimmed, false)

        case "suggest_session":
            if sessionCache.isEmpty { _ = await refreshSessions() }
            guard let s = resolve(params["session"] as? String), let id = s["id"] as? String else {
                return ("No session matches that.", false)
            }
            await control.reportVoice(["suggest": id])
            current = id
            try? await control.setCurrent(id)
            return (jsonString(["ok": true, "highlighted": s["title"] ?? ""]), false)

        case "open_session":
            if sessionCache.isEmpty { _ = await refreshSessions() }
            guard let s = resolve(params["session"] as? String), let id = s["id"] as? String else {
                return ("No session matches that.", false)
            }
            do { try await control.post("/api/sessions/\(id)/open", [:]) }
            catch { return ("I couldn't open that.", true) }
            current = id
            return (jsonString(["ok": true, "opened": s["title"] ?? ""]), false)

        case "rename_session":
            if sessionCache.isEmpty { _ = await refreshSessions() }
            guard let s = resolve(params["session"] as? String), let id = s["id"] as? String else {
                return ("No session matches that.", false)
            }
            let name = (params["name"] as? String) ?? ""
            if name.trimmingCharacters(in: .whitespaces).isEmpty { return ("What should I call it?", false) }
            do { try await control.post("/api/sessions/\(id)/rename", ["name": name]) }
            catch { return ("Rename failed.", true) }
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
            let obj = (try? JSONSerialization.jsonObject(with: data)) ?? ["ok": true]
            _ = await refreshSessions()
            return (jsonString(obj), false)

        case "send_prompt":
            if sessionCache.isEmpty { _ = await refreshSessions() }
            guard let s = resolve(params["session"] as? String), let id = s["id"] as? String else {
                return ("Which session should I write to? I could not tell.", false)
            }
            let message = (params["message"] as? String) ?? ""
            if message.trimmingCharacters(in: .whitespaces).isEmpty {
                return ("There is no message to send.", false)
            }
            do { try await control.send(id, message) }
            catch { return ("That failed to send.", true) }
            await control.reportVoice(["caption": "→ sent to \(s["title"] ?? "")"])
            return (jsonString(["ok": true, "title": s["title"] ?? ""]), false)

        case "list_reviews":
            let obj = (try? await control.getJSON("/api/reviews")) as? [String: Any]
            let reviews = (obj?["reviews"] as? [[String: Any]]) ?? []
            let out = reviews.map { r -> [String: Any] in
                ["number": r["number"] ?? 0, "title": r["title"] ?? "", "author": r["author"] ?? "", "repo": r["repo"] ?? ""]
            }
            return (jsonString(["reviews": out]), false)

        case "start_review":
            guard let pr = intOf(params["pr"]) else { return ("Which PR number?", false) }
            var body: [String: Any] = [:]
            if let p = params["project"] as? String { body["project"] = p }
            if let a = params["agent"] as? String { body["agent"] = a }
            let data = (try? await control.post("/api/pr/\(pr)/review", body)) ?? Data()
            let obj = (try? JSONSerialization.jsonObject(with: data)) ?? ["ok": true]
            _ = await refreshSessions()
            return (jsonString(obj), false)

        case "build_pr":
            guard let pr = intOf(params["pr"]) else { return ("Which PR number?", false) }
            var body: [String: Any] = [:]
            if let p = params["project"] as? String { body["project"] = p }
            if let m = params["message"] as? String { body["message"] = m }
            let data = (try? await control.post("/api/pr/\(pr)/build", body)) ?? Data()
            let obj = (try? JSONSerialization.jsonObject(with: data)) ?? ["ok": true]
            _ = await refreshSessions()
            return (jsonString(obj), false)

        case "pr_status":
            guard let pr = intOf(params["pr"]) else { return ("Which PR number?", false) }
            var path = "/api/pr/\(pr)/status"
            if let p = params["project"] as? String, !p.isEmpty { path += "?project=\(p.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) ?? p)" }
            let obj = (try? await control.getJSON(path)) ?? ["ok": true]
            return (jsonString(obj), false)

        case "today_board":
            let all = (params["all"] as? Bool) ?? false
            let obj = (try? await control.getJSON("/api/board" + (all ? "?all=1" : ""))) ?? [:]
            return (jsonString(obj), false)

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

    // ---- lifecycle ----
    func start() async {
        _ = await refreshSessions()
        let cfg = (try? await control.config()) ?? [:]
        guard let agentId = cfg["agentId"] as? String, !agentId.isEmpty else {
            log("no agentId configured; exiting"); exit(2)
        }
        self.agentId = agentId
        // Pre-authorize the wake listener in the background so sleep→wake is seamless.
        Task { self.wakeReady = await WakeWord.authorize() }
        await connect()
    }

    /// Open (or re-open) the ElevenLabs conversation, injecting memory so Glasscode continues.
    private func connect() async {
        guard conversation == nil, let agentId else { return }
        var vars: [String: String] = [:]
        if let (memory, opener) = try? await control.memoryContext() {
            if !memory.isEmpty { vars["memory"] = memory }
            if !opener.isEmpty { vars["opener"] = opener }
        }
        let vc = ConversationConfig(dynamicVariables: vars.isEmpty ? nil : vars)
        do {
            let token = try? await control.conversationToken()
            if let token, !token.isEmpty {
                conversation = try await ElevenLabs.startConversation(tokenProvider: { token }, config: vc)
            } else {
                conversation = try await ElevenLabs.startConversation(agentId: agentId, config: vc)
            }
        } catch {
            log("failed to start conversation: \(error)")
            await control.reportVoice(["live": false, "mode": "idle", "caption": ""])
            exit(3)
        }
        guard let conversation else { exit(3) }
        log(sleeping ? "conversation re-opened (woke)" : "conversation started")
        sleeping = false; mode = "listening"; markActivity()
        await control.reportVoice(["live": true, "mode": "listening", "caption": ""])
        wire(conversation)
        startIdleTimer()
    }

    private func wire(_ conversation: Conversation) {
        // tool calls
        conversation.$pendingToolCalls
            .sink { [weak self] calls in
                guard let self else { return }
                if !calls.isEmpty { self.markActivity() }
                for call in calls where !self.handledCalls.contains(call.toolCallId) {
                    self.handledCalls.insert(call.toolCallId)
                    let name = call.toolName
                    let params = (try? call.getParameters()) ?? [:]
                    let id = call.toolCallId
                    Task { @MainActor in
                        self.log("tool: \(name) \(params)")
                        let (result, isError) = await self.dispatch(tool: name, params: params)
                        try? await conversation.sendToolResult(for: id, result: result, isError: isError)
                    }
                }
            }
            .store(in: &cancellables)

        // agent state → mode (and keep the session awake while turn-taking)
        conversation.$agentState
            .sink { [weak self] st in
                guard let self else { return }
                let s = String(describing: st).lowercased()
                let m = s.contains("speak") ? "speaking" : (s.contains("think") ? "thinking" : "listening")
                self.mode = m
                self.markActivity()
                Task { await self.control.reportVoice(["mode": m]) }
            }
            .store(in: &cancellables)

        // connection state → teardown only on an UNINTENDED end (not our idle-sleep)
        conversation.$state
            .sink { [weak self] st in
                guard let self else { return }
                let s = String(describing: st).lowercased()
                if (s.contains("ended") || s.contains("error")) && !self.sleeping {
                    Task { await self.control.reportVoice(["live": false, "mode": "idle", "caption": ""]); try? await Task.sleep(nanoseconds: 150_000_000); exit(0) }
                }
            }
            .store(in: &cancellables)
    }

    // ---- idle auto-sleep + "Hey Glasscode" wake ----
    func markActivity() { lastActivity = Date() }

    private func startIdleTimer() {
        stopIdleTimer()
        // GCD timer (not Timer) because the helper runs under dispatchMain(), not a RunLoop.
        let t = DispatchSource.makeTimerSource(queue: .main)
        t.schedule(deadline: .now() + 3, repeating: 3)
        t.setEventHandler { [weak self] in Task { @MainActor in self?.checkIdle() } }
        idleTimer = t
        t.resume()
    }
    private func stopIdleTimer() { idleTimer?.cancel(); idleTimer = nil }

    private func checkIdle() {
        guard conversation != nil, !sleeping else { return }
        if mode == "speaking" || mode == "thinking" { markActivity(); return }
        if Date().timeIntervalSince(lastActivity) >= idleTimeout { Task { await sleep() } }
    }

    /// Idle too long → drop the paid session; stay alive listening for "Hey Glasscode".
    private func sleep() async {
        guard conversation != nil, !sleeping else { return }
        sleeping = true
        stopIdleTimer()
        let c = conversation; conversation = nil
        cancellables.removeAll(); handledCalls.removeAll()
        if let c, c.state.isActive { await c.endConversation() }
        mode = "asleep"
        if wakeReady || wakeWord.available {
            wakeWord.onWake = { [weak self] in Task { @MainActor in await self?.wake() } }
            wakeWord.start()
            await control.reportVoice(["mode": "asleep", "caption": "Say “Hey Glasscode” to wake me"])
            log("asleep — listening for Hey Glasscode")
        } else {
            // No speech permission → idle-off cleanly (cost still saved; re-open via the moon).
            await control.reportVoice(["live": false, "mode": "idle", "caption": ""])
            log("idle — no wake permission; exiting")
            try? await Task.sleep(nanoseconds: 150_000_000); exit(0)
        }
    }

    /// "Hey Glasscode" heard while asleep → re-open the conversation.
    func wake() async {
        guard sleeping else { return }
        wakeWord.stop()
        sleeping = false
        await connect()
    }

    func stop() async {
        stopIdleTimer(); wakeWord.stop()
        if let c = conversation, c.state.isActive { await c.endConversation() }
        await control.reportVoice(["live": false, "mode": "idle", "caption": ""])
    }
}

// ---- boot ----
let helper = GlasscodeHelper()
var signalSources: [DispatchSourceSignal] = []

// End cleanly when Glasscode kills us (idle click) or on Ctrl-C.
for sig in [SIGINT, SIGTERM] {
    signal(sig, SIG_IGN)
    let src = DispatchSource.makeSignalSource(signal: sig, queue: .main)
    src.setEventHandler { Task { @MainActor in await helper.stop(); exit(0) } }
    src.resume()
    signalSources.append(src) // keep alive for the process lifetime
}

Task { @MainActor in await helper.start() }
dispatchMain()
