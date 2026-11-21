// A stand-in phone for the device door's end-to-end test (`device_door.rs`,
// `door_end_to_end_with_the_swift_client`): the iPhone app's own link code (DeviceKey,
// LinkProtocol, LineConnection, Pairing, DoorLink), built for macOS by `build.sh`. It connects
// to the door, does the handshake, calls `manager.home`, waits for one event, and prints what it
// got as JSON lines for the Rust test to check.
import Foundation

let env = ProcessInfo.processInfo.environment
guard let address = env["DOOR_ADDR"], let pinned = env["DOOR_KEY"], let hex = env["PHONE_RAW"] else {
    print("{\"error\":\"DOOR_ADDR, DOOR_KEY and PHONE_RAW are needed\"}")
    exit(2)
}
let raw = Data(stride(from: 0, to: hex.count, by: 2).compactMap { i -> UInt8? in
    let start = hex.index(hex.startIndex, offsetBy: i)
    return UInt8(hex[start..<hex.index(start, offsetBy: 2)], radix: 16)
})

func out(_ object: [String: Any]) {
    if let data = try? JSONSerialization.data(withJSONObject: object), let line = String(data: data, encoding: .utf8) { print(line) }
    fflush(stdout)
}

let done = DispatchSemaphore(value: 0)
Task {
    do {
        let key = try DeviceKey(raw: raw)
        let mac = Paired.Mac(name: "Test Mac", id: "", key: pinned, addresses: [address])
        let link = try await DoorLink.connect(mac, key: key, phoneName: "e2e phone")
        out(["connected": true, "launch": link.launch, "mac_id": link.macID])
        let gotEvent = DispatchSemaphore(value: 0)
        var event = ""
        link.onEvent = { name, _ in event = name; gotEvent.signal() }
        let home = try await link.call("manager.home", [:], timeout: 10)
        out(["home": home])
        do {
            _ = try await link.call("sessions.create", [:], timeout: 5)
            out(["refused": false])
        } catch {
            out(["refused": true, "why": error.localizedDescription])
        }
        let signed = LinkProtocol.signed(.text("rebase on main", enter: true), session: "s1", launch: link.launch, key: key)
        let sent = try await link.call("sessions.send", ["session_id": "s1", "text": "rebase on main", "enter": true, "signed": signed], timeout: 10)
        out(["sent": sent])
        _ = gotEvent.wait(timeout: .now() + 10)
        out(["event": event])
        link.close()
    } catch {
        out(["error": error.localizedDescription])
    }
    done.signal()
}
done.wait()
