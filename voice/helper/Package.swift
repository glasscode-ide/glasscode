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
    ],
    targets: [
        .executableTarget(
            name: "glasscode-voice",
            dependencies: [.product(name: "ElevenLabs", package: "elevenlabs-swift-sdk")]
        ),
    ]
)
