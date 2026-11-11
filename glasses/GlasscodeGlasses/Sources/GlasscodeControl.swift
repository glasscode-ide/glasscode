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
}
