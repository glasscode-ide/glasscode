import Foundation

/// What this phone pinned at pairing (G05): the Mac's door, and the server when there is one. Only
/// public keys and addresses: nothing secret, so it lives in the app's defaults.
struct Paired: Codable, Equatable {
    struct Mac: Codable, Equatable {
        var name: String
        var id: String
        /// The door's key, OpenSSH form: the handshake checks the Mac against it.
        var key: String
        /// `ip:port` on the tailnet.
        var addresses: [String]
    }

    struct Server: Codable, Equatable {
        var name: String
        var host: String
        var port: Int
        var user: String
        /// The server's SSH host keys (`ssh-ed25519 AAAA…`): SSH checks the server against them.
        var host_keys: [String]
        var bin: String?
        var hostname: String?
    }

    /// This phone's device id on the Mac.
    var device: String
    var mac: Mac
    var server: Server?

    private static let key = "glasscode.paired"

    static func load() -> Paired? {
        guard let data = UserDefaults.standard.data(forKey: key) else { return nil }
        return try? JSONDecoder().decode(Paired.self, from: data)
    }

    func save() {
        if let data = try? JSONEncoder().encode(self) { UserDefaults.standard.set(data, forKey: Self.key) }
    }

    static func forget() {
        UserDefaults.standard.removeObject(forKey: key)
    }
}

/// The pairing link of the Mac's QR code: `glasscode://pair?v=2&t=<token>&a=<ip:port,…>&n=<name>`.
struct PairingLink: Equatable {
    var token: String
    var addresses: [String]
    var name: String

    init?(_ text: String) {
        guard let url = URLComponents(string: text.trimmingCharacters(in: .whitespacesAndNewlines)),
              url.scheme == "glasscode" || url.scheme == "glasscodeglasses", url.host == "pair" else { return nil }
        let items = Dictionary((url.queryItems ?? []).map { ($0.name, $0.value ?? "") }, uniquingKeysWith: { first, _ in first })
        guard let token = items["t"], !token.isEmpty, let a = items["a"], !a.isEmpty else { return nil }
        self.token = token
        addresses = a.split(separator: ",").map(String.init).filter { !$0.isEmpty }
        name = items["n"] ?? "Mac"
    }

    /// Post this phone's key to each address at once; the first answer wins.
    func pair(key: DeviceKey, phoneName: String) async throws -> Paired {
        let body: JSON = ["token": token, "name": phoneName, "key": key.openSSH]
        guard let json = Lines.encode(body) else { throw LinkError(text: "The pairing request could not be made.") }
        return try await withThrowingTaskGroup(of: Paired?.self) { group in
            for address in addresses {
                group.addTask { try? await Self.post(json, to: address) }
            }
            var lastError: Error = LinkError(text: "The Mac did not answer. Is the phone on Tailscale, and the code still on the Mac's screen?")
            for try await result in group {
                if let paired = result {
                    group.cancelAll()
                    return paired
                }
            }
            if addresses.isEmpty { lastError = LinkError(text: "The code has no address to reach the Mac at.") }
            throw lastError
        }
    }

    private static func post(_ json: Data, to address: String) async throws -> Paired {
        guard let connection = LineConnection(address: address) else { throw LinkError(text: "Not an address: \(address)") }
        try await connection.open(within: 8)
        var request = "POST /pair HTTP/1.1\r\nHost: \(address)\r\nContent-Type: application/json\r\nContent-Length: \(json.count)\r\nConnection: close\r\n\r\n".data(using: .utf8)!
        request.append(json)
        connection.write(request)
        let answer = await connection.readAll()
        connection.close()
        guard let text = String(data: answer, encoding: .utf8), let split = text.range(of: "\r\n\r\n") ?? text.range(of: "\n\n") else {
            throw LinkError(text: "The Mac's answer could not be read.")
        }
        let status = text[..<split.lowerBound].split(separator: " ").dropFirst().first.map(String.init) ?? ""
        let body = Lines.decode(Data(text[split.upperBound...].utf8)) ?? [:]
        guard status == "200" else { throw LinkError(text: body.string("error") ?? "The Mac said no (\(status)).") }
        guard let device = body.string("device"), let mac = body.object("mac"),
              let macData = try? JSONSerialization.data(withJSONObject: mac),
              let pinnedMac = try? JSONDecoder().decode(Paired.Mac.self, from: macData) else {
            throw LinkError(text: "The Mac's answer is missing its door. Update Glasscode on the Mac.")
        }
        var server: Paired.Server?
        if let s = body.object("server"), let data = try? JSONSerialization.data(withJSONObject: s) {
            server = try? JSONDecoder().decode(Paired.Server.self, from: data)
        }
        return Paired(device: device, mac: pinnedMac, server: server)
    }
}
