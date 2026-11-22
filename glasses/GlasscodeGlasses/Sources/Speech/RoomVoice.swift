import AVFoundation
import Foundation
import LiveKit

/// A pass to the manager's voice room, from the Mac's door (`manager.voice`).
struct VoicePass {
    let url: String
    let token: String
    let identity: String
    /// The worker has a talker: it holds the conversation and asks this phone to run its tools.
    let talker: Bool

    init?(_ json: JSON) {
        guard let url = json.string("url"), let token = json.string("token"), !url.isEmpty, !token.isEmpty else { return nil }
        self.url = url
        self.token = token
        identity = json.string("identity") ?? ""
        talker = json.bool("talker") ?? false
    }
}

/// The manager's voice room (Settings › Manager › Voice › On this Mac): the phone joins it with
/// its microphone, the worker listens (Soniox), a fast talker answers at once and hands the work
/// to the manager through this phone, and the voice plays on the glasses. The talk stays open
/// while the person talks, like a call, and ends when they swipe it away or after two quiet
/// minutes. Without the room the app speaks on the phone itself (`Listener`, `Voice`).
///
/// The room's messages are the ones in `crates/glasscode_core/src/manager/voice.rs`:
///
///   worker → us   `manager.heard`  {"text","final","from","handled"}   what it heard
///   worker → us   `manager.said`   {"text"}                            what the talker said
///   worker → us   `manager.state`  {"state","id","error"?}             idle | listening | thinking | speaking
///   worker → us   `manager.tool`   {"call","name","arguments","to"}    run one of the talker's tools here
///   us → worker   `manager.result` {"call","text"}                     what it found
///   us → worker   `manager.say`    {"id","text"}                       speak this (the manager's answer)
///   us → worker   `manager.stop`   {}                                  stop speaking now
///   us → worker   `manager.config` {"name","listen","talker","place"}  who it is, and to keep listening
@MainActor
final class RoomVoice {
    enum State: String { case off, connecting, idle, listening, thinking, speaking }

    private(set) var state: State = .off
    var live: Bool { room != nil }
    /// The person made it silent: the microphone is off and nothing said counts, until they
    /// talk again. The talk stays open.
    private(set) var muted = false
    var speaking: Bool { state == .speaking }
    /// The room's worker has a talker: it retells the manager's answer itself, so the answer goes
    /// to it whole, once, not sentence by sentence.
    private(set) var talker = false

    /// What it heard from this phone (final), and whether the talker answers it (else: ask).
    var onHeard: ((String, Bool) -> Void)?
    /// What the talker itself said.
    var onSaid: ((String) -> Void)?
    var onState: ((State) -> Void)?
    /// Run one of the talker's tools: its name and arguments; what it found, in words.
    var onTool: ((String, JSON) async -> String)?
    /// The room ended by itself: why.
    var onEnded: ((String) -> Void)?
    /// A speech service failed on the Mac.
    var onTrouble: ((String) -> Void)?

    private var room: Room?
    private var events: RoomEvents?
    private var identity = ""
    private var name = "Manager"
    /// Where the person is ("On the Mac's screen now: …") and the conversation's last turns, for
    /// the talker: it starts from there, and a talk started again carries on.
    private var now = ""
    private var recent: [[String: String]] = []
    private var workerHere = false
    private var generation = 0
    private var lastActivity = Date()
    private var quietTimer: Timer?
    // A forgotten talk must not keep a microphone streaming: hang up after two quiet minutes.
    private let quietLimit: TimeInterval = 120

    private enum Topic {
        static let heard = "manager.heard"
        static let said = "manager.said"
        static let state = "manager.state"
        static let tool = "manager.tool"
        static let result = "manager.result"
        static let say = "manager.say"
        static let stop = "manager.stop"
        static let config = "manager.config"
    }

    /// Join the room and open the microphone. Throws (with words for the person) when it cannot.
    func start(_ pass: VoicePass, name: String) async throws {
        if room != nil { return }
        generation += 1
        let gen = generation
        self.name = name
        identity = pass.identity
        talker = pass.talker
        workerHere = false
        set(.connecting)
        guard await Self.micPermission() else {
            set(.off)
            throw LinkError(text: "Allow the microphone for Glasscode in iOS Settings.")
        }
        AudioRoute.configureForRoom()
        let events = RoomEvents(owner: self)
        let room = Room(delegate: events)
        self.room = room
        self.events = events
        do {
            try await room.connect(url: pass.url, token: pass.token, connectOptions: ConnectOptions(autoSubscribe: true))
        } catch {
            guard gen == generation else { throw CancellationError() }
            self.room = nil
            self.events = nil
            AudioRoute.release()
            set(.off)
            throw LinkError(text: "The voice on your Mac could not be reached (\(error.localizedDescription)).")
        }
        guard gen == generation else {
            await room.disconnect()
            throw CancellationError()
        }
        if identity.isEmpty { identity = room.localParticipant.identity?.stringValue ?? "" }
        do {
            try await room.localParticipant.setMicrophone(enabled: true)
        } catch {
            await hangUp()
            throw LinkError(text: "The microphone could not be used: \(error.localizedDescription)")
        }
        workerHere = room.remoteParticipants.values.contains(where: RoomEvents.isWorker)
        await sendConfig()
        touch()
        startQuietTimer()
        set(.idle)
    }

    /// Speak this: a sentence of the manager's answer (`id`: the answer it belongs to).
    func say(id: String, text: String) async {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard room != nil, !text.isEmpty else { return }
        touch()
        // What is sent before the worker is in the room never reaches it.
        let gen = generation
        var waited = 0
        while !workerHere, waited < 40, gen == generation {
            try? await Task.sleep(for: .milliseconds(250))
            waited += 1
        }
        guard gen == generation else { return }
        await publish(Topic.say, ["id": id, "text": text])
    }

    /// Make it silent, or let it listen again: the microphone goes off (or on), what it was
    /// saying stops, and the worker is told that nothing said counts.
    func setMuted(_ on: Bool) async {
        guard let room, on != muted else { return }
        muted = on
        try? await room.localParticipant.setMicrophone(enabled: !on)
        if on { await publish(Topic.stop, [:]) }
        await sendConfig()
        onState?(state)
    }

    /// Stop speaking now.
    func stop() async {
        guard room != nil else { return }
        await publish(Topic.stop, [:])
    }

    /// Leave the room and give the microphone back.
    func hangUp() async {
        muted = false
        generation += 1
        stopQuietTimer()
        let leaving = room
        room = nil
        events = nil
        workerHere = false
        set(.off)
        if let leaving {
            await publish(Topic.stop, [:], in: leaving)
            await leaving.disconnect()
        }
        if room == nil { AudioRoute.release() }
    }

    // MARK: - messages

    private func sendConfig() async {
        // The person opened this talk: everything said counts, no wake word.
        var config: [String: Any] = ["name": name, "listen": muted ? "never" : "always", "talker": true, "place": "glasses"]
        if !now.isEmpty { config["now"] = now }
        if !recent.isEmpty { config["recent"] = recent }
        await publish(Topic.config, config)
    }

    /// Where the person is, and what was said lately: sent now when it changed and a talk is open.
    func context(now: String, recent: [[String: String]]) async {
        guard now != self.now || recent != self.recent else { return }
        self.now = now
        self.recent = recent
        if room != nil { await sendConfig() }
    }

    private func publish(_ topic: String, _ object: [String: Any], in target: Room? = nil) async {
        guard let room = target ?? room, let data = try? JSONSerialization.data(withJSONObject: object) else { return }
        try? await room.localParticipant.publish(data: data, options: DataPublishOptions(topic: topic, reliable: true))
    }

    fileprivate func received(topic: String, data: Data, in source: Room) {
        guard source === room, let object = (try? JSONSerialization.jsonObject(with: data)) as? JSON else { return }
        workerHere = true
        switch topic {
        case Topic.heard:
            let from = object.string("from") ?? ""
            guard from.isEmpty || from == identity, let text = object.string("text")?.trimmingCharacters(in: .whitespacesAndNewlines), !text.isEmpty, object.bool("final") ?? true else { return }
            touch()
            onHeard?(text, object.bool("handled") == true)
        case Topic.said:
            if let text = object.string("text"), !text.isEmpty {
                touch()
                onSaid?(text)
            }
        case Topic.state:
            if let trouble = object.string("error"), !trouble.isEmpty { onTrouble?(trouble) }
            if let raw = object.string("state"), let next = State(rawValue: raw), next != .off, next != .connecting {
                if next != .idle { touch() }
                set(next)
            }
        case Topic.tool:
            guard object.string("to") == identity, let call = object.string("call"), let name = object.string("name") else { return }
            touch()
            let arguments = object.object("arguments") ?? [:]
            let gen = generation
            Task {
                let text = await self.onTool?(name, arguments) ?? "This phone cannot do that."
                guard gen == self.generation else { return }
                await self.publish(Topic.result, ["call": call, "text": text])
            }
        default:
            break
        }
    }

    fileprivate func worker(joined: Bool, in source: Room) {
        guard source === room else { return }
        workerHere = joined
        if joined { Task { await sendConfig() } } else { set(.idle) }
    }

    fileprivate func disconnected(_ source: Room) {
        guard source === room else { return }
        Task {
            await hangUp()
            onEnded?("The voice connection dropped.")
        }
    }

    private func set(_ next: State) {
        guard next != state else { return }
        state = next
        onState?(next)
    }

    // MARK: - quiet for too long

    private func touch() { lastActivity = Date() }

    private func startQuietTimer() {
        stopQuietTimer()
        quietTimer = Timer.scheduledTimer(withTimeInterval: 5, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.checkQuiet() }
        }
    }

    private func stopQuietTimer() {
        quietTimer?.invalidate()
        quietTimer = nil
    }

    private func checkQuiet() {
        guard room != nil else { return }
        if state == .speaking || state == .thinking { touch(); return }
        guard Date().timeIntervalSince(lastActivity) >= quietLimit else { return }
        Task {
            await hangUp()
            onEnded?("Hung up after two quiet minutes.")
        }
    }

    private static func micPermission() async -> Bool {
        switch AVAudioApplication.shared.recordPermission {
        case .granted: return true
        case .denied: return false
        default: return await AVAudioApplication.requestRecordPermission()
        }
    }
}

/// The room calls its delegate on a thread of its own; this hands each event to the main actor,
/// in the order it arrived.
private final class RoomEvents: NSObject, RoomDelegate, @unchecked Sendable {
    private weak var owner: RoomVoice?

    /// The worker is the participant named `manager-voice` (or any LiveKit agent).
    static func isWorker(_ participant: RemoteParticipant) -> Bool {
        participant.identity?.stringValue == "manager-voice" || participant.kind == .agent
    }

    init(owner: RoomVoice) {
        self.owner = owner
    }

    private func onMain(_ work: @escaping @MainActor (RoomVoice) -> Void) {
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
        onMain { $0.worker(joined: true, in: room) }
    }

    func room(_ room: Room, participantDidDisconnect participant: RemoteParticipant) {
        guard Self.isWorker(participant) else { return }
        onMain { $0.worker(joined: false, in: room) }
    }

    func room(_ room: Room, didDisconnectWithError error: LiveKitError?) {
        onMain { $0.disconnected(room) }
    }
}
