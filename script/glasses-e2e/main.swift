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

// MODE=whoami: this stand-in's key and the device id the Mac gives it (to pin it in a scratch home).
if env["MODE"] == "whoami", let key = try? DeviceKey(raw: raw) {
    out(["key": key.openSSH, "device": key.macDeviceID])
    exit(0)
}

/// The `place` events the Mac sent, in order.
final class PlaceEvents: @unchecked Sendable {
    private let lock = NSLock()
    private var pages: [[String: Any]] = []
    func add(_ page: [String: Any]) { lock.lock(); pages.append(page); lock.unlock() }
    /// The newest event that fits, waited for.
    func wait(_ seconds: Double, _ fits: @escaping ([String: Any]) -> Bool) async -> [String: Any]? {
        let end = Date().addingTimeInterval(seconds)
        while Date() < end {
            lock.lock(); let found = pages.last(where: fits); lock.unlock()
            if let found { return found }
            try? await Task.sleep(nanoseconds: 100_000_000)
        }
        return nil
    }
}

/// One place everywhere, as a phone lives it (journey 9): the voice's tools answered by the Mac,
/// a place opened from the phone and by the voice, the Mac's window going there (`place.get`
/// with no place is the window's), and its page coming back as a `place` event.
func places(_ link: DoorLink, _ events: PlaceEvents) async {
    func call(_ method: String, _ params: [String: Any]) async -> Any {
        do { return try await link.call(method, params, timeout: 20) } catch { return ["error": error.localizedDescription] }
    }
    func tool(_ name: String, _ arguments: [String: Any]) async -> Any { await call("voice.tool", ["name": name, "arguments": arguments]) }
    func window() async -> String { ((await call("place.get", ["place": ""]) as? [String: Any])?["place"] as? String) ?? "" }
    func seen(_ place: String) async -> Bool { await events.wait(6) { ($0["place"] as? String) == place } != nil }

    out(["place_first": await events.wait(8) { _ in true } ?? [:]])
    out(["tool_list": await tool("list_sessions", [:])])
    // The voice opens pages: the Mac's window goes there, and the page comes to the phone.
    for page in ["tickets", "usage", "pull-requests", "sessions"] {
        out(["tool_\(page)": await tool("open_place", ["kind": page, "id": ""]), "window_\(page)": await window(), "event_\(page)": await seen(page)])
    }
    // A session by words of its title, as the person would say them, and in its project.
    let state = (await call("state", [:]) as? [String: Any]) ?? [:]
    let sessions = (state["sessions"] as? [[String: Any]]) ?? []
    let projects = (state["projects"] as? [[String: Any]]) ?? []
    if let entry = sessions.first, let session = entry["session"] as? [String: Any], let id = session["id"] as? String, let title = session["title"] as? String {
        let project = projects.first { ($0["id"] as? String) == (session["project_id"] as? String) }?["name"] as? String ?? ""
        out(["session_title": title, "session_id": id, "session_project": project])
        out(["tool_search": await tool("list_sessions", ["search": title, "project": project])])
        out(["tool_session": await tool("open_place", ["kind": "session", "id": "\(title) in \(project)"]), "window_session": await window(), "event_session": await seen(id)])
        // From the phone itself (a tap or the band): today, then back to the session by its id.
        out(["open_today": await call("place.open", ["place": "today"]), "window_today": await window(), "event_today": await seen("today")])
        out(["open_session": await call("place.open", ["place": id]), "window_back": await window()])
    } else {
        out(["session_title": ""])
    }
    // What is not there opens nothing, and says so.
    out(["tool_missing": await tool("open_place", ["kind": "ticket", "id": "nope 999"]), "window_missing": await window()])
    out(["tool_nobody": await tool("open_place", ["kind": "session", "id": "zzqx nothing like this"])])
}

// MODE=live: against a real Glasscode (journey 9): the phone's reads and a question to the manager.
if env["MODE"] == "live" {
    let finished = DispatchSemaphore(value: 0)
    Task {
        do {
            let key = try DeviceKey(raw: raw)
            let link = try await DoorLink.connect(Paired.Mac(name: "Mac", id: "", key: pinned, addresses: [address]), key: key, phoneName: "Journey phone")
            out(["connected": true])
            // One place everywhere: every `place` event the Mac sends (its window's lens page).
            let events = PlaceEvents()
            link.onEvent = { name, data in if name == "place", let page = data as? [String: Any] { events.add(page) } }
            out(["home": try await link.call("manager.home", [:], timeout: 20)])
            out(["state": try await link.call("state", [:], timeout: 30)])
            out(["today": try await link.call("today.get", [:], timeout: 20)])
            out(["presence": try await link.call("device.presence", ["glasses": true, "worn": true, "lens": true, "voice": "idle"], timeout: 10)])
            out(["ask": try await link.call("manager.ask", ["text": "what needs me?", "voice": true, "focus": ["view": "glance"], "client_id": "journey-1"], timeout: 90)])
            do { _ = try await link.call("sessions.create", [:], timeout: 10); out(["refused": false]) } catch { out(["refused": true]) }
            await places(link, events)
            link.close()
        } catch {
            out(["error": error.localizedDescription])
        }
        finished.signal()
    }
    finished.wait()
    exit(0)
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
