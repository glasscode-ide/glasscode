import CryptoKit
import Foundation
import NIOSSH
import Security

/// This phone's key (G11): P-256, made once and kept in the Secure Enclave where the phone has
/// one (a software key in the simulator). The Mac pins its public half at pairing; the device
/// door's handshake, signed input to a session (#288) and approvals are signed with it.
///
/// The public key is written in OpenSSH form (`ecdsa-sha2-nistp256 AAAA…`), as Glasscode's Rust
/// side writes and reads keys (`device_link::openssh_p256`). Signatures are SHA-256 ECDSA, raw
/// r‖s, base64: what `remote::verify_phone_signature` checks.
final class DeviceKey {
    static let shared = DeviceKey()

    private enum Backing {
        case enclave(SecureEnclave.P256.Signing.PrivateKey)
        case software(P256.Signing.PrivateKey)
    }

    private let backing: Backing
    private static let service = "dev.glasscode.glasses.device-key"

    private init() {
        if let data = Keychain.read(service: Self.service, account: "key"),
           let backing = Self.restore(data) {
            self.backing = backing
            return
        }
        let backing: Backing
        if SecureEnclave.isAvailable, let key = try? SecureEnclave.P256.Signing.PrivateKey() {
            backing = .enclave(key)
            Keychain.write(service: Self.service, account: "key", data: Data([1]) + key.dataRepresentation)
        } else {
            let key = P256.Signing.PrivateKey()
            backing = .software(key)
            Keychain.write(service: Self.service, account: "key", data: Data([0]) + key.rawRepresentation)
        }
        self.backing = backing
    }

    /// For tests: a software key from its raw 32 bytes.
    init(raw: Data) throws {
        backing = .software(try P256.Signing.PrivateKey(rawRepresentation: raw))
    }

    private static func restore(_ data: Data) -> Backing? {
        guard let kind = data.first else { return nil }
        let body = data.dropFirst()
        switch kind {
        case 1: return (try? SecureEnclave.P256.Signing.PrivateKey(dataRepresentation: body)).map(Backing.enclave)
        case 0: return (try? P256.Signing.PrivateKey(rawRepresentation: body)).map(Backing.software)
        default: return nil
        }
    }

    private var publicKey: P256.Signing.PublicKey {
        switch backing {
        case .enclave(let key): return key.publicKey
        case .software(let key): return key.publicKey
        }
    }

    /// `ecdsa-sha2-nistp256 AAAA…`
    var openSSH: String { Self.openSSH(publicKey) }

    /// The device id the Mac gives this key (`gate::device_id`: SHA-256 of "type body", 8 bytes hex).
    var macDeviceID: String { Self.macDeviceID(openSSH) }

    /// The same key for SSH to the server (the gate, G11).
    var sshPrivateKey: NIOSSHPrivateKey {
        switch backing {
        case .enclave(let key): return NIOSSHPrivateKey(secureEnclaveP256Key: key)
        case .software(let key): return NIOSSHPrivateKey(p256Key: key)
        }
    }

    func sign(_ message: String) -> String {
        let data = Data(message.utf8)
        let signature: P256.Signing.ECDSASignature?
        switch backing {
        case .enclave(let key): signature = try? key.signature(for: data)
        case .software(let key): signature = try? key.signature(for: data)
        }
        return signature?.rawRepresentation.base64EncodedString() ?? ""
    }

    static func openSSH(_ key: P256.Signing.PublicKey) -> String {
        var blob = Data()
        for part in [Data("ecdsa-sha2-nistp256".utf8), Data("nistp256".utf8), key.x963Representation] {
            var length = UInt32(part.count).bigEndian
            blob.append(Data(bytes: &length, count: 4))
            blob.append(part)
        }
        return "ecdsa-sha2-nistp256 " + blob.base64EncodedString()
    }

    static func macDeviceID(_ openSSH: String) -> String {
        let words = openSSH.split(separator: " ").prefix(2).joined(separator: " ")
        let digest = SHA256.hash(data: Data(words.utf8))
        return digest.prefix(8).map { String(format: "%02x", $0) }.joined()
    }

    /// Whether `signature` (raw r‖s or DER, base64) is `openSSH`'s of `message`.
    static func verify(openSSH: String, message: String, signature: String) -> Bool {
        let parts = openSSH.split(separator: " ")
        guard parts.count >= 2, let blob = Data(base64Encoded: String(parts[1])) else { return false }
        var fields: [Data] = []
        var rest = blob[...]
        while rest.count >= 4, fields.count < 3 {
            let n = rest.prefix(4).reduce(0) { ($0 << 8) | Int($1) }
            rest = rest.dropFirst(4)
            guard rest.count >= n else { return false }
            fields.append(Data(rest.prefix(n)))
            rest = rest.dropFirst(n)
        }
        guard fields.count == 3, fields[0] == Data("ecdsa-sha2-nistp256".utf8), fields[1] == Data("nistp256".utf8),
              let key = try? P256.Signing.PublicKey(x963Representation: fields[2]),
              let bytes = Data(base64Encoded: signature.trimmingCharacters(in: .whitespaces)) else { return false }
        let sig = (try? P256.Signing.ECDSASignature(rawRepresentation: bytes)) ?? (try? P256.Signing.ECDSASignature(derRepresentation: bytes))
        guard let sig else { return false }
        return key.isValidSignature(sig, for: Data(message.utf8))
    }
}

/// The few Keychain calls the app needs: one generic password item per service and account.
enum Keychain {
    static func read(service: String, account: String) -> Data? {
        let query: [String: Any] = [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service, kSecAttrAccount as String: account, kSecReturnData as String: true, kSecMatchLimit as String: kSecMatchLimitOne]
        var out: AnyObject?
        guard SecItemCopyMatching(query as CFDictionary, &out) == errSecSuccess else { return nil }
        return out as? Data
    }

    static func write(service: String, account: String, data: Data) {
        let query: [String: Any] = [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service, kSecAttrAccount as String: account]
        SecItemDelete(query as CFDictionary)
        var add = query
        add[kSecValueData as String] = data
        // The key works while the phone is locked in a pocket: the glasses are used that way.
        add[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        SecItemAdd(add as CFDictionary, nil)
    }

    static func delete(service: String, account: String) {
        let query: [String: Any] = [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service, kSecAttrAccount as String: account]
        SecItemDelete(query as CFDictionary)
    }
}
