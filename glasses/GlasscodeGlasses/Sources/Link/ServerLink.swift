import Foundation
import NIOCore
import NIOPosix
import NIOSSH

/// The other carrier (G11): the server, over SSH with this phone's key, into the forced command
/// `glasscoded gate --device <id>` that pairing wrote, which speaks the same JSON lines as the
/// Mac's door (`glasscoded rpc`, the phone's allow-list). The server's host keys are pinned from
/// pairing: SSH refuses any other.
final class ServerLink: Carrier {
    let name: String
    /// The Mac's run, for input to a Mac session relayed by the server (from Today's snapshot).
    var launch = ""
    var onEvent: ((String, Any) -> Void)?
    var onClose: (() -> Void)?

    private let group = MultiThreadedEventLoopGroup(numberOfThreads: 1)
    private var connection: Channel?
    private var session: Channel?
    private let pending = Pending()

    private init(name: String) {
        self.name = name
    }

    static func connect(_ server: Paired.Server, key: DeviceKey) async throws -> ServerLink {
        let link = ServerLink(name: server.name)
        do {
            try await link.open(server, key: key)
        } catch {
            try? await link.group.shutdownGracefully()
            throw LinkError(text: "Your server cannot be reached: \(error.localizedDescription)")
        }
        return link
    }

    private func open(_ server: Paired.Server, key: DeviceKey) async throws {
        let pinned = server.host_keys.compactMap { try? NIOSSHPublicKey(openSSHPublicKey: $0) }
        guard !pinned.isEmpty else { throw LinkError(text: "Pair again: the server's keys are missing.") }
        let auth = KeyAuth(user: server.user, key: key.sshPrivateKey)
        let hosts = PinnedHosts(pinned: pinned)
        let bootstrap = ClientBootstrap(group: group)
            .connectTimeout(.seconds(8))
            .channelInitializer { channel in
                channel.eventLoop.makeCompletedFuture {
                    try channel.pipeline.syncOperations.addHandler(NIOSSHHandler(role: .client(.init(userAuthDelegate: auth, serverAuthDelegate: hosts)), allocator: channel.allocator, inboundChildChannelInitializer: nil))
                }
            }
        let connection = try await bootstrap.connect(host: server.host, port: server.port).get()
        self.connection = connection
        let lines = LineSplitter { [weak self] line in
            guard let self, let json = Lines.decode(line) else { return }
            self.pending.take(json, event: self.onEvent)
        } closed: { [weak self] in
            self?.pending.failAll(LinkError(text: "The connection to your server ended."))
            self?.onClose?()
        }
        let session: Channel = try await connection.pipeline.handler(type: NIOSSHHandler.self).flatMap { ssh in
            let promise = connection.eventLoop.makePromise(of: Channel.self)
            ssh.createChannel(promise) { child, type in
                guard type == .session else { return child.eventLoop.makeFailedFuture(LinkError(text: "not a session")) }
                return child.eventLoop.makeCompletedFuture {
                    try child.pipeline.syncOperations.addHandler(ExecRPC(lines: lines))
                }
            }
            return promise.futureResult
        }.get()
        self.session = session
    }

    func call(_ method: String, _ params: JSON = [:], timeout: Double = 20) async throws -> Any {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Any, Error>) in
            let id = pending.add(continuation)
            guard let session, let line = Lines.encode(["id": id, "method": method, "params": params]) else {
                pending.finish(id, .failure(LinkError(text: "Not connected to your server.")))
                return
            }
            var buffer = session.allocator.buffer(capacity: line.count)
            buffer.writeBytes(line)
            session.writeAndFlush(SSHChannelData(type: .channel, data: .byteBuffer(buffer)), promise: nil)
            Task { [weak self] in
                try? await Task.sleep(for: .seconds(timeout))
                self?.pending.finish(id, .failure(LinkError(text: "Your server took too long to answer.")))
            }
        }
    }

    func close() {
        session?.close(promise: nil)
        connection?.close(promise: nil)
        let group = group
        Task.detached { try? await group.shutdownGracefully() }
    }
}

/// Lines out of the session's bytes.
final class LineSplitter: @unchecked Sendable {
    private var buffer = Data()
    private let onLine: (Data) -> Void
    private let onClosed: () -> Void

    init(_ onLine: @escaping (Data) -> Void, closed: @escaping () -> Void) {
        self.onLine = onLine
        onClosed = closed
    }

    func add(_ bytes: [UInt8]) {
        buffer.append(contentsOf: bytes)
        while let newline = buffer.firstIndex(of: 0x0A) {
            let line = Data(buffer[buffer.startIndex..<newline])
            buffer.removeSubrange(buffer.startIndex...newline)
            if !line.isEmpty { onLine(line) }
        }
    }

    func closed() { onClosed() }
}

/// The session's handler: run `rpc` (the gate picks it from the command), then hand on its
/// output.
final class ExecRPC: ChannelInboundHandler, @unchecked Sendable {
    typealias InboundIn = SSHChannelData
    private let lines: LineSplitter

    init(lines: LineSplitter) {
        self.lines = lines
    }

    func handlerAdded(context: ChannelHandlerContext) {
        context.channel.setOption(ChannelOptions.allowRemoteHalfClosure, value: true).whenFailure { _ in context.close(promise: nil) }
    }

    func channelActive(context: ChannelHandlerContext) {
        context.triggerUserOutboundEvent(SSHChannelRequestEvent.ExecRequest(command: "rpc", wantReply: true), promise: nil)
        context.fireChannelActive()
    }

    func channelRead(context: ChannelHandlerContext, data: NIOAny) {
        let data = unwrapInboundIn(data)
        guard data.type == .channel, case .byteBuffer(var bytes) = data.data, let read = bytes.readBytes(length: bytes.readableBytes) else { return }
        lines.add(read)
    }

    func channelInactive(context: ChannelHandlerContext) {
        lines.closed()
        context.fireChannelInactive()
    }

    func errorCaught(context: ChannelHandlerContext, error: Error) {
        context.close(promise: nil)
    }
}

/// The phone's key, offered once.
final class KeyAuth: NIOSSHClientUserAuthenticationDelegate, @unchecked Sendable {
    private let user: String
    private let key: NIOSSHPrivateKey
    private var offered = false

    init(user: String, key: NIOSSHPrivateKey) {
        self.user = user
        self.key = key
    }

    func nextAuthenticationType(availableMethods: NIOSSHAvailableUserAuthenticationMethods, nextChallengePromise: EventLoopPromise<NIOSSHUserAuthenticationOffer?>) {
        guard !offered, availableMethods.contains(.publicKey) else {
            nextChallengePromise.succeed(nil)
            return
        }
        offered = true
        nextChallengePromise.succeed(NIOSSHUserAuthenticationOffer(username: user, serviceName: "", offer: .privateKey(.init(privateKey: key))))
    }
}

/// Only the host keys pinned at pairing.
final class PinnedHosts: NIOSSHClientServerAuthenticationDelegate, @unchecked Sendable {
    private let pinned: [NIOSSHPublicKey]

    init(pinned: [NIOSSHPublicKey]) {
        self.pinned = pinned
    }

    func validateHostKey(hostKey: NIOSSHPublicKey, validationCompletePromise: EventLoopPromise<Void>) {
        if pinned.contains(hostKey) {
            validationCompletePromise.succeed(())
        } else {
            validationCompletePromise.fail(LinkError(text: "The server's key is not the one you paired with."))
        }
    }
}
