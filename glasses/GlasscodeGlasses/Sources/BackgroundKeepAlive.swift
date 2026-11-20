import AVFoundation
import UIKit

/// Keeps the app running while it is in the background, so the glasses stay on Glasscode.
///
/// iOS suspends an app a few seconds after it leaves the screen, and a suspended app cannot
/// keep its glasses session or start a new one. An app that plays audio is not suspended, so
/// while the person wants the glasses on Glasscode this plays silence (mixed with whatever
/// else is playing, so music is not interrupted). It stops when the app is back on screen,
/// when a talk needs the audio, and when the person disconnects.
///
/// Swiping the app away still ends it: nothing can run after that.
@MainActor
final class BackgroundKeepAlive {
    static let shared = BackgroundKeepAlive()

    /// The setting's key. On unless the person switched it off (under ⋯ on the phone screen).
    static let enabledKey = "keepAlive.enabled"

    /// Whether the glasses should stay on Glasscode right now (set by the screen).
    var wanted: () -> Bool = { false }

    private let engine = AVAudioEngine()
    private let player = AVAudioPlayerNode()
    private var wired = false
    private var inBackground = false
    /// A talk or the wake word owns the audio; silence waits until it is given back.
    private var yielded = false
    private var observers: [NSObjectProtocol] = []

    private var enabled: Bool {
        UserDefaults.standard.object(forKey: Self.enabledKey) as? Bool ?? true
    }

    private init() {
        let center = NotificationCenter.default
        observers.append(center.addObserver(forName: UIApplication.didEnterBackgroundNotification, object: nil, queue: .main) { [weak self] _ in
            Task { @MainActor in self?.inBackground = true; self?.refresh() }
        })
        observers.append(center.addObserver(forName: UIApplication.willEnterForegroundNotification, object: nil, queue: .main) { [weak self] _ in
            Task { @MainActor in self?.inBackground = false; self?.refresh() }
        })
        // A phone call or another app took the audio: start again when it is given back.
        observers.append(center.addObserver(forName: AVAudioSession.interruptionNotification, object: nil, queue: .main) { [weak self] note in
            let raw = note.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt
            guard raw == AVAudioSession.InterruptionType.ended.rawValue else { return }
            Task { @MainActor in self?.refresh() }
        })
    }

    /// A talk is about to take the audio.
    func yield() {
        yielded = true
        stop()
    }

    /// The talk gave the audio back.
    func resume() {
        yielded = false
        refresh()
    }

    /// Start or stop the silence to match what is wanted now. Call it when `wanted` changes.
    func refresh() {
        if inBackground, enabled, !yielded, wanted() { start() } else { stop() }
    }

    private func start() {
        guard !engine.isRunning else { return }
        let session = AVAudioSession.sharedInstance()
        do {
            try session.setCategory(.playback, mode: .default, options: [.mixWithOthers])
            try session.setActive(true, options: [])
            let format = engine.mainMixerNode.outputFormat(forBus: 0)
            guard format.sampleRate > 0, format.channelCount > 0 else { return }
            if !wired {
                engine.attach(player)
                engine.connect(player, to: engine.mainMixerNode, format: format)
                wired = true
            }
            let frames = AVAudioFrameCount(max(format.sampleRate, 8000))
            guard let silence = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: frames) else { return }
            silence.frameLength = frames
            try engine.start()
            player.scheduleBuffer(silence, at: nil, options: .loops)
            player.play()
        } catch {
            // Not being able to stay awake is not worth an error on screen: the glasses
            // reconnect when the app is opened again.
        }
    }

    private func stop() {
        guard engine.isRunning else { return }
        player.stop()
        engine.stop()
        // The audio session is left to whoever uses it next; a talk sets it up itself.
        if !yielded {
            try? AVAudioSession.sharedInstance().setActive(false, options: [.notifyOthersOnDeactivation])
        }
    }
}
