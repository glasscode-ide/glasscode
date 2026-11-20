import SwiftUI
import MWDATCore

@main
struct GlasscodeGlassesApp: App {
    init() {
        do {
            try Wearables.configure()
        } catch {
            assertionFailure("Wearables SDK configuration failed: \(error)")
        }
    }

    var body: some Scene {
        WindowGroup {
            // Under the unit tests the app is only a host: the tests make their own session
            // against simulated glasses, and the real screen would compete for them.
            if ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] != nil {
                Color.clear
            } else {
                ContentView()
                    .onOpenURL { url in
                        guard
                            let components = URLComponents(url: url, resolvingAgainstBaseURL: false),
                            components.queryItems?.contains(where: { $0.name == "metaWearablesAction" }) == true
                        else { return }
                        Task { _ = try? await Wearables.shared.handleUrl(url) }
                    }
            }
        }
    }
}
