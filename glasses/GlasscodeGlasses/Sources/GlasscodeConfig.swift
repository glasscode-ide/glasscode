import Foundation

// Where Glasscode on the glasses reaches Glasscode. The phone is OFF-host, so it talks to the
// Mac's control plane over the network and must present the shared token.
//
//   • Same Wi-Fi as the Mac → use the LAN address.
//   • Anywhere (if the phone has Tailscale) → use the Tailscale address; it's stable.
//
// Find/rotate the token in voice/.glasscode-token on the Mac (printed on server start).
enum GlasscodeConfig {
    // Pick one. LAN works at your desk; Tailscale works from anywhere both devices share it.
    static let controlBaseURL = "http://192.168.1.20:4517"          // the Mac on your Wi-Fi
    // static let controlBaseURL = "http://100.101.102.103:4517"   // Tailscale (works on any network)

    static let token = ""   // paste the token the voice server prints on start

    // MARK: - Optional self-hosted relay
    //
    // For when you can't put both devices on one Tailscale tailnet. Deploy the relay
    // (see relay/README.md), run `glasscoded relay <url> <secret>` on the Mac, then set
    // these to the same values and flip `useRelay` to true. The relay carries the control
    // plane from any network; `controlBaseURL` above is then ignored.
    static let useRelay = false
    static let relayURL = "wss://relay.example.com"   // your relay (ws:// or wss://)
    static let relaySecret = ""                        // the shared secret, same on every device
    static let macDeviceID = ""                        // the Mac's device id (printed by `glasscoded relay …`)
    static let phoneDeviceID = "phone"                 // this phone's label (any unique string)
    static let phoneDeviceName = "iPhone"
}
