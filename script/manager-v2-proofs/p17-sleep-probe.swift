// P17 sleep probe: how much time does a Mac give an app before it sleeps, and
// which clocks stop while it sleeps? A standalone program (system frameworks
// only; it is not part of the Cargo workspace).
//
//   swiftc -O -swift-version 5 -o <scratch>/p17-sleep-probe script/manager-v2-proofs/p17-sleep-probe.swift
//   <scratch>/p17-sleep-probe <log-file> [--hold <seconds>] [--url <test-server-url>]
//
// It logs one JSON line per event to <log-file>:
//   - "workspace_will_sleep" / "workspace_did_wake": the NSWorkspace notices
//     that GPUI's on_system_sleep / on_system_wake use (vendor/gpui_macos);
//   - "iokit_can_sleep" (idle sleep only), "iokit_will_sleep",
//     "iokit_allowed" (when it called IOAllowPowerChange), "iokit_powered_on":
//     the I/O Kit power registration (IORegisterForSystemPower). On
//     will_sleep it delays IOAllowPowerChange by up to --hold seconds
//     (default 20), and meanwhile sends one request to --url, logging
//     "net_ok" or "net_error" with the time it took;
//   - "tick" every 5 s.
// Every line has wall (Unix ms), uptime_raw_ms (CLOCK_UPTIME_RAW, the clock of
// Rust's std::time::Instant on Apple), monotonic_ms (CLOCK_MONOTONIC) and
// continuous_ms (mach_continuous_time).
import AppKit
import Foundation
import IOKit.pwr_mgt

let args = CommandLine.arguments
guard args.count >= 2 else {
    FileHandle.standardError.write("usage: p17-sleep-probe <log-file> [--hold <s>] [--url <url>]\n".data(using: .utf8)!)
    exit(2)
}
let logPath = args[1]
func opt(_ name: String) -> String? {
    guard let i = args.firstIndex(of: name), i + 1 < args.count else { return nil }
    return args[i + 1]
}
let hold = Double(opt("--hold") ?? "20") ?? 20
let url = opt("--url").flatMap(URL.init(string:))

FileManager.default.createFile(atPath: logPath, contents: nil)
let log = FileHandle(forWritingAtPath: logPath)!
log.seekToEndOfFile()

var timebase = mach_timebase_info_data_t()
mach_timebase_info(&timebase)
func ms(_ clock: clockid_t) -> Double {
    var ts = timespec()
    clock_gettime(clock, &ts)
    return Double(ts.tv_sec) * 1000 + Double(ts.tv_nsec) / 1e6
}
func continuousMs() -> Double {
    Double(mach_continuous_time()) * Double(timebase.numer) / Double(timebase.denom) / 1e6
}
func record(_ event: String, _ extra: [String: Any] = [:]) {
    var line: [String: Any] = [
        "event": event,
        "wall_ms": Int(Date().timeIntervalSince1970 * 1000),
        "uptime_raw_ms": Int(ms(CLOCK_UPTIME_RAW)),
        "monotonic_ms": Int(ms(CLOCK_MONOTONIC)),
        "continuous_ms": Int(continuousMs()),
    ]
    for (k, v) in extra { line[k] = v }
    if let data = try? JSONSerialization.data(withJSONObject: line, options: [.sortedKeys]) {
        log.write(data)
        log.write("\n".data(using: .utf8)!)
    }
}

// I/O Kit power messages: C macros in IOKit/IOMessage.h, not imported into
// Swift. iokit_common_msg(x) = sys_iokit (0xE0000000) | sub_iokit_common (0) | x.
let msgCanSystemSleep: UInt32 = 0xE000_0270
let msgSystemWillSleep: UInt32 = 0xE000_0280
let msgSystemHasPoweredOn: UInt32 = 0xE000_0300
let msgSystemWillPowerOn: UInt32 = 0xE000_0320

// I/O Kit power registration.
var rootPort: io_connect_t = 0
var notifier: io_object_t = 0
var pendingArg: Int = 0
var allowed = true

func allow(_ why: String) {
    if allowed { return }
    allowed = true
    IOAllowPowerChange(rootPort, pendingArg)
    record("iokit_allowed", ["why": why])
}

let callback: IOServiceInterestCallback = { _, _, messageType, messageArgument in
    switch messageType {
    case msgCanSystemSleep:
        record("iokit_can_sleep")
        IOAllowPowerChange(rootPort, Int(bitPattern: messageArgument))
    case msgSystemWillSleep:
        pendingArg = Int(bitPattern: messageArgument)
        allowed = false
        record("iokit_will_sleep", ["hold_s": hold])
        if let url = url {
            let t0 = Date()
            var req = URLRequest(url: url)
            req.timeoutInterval = hold
            URLSession.shared.dataTask(with: req) { _, resp, err in
                let dt = Int(Date().timeIntervalSince(t0) * 1000)
                DispatchQueue.main.async {
                    if let err = err {
                        record("net_error", ["ms": dt, "error": err.localizedDescription])
                    } else {
                        record("net_ok", ["ms": dt, "status": (resp as? HTTPURLResponse)?.statusCode ?? 0])
                    }
                }
            }.resume()
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + hold) { allow("hold elapsed") }
    case msgSystemWillPowerOn:
        record("iokit_will_power_on")
    case msgSystemHasPoweredOn:
        record("iokit_powered_on")
    default:
        break
    }
}

var notifyPort: IONotificationPortRef?
rootPort = IORegisterForSystemPower(nil, &notifyPort, callback, &notifier)
if rootPort == 0 {
    record("iokit_register_failed")
} else {
    IONotificationPortSetDispatchQueue(notifyPort, DispatchQueue.main)
    record("iokit_registered")
}

// The NSWorkspace notices GPUI uses.
let nc = NSWorkspace.shared.notificationCenter
nc.addObserver(forName: NSWorkspace.willSleepNotification, object: nil, queue: .main) { _ in
    record("workspace_will_sleep")
}
nc.addObserver(forName: NSWorkspace.didWakeNotification, object: nil, queue: .main) { _ in
    record("workspace_did_wake")
}

record("start", ["hold_s": hold, "url": url?.absoluteString ?? ""])
Timer.scheduledTimer(withTimeInterval: 5, repeats: true) { _ in record("tick") }
if args.contains("--once") {
    // A quick self-check without sleeping: log one tick and exit.
    DispatchQueue.main.asyncAfter(deadline: .now() + 1) { record("tick"); exit(0) }
}
let app = NSApplication.shared
app.setActivationPolicy(.prohibited)
app.run()
