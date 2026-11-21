import AVFoundation

/// Where voice goes in and out, decided in one place for everything that opens the microphone.
///
/// The glasses end the app's session when their microphone opens as a hands-free call
/// (Bluetooth HFP) while the session runs. So by default the microphone is the phone's and
/// only the sound goes to the glasses (Bluetooth A2DP), which leaves the session alone.
/// The person can switch the microphone back to the glasses on the phone screen (under ⋯).
@MainActor
enum AudioRoute {
    /// The setting's key: true = the glasses' microphone (hands-free), false = the phone's.
    static let micOnGlassesKey = "voice.micOnGlasses"

    static var micOnGlasses: Bool { UserDefaults.standard.bool(forKey: micOnGlassesKey) }

    private static var routeWatch: NSObjectProtocol?

    /// A two-way talk: the phone listens and the manager speaks (G13).
    static func configureForTalk() {
        BackgroundKeepAlive.shared.yield()
        let session = AVAudioSession.sharedInstance()
        if micOnGlasses {
            // The glasses are both microphone and speaker: a hands-free call.
            try? session.setCategory(.playAndRecord, mode: .voiceChat, options: [.allowBluetooth])
            try? session.setActive(true, options: [])
            return
        }
        // The phone's microphone, the glasses' speakers (A2DP): no hands-free call, which would
        // end the lens's session. No voice processing either: iOS turns the session into a call
        // for it.
        try? session.setCategory(.playAndRecord, mode: .default, options: [.allowBluetoothA2DP, .defaultToSpeaker])
        try? session.setActive(true, options: [])
        keepPhoneMic()
        watchRoute()
    }

    /// Listening only (the wake word).
    static func configureForListening() {
        BackgroundKeepAlive.shared.yield()
        let session = AVAudioSession.sharedInstance()
        try? session.setCategory(.record, mode: .measurement, options: micOnGlasses ? [.allowBluetooth] : [])
        try? session.setActive(true, options: [])
    }

    /// Give the audio back when a talk or the listener ends.
    static func release() {
        if let routeWatch { NotificationCenter.default.removeObserver(routeWatch) }
        routeWatch = nil
        try? AVAudioSession.sharedInstance().setActive(false, options: [.notifyOthersOnDeactivation])
        BackgroundKeepAlive.shared.resume()
    }

    /// If iOS moved the microphone to the glasses anyway, move it back to the phone.
    private static func keepPhoneMic() {
        let session = AVAudioSession.sharedInstance()
        guard session.currentRoute.inputs.contains(where: { $0.portType == .bluetoothHFP }) else { return }
        if let phone = session.availableInputs?.first(where: { $0.portType == .builtInMic }) {
            try? session.setPreferredInput(phone)
        }
    }

    private static func watchRoute() {
        guard routeWatch == nil else { return }
        routeWatch = NotificationCenter.default.addObserver(
            forName: AVAudioSession.routeChangeNotification, object: nil, queue: .main
        ) { _ in
            Task { @MainActor in
                if !micOnGlasses { keepPhoneMic() }
            }
        }
    }
}
