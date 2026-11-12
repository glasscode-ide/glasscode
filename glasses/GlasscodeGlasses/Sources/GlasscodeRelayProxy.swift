import Foundation
import Network

/// A loopback TCP proxy that tunnels the Mac's voice control server over the self-hosted
/// Glasscode relay (see `relay/`).
///
/// `GlasscodeControl` speaks plain HTTP to `127.0.0.1:<port>`. For each connection this proxy
/// opens one relay WebSocket (`<relay>/v1/connect?to=<mac>&channel=control`) and splices the
/// bytes both ways. The Mac's relay host joins that socket to its local `:4517`, so the phone
/// reaches the control plane from any network with no VPN and no LAN.
///
/// The relay's shared secret authorises the WebSocket (`Authorization: Bearer`); the voice
/// token still rides inside the HTTP, unchanged — the relay only passes bytes through.
final class GlasscodeRelayProxy: @unchecked Sendable {
    private let connectURL: URL
    private let secret: String
    private let deviceID: String
    private let deviceName: String
    private let queue = DispatchQueue(label: "glasscode.relay.proxy")
    private var listener: NWListener?
    private(set) var port: UInt16?

    /// - connectURL: the relay's `…/v1/connect?to=<mac>&channel=control` URL (ws/wss).
    /// - secret: the relay's shared secret.
    /// - deviceID/deviceName: how this phone names itself to the relay.
    init(connectURL: URL, secret: String, deviceID: String, deviceName: String) {
        self.connectURL = connectURL
        self.secret = secret
        self.deviceID = deviceID
        self.deviceName = deviceName
    }

    /// Start the loopback listener and return its port. Idempotent: a second call returns the
    /// port already in use.
    func start() async throws -> UInt16 {
        if let port { return port }
        let params = NWParameters.tcp
        params.requiredInterfaceType = .loopback
        let listener = try NWListener(using: params, on: .any)
        self.listener = listener
        listener.newConnectionHandler = { [weak self] conn in self?.handle(conn) }
        return try await withCheckedThrowingContinuation { (cont: CheckedContinuation<UInt16, Error>) in
            let once = OnceFlag()
            listener.stateUpdateHandler = { state in
                switch state {
                case .ready:
                    guard let raw = listener.port?.rawValue else { return }
                    self.port = raw
                    if once.claim() { cont.resume(returning: raw) }
                case .failed(let error):
                    if once.claim() { cont.resume(throwing: error) }
                case .cancelled:
                    if once.claim() { cont.resume(throwing: GlasscodeRelayError.cancelled) }
                default:
                    break
                }
            }
            listener.start(queue: queue)
        }
    }

    func stop() {
        listener?.cancel()
        listener = nil
        port = nil
    }

    // One inbound loopback connection ↔ one relay WebSocket.
    private func handle(_ conn: NWConnection) {
        conn.start(queue: queue)
        var request = URLRequest(url: connectURL)
        request.setValue("Bearer \(secret)", forHTTPHeaderField: "Authorization")
        request.setValue(deviceID, forHTTPHeaderField: "x-device-id")
        request.setValue("phone", forHTTPHeaderField: "x-device-kind")
        request.setValue(deviceName, forHTTPHeaderField: "x-device-name")
        request.timeoutInterval = 30
        let ws = URLSession.shared.webSocketTask(with: request)
        ws.resume()
        Pump(conn: conn, ws: ws).run()
    }
}

enum GlasscodeRelayError: Error { case cancelled }

/// Copies bytes between a loopback `NWConnection` and a relay `URLSessionWebSocketTask` until
/// either side ends, then tears both down.
private final class Pump: @unchecked Sendable {
    private let conn: NWConnection
    private let ws: URLSessionWebSocketTask
    private let done = OnceFlag()
    // Keep the pump alive for the life of the tunnel; nothing else retains it.
    private var keepAlive: Pump?

    init(conn: NWConnection, ws: URLSessionWebSocketTask) {
        self.conn = conn
        self.ws = ws
    }

    func run() {
        keepAlive = self
        pumpConnToWS()
        pumpWSToConn()
    }

    private func finish() {
        guard done.claim() else { return }
        ws.cancel(with: .goingAway, reason: nil)
        conn.cancel()
        keepAlive = nil
    }

    // Loopback → relay.
    private func pumpConnToWS() {
        conn.receive(minimumIncompleteLength: 1, maximumLength: 64 * 1024) { [weak self] data, _, isComplete, error in
            guard let self else { return }
            if let data, !data.isEmpty {
                self.ws.send(.data(data)) { [weak self] sendError in
                    if sendError != nil { self?.finish() } else { self?.pumpConnToWS() }
                }
            } else if isComplete || error != nil {
                self.finish()
            } else {
                self.pumpConnToWS()
            }
        }
    }

    // Relay → loopback.
    private func pumpWSToConn() {
        ws.receive { [weak self] result in
            guard let self else { return }
            switch result {
            case .success(let message):
                let data: Data
                switch message {
                case .data(let d): data = d
                case .string(let s): data = Data(s.utf8)
                @unknown default: data = Data()
                }
                if data.isEmpty {
                    self.pumpWSToConn()
                    return
                }
                self.conn.send(content: data, completion: .contentProcessed { [weak self] sendError in
                    if sendError != nil { self?.finish() } else { self?.pumpWSToConn() }
                })
            case .failure:
                self.finish()
            }
        }
    }
}

/// Resolve a continuation or run a teardown exactly once, from whichever callback fires first.
final class OnceFlag: @unchecked Sendable {
    private let lock = NSLock()
    private var used = false
    func claim() -> Bool {
        lock.lock(); defer { lock.unlock() }
        if used { return false }
        used = true
        return true
    }
}
