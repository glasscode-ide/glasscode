import Foundation
import LiveKit

// manager-voice — headless. Glasscode starts it when the manager's voice is switched on and
// ends it when it is switched off. It joins the manager's LiveKit room, publishes the
// microphone (echo-cancelled, so the manager does not hear itself), plays what the voice
// worker speaks, and talks to Glasscode in JSON lines:
//
//   stdout   {"ready":true}   {"heard":"text"}   {"state":"idle|listening|thinking|speaking"}
//            {"error":"why, in words"}
//            with a talker in the worker: {"heard":"text","handled":true} (shown, not asked),
//            {"said":"what the talker said"}, {"tool":{"call","name","arguments"}}
//   Exit codes: 0 ended as asked, 2 started wrong, 3 could not join, 4 the connection was
//   lost, 5 the worker reported a failure and closed the room.
//
//   stdin    {"say":{"id":"…","text":"…"}}   {"stop":true}
//            {"config":{"name":"…","listen":"wake|always|never","talker":true,"place":"mac"}}
//            {"result":{"call":"…","text":"…"}}   (what one of the talker's tools found)
//            ("wake": after the manager's name; "always": everything counts, as while the
//            talk key is held; "never": the microphone is off)
//
// The room's side of it is crates/glasscode_core/src/manager/voice.rs. No window, no dock
// icon: a plain command-line process. It ends when its stdin closes, so it never outlives
// the app with the microphone on.

let workerIdentity = "manager-voice"
let topicHeard = "manager.heard"
let topicSay = "manager.say"
let topicStop = "manager.stop"
let topicState = "manager.state"
let topicConfig = "manager.config"
let topicSaid = "manager.said"
let topicTool = "manager.tool"
let topicResult = "manager.result"
let listenValues = ["wake", "always", "never"]

// ---- options: flags first, then the environment ----

struct Options {
    var url = ""
    var token = ""
    var name = "Manager"
    var listen = "wake"
    /// The app runs the talker's tools (it says so in `config`).
    var talker = false
    var place = ""
    /// For tests: join and speak the protocol, but touch no microphone and play nothing.
    var noAudio = false

    static func read() -> Options {
        var o = Options()
        let env = ProcessInfo.processInfo.environment
        o.url = env["MANAGER_VOICE_URL"] ?? ""
        o.token = env["MANAGER_VOICE_TOKEN"] ?? ""
        if let n = env["MANAGER_VOICE_NAME"], !n.isEmpty { o.name = n }
        if let l = env["MANAGER_VOICE_LISTEN"], !l.isEmpty { o.listen = l }
        o.noAudio = env["MANAGER_VOICE_NO_AUDIO"] == "1"

        var args = Array(CommandLine.arguments.dropFirst())
        while !args.isEmpty {
            let flag = args.removeFirst()
            if flag == "--no-audio" { o.noAudio = true; continue }
            guard !args.isEmpty else { break }
            let value = args.removeFirst()
            switch flag {
            case "--url": o.url = value
            case "--token": o.token = value
            case "--name": o.name = value
            case "--listen": o.listen = value
            default: break
            }
        }
        if !listenValues.contains(o.listen) { o.listen = "wake" }
        return o
    }
}

// ---- lines to Glasscode ----

let outLock = NSLock()

func emit(_ object: [String: Any]) {
    guard var data = try? JSONSerialization.data(withJSONObject: object) else { return }
    data.append(0x0A)
    outLock.lock()
    FileHandle.standardOutput.write(data)
    outLock.unlock()
}

func note(_ text: String) {
    FileHandle.standardError.write(Data(("[manager-voice] " + text + "\n").utf8))
}

/// A connection error in words the person can act on.
func plain(_ error: Error?) -> String {
    guard let error else { return "The connection to the voice server was lost." }
    let text = String(describing: error).lowercased()
    if text.contains("401") || text.contains("unauthor") || text.contains("invalid token") || text.contains("invalid api key") {
        return "The voice server did not accept the key. The LiveKit key and secret in Glasscode must be the same as in voice/manager/.env on the server."
    }
    if text.contains("timed out") || text.contains("timeout") || text.contains("network") || text.contains("could not connect") || text.contains("hostname") {
        return "The voice server could not be reached. Check the address in Settings › Manager › Voice and that the server is running."
    }
    return "The voice connection failed: \(error.localizedDescription)"
}

// ---- the room ----

@MainActor
final class Helper {
    let room = Room()
    var options = Options()
    let listener = Listener()
    var micOn = false
    /// Joined the room (the microphone can only be published after that).
    var connected = false
    var workerPresent = false
    var workerEverSeen = false
    var leaving = false
    /// What the worker last said went wrong (a speech service, mostly).
    var workerError: String?

    nonisolated init() {}

    var identity: String { room.localParticipant.identity?.stringValue ?? "" }

    func start() async {
        options = Options.read()
        guard !options.url.isEmpty, !options.token.isEmpty else {
            emit(["error": "The voice helper was started without an address or a token."])
            exit(2)
        }
        listener.helper = self
        room.add(delegate: listener)

        let capture = AudioCaptureOptions(echoCancellation: true, autoGainControl: true, noiseSuppression: true)
        do {
            try await room.connect(url: options.url,
                                   token: options.token,
                                   connectOptions: ConnectOptions(autoSubscribe: !options.noAudio),
                                   roomOptions: RoomOptions(defaultAudioCaptureOptions: capture))
        } catch {
            note("connect failed: \(error)")
            emit(["error": plain(error)])
            exit(3)
        }

        connected = true
        workerPresent = room.remoteParticipants.keys.contains { $0.stringValue == workerIdentity }
        workerEverSeen = workerPresent
        await applyMicrophone()
        emit(["ready": true])
        await sendConfig()

        // The worker comes in a moment after the first client. If it never does, say so.
        Task { @MainActor in
            try? await Task.sleep(nanoseconds: 15_000_000_000)
            if !self.workerEverSeen, !self.leaving {
                emit(["error": "The voice worker on the server did not join. Check that it is running (docker compose ps in voice/manager)."])
            }
        }
    }

    /// The microphone is on for "wake" and "always" and off for "never". Off means muted:
    /// nothing leaves the Mac.
    func applyMicrophone() async {
        guard connected, !options.noAudio else { return }
        let want = options.listen != "never"
        guard want != micOn else { return }
        do {
            try await room.localParticipant.setMicrophone(enabled: want)
            micOn = want
        } catch {
            note("microphone: \(error)")
            emit(["error": "The microphone could not be used. Allow Glasscode to use it in System Settings › Privacy & Security › Microphone."])
        }
    }

    func publish(_ topic: String, _ object: [String: Any]) async {
        guard connected, let data = try? JSONSerialization.data(withJSONObject: object) else { return }
        do {
            try await room.localParticipant.publish(data: data, options: DataPublishOptions(topic: topic, reliable: true))
        } catch {
            note("publish \(topic): \(error)")
        }
    }

    func sendConfig() async {
        guard connected else { return }
        var config: [String: Any] = ["name": options.name, "listen": options.listen]
        if options.talker { config["talker"] = true }
        if !options.place.isEmpty { config["place"] = options.place }
        await publish(topicConfig, config)
    }

    // -- from Glasscode --

    func handle(line: String) async {
        guard let data = line.data(using: .utf8),
              let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
        else { return }
        if let say = object["say"] as? [String: Any], let text = say["text"] as? String {
            await publish(topicSay, ["id": (say["id"] as? String) ?? "", "text": text])
        }
        if object["stop"] as? Bool == true {
            await publish(topicStop, [:])
        }
        if let result = object["result"] as? [String: Any], let call = result["call"] as? String {
            await publish(topicResult, ["call": call, "text": (result["text"] as? String) ?? ""])
        }
        if let config = object["config"] as? [String: Any] {
            if let name = config["name"] as? String, !name.isEmpty { options.name = name }
            if let listen = config["listen"] as? String, listenValues.contains(listen) { options.listen = listen }
            if let talker = config["talker"] as? Bool { options.talker = talker }
            if let place = config["place"] as? String { options.place = place }
            await applyMicrophone()
            await sendConfig()
        }
    }

    // -- from the room --

    func received(topic: String, data: Data) {
        guard let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] else { return }
        switch topic {
        case topicHeard:
            // Every device in the room gets this. Only the one whose microphone heard it asks.
            let from = (object["from"] as? String) ?? ""
            guard from.isEmpty || from == identity else { return }
            guard let text = object["text"] as? String, !text.isEmpty, (object["final"] as? Bool) ?? true else { return }
            if object["handled"] as? Bool == true {
                emit(["heard": text, "handled": true])
            } else {
                emit(["heard": text])
            }
        case topicSaid:
            if let text = object["text"] as? String, !text.isEmpty { emit(["said": text]) }
        case topicTool:
            // Only the device the worker listens to runs the talker's tools.
            guard (object["to"] as? String) == identity, let call = object["call"] as? String, let name = object["name"] as? String else { return }
            emit(["tool": ["call": call, "name": name, "arguments": object["arguments"] ?? [String: Any]()]])
        case topicState:
            // The state first: Glasscode takes a state line as "it works again".
            if let state = object["state"] as? String { emit(["state": state]) }
            if let error = object["error"] as? String, !error.isEmpty {
                workerError = error
                emit(["error": error])
            }
        default:
            break
        }
    }

    func participantJoined(_ id: String) async {
        guard id == workerIdentity else { return }
        workerPresent = true
        workerEverSeen = true
        // It was not there when the config was first sent, and messages are not kept.
        await sendConfig()
    }

    func participantLeft(_ id: String) {
        guard id == workerIdentity, !leaving else { return }
        workerPresent = false
        emit(["state": "idle"])
        emit(["error": "The voice worker on the server left the room."])
    }

    func disconnected(_ error: Error?) {
        guard !leaving else { return }
        leaving = true
        note("disconnected: \(String(describing: error))")
        if let workerError {
            // The worker closed the room after it failed: its reason is the one that helps.
            emit(["error": workerError])
            exit(5)
        }
        emit(["error": plain(error)])
        exit(4)
    }

    func stop() async {
        guard !leaving else { return }
        leaving = true
        await room.disconnect()
    }
}

/// The room's callbacks arrive on its own queues; they are handed to the main actor.
final class Listener: NSObject, RoomDelegate, @unchecked Sendable {
    weak var helper: Helper?

    func room(_: Room, participant _: RemoteParticipant?, didReceiveData data: Data, forTopic topic: String, encryptionType _: EncryptionType) {
        Task { @MainActor in self.helper?.received(topic: topic, data: data) }
    }

    func room(_: Room, participantDidConnect participant: RemoteParticipant) {
        let id = participant.identity?.stringValue ?? ""
        Task { @MainActor in await self.helper?.participantJoined(id) }
    }

    func room(_: Room, participantDidDisconnect participant: RemoteParticipant) {
        let id = participant.identity?.stringValue ?? ""
        Task { @MainActor in self.helper?.participantLeft(id) }
    }

    func room(_: Room, didDisconnectWithError error: LiveKitError?) {
        Task { @MainActor in self.helper?.disconnected(error) }
    }
}

// ---- boot ----

setvbuf(stdout, nil, _IOLBF, 0)
let helper = Helper()
var signalSources: [DispatchSourceSignal] = []

// End cleanly when Glasscode ends us, or on Ctrl-C.
for sig in [SIGINT, SIGTERM] {
    signal(sig, SIG_IGN)
    let src = DispatchSource.makeSignalSource(signal: sig, queue: .main)
    src.setEventHandler { Task { @MainActor in await helper.stop(); exit(0) } }
    src.resume()
    signalSources.append(src) // keep alive for the process lifetime
}

// Lines from Glasscode. When stdin closes the app is gone: leave the room and end.
let stdinThread = Thread {
    while let line = readLine(strippingNewline: true) {
        let trimmed = line.trimmingCharacters(in: .whitespaces)
        if trimmed.isEmpty { continue }
        Task { @MainActor in await helper.handle(line: trimmed) }
    }
    Task { @MainActor in await helper.stop(); exit(0) }
}
stdinThread.start()

Task { @MainActor in await helper.start() }
dispatchMain()
