import AVFoundation
import Foundation
import Speech

/// Speech on the phone (G13, decision O3): what the person says is turned into text on the phone
/// itself (on-device recognition, nothing but text leaves it), and what the manager says is spoken
/// with the system's voices, sentence by sentence as it is written, to the glasses' speakers.
///
/// `TurnTaking` is the state machine, pure, so the tests drive it with no microphone.
struct TurnTaking: Equatable {
    enum State: Equatable {
        case idle
        case listening(heard: String)
        case thinking(asked: String)
        case speaking
    }

    private(set) var state: State = .idle
    /// When the words last changed while listening (the pause is measured from it).
    private(set) var changedAt: Date?
    /// The pause that ends a turn.
    var pause: TimeInterval = 0.7

    /// The person starts to talk (a pinch, or the wake word). A pinch while it speaks stops it.
    mutating func talk(now: Date) {
        state = .listening(heard: "")
        changedAt = now
    }

    /// More words heard.
    mutating func heard(_ text: String, now: Date) {
        guard case .listening(let before) = state, text != before else { return }
        state = .listening(heard: text)
        changedAt = now
    }

    /// The clock ticks while listening: after a pause with words, the turn is over and they go.
    /// Returns what to ask, once.
    mutating func tick(now: Date) -> String? {
        guard case .listening(let heard) = state, let changedAt else { return nil }
        let words = heard.trimmingCharacters(in: .whitespacesAndNewlines)
        // Nothing said for a long time: back to rest.
        if words.isEmpty {
            if now.timeIntervalSince(changedAt) > 8 { state = .idle }
            return nil
        }
        guard now.timeIntervalSince(changedAt) >= pause else { return nil }
        state = .thinking(asked: words)
        return words
    }

    /// The person cancels (swipe left) while listening.
    mutating func cancel() {
        if case .listening = state { state = .idle }
    }

    mutating func speaking() { state = .speaking }
    mutating func done() { state = .idle }

    var isListening: Bool { if case .listening = state { return true } else { return false } }

    /// The word the Mac's title bar shows.
    var word: String {
        switch state {
        case .idle: return "idle"
        case .listening: return "listening"
        case .thinking: return "thinking"
        case .speaking: return "speaking"
        }
    }
}

/// Sentences out of text as it streams: each finished sentence once.
struct Sentences {
    private var said = 0

    /// The sentences of `text` (the whole so far) that end now; `final` takes the rest too.
    mutating func next(_ text: String, final: Bool) -> [String] {
        let chars = Array(text)
        guard said < chars.count else { return [] }
        var out: [String] = []
        var start = said
        var i = said
        while i < chars.count {
            let c = chars[i]
            let ends = ".!?\n".contains(c) && (i + 1 == chars.count ? final : chars[i + 1].isWhitespace)
            if ends {
                let sentence = String(chars[start...i]).trimmingCharacters(in: .whitespacesAndNewlines)
                if !sentence.isEmpty { out.append(sentence) }
                start = i + 1
            }
            i += 1
        }
        if final, start < chars.count {
            let rest = String(chars[start...]).trimmingCharacters(in: .whitespacesAndNewlines)
            if !rest.isEmpty { out.append(rest) }
            start = chars.count
        }
        said = start
        return out
    }
}

/// The microphone and on-device recognition.
@MainActor
final class Listener: NSObject {
    private let engine = AVAudioEngine()
    private var request: SFSpeechAudioBufferRecognitionRequest?
    private var task: SFSpeechRecognitionTask?
    private let recognizer = SFSpeechRecognizer(locale: Locale.current) ?? SFSpeechRecognizer(locale: Locale(identifier: "en-US"))
    var onWords: ((String) -> Void)?
    var onError: ((String) -> Void)?

    /// Ask once for the microphone and recognition.
    static func permission() async -> Bool {
        let speech = await withCheckedContinuation { done in SFSpeechRecognizer.requestAuthorization { done.resume(returning: $0 == .authorized) } }
        let mic = await AVAudioApplication.requestRecordPermission()
        return speech && mic
    }

    func start() {
        stop()
        guard let recognizer, recognizer.isAvailable else {
            onError?("Speech recognition is not available on this phone now.")
            return
        }
        AudioRoute.configureForTalk()
        let request = SFSpeechAudioBufferRecognitionRequest()
        request.shouldReportPartialResults = true
        // On the phone itself, where it can: nothing but the words leave it.
        if recognizer.supportsOnDeviceRecognition { request.requiresOnDeviceRecognition = true }
        request.taskHint = .dictation
        self.request = request
        let input = engine.inputNode
        let format = input.outputFormat(forBus: 0)
        input.removeTap(onBus: 0)
        input.installTap(onBus: 0, bufferSize: 1024, format: format) { [weak request] buffer, _ in
            request?.append(buffer)
        }
        engine.prepare()
        do {
            try engine.start()
        } catch {
            onError?("The microphone could not start: \(error.localizedDescription)")
            return
        }
        task = recognizer.recognitionTask(with: request) { [weak self] result, error in
            Task { @MainActor in
                if let result { self?.onWords?(result.bestTranscription.formattedString) }
                if let error, (error as NSError).code != 301, (error as NSError).code != 216 {
                    // 301: cancelled; 216: ended by us.
                    self?.onError?(error.localizedDescription)
                }
            }
        }
    }

    func stop() {
        task?.cancel()
        task = nil
        request?.endAudio()
        request = nil
        if engine.isRunning {
            engine.stop()
            engine.inputNode.removeTap(onBus: 0)
        }
    }
}

/// The manager's voice on the phone, played to the glasses.
@MainActor
final class Voice: NSObject, AVSpeechSynthesizerDelegate {
    private let synthesizer = AVSpeechSynthesizer()
    private var queued = 0
    var onFinished: (() -> Void)?
    private let voice: AVSpeechSynthesisVoice? = {
        let language = Locale.current.language.languageCode?.identifier ?? "en"
        let voices = AVSpeechSynthesisVoice.speechVoices().filter { $0.language.hasPrefix(language) }
        return voices.first { $0.quality == .premium } ?? voices.first { $0.quality == .enhanced } ?? AVSpeechSynthesisVoice(language: AVSpeechSynthesisVoice.currentLanguageCode())
    }()

    override init() {
        super.init()
        synthesizer.delegate = self
    }

    var isSpeaking: Bool { queued > 0 }

    func say(_ sentence: String) {
        let utterance = AVSpeechUtterance(string: sentence)
        utterance.voice = voice
        utterance.rate = AVSpeechUtteranceDefaultSpeechRate * 1.05
        queued += 1
        synthesizer.speak(utterance)
    }

    func stop() {
        queued = 0
        synthesizer.stopSpeaking(at: .immediate)
    }

    nonisolated func speechSynthesizer(_ synthesizer: AVSpeechSynthesizer, didFinish utterance: AVSpeechUtterance) {
        Task { @MainActor in
            self.queued = max(0, self.queued - 1)
            if self.queued == 0 { self.onFinished?() }
        }
    }
}
