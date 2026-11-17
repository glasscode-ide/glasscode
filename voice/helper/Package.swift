// swift-tools-version:5.9
import PackageDescription

// glasscode-voice — the headless voice engine Glasscode launches when you tap the moon.
// No window, no dock icon: it opens the live ElevenLabs session, does the mic + speaker
// (LiveKit WebRTC, so echo cancellation comes for free), and drives your sessions through
// the local control plane. The same ElevenLabs Swift core is what the glasses app reuses.
let package = Package(
    name: "glasscode-voice",
    platforms: [.macOS(.v13)],
    dependencies: [
        .package(url: "https://github.com/elevenlabs/elevenlabs-swift-sdk.git", branch: "main"),
        // The manager's voice talks to LiveKit directly. Pinned to the version the ElevenLabs
        // SDK already resolves (Package.resolved), so nothing new is downloaded.
        .package(url: "https://github.com/livekit/client-sdk-swift.git", exact: "2.19.0"),
    ],
    targets: [
        .executableTarget(
            name: "glasscode-voice",
            dependencies: [.product(name: "ElevenLabs", package: "elevenlabs-swift-sdk")],
            // The folder kept its earlier name; the product is glasscode-voice.
            path: "Sources/luna-voice"
        ),
        // manager-voice — the manager's voice in its LiveKit room: microphone in, the worker's
        // speech out, JSON lines to Glasscode. Headless like glasscode-voice.
        .executableTarget(
            name: "manager-voice",
            dependencies: [.product(name: "LiveKit", package: "client-sdk-swift")]
        ),
    ]
)
