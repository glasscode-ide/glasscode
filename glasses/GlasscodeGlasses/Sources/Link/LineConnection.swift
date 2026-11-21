import Foundation
import Network

/// A TCP connection that reads and writes lines (Network.framework, so App Transport Security does
/// not apply: the tailnet's WireGuard carries the encryption, the device link's handshake who is
/// who). Used by the device door and by pairing.
final class LineConnection {
    private let connection: NWConnection
    private let queue = DispatchQueue(label: "dev.glasscode.line")
    private var buffer = Data()
    private var waiting: [CheckedContinuation<Data?, Never>] = []
    private var lines: [Data] = []
    private var closed = false
    private static let maxLine = 1024 * 1024

    init(host: String, port: UInt16) {
        connection = NWConnection(host: NWEndpoint.Host(host), port: NWEndpoint.Port(rawValue: port) ?? 4519, using: .tcp)
    }

    /// `ip:port` (an IPv6 address in brackets).
    convenience init?(address: String) {
        guard let colon = address.lastIndex(of: ":"), let port = UInt16(address[address.index(after: colon)...]) else { return nil }
        var host = String(address[..<colon])
        if host.hasPrefix("["), host.hasSuffix("]") { host = String(host.dropFirst().dropLast()) }
        self.init(host: host, port: port)
    }

    /// Connect within `seconds`.
    func open(within seconds: Double = 6) async throws {
        try await withCheckedThrowingContinuation { (done: CheckedContinuation<Void, Error>) in
            var finished = false
            let finish: (Result<Void, Error>) -> Void = { result in
                guard !finished else { return }
                finished = true
                done.resume(with: result)
            }
            connection.stateUpdateHandler = { [weak self] state in
                switch state {
                case .ready:
                    finish(.success(()))
                    self?.receive()
                case .failed(let error):
                    finish(.failure(error))
                    self?.finish()
                case .waiting(let error):
                    // Unreachable for now: give up on this address (the caller tries the next).
                    finish(.failure(error))
                    self?.finish()
                case .cancelled:
                    finish(.failure(LinkError(text: "The connection was closed.")))
                    self?.finish()
                default:
                    break
                }
            }
            connection.start(queue: queue)
            queue.asyncAfter(deadline: .now() + seconds) { [weak self] in
                guard !finished else { return }
                finish(.failure(LinkError(text: "No answer in time.")))
                self?.finish()
            }
        }
    }

    private func receive() {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 64 * 1024) { [weak self] data, _, complete, error in
            guard let self else { return }
            if let data, !data.isEmpty {
                self.buffer.append(data)
                while let newline = self.buffer.firstIndex(of: 0x0A) {
                    let line = self.buffer[self.buffer.startIndex..<newline]
                    self.buffer.removeSubrange(self.buffer.startIndex...newline)
                    self.deliver(Data(line))
                }
                if self.buffer.count > Self.maxLine { self.finish() }
            }
            if complete || error != nil {
                if !self.buffer.isEmpty { self.deliver(self.buffer); self.buffer.removeAll() }
                self.finish()
            } else {
                self.receive()
            }
        }
    }

    private func deliver(_ line: Data) {
        if waiting.isEmpty { lines.append(line) } else { waiting.removeFirst().resume(returning: line) }
    }

    private func finish() {
        guard !closed else { return }
        closed = true
        connection.cancel()
        for wait in waiting { wait.resume(returning: nil) }
        waiting.removeAll()
    }

    /// The next line, or nil when the connection ended.
    func readLine() async -> Data? {
        await withCheckedContinuation { (next: CheckedContinuation<Data?, Never>) in
            queue.async {
                if !self.lines.isEmpty { next.resume(returning: self.lines.removeFirst()) }
                else if self.closed { next.resume(returning: nil) }
                else { self.waiting.append(next) }
            }
        }
    }

    /// Everything until the other side closes (an HTTP answer).
    func readAll() async -> Data {
        var out = Data()
        while let line = await readLine() {
            out.append(line)
            out.append(0x0A)
        }
        return out
    }

    func write(_ data: Data) {
        connection.send(content: data, completion: .contentProcessed { [weak self] error in
            if error != nil { self?.queue.async { self?.finish() } }
        })
    }

    func close() {
        queue.async { self.finish() }
    }

    var isClosed: Bool { queue.sync { closed } }
}
