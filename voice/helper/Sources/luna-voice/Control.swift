import Foundation

// A thin client for the Glasscode control plane (the Node server). The helper runs on the
// same host, so it talks to loopback and needs no token; the base URL and token are still
// taken from the environment so the exact same code can point at a remote machine later.
struct Control {
    let base: URL
    let token: String?

    init() {
        let b = ProcessInfo.processInfo.environment["LUNA_CONTROL"] ?? "http://127.0.0.1:4517"
        self.base = URL(string: b) ?? URL(string: "http://127.0.0.1:4517")!
        self.token = ProcessInfo.processInfo.environment["LUNA_TOKEN"]
    }

    private func request(_ pathAndQuery: String, method: String = "GET", json: [String: Any]? = nil) -> URLRequest {
        var req = URLRequest(url: URL(string: pathAndQuery, relativeTo: base)!)
        req.httpMethod = method
        if let token { req.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization") }
        if let json {
            req.setValue("application/json", forHTTPHeaderField: "Content-Type")
            req.httpBody = try? JSONSerialization.data(withJSONObject: json)
        }
        return req
    }

    func getData(_ pathAndQuery: String) async throws -> Data {
        let (data, _) = try await URLSession.shared.data(for: request(pathAndQuery))
        return data
    }
    func getJSON(_ pathAndQuery: String) async throws -> Any {
        try JSONSerialization.jsonObject(with: try await getData(pathAndQuery))
    }
    func getText(_ pathAndQuery: String) async throws -> String {
        String(data: try await getData(pathAndQuery), encoding: .utf8) ?? ""
    }
    @discardableResult
    func post(_ path: String, _ json: [String: Any]) async throws -> Data {
        let (data, _) = try await URLSession.shared.data(for: request(path, method: "POST", json: json))
        return data
    }

    // --- the endpoints Glasscode's tools map onto ---

    func config() async throws -> [String: Any] {
        (try await getJSON("/api/config") as? [String: Any]) ?? [:]
    }
    /// A conversation token for the private agent (keeps the API key on the server).
    func conversationToken() async throws -> String? {
        let data = try await post("/api/voice/token", [:])
        let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        return (obj?["token"] as? String)
    }
    /// All sessions (optionally only the ones that need the user, or in one project) + focus.
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
    func setCurrent(_ id: String?) async throws {
        try await post("/api/current", ["id": id as Any])
    }
    func send(_ id: String, _ message: String) async throws {
        try await post("/api/sessions/\(id)/send", ["message": message])
    }
    /// Report what Glasscode is doing back to the shared state, so the IDE panel mirrors him.
    func reportVoice(_ patch: [String: Any]) async {
        _ = try? await post("/api/voice/state", patch)
    }

    // --- Glasscode's memory: digest + opener at session start, recall, and notes ---
    func memoryContext() async throws -> (memory: String, opener: String) {
        let obj = (try await getJSON("/api/memory/context") as? [String: Any]) ?? [:]
        return ((obj["memory"] as? String) ?? "", (obj["opener"] as? String) ?? "")
    }
    func recall(_ query: String) async throws -> String {
        String(data: try await post("/api/memory/recall", ["query": query]), encoding: .utf8) ?? "{}"
    }
    func note(_ text: String) async throws { try await post("/api/memory/note", ["text": text]) }
}
