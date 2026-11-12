import Foundation

/// Glasscode's link to the Mac's control plane. By default this is a plain LAN connection: the
/// phone and the Mac are on the same network (Wi-Fi, a shared hotspot, or Tailscale), and the
/// glasses reach the Mac's voice server directly on `:4517` at `GlasscodeConfig.controlBaseURL`.
///
/// When `GlasscodeConfig.useRelay` is on, control calls instead go through the self-hosted
/// Glasscode relay (see `relay/`): a loopback proxy on the phone tunnels them over one WebSocket
/// to the Mac, so the phone reaches it from any network with no VPN and no shared LAN.
@MainActor
final class GlasscodeLink: ObservableObject {
    static let shared = GlasscodeLink()

    /// Where control calls go right now, for the HUD: "LAN" or "Relay".
    @Published var transport: String = "LAN"

    /// The persistent loopback proxy, created the first time the relay is used.
    private var relayProxy: GlasscodeRelayProxy?

    /// The base URL `GlasscodeControl` should use. With the relay on, this is the loopback proxy
    /// (`http://127.0.0.1:<port>`); otherwise the configured LAN/Tailscale address on `:4517`.
    func controlBase() async -> URL {
        if GlasscodeConfig.useRelay, let connectURL = relayConnectURL() {
            do {
                let proxy = relayProxy ?? GlasscodeRelayProxy(
                    connectURL: connectURL,
                    secret: GlasscodeConfig.relaySecret,
                    deviceID: GlasscodeConfig.phoneDeviceID,
                    deviceName: GlasscodeConfig.phoneDeviceName
                )
                relayProxy = proxy
                let port = try await proxy.start()
                transport = "Relay"
                return URL(string: "http://127.0.0.1:\(port)")!
            } catch {
                // If the loopback proxy can't even start, fall back to the LAN/Tailscale address.
                relayProxy = nil
            }
        }
        transport = "LAN"
        return URL(string: GlasscodeConfig.controlBaseURL) ?? URL(string: "http://127.0.0.1:4517")!
    }

    /// `<relay>/v1/connect?to=<mac>&channel=control` as a ws/wss URL, or nil if not configured.
    private func relayConnectURL() -> URL? {
        guard !GlasscodeConfig.relaySecret.isEmpty,
              !GlasscodeConfig.macDeviceID.isEmpty,
              var comps = URLComponents(string: GlasscodeConfig.relayURL) else { return nil }
        switch comps.scheme?.lowercased() {
        case "http": comps.scheme = "ws"
        case "https": comps.scheme = "wss"
        case "ws", "wss": break
        default: comps.scheme = "wss"
        }
        var path = comps.path
        if path.hasSuffix("/") { path.removeLast() }
        comps.path = path + "/v1/connect"
        comps.queryItems = [
            URLQueryItem(name: "to", value: GlasscodeConfig.macDeviceID),
            URLQueryItem(name: "channel", value: "control"),
        ]
        return comps.url
    }

    /// A failed control call: keep the persistent proxy up (individual tunnels are per-request),
    /// so there is no cached route to drop here.
    func invalidateRoute() {}

    /// Tear the relay tunnel down (e.g. when turning the relay off); the next call re-probes.
    func resetTransport() {
        relayProxy?.stop()
        relayProxy = nil
        transport = "LAN"
    }
}
