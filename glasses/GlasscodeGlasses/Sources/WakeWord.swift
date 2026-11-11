import Foundation
import Speech
import AVFoundation

/// A free, on-device wake-word listener. While Glasscode is asleep it listens for "Glasscode"
/// (and close mishears) and fires `onWake`. It uses the mic ONLY while asleep — the live
/// conversation owns the mic the rest of the time — so the two never contend, and no audio
/// ever leaves the device (on-device recognition), so it costs nothing.
@MainActor
final class WakeWord {
    var onWake: (() -> Void)?
    private(set) var running = false

    private let recognizer = SFSpeechRecognizer(locale: Locale(identifier: "en_US"))
    private let engine = AVAudioEngine()
    private var request: SFSpeechAudioBufferRecognitionRequest?
    private var task: SFSpeechRecognitionTask?
    private var cycling = false

    // "glasscode" / "hey glasscode" / "hi glasscode" plus the mishears the recognizer tends to produce.
    private let needles = ["glasscode", "teddie", "tedy", " glasscode", "hey ted", "hi ted", "okay ted"]

    /// Request Speech + mic permission. True only if both are granted.
    static func authorize() async -> Bool {
        let speech = await withCheckedContinuation { (c: CheckedContinuation<SFSpeechRecognizerAuthorizationStatus, Never>) in
            SFSpeechRecognizer.requestAuthorization { c.resume(returning: $0) }
        }
        guard speech == .authorized else { return false }
        #if os(iOS)
        return await withCheckedContinuation { (c: CheckedContinuation<Bool, Never>) in
            AVAudioApplication.requestRecordPermission { c.resume(returning: $0) }
        }
        #else
        return true
        #endif
    }

    var available: Bool { (recognizer?.isAvailable ?? false) }

    func start() {
        guard !running, let recognizer, recognizer.isAvailable else { return }
        #if os(iOS)
        let session = AVAudioSession.sharedInstance()
        // Listen through the glasses Bluetooth mic, same route the conversation uses.
        try? session.setCategory(.record, mode: .measurement, options: [.allowBluetooth])
        try? session.setActive(true, options: [])
        #endif
        running = true
        launch()
    }

    private func launch() {
        guard running, let recognizer else { return }
        let req = SFSpeechAudioBufferRecognitionRequest()
        req.shouldReportPartialResults = true
        if recognizer.supportsOnDeviceRecognition { req.requiresOnDeviceRecognition = true }
        request = req

        let input = engine.inputNode
        input.removeTap(onBus: 0)
        input.installTap(onBus: 0, bufferSize: 1024, format: input.outputFormat(forBus: 0)) { [weak req] buf, _ in
            req?.append(buf)
        }
        engine.prepare()
        do { try engine.start() } catch { running = false; return }

        task = recognizer.recognitionTask(with: req) { [weak self] result, error in
            guard let self else { return }
            if let result {
                let text = result.bestTranscription.formattedString.lowercased()
                if self.needles.contains(where: { text.contains($0) }) {
                    Task { @MainActor in self.hit() }
                    return
                }
            }
            if error != nil || (result?.isFinal ?? false) {
                Task { @MainActor in self.cycle() }
            }
        }
    }

    private func hit() {
        guard running else { return }
        stop()
        onWake?()
    }

    // The on-device task finalizes after a stretch of audio/silence; relaunch while asleep.
    private func cycle() {
        guard running, !cycling else { return }
        cycling = true
        teardownTask()
        cycling = false
        launch()
    }

    private func teardownTask() {
        task?.cancel(); task = nil
        request?.endAudio(); request = nil
        engine.inputNode.removeTap(onBus: 0)
        if engine.isRunning { engine.stop() }
    }

    func stop() {
        running = false
        teardownTask()
        #if os(iOS)
        try? AVAudioSession.sharedInstance().setActive(false, options: [.notifyOthersOnDeactivation])
        #endif
    }
}
