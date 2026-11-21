import Foundation

/// Where the manager acts from now, as `manager.home` says.
struct ManagerHome: Equatable {
    enum Place: String { case mac, server }
    var place: Place
    var name: String
    var initials: String
    var enabled: Bool
    var macName: String
    var serverName: String?

    init(_ json: JSON, fallbackMac: String) {
        place = Place(rawValue: json.string("home") ?? "mac") ?? .mac
        let manager = json.object("manager") ?? [:]
        name = manager.string("name") ?? "Manager"
        initials = manager.string("initials").flatMap { $0.isEmpty ? nil : $0 } ?? String(name.prefix(2)).uppercased()
        enabled = manager.bool("enabled") ?? false
        macName = json.string("mac") ?? fallbackMac
        serverName = json.string("server")
    }
}

/// One step of an answer as it comes.
struct AnswerUpdate {
    var text: String
    var activity: String?
    var done: Bool
}

/// The device link as the app uses it (G11): both ends kept connected, the manager's home asked
/// for, and each call sent where it belongs. Questions go to the home; when it cannot be reached,
/// to the other end, which says what it can.
@MainActor
final class ManagerLink: ObservableObject {
    enum Status: Equatable {
        case unpaired
        case connecting
        case connected
        case unreachable(String)
    }

    @Published private(set) var status: Status = .unpaired
    @Published private(set) var home: ManagerHome?
    @Published private(set) var macConnected = false
    @Published private(set) var serverConnected = false

    /// Events from either end (conversation items, Today, places to show).
    var onEvent: ((String, Any) -> Void)?

    private(set) var paired: Paired?
    private var mac: DoorLink?
    private var server: ServerLink?
    private let key = DeviceKey.shared
    private var keepAlive: Task<Void, Never>?
    private var partials: [String: (AnswerUpdate) -> Void] = [:]
    private var connecting = false
    private var failures = 0
    private var macLaunch = ""

    init() {
        paired = Paired.load()
        status = paired == nil ? .unpaired : .connecting
    }

    var phoneName: String { UIDeviceName.current }
    var deviceKey: DeviceKey { key }

    /// Pair with the Mac whose code was scanned, then connect.
    func pair(_ link: PairingLink) async throws {
        let paired = try await link.pair(key: key, phoneName: phoneName)
        paired.save()
        self.paired = paired
        disconnect()
        start()
    }

    func unpair() {
        disconnect()
        Paired.forget()
        paired = nil
        status = .unpaired
        home = nil
    }

    /// Keep both ends connected: now, after a drop, and when the network comes back.
    func start() {
        guard paired != nil else { status = .unpaired; return }
        keepAlive?.cancel()
        keepAlive = Task { [weak self] in
            while !Task.isCancelled {
                await self?.connectWhatIsDown()
                await self?.refreshHome()
                try? await Task.sleep(for: .seconds(self?.retryDelay ?? 10))
            }
        }
    }

    /// The app came back or the network changed: try again now.
    func nudge() {
        failures = 0
        start()
    }

    func disconnect() {
        keepAlive?.cancel()
        mac?.close(); mac = nil
        server?.close(); server = nil
        macConnected = false
        serverConnected = false
    }

    private var retryDelay: Double {
        macConnected || serverConnected ? 25 : min(30, 2 * pow(2, Double(min(failures, 4))))
    }

    private func connectWhatIsDown() async {
        guard let paired, !connecting else { return }
        connecting = true
        defer { connecting = false }
        if mac == nil {
            do {
                let link = try await DoorLink.connect(paired.mac, key: key, phoneName: phoneName)
                link.onEvent = { [weak self] name, data in Task { @MainActor in self?.event(name, data) } }
                link.onClose = { [weak self] in Task { @MainActor in self?.macClosed() } }
                mac = link
                macLaunch = link.launch
                macConnected = true
            } catch {
                macConnected = false
                if server == nil { status = .unreachable(error.localizedDescription) }
            }
        } else {
            // Still there? (A connection the Mac closed while asleep is found here.)
            if (try? await mac?.call("hello", [:], timeout: 8)) == nil { macClosed() }
        }
        if server == nil, let pinned = paired.server {
            if let link = try? await ServerLink.connect(pinned, key: key) {
                link.onClose = { [weak self] in Task { @MainActor in self?.serverClosed() } }
                server = link
                serverConnected = true
            }
        } else if server != nil {
            if (try? await server?.call("hello", [:], timeout: 10)) == nil { serverClosed() }
        }
        if macConnected || serverConnected {
            status = .connected
            failures = 0
        } else {
            failures += 1
            if case .unreachable = status {} else { status = .unreachable("Neither your Mac nor your server answers. Is Tailscale on?") }
        }
    }

    private func macClosed() {
        mac = nil
        macConnected = false
        if !serverConnected { status = .connecting }
    }

    private func serverClosed() {
        server = nil
        serverConnected = false
        if !macConnected { status = .connecting }
    }

    func refreshHome() async {
        let fallback = paired?.mac.name ?? "Mac"
        if let mac, let json = try? await mac.call("manager.home", [:], timeout: 8) as? JSON {
            home = ManagerHome(json, fallbackMac: fallback)
            return
        }
        if let server, let json = try? await server.call("manager.home", [:], timeout: 10) as? JSON {
            var found = ManagerHome(json, fallbackMac: fallback)
            if found.serverName == nil { found.serverName = server.name }
            home = found
        }
    }

    /// The end the manager is home at, else the other.
    private var homeCarrier: Carrier? {
        switch home?.place {
        case .server: return (server as Carrier?) ?? mac
        default: return (mac as Carrier?) ?? server
        }
    }

    private func other(than carrier: Carrier) -> Carrier? {
        carrier === mac ? server : mac
    }

    private func event(_ name: String, _ data: Any) {
        if name == "manager.answer", let json = data as? JSON, let ask = json.string("ask"), let update = partials[ask] {
            update(AnswerUpdate(text: json.string("text") ?? "", activity: nil, done: json.bool("done") ?? false))
        } else if name == "manager.working", let json = data as? JSON, let ask = json.string("ask"), let update = partials[ask] {
            update(AnswerUpdate(text: "", activity: json.string("line"), done: false))
        } else if name == "manager.home", let json = data as? JSON {
            home = ManagerHome(json, fallbackMac: paired?.mac.name ?? "Mac")
        }
        onEvent?(name, data)
    }

    // MARK: - the manager

    /// Ask the manager. `onUpdate` gets the answer as it is written; the result is the whole.
    func ask(_ text: String, focus: JSON, onUpdate: @escaping (AnswerUpdate) -> Void) async throws -> String {
        guard var carrier = homeCarrier else { throw LinkError(text: "Not connected to your Mac or your server.") }
        let clientID = UUID().uuidString.lowercased()
        partials[clientID] = onUpdate
        defer { partials[clientID] = nil }
        for _ in 0..<2 {
            let poll = carrier === server ? pollWriting(on: carrier, onUpdate) : nil
            defer { poll?.cancel() }
            let result = try await carrier.call("manager.ask", ["text": text, "voice": true, "focus": focus, "client_id": clientID], timeout: 180)
            let json = result as? JSON ?? [:]
            if let elsewhere = json.string("elsewhere") {
                // The other end is home now: ask there.
                if let home = json.object("home") { self.home = ManagerHome(home, fallbackMac: paired?.mac.name ?? "Mac") }
                guard let next = elsewhere == "server" ? (server as Carrier?) : (mac as Carrier?) else {
                    throw LinkError(text: elsewhere == "server" ? "The manager works from your server now, which this phone cannot reach." : "The manager works from your Mac now, which this phone cannot reach.")
                }
                carrier = next
                continue
            }
            return json.string("say") ?? ""
        }
        throw LinkError(text: "The manager could not be found.")
    }

    /// The server tells what is being written when asked: ask a few times a second.
    private func pollWriting(on carrier: Carrier, _ onUpdate: @escaping (AnswerUpdate) -> Void) -> Task<Void, Never> {
        Task { [weak carrier] in
            var last = ""
            while !Task.isCancelled, let carrier {
                try? await Task.sleep(for: .milliseconds(400))
                guard let json = try? await carrier.call("manager.conversation", ["limit": 1], timeout: 5) as? JSON,
                      let writing = json.object("writing"), let text = writing.string("text"), text != last else { continue }
                last = text
                await MainActor.run { onUpdate(AnswerUpdate(text: text, activity: nil, done: false)) }
            }
        }
    }

    func stop() async {
        _ = try? await homeCarrier?.call("manager.stop", [:], timeout: 5)
    }

    func conversation(limit: Int = 30) async -> [JSON] {
        guard let carrier = homeCarrier, let json = try? await carrier.call("manager.conversation", ["limit": limit], timeout: 10) as? JSON else { return [] }
        return json.array("items")
    }

    // MARK: - sessions, Today, approvals

    /// Sessions: the Mac's own when it answers, else the server's (with the Mac's it mirrors).
    func sessions() async -> [JSON] {
        if let mac, let json = try? await mac.call("state", [:], timeout: 15) as? JSON { return json.array("sessions") }
        if let server, let json = try? await server.call("state", [:], timeout: 15) as? JSON {
            return json.array("sessions") + json.array("mirrors")
        }
        return []
    }

    func today() async -> JSON? {
        if let mac, let json = try? await mac.call("today.get", [:], timeout: 10) as? JSON { return json }
        if let server, let json = try? await server.call("today.get", [:], timeout: 10) as? JSON {
            if let launch = json.string("launch") { server.launch = launch }
            return json
        }
        return nil
    }

    func transcript(_ session: String) async -> [JSON] {
        let carrier: Carrier? = mac ?? server
        guard let json = try? await carrier?.call("sessions.transcript", ["session_id": session, "limit": 40], timeout: 15) as? JSON else { return [] }
        return json.array("items")
    }

    func peek(_ session: String) async -> String {
        let carrier: Carrier? = mac ?? server
        guard let json = try? await carrier?.call("sessions.peek", ["session_id": session], timeout: 10) as? JSON else { return "" }
        return json.string("text") ?? ""
    }

    /// Allow or deny a card: signed by this phone, for the Mac's run that asked.
    func decide(approval: Int, allow: Bool, launch: String) async throws {
        let message = LinkProtocol.decideMessage(launch: launch, approval: approval, allow: allow)
        let op: JSON = ["op": "decide", "approval": approval, "allow": allow, "launch": launch, "key": key.openSSH, "sig": key.sign(message)]
        guard let carrier: Carrier = mac ?? server else { throw LinkError(text: "Not connected.") }
        _ = try await carrier.call("today.op", op, timeout: 15)
    }

    /// Answer a session's question (never an option that allows for good: the Mac refuses it).
    func answer(session: String, option: Int, text: String? = nil) async throws {
        var params: JSON = ["option": option]
        if let text { params["text"] = text }
        try await input(session: session, method: "sessions.answer", input: .answer(option: option, text: text), params: params)
    }

    /// Type into a session: the person's own words, signed.
    func send(session: String, text: String) async throws {
        try await input(session: session, method: "sessions.send", input: .text(text, enter: true), params: ["text": text, "enter": true])
    }

    private func input(session: String, method: String, input: LinkProtocol.Input, params: JSON) async throws {
        guard let carrier: Carrier = mac ?? server else { throw LinkError(text: "Not connected.") }
        let launch = carrier === mac ? macLaunch : (server?.launch ?? "")
        var params = params
        params["session_id"] = session
        params["signed"] = LinkProtocol.signed(input, session: session, launch: launch, key: key)
        _ = try await carrier.call(method, params, timeout: 15)
    }

    /// What the glasses are doing, to both ends (the Mac's title bar shows it).
    func presence(_ presence: JSON) {
        let (mac, server) = (mac, server)
        Task {
            _ = try? await mac?.call("device.presence", presence, timeout: 5)
            _ = try? await server?.call("device.presence", presence, timeout: 5)
        }
    }
}

import UIKit

/// The phone's name, as the Mac shows it.
enum UIDeviceName {
    @MainActor static var current: String { UIDevice.current.name }
}
