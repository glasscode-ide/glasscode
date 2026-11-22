// swift-tools-version:5.9
import PackageDescription

// manager-voice: the manager's voice in its LiveKit room. Microphone in, the worker's speech out,
// JSON lines to Glasscode. No window, no dock icon. (The ElevenLabs copilot, glasscode-voice,
// went with the glasses plan's G16.)
let package = Package(
    name: "manager-voice",
    platforms: [.macOS(.v13)],
    dependencies: [
        .package(url: "https://github.com/livekit/client-sdk-swift.git", exact: "2.19.0"),
    ],
    targets: [
        .executableTarget(
            name: "manager-voice",
            dependencies: [.product(name: "LiveKit", package: "client-sdk-swift")]
        ),
    ]
)
