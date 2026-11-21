import Foundation

/// The device link's lines (G03, `glasscode_core::device_link` and `remote`): the same text,
/// byte for byte, as the Rust side signs and checks. A change here is a change there.
enum LinkProtocol {
    static let doorVersion = 1

    /// What the Mac's door signs in its hello.
    static func doorMessage(key: String, launch: String, nonce: String) -> String {
        "glasscode-door\n\(doorVersion)\n\(key)\n\(launch)\n\(nonce)"
    }

    /// What this phone signs to answer the door.
    static func phoneMessage(doorKey: String, launch: String, doorNonce: String, phoneNonce: String) -> String {
        "glasscode-phone\n\(doorVersion)\n\(doorKey)\n\(launch)\n\(doorNonce)\n\(phoneNonce)"
    }

    /// One input to a Mac session, as the phone signs it (#288, `remote::input_message`).
    enum Input {
        case text(String, enter: Bool)
        case answer(option: Int, text: String?)
        case keys([String])

        var body: String {
            switch self {
            case .text(let text, let enter): return "text\n\(enter)\n\(text)"
            case .answer(let option, let text): return "answer\n\(option)\n\(text ?? "")"
            case .keys(let keys): return "keys\n" + keys.joined(separator: "\n")
            }
        }
    }

    static func inputMessage(launch: String, session: String, nonce: String, input: Input) -> String {
        "glasscode-input\n\(launch)\n\(session)\n\(nonce)\n\(input.body)"
    }

    /// What the phone signs to answer an approval (`remote::decide_message`).
    static func decideMessage(launch: String, approval: Int, allow: Bool) -> String {
        "glasscode-decide\n\(launch)\n\(approval)\n\(allow ? "allow" : "deny")"
    }

    /// A fresh nonce: 16 random bytes, hex.
    static func nonce() -> String {
        var bytes = [UInt8](repeating: 0, count: 16)
        _ = SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes)
        return bytes.map { String(format: "%02x", $0) }.joined()
    }

    /// The `signed` part of an input: this phone's key, a nonce, and its signature.
    static func signed(_ input: Input, session: String, launch: String, key: DeviceKey) -> [String: Any] {
        let nonce = nonce()
        return ["key": key.openSSH, "nonce": nonce, "sig": key.sign(inputMessage(launch: launch, session: session, nonce: nonce, input: input))]
    }
}

/// The door's first line.
struct DoorHello: Codable, Equatable {
    var door: Int
    var mac: String
    var mac_id: String?
    var key: String
    var launch: String
    var nonce: String
    var sig: String
    var features: [String]?

    /// Whether it comes from the door this phone pinned at pairing.
    func check(pinned: String) -> String? {
        if door != LinkProtocol.doorVersion { return "This Mac's Glasscode speaks another version; update one of them." }
        let words = { (k: String) in k.split(separator: " ").prefix(2).joined(separator: " ") }
        if words(key) != words(pinned) { return "This is not the Mac you paired." }
        if nonce.count < 16 || launch.isEmpty || !DeviceKey.verify(openSSH: pinned, message: LinkProtocol.doorMessage(key: key, launch: launch, nonce: nonce), signature: sig) {
            return "The Mac's signature is not good."
        }
        return nil
    }
}

/// This phone's answer to the door.
struct PhoneHello: Codable {
    var key: String
    var nonce: String
    var sig: String
    var name: String
    var app: String

    init(answering hello: DoorHello, key: DeviceKey, name: String, app: String) {
        self.key = key.openSSH
        nonce = LinkProtocol.nonce()
        sig = key.sign(LinkProtocol.phoneMessage(doorKey: hello.key, launch: hello.launch, doorNonce: hello.nonce, phoneNonce: nonce))
        self.name = name
        self.app = app
    }
}

struct Welcome: Codable {
    var ok: Bool
    var device: String?
    var error: String?
}

/// A failure the person reads as it is.
struct LinkError: LocalizedError, Equatable {
    let text: String
    var errorDescription: String? { text }
}

/// JSON values as the lines carry them.
typealias JSON = [String: Any]

extension Dictionary where Key == String, Value == Any {
    func string(_ key: String) -> String? { self[key] as? String }
    func int(_ key: String) -> Int? { (self[key] as? NSNumber)?.intValue }
    func bool(_ key: String) -> Bool? { (self[key] as? NSNumber)?.boolValue }
    func object(_ key: String) -> JSON? { self[key] as? JSON }
    func array(_ key: String) -> [JSON] { (self[key] as? [JSON]) ?? [] }
}

/// One line of JSON.
enum Lines {
    static func encode(_ object: Any) -> Data? {
        guard JSONSerialization.isValidJSONObject(object), var data = try? JSONSerialization.data(withJSONObject: object) else { return nil }
        data.append(0x0A)
        return data
    }

    static func decode(_ line: Data) -> JSON? {
        (try? JSONSerialization.jsonObject(with: line)) as? JSON
    }
}
