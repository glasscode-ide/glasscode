import Foundation

// The same control plane the IDE helper uses — only the base URL + token differ (off-host).
// The base is resolved per request by `GlasscodeLink`: the LAN when both are on the same network,
// the LAN/Tailscale address on :4517. (An optional self-hosted relay can be added later for
// off-network use without Tailscale; Glasscode ships Tailscale as the default off-LAN path.)
struct GlasscodeControl {
    let token: String?

    init() {
        self.token = GlasscodeConfig.token.isEmpty ? nil : GlasscodeConfig.token
    }

    private func request(_ pathAndQuery: String, method: String = "GET", json: [String: Any]? = nil) async -> URLRequest {
        let base = await GlasscodeLink.shared.controlBase()
        var req = URLRequest(url: URL(string: pathAndQuery, relativeTo: base)!)
        req.httpMethod = method
        if let token { req.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization") }
        if let json {
            req.setValue("application/json", forHTTPHeaderField: "Content-Type")
            req.httpBody = try? JSONSerialization.data(withJSONObject: json)
        }
        return req
    }

    // A failed call drops the cached transport choice, so the next one re-probes LAN vs relay.
    private func send(_ req: URLRequest) async throws -> Data {
        do {
            let (data, _) = try await URLSession.shared.data(for: req)
            return data
        } catch {
            await GlasscodeLink.shared.invalidateRoute()
            throw error
        }
    }

    func getData(_ pathAndQuery: String) async throws -> Data {
        try await send(await request(pathAndQuery))
    }
    func getJSON(_ pathAndQuery: String) async throws -> Any {
        try JSONSerialization.jsonObject(with: try await getData(pathAndQuery))
    }
    @discardableResult
    func post(_ path: String, _ json: [String: Any]) async throws -> Data {
        try await send(await request(path, method: "POST", json: json))
    }

    func config() async throws -> [String: Any] { (try await getJSON("/api/config") as? [String: Any]) ?? [:] }
    func conversationToken() async throws -> String? {
        let data = try await post("/api/voice/token", [:])
        let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        return obj?["token"] as? String
    }
    func sessions(needs: Bool = false, project: String? = nil) async throws -> (list: [[String: Any]], current: String?) {
        var q: [String] = []
        if needs { q.append("needs=1") }
        if let project, !project.isEmpty { q.append("project=\(project.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) ?? project)") }
        let path = "/api/sessions" + (q.isEmpty ? "" : "?" + q.joined(separator: "&"))
        let obj = (try await getJSON(path) as? [String: Any]) ?? [:]
        return ((obj["sessions"] as? [[String: Any]]) ?? [], obj["current"] as? String)
    }
    func transcript(_ id: String, last: Int) async throws -> String {
        let obj = (try await getJSON("/api/sessions/\(id)/transcript?last=\(last)") as? [String: Any]) ?? [:]
        return (obj["text"] as? String) ?? ""
    }
    func setCurrent(_ id: String?) async throws { try await post("/api/current", ["id": id as Any]) }
    func send(_ id: String, _ message: String) async throws { try await post("/api/sessions/\(id)/send", ["message": message]) }
    func reportVoice(_ patch: [String: Any]) async { _ = try? await post("/api/voice/state", patch) }

    // Glasscode's memory: the digest + opener injected at session start, plus recall and notes.
    func memoryContext() async throws -> (memory: String, opener: String) {
        let obj = (try await getJSON("/api/memory/context") as? [String: Any]) ?? [:]
        return ((obj["memory"] as? String) ?? "", (obj["opener"] as? String) ?? "")
    }
    func recall(_ query: String) async throws -> String {
        String(data: try await post("/api/memory/recall", ["query": query]), encoding: .utf8) ?? "{}"
    }
    func note(_ text: String) async throws { try await post("/api/memory/note", ["text": text]) }

    // MARK: - the Manager (the one assistant above all sessions; it replaces the earlier voice copilot)

    /// A manager call's answer as an object. The control plane answers a failed manager command
    /// with `{ "error": "<a sentence for the person>" }`, which is thrown as `ManagerError`.
    private func managerObject(_ data: Data) throws -> [String: Any] {
        guard let obj = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] else {
            throw ManagerError(text: "Glasscode gave an answer I could not read.")
        }
        if let error = obj["error"] as? String { throw ManagerError(text: error) }
        return obj
    }

    /// Whether the manager is on, its name and what waits for the person.
    func managerStatus() async throws -> ManagerStatus {
        ManagerStatus(try managerObject(try await getData("/api/manager/status")))
    }

    /// Ask the manager something and wait for what it says (up to about two minutes). The
    /// question is started on the Mac and the answer fetched in short calls, so neither the
    /// relay tunnel nor a request has to stay silent for a minute.
    func managerAsk(_ text: String, voice: Bool = true) async throws -> String {
        let started = try managerObject(try await post("/api/manager/ask", ["text": text, "voice": voice, "wait": false]))
        if let say = started["say"] as? String { return say }
        guard let job = started["job"] as? String, !job.isEmpty else {
            throw ManagerError(text: "Glasscode did not take the question.")
        }
        let deadline = Date().addingTimeInterval(130)
        var misses = 0
        while Date() < deadline {
            try await Task.sleep(for: .milliseconds(700))
            let obj: [String: Any]
            do {
                obj = try managerObject(try await getData("/api/manager/ask/\(job)"))
                misses = 0
            } catch let error as ManagerError {
                throw error
            } catch {
                // The network blinked (the route is re-probed on the next call); try a few times.
                misses += 1
                if misses >= 6 { throw error }
                continue
            }
            if (obj["done"] as? Bool) == true { return (obj["say"] as? String) ?? "" }
        }
        throw ManagerError(text: "The manager took too long to answer.")
    }

    /// Today's update: lines to show one at a time and one text to read out.
    func managerBrief() async throws -> ManagerBrief {
        ManagerBrief(try managerObject(try await getData("/api/manager/brief")))
    }

    /// What this device needs to join the manager's voice room. Fails with the Mac's own
    /// sentence when voice is not set up.
    func managerVoiceToken(identity: String) async throws -> ManagerVoiceGrant {
        let obj = try managerObject(try await post("/api/manager/voice/token", ["identity": identity]))
        guard let url = obj["url"] as? String, !url.isEmpty, let token = obj["token"] as? String, !token.isEmpty else {
            throw ManagerError(text: "Voice is not set up yet.")
        }
        return ManagerVoiceGrant(url: url, token: token,
                                 room: (obj["room"] as? String) ?? "manager",
                                 identity: (obj["identity"] as? String) ?? identity)
    }

    /// Messages the manager drafted that wait for the person's OK.
    func managerMessages() async throws -> [ManagerDraft] {
        let obj = try managerObject(try await getData("/api/manager/messages"))
        return ((obj["messages"] as? [[String: Any]]) ?? []).map(ManagerDraft.init)
    }

    /// What the person asked of teammates and the manager still follows (as the Mac lists them).
    func managerFollowUps() async throws -> [[String: Any]] {
        let obj = try managerObject(try await getData("/api/manager/followups"))
        return (obj["followups"] as? [[String: Any]]) ?? []
    }
}

// MARK: - what the manager calls answer with

/// A manager command said no, in a sentence for the person ("The manager is off…").
struct ManagerError: LocalizedError, Equatable {
    let text: String
    var errorDescription: String? { text }
}

/// `glasscode manager status`.
struct ManagerStatus: Equatable {
    var enabled = false
    var name = "Manager"
    var initials = "M"
    var state = "idle"
    var caption = ""
    var needsYou = 0
    var followUpsOpen = 0
    var messagesWaiting = 0
    var home = ""
    var onServer = false

    init(_ obj: [String: Any]) {
        enabled = (obj["enabled"] as? Bool) ?? false
        if let v = obj["name"] as? String, !v.isEmpty { name = v }
        if let v = obj["initials"] as? String, !v.isEmpty { initials = v }
        if let v = obj["state"] as? String, !v.isEmpty { state = v }
        caption = (obj["caption"] as? String) ?? ""
        needsYou = (obj["needs_you"] as? NSNumber)?.intValue ?? 0
        followUpsOpen = (obj["follow_ups_open"] as? NSNumber)?.intValue ?? 0
        messagesWaiting = (obj["messages_waiting"] as? NSNumber)?.intValue ?? 0
        home = (obj["home"] as? String) ?? ""
        onServer = (obj["on_server"] as? Bool) ?? false
    }

    /// One line for the lens: "3 need you · 2 follow-ups open".
    var line: String {
        var parts: [String] = []
        if needsYou > 0 { parts.append(needsYou == 1 ? "1 needs you" : "\(needsYou) need you") }
        if followUpsOpen > 0 { parts.append("\(followUpsOpen) follow-up\(followUpsOpen == 1 ? "" : "s") open") }
        if messagesWaiting > 0 { parts.append("\(messagesWaiting) message\(messagesWaiting == 1 ? "" : "s") for your OK") }
        return parts.isEmpty ? "Nothing needs you" : parts.joined(separator: " · ")
    }
}

/// `glasscode manager brief`: today's update.
struct ManagerBrief: Equatable {
    struct Line: Equatable {
        let kind: String
        let text: String
    }

    var day = ""
    var lines: [Line] = []
    /// The whole update as one text to read out.
    var text = ""

    init(_ obj: [String: Any]) {
        day = (obj["day"] as? String) ?? ""
        text = ((obj["text"] as? String) ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        lines = ((obj["lines"] as? [[String: Any]]) ?? []).compactMap {
            let t = (($0["text"] as? String) ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            return t.isEmpty ? nil : Line(kind: ($0["kind"] as? String) ?? "", text: t)
        }
        // No lines but a text: show the text, a paragraph at a time.
        if lines.isEmpty {
            lines = text.split(separator: "\n").map { $0.trimmingCharacters(in: .whitespaces) }
                .filter { !$0.isEmpty }.map { Line(kind: "", text: $0) }
        }
        if text.isEmpty { text = lines.map(\.text).joined(separator: " ") }
    }
}

/// `glasscode manager voice-token`: where the voice room is and the pass to join it.
struct ManagerVoiceGrant {
    let url: String
    let token: String
    let room: String
    let identity: String
}

/// One message the manager drafted for a teammate (`glasscode manager message list`).
struct ManagerDraft: Equatable {
    let id: String
    let to: String
    let body: String
    let tier: String
    let state: String
    let why: String

    init(_ obj: [String: Any]) {
        id = (obj["id"] as? String) ?? ""
        to = (obj["to"] as? String) ?? ""
        body = (obj["body"] as? String) ?? ""
        tier = (obj["tier"] as? String) ?? ""
        state = (obj["state"] as? String) ?? ""
        why = (obj["why"] as? String) ?? ""
    }
}
