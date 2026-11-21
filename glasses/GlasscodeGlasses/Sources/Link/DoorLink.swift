import Foundation

/// One carrier of the device link: a JSON-lines connection that answers requests and may tell
/// the phone things by itself (events). The Mac's door and the server's SSH gate both are one.
protocol Carrier: AnyObject {
    var name: String { get }
    var launch: String { get }
    var onEvent: ((String, Any) -> Void)? { get set }
    var onClose: (() -> Void)? { get set }
    func call(_ method: String, _ params: JSON, timeout: Double) async throws -> Any
    func close()
}

/// Answers by request id, for a carrier's reader.
final class Pending {
    private var next = 1
    private var waiting: [Int: CheckedContinuation<Any, Error>] = [:]
    private let lock = NSLock()

    func add(_ continuation: CheckedContinuation<Any, Error>) -> Int {
        lock.lock(); defer { lock.unlock() }
        let id = next
        next += 1
        waiting[id] = continuation
        return id
    }

    func finish(_ id: Int, _ result: Result<Any, Error>) {
        lock.lock()
        let continuation = waiting.removeValue(forKey: id)
        lock.unlock()
        continuation?.resume(with: result)
    }

    func failAll(_ error: Error) {
        lock.lock()
        let all = waiting
        waiting.removeAll()
        lock.unlock()
        for (_, c) in all { c.resume(throwing: error) }
    }

    /// A line: an answer (with an id), or an event.
    func take(_ line: JSON, event: ((String, Any) -> Void)?) {
        if let name = line.string("event") {
            event?(name, line["data"] ?? NSNull())
            return
        }
        guard let id = line.int("id"), id != 0 else { return }
        if let error = line.string("error") {
            finish(id, .failure(LinkError(text: error)))
        } else {
            finish(id, .success(line["result"] ?? NSNull()))
        }
    }
}

/// The Mac's device door (G04): the tailnet address the Mac gave at pairing, its door key pinned.
final class DoorLink: Carrier {
    let name: String
    private(set) var launch = ""
    private(set) var macID = ""
    var onEvent: ((String, Any) -> Void)?
    var onClose: (() -> Void)?
    private let connection: LineConnection
    private let pending = Pending()

    private init(name: String, connection: LineConnection) {
        self.name = name
        self.connection = connection
    }

    /// Connect to the first of `mac`'s addresses that answers, and do the handshake.
    static func connect(_ mac: Paired.Mac, key: DeviceKey, phoneName: String) async throws -> DoorLink {
        var lastError: Error = LinkError(text: "Your Mac cannot be reached. Is it awake, with Glasscode open and Tailscale on?")
        for address in mac.addresses {
            guard let connection = LineConnection(address: address) else { continue }
            do {
                try await connection.open(within: 5)
                let link = DoorLink(name: mac.name, connection: connection)
                try await link.handshake(pinned: mac.key, key: key, phoneName: phoneName)
                link.read()
                return link
            } catch {
                connection.close()
                lastError = error
            }
        }
        throw lastError
    }

    private func handshake(pinned: String, key: DeviceKey, phoneName: String) async throws {
        guard let line = await connection.readLine(), let hello = try? JSONDecoder().decode(DoorHello.self, from: line) else {
            throw LinkError(text: "The Mac did not say hello.")
        }
        if let why = hello.check(pinned: pinned) { throw LinkError(text: why) }
        let app = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? ""
        guard let answer = try? JSONEncoder().encode(PhoneHello(answering: hello, key: key, name: phoneName, app: app)) else { throw LinkError(text: "The answer could not be made.") }
        connection.write(answer + Data([0x0A]))
        guard let line = await connection.readLine(), let welcome = try? JSONDecoder().decode(Welcome.self, from: line) else {
            throw LinkError(text: "The Mac did not answer.")
        }
        guard welcome.ok else { throw LinkError(text: welcome.error ?? "The Mac did not let this phone in.") }
        launch = hello.launch
        macID = hello.mac_id ?? ""
    }

    private func read() {
        Task.detached { [weak self] in
            while let self, let line = await self.connection.readLine() {
                guard let json = Lines.decode(line) else { continue }
                self.pending.take(json, event: self.onEvent)
            }
            self?.pending.failAll(LinkError(text: "The connection to your Mac ended."))
            self?.onClose?()
        }
    }

    func call(_ method: String, _ params: JSON = [:], timeout: Double = 20) async throws -> Any {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Any, Error>) in
            let id = pending.add(continuation)
            guard let line = Lines.encode(["id": id, "method": method, "params": params]) else {
                pending.finish(id, .failure(LinkError(text: "The request could not be made.")))
                return
            }
            connection.write(line)
            Task { [weak self] in
                try? await Task.sleep(for: .seconds(timeout))
                self?.pending.finish(id, .failure(LinkError(text: "Your Mac took too long to answer.")))
            }
        }
    }

    func close() {
        connection.close()
    }
}
