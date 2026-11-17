import Foundation
import Combine
import AVFoundation
import LiveKit

/// The manager's voice on the glasses: a LiveKit room on the person's server.
///
/// This phone publishes the microphone (its own by default, see `AudioRoute`) and plays what
/// the room's worker speaks through the glasses. The worker turns speech into text and text into speech; the brain is
/// the manager on the Mac (or on the server), reached through the control plane. The room's
/// messages are the ones fixed in `glasscode_core/src/manager/voice.rs`:
///
///   worker → us   `manager.heard`  {"text","final","from"}   what it heard, and from whom
///   worker → us   `manager.state`  {"state","id","error"?}   idle | listening | thinking | speaking
///   us → worker   `manager.say`    {"id","text"}             speak this
///   us → worker   `manager.stop`   {}                        stop speaking now
///   us → worker   `manager.config` {"name","listen"}         its name, and to keep listening
///
/// A final `manager.heard` of ours is asked of the manager (`POST /api/manager/ask`); the
/// answer goes back on `manager.say` and is published as the caption.
@MainActor
final class ManagerVoice: ObservableObject {
    enum State: String { case off, connecting, idle, listening, thinking, speaking }

    /// Why voice is not available, in words for the person.
    struct Problem: Equatable {
        let title: String
        let detail: String
    }

    @Published private(set) var state: State = .off
    /// The last thing the worker heard from this device (it grows while the person speaks).
    @Published private(set) var heard = ""
    /// What the manager is saying, or said last.
    @Published private(set) var caption = ""
    @Published private(set) var problem: Problem?
    /// Whether the microphone is really published (the room can connect without it).
    @Published private(set) var micOn = false
    /// Whether the worker that listens and speaks is in the room.
    @Published private(set) var workerHere = false

    var live: Bool { state != .off }

    /// The room ended by itself (it dropped, or it was quiet for too long): why, for the person.
    var onEnded: ((String) -> Void)?
    /// The person asked something (a final `manager.heard` of theirs).
    var onQuestion: ((String) -> Void)?

    private enum Topic {
        static let heard = "manager.heard"
        static let say = "manager.say"
        static let stop = "manager.stop"
        static let state = "manager.state"
        static let config = "manager.config"
    }

    private let control = GlasscodeControl()
    private var room: Room?
    private var events: RoomEvents?
    private var identity: String?
    private var managerName = ""
    /// The workers in the room (normally the one), by identity.
    private var workers = Set<String>()
    /// Goes up on every start and hang-up, so work begun for an earlier call drops its result.
    private var generation = 0

    private var workerState: State = .idle
    private var asking = false
    private var waiting: [String] = []
    /// Put in front of the first question: which session the person has open.
    private var pendingContext: String?
    private var lastSpokeAt = Date.distantPast

    // A forgotten call must not keep an open microphone streaming to the server: hang up
    // after two quiet minutes (nothing heard, nothing said).
    private let quietLimit: TimeInterval = 120
    private var lastActivity = Date()
    private var quietTimer: Timer?

    // MARK: - lifecycle

    /// Join the room and open the microphone. `context` is put in front of the first question.
    /// False, with `problem` set, when voice cannot start (or the person hung up meanwhile).
    @discardableResult
    func start(name: String, context: String? = nil) async -> Bool {
        if room != nil {
            if let context { pendingContext = context }
            return true
        }
        generation += 1
        let gen = generation
        problem = nil; heard = ""; caption = ""
        pendingContext = context
        managerName = name
        workerState = .idle; asking = false; waiting = []
        workers = []; workerHere = false
        state = .connecting

        let grant: ManagerVoiceGrant
        do {
            grant = try await control.managerVoiceToken(identity: Self.deviceIdentity)
        } catch let error as ManagerError {
            guard gen == generation else { return false }
            // The Mac says why in a sentence of its own (voice not set up, the manager off…).
            let setUp = "Voice is not set up yet"
            return fail(setUp, error.text.hasPrefix(setUp) ? "Set it up in Glasscode › Settings › Manager." : error.text)
        } catch {
            guard gen == generation else { return false }
            return fail("Can't reach Glasscode", "The voice room needs the Mac. Check that it is awake.")
        }
        guard gen == generation else { return false }

        guard await ensureMicPermission() else {
            guard gen == generation else { return false }
            return fail("Microphone is off", "Turn on Microphone for Glasscode in iOS Settings › Glasscode.")
        }
        guard gen == generation else { return false }
        configureAudioForGlasses()

        let events = RoomEvents(owner: self)
        let room = Room(delegate: events)
        self.room = room
        self.events = events
        identity = grant.identity
        do {
            try await room.connect(url: grant.url, token: grant.token)
        } catch {
            guard gen == generation else { return false }
            self.room = nil; self.events = nil
            releaseAudio()
            return fail("Couldn't reach the manager's voice", error.localizedDescription)
        }
        guard gen == generation else {
            await room.disconnect()
            return false
        }

        // Remote audio (the worker's speech) plays by itself once subscribed. The microphone
        // has to be asked for; the room stays useful for captions even when it fails.
        do {
            try await room.localParticipant.setMicrophone(enabled: true)
            micOn = room.localParticipant.isMicrophoneEnabled()
        } catch {
            micOn = false
        }
        guard gen == generation else { return false }

        for other in room.remoteParticipants.values where RoomEvents.isWorker(other) {
            workers.insert(other.identity?.stringValue ?? "")
        }
        workerHere = !workers.isEmpty
        // Right after joining, and again whenever the worker comes in (see `worker(_:joined:in:)`):
        // data messages are not kept, and the worker only joins once a client is in the room.
        await sendConfig()
        touch()
        startQuietTimer()
        refresh()
        return true
    }

    /// Have the manager's voice speak this (today's update, an answer). Also the caption.
    func say(_ text: String) async {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard room != nil, !text.isEmpty else { return }
        caption = text
        touch()
        // What is sent before the worker is in the room never reaches it: give it up to ten
        // seconds to join (it is started when the first device comes in).
        let gen = generation
        var waited = 0
        while !workerHere, waited < 40, gen == generation {
            try? await Task.sleep(for: .milliseconds(250))
            waited += 1
        }
        guard gen == generation, room != nil else { return }
        touch()
        await publish(Topic.say, ["id": UUID().uuidString, "text": text])
    }

    /// Tell the worker the manager's name and to keep listening: the person opened this talk
    /// themselves, so there is no wake word. Sent whenever a worker comes into the room.
    private func sendConfig() async {
        await publish(Topic.config, ["name": managerName, "listen": "always"])
    }

    /// Stop speaking now (the person cut in).
    func interrupt() async {
        guard room != nil else { return }
        await publish(Topic.stop, [:])
    }

    /// Leave the room and give the microphone back.
    func hangUp() async {
        generation += 1
        stopQuietTimer()
        let leaving = room
        let wasSpeaking = workerState == .speaking
        room = nil; events = nil; identity = nil
        asking = false; waiting = []; pendingContext = nil
        workerState = .idle; micOn = false; workers = []; workerHere = false
        state = .off
        if let leaving {
            if wasSpeaking { await publish(Topic.stop, [:], in: leaving) }
            await leaving.disconnect()
        }
        // A new call may have begun while this one was leaving: its audio is not ours to stop.
        if room == nil { releaseAudio() }
    }

    private func fail(_ title: String, _ detail: String) -> Bool {
        problem = Problem(title: title, detail: detail)
        state = .off
        return false
    }

    /// The room is gone without the person hanging up.
    private func ended(_ why: String) {
        guard room != nil else { return }
        Task {
            await hangUp()
            onEnded?(why)
        }
    }

    // MARK: - audio session

    /// Make sure iOS has granted the app microphone access before the room asks for it.
    private func ensureMicPermission() async -> Bool {
        switch AVAudioApplication.shared.recordPermission {
        case .granted: return true
        case .denied: return false
        default:
            return await withCheckedContinuation { c in
                AVAudioApplication.requestRecordPermission { c.resume(returning: $0) }
            }
        }
    }

    /// A two-way talk. By default the phone is the microphone and the glasses are the
    /// speakers, because the glasses end the app's session when their own microphone opens
    /// (see `AudioRoute`).
    private func configureAudioForGlasses() {
        AudioRoute.configureForTalk()
    }

    private func releaseAudio() {
        AudioRoute.release()
    }

    // MARK: - the room's messages

    private func publish(_ topic: String, _ object: [String: Any], in target: Room? = nil) async {
        guard let room = target ?? self.room,
              let data = try? JSONSerialization.data(withJSONObject: object) else { return }
        try? await room.localParticipant.publish(data: data, options: DataPublishOptions(topic: topic, reliable: true))
    }

    fileprivate func received(topic: String, data: Data, in source: Room) {
        guard source === room,
              let obj = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] else { return }
        switch topic {
        case Topic.heard: workerHere = true; onHeard(obj)
        case Topic.state: workerHere = true; onWorkerState(obj)
        default: break
        }
    }

    private func onHeard(_ obj: [String: Any]) {
        // Every device of the person is in the same room. Only the one that was spoken into
        // asks the manager; the others stay quiet.
        let from = (obj["from"] as? String) ?? ""
        if !from.isEmpty, let identity, from != identity { return }
        let text = ((obj["text"] as? String) ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        // The manager's own voice, picked up by the microphone, is not a question.
        if isOwnVoice(text) { return }
        touch()
        heard = text
        guard (obj["final"] as? Bool) == true else { return }
        onQuestion?(text)
        waiting.append(text)
        askNext()
    }

    private func onWorkerState(_ obj: [String: Any]) {
        // A speech service on the server failed: say so where the caption is.
        if let trouble = (obj["error"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines), !trouble.isEmpty {
            caption = "The voice on the server has a problem: \(trouble)"
        }
        guard let raw = obj["state"] as? String, let next = State(rawValue: raw),
              next != .off, next != .connecting else { return }
        if workerState == .speaking, next != .speaking { lastSpokeAt = Date() }
        workerState = next
        if next != .idle { touch() }
        refresh()
    }

    /// One question at a time: what is said while the manager thinks is asked next, together.
    private func askNext() {
        guard !asking, !waiting.isEmpty, room != nil else { return }
        var question = waiting.joined(separator: " ")
        waiting.removeAll()
        if let context = pendingContext {
            pendingContext = nil
            question = context + "\n\n" + question
        }
        asking = true
        let gen = generation
        let spokeOver = workerState == .speaking
        refresh()
        Task {
            if spokeOver { await interrupt() }
            var answer = ""
            var trouble: String?
            do {
                answer = try await control.managerAsk(question, voice: true)
            } catch let error as ManagerError {
                trouble = error.text
            } catch {
                trouble = "Can't reach Glasscode right now."
            }
            guard gen == generation else { return }
            asking = false
            if let trouble {
                caption = trouble
            } else if !answer.isEmpty {
                await say(answer)
            }
            guard gen == generation else { return }
            touch()
            refresh()
            askNext()
        }
    }

    /// What to show: the worker's state, with our own waiting for the manager on top of it.
    private func refresh() {
        guard room != nil else { return }
        if workerState == .speaking {
            state = .speaking
        } else if asking {
            state = .thinking
        } else {
            state = workerState
        }
    }

    /// Whether `text` is (part of) what the manager is saying or has just said: the worker
    /// heard its own voice through the microphone.
    private func isOwnVoice(_ text: String) -> Bool {
        guard workerState == .speaking || Date().timeIntervalSince(lastSpokeAt) < 4 else { return false }
        let said = Self.plain(caption)
        let got = Self.plain(text)
        return got.split(separator: " ").count >= 3 && said.contains(got)
    }

    private static func plain(_ text: String) -> String {
        text.lowercased()
            .components(separatedBy: CharacterSet.alphanumerics.inverted)
            .filter { !$0.isEmpty }
            .joined(separator: " ")
    }

    // MARK: - the room itself

    fileprivate func worker(_ who: String, joined: Bool, in source: Room) {
        guard source === room else { return }
        if joined {
            workers.insert(who)
            workerHere = true
            Task { await sendConfig() }
        } else {
            workers.remove(who)
            workerHere = !workers.isEmpty
            if !workerHere { workerState = .idle; refresh() }
        }
    }

    fileprivate func disconnected(_ source: Room) {
        guard source === room else { return }
        ended("The voice connection dropped")
    }

    // MARK: - quiet for too long

    private func touch() { lastActivity = Date() }

    private func startQuietTimer() {
        stopQuietTimer()
        quietTimer = Timer.scheduledTimer(withTimeInterval: 5, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.checkQuiet() }
        }
    }

    private func stopQuietTimer() { quietTimer?.invalidate(); quietTimer = nil }

    private func checkQuiet() {
        guard room != nil else { return }
        if asking || workerState == .speaking || workerState == .thinking { touch(); return }
        if Date().timeIntervalSince(lastActivity) >= quietLimit { ended("Hung up after two quiet minutes") }
    }

    // MARK: - who we are in the room

    /// This phone's name in the room, the same on every call (`glasses-1a2b3c4d`).
    private static let deviceIdentity: String = {
        let key = "manager.voice.identity"
        if let saved = UserDefaults.standard.string(forKey: key), !saved.isEmpty { return saved }
        let fresh = "glasses-" + UUID().uuidString.replacingOccurrences(of: "-", with: "").prefix(8).lowercased()
        UserDefaults.standard.set(fresh, forKey: key)
        return fresh
    }()
}

/// The room calls its delegate on a thread of its own; this hands each event to the main
/// actor, in the order it arrived.
private final class RoomEvents: NSObject, RoomDelegate, @unchecked Sendable {
    private weak var owner: ManagerVoice?

    /// The worker is the participant named `manager-voice` (or any LiveKit agent).
    static func isWorker(_ participant: RemoteParticipant) -> Bool {
        participant.identity?.stringValue == "manager-voice" || participant.kind == .agent
    }

    init(owner: ManagerVoice) {
        self.owner = owner
    }

    private func onMain(_ work: @escaping @MainActor (ManagerVoice) -> Void) {
        DispatchQueue.main.async { [weak owner] in
            guard let owner else { return }
            MainActor.assumeIsolated { work(owner) }
        }
    }

    func room(_ room: Room, participant: RemoteParticipant?, didReceiveData data: Data, forTopic topic: String, encryptionType: EncryptionType) {
        onMain { $0.received(topic: topic, data: data, in: room) }
    }

    func room(_ room: Room, participantDidConnect participant: RemoteParticipant) {
        guard Self.isWorker(participant) else { return }
        let who = participant.identity?.stringValue ?? ""
        onMain { $0.worker(who, joined: true, in: room) }
    }

    func room(_ room: Room, participantDidDisconnect participant: RemoteParticipant) {
        guard Self.isWorker(participant) else { return }
        let who = participant.identity?.stringValue ?? ""
        onMain { $0.worker(who, joined: false, in: room) }
    }

    func room(_ room: Room, didDisconnectWithError error: LiveKitError?) {
        onMain { $0.disconnected(room) }
    }
}
