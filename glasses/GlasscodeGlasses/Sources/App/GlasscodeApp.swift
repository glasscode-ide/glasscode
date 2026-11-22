import MWDATCore
import SwiftUI
import UserNotifications

/// Push (G15): the server tells the phone what needs the person while the app is away.
final class AppDelegate: NSObject, UIApplicationDelegate {
    static var onToken: ((String) -> Void)?

    func application(_ application: UIApplication, didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil) -> Bool {
        UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound, .badge]) { granted, _ in
            guard granted else { return }
            DispatchQueue.main.async { application.registerForRemoteNotifications() }
        }
        return true
    }

    func application(_ application: UIApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        Self.onToken?(deviceToken.map { String(format: "%02x", $0) }.joined())
    }

    func application(_ application: UIApplication, didFailToRegisterForRemoteNotificationsWithError error: Error) {}
}

/// Glasscode for Ray-Ban Display (G11): one app, the iPhone and the glasses, built around the
/// manager. A pairing link (`glasscode://pair?…`) pairs; the toolkit's own links go to it.
@main
struct GlasscodeApp: App {
    @StateObject private var app: AppModel
    @UIApplicationDelegateAdaptor(AppDelegate.self) private var delegate

    init() {
        do {
            try Wearables.configure()
        } catch {
            assertionFailure("Wearables SDK configuration failed: \(error)")
        }
        let model = AppModel()
        _app = StateObject(wrappedValue: model)
        AppDelegate.onToken = { [weak model] token in
            #if DEBUG
            let sandbox = true
            #else
            let sandbox = false
            #endif
            Task { @MainActor in model?.pushToken = (token, sandbox) }
        }
    }

    var body: some Scene {
        WindowGroup {
            // Under the unit tests the app is only a host: the tests make their own session
            // against simulated glasses, and the real screen would compete for them.
            if ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] != nil {
                Color.clear
            } else {
                HomeView(app: app, link: app.link, glasses: app.glasses)
                    .onAppear { app.start() }
                    .onOpenURL { url in
                        if let pairing = PairingLink(url.absoluteString) {
                            Task { try? await app.link.pair(pairing); app.start() }
                            return
                        }
                        guard let components = URLComponents(url: url, resolvingAgainstBaseURL: false),
                              components.queryItems?.contains(where: { $0.name == "metaWearablesAction" }) == true else { return }
                        Task { _ = try? await Wearables.shared.handleUrl(url) }
                    }
            }
        }
    }
}
