import CryptoKit
import XCTest
@testable import GlasscodeGlasses

/// The device link speaks the Rust side's text exactly (`device_link.rs`, `remote.rs`).
final class LinkTests: XCTestCase {
    /// `device_link::vector`: the same key, the same OpenSSH form and device id; the Mac's
    /// signature verifies here.
    func testTheRustVectorHolds() throws {
        let key = try DeviceKey(raw: Data(repeating: 0x07, count: 32))
        let public_ = "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBB4YUy/UdUwC8wQdnHXOszuD/9gax85P6ILMscmLxYlupGwxHE4v9A3ZajZT5uRURdMt/khuztdcepDGoYiBwKM="
        XCTAssertEqual(key.openSSH, public_)
        XCTAssertEqual(key.macDeviceID, "901581028177fd03")
        let message = LinkProtocol.doorMessage(key: public_, launch: "launch-1", nonce: "00112233445566778899aabbccddeeff")
        XCTAssertTrue(DeviceKey.verify(openSSH: public_, message: message, signature: "uLEuVNHNnU5Fn+OE2bwJMZDxu4ZsTBMuDHTcOUdILpvCPlNKZ3U3ZyIZ8SzgZm2Oy7uRmG4bqKRCBezAFNiBzg=="))
        XCTAssertFalse(DeviceKey.verify(openSSH: public_, message: message + "x", signature: "uLEuVNHNnU5Fn+OE2bwJMZDxu4ZsTBMuDHTcOUdILpvCPlNKZ3U3ZyIZ8SzgZm2Oy7uRmG4bqKRCBezAFNiBzg=="))
    }

    /// The door's hello is checked against the pinned key, and this phone's answer verifies.
    func testTheHandshakeChecksBothSides() throws {
        let door = try DeviceKey(raw: Data(repeating: 0x07, count: 32))
        let phone = try DeviceKey(raw: Data(repeating: 0x09, count: 32))
        let stranger = try DeviceKey(raw: Data(repeating: 0x0b, count: 32))
        let nonce = "00112233445566778899aabbccddeeff"
        let hello = DoorHello(door: 1, mac: "Mac", mac_id: "", key: door.openSSH, launch: "launch-1", nonce: nonce, sig: door.sign(LinkProtocol.doorMessage(key: door.openSSH, launch: "launch-1", nonce: nonce)), features: [])
        XCTAssertNil(hello.check(pinned: door.openSSH))
        XCTAssertNotNil(hello.check(pinned: stranger.openSSH), "not the pinned Mac")
        var forged = hello
        forged.sig = stranger.sign(LinkProtocol.doorMessage(key: door.openSSH, launch: "launch-1", nonce: nonce))
        XCTAssertNotNil(forged.check(pinned: door.openSSH), "a stand-in cannot sign")
        let answer = PhoneHello(answering: hello, key: phone, name: "iPhone", app: "0.1")
        XCTAssertTrue(DeviceKey.verify(openSSH: phone.openSSH, message: LinkProtocol.phoneMessage(doorKey: door.openSSH, launch: "launch-1", doorNonce: nonce, phoneNonce: answer.nonce), signature: answer.sig))
    }

    /// `remote::input_message` and `decide_message`, byte for byte.
    func testSignedTextIsTheMacs() {
        XCTAssertEqual(LinkProtocol.inputMessage(launch: "L", session: "s1", nonce: "n", input: .text("rebase on main", enter: true)), "glasscode-input\nL\ns1\nn\ntext\ntrue\nrebase on main")
        XCTAssertEqual(LinkProtocol.inputMessage(launch: "L", session: "s1", nonce: "n", input: .answer(option: 2, text: nil)), "glasscode-input\nL\ns1\nn\nanswer\n2\n")
        XCTAssertEqual(LinkProtocol.inputMessage(launch: "L", session: "s1", nonce: "n", input: .keys(["Up", "Enter"])), "glasscode-input\nL\ns1\nn\nkeys\nUp\nEnter")
        XCTAssertEqual(LinkProtocol.decideMessage(launch: "L", approval: 7, allow: false), "glasscode-decide\nL\n7\ndeny")
    }

    func testPairingLinksParse() {
        let link = PairingLink("glasscode://pair?v=2&t=abc123&a=100.70.0.2:51000,192.168.1.4:51000&n=Studio-Mac")
        XCTAssertEqual(link?.token, "abc123")
        XCTAssertEqual(link?.addresses, ["100.70.0.2:51000", "192.168.1.4:51000"])
        XCTAssertEqual(link?.name, "Studio-Mac")
        XCTAssertNil(PairingLink("https://example.com/pair?t=1&a=2"))
        XCTAssertNil(PairingLink("glasscode://pair?v=2&n=x"), "no token, no address")
    }

    func testAnswersAndEventsAreTold() {
        let pending = Pending()
        var events: [String] = []
        pending.take(["event": "device.open", "data": ["kind": "today"]]) { name, _ in events.append(name) }
        XCTAssertEqual(events, ["device.open"])
    }
}
