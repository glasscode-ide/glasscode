import XCTest
import MWDATCore
import MWDATMockDevice
import SwiftUI
import UIKit
@testable import GlasscodeGlasses

/// Pictures of the lens: each page drawn on the toolkit's simulated Ray-Ban Display and saved
/// as a PNG, to look at the design without the glasses. Runs only when asked:
///   TEST_RUNNER_LENS_SHOTS=/some/folder xcodebuild test … -only-testing:GlasscodeGlassesTests/LensShotsTests
@MainActor
final class LensShotsTests: XCTestCase {
    func testDrawTheLens() async throws {
        guard let folder = ProcessInfo.processInfo.environment["LENS_SHOTS"], !folder.isEmpty else { throw XCTSkip("LENS_SHOTS is not set") }
        MockDeviceKit.shared.enable()
        let glasses = try MockDeviceKit.shared.pairGlasses(model: .metaRayBanDisplay)
        glasses.powerOn()
        glasses.unfold()
        glasses.don()
        let session = GlassesSession()
        session.show(LensCards.blank())
        let end = Date().addingTimeInterval(25)
        while !session.isConnected, Date() < end { try await Task.sleep(for: .milliseconds(100)) }
        XCTAssertTrue(session.isConnected, "the simulated lens did not start")

        let preview = glasses.services.display.createPreviewView()
        // In the host app's own scene, on screen: the preview draws only there.
        let scene = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first
        let window = scene.map { UIWindow(windowScene: $0) } ?? UIWindow(frame: UIScreen.main.bounds)
        // Taller than the lens: the simulated display draws its 600 by 600 low in its view, so a
        // 600-high window would cut off what the glasses show whole.
        window.frame = CGRect(x: 0, y: 0, width: 600, height: 900)
        window.backgroundColor = .black
        let host = UIViewController()
        host.view.backgroundColor = .black
        window.rootViewController = host
        preview.frame = host.view.bounds
        preview.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        host.view.addSubview(preview)
        window.makeKeyAndVisible()

        for (name, model) in Self.cards() {
            session.show(LensCards.render(model, voice: "idle"))
            try await Task.sleep(for: .seconds(2.5))
            preview.setNeedsLayout()
            preview.layoutIfNeeded()
            let image = UIGraphicsImageRenderer(bounds: window.bounds).image { _ in window.drawHierarchy(in: window.bounds, afterScreenUpdates: true) }
            try image.pngData()?.write(to: URL(fileURLWithPath: folder).appendingPathComponent("lens-\(name).png"))
        }
        // The phone's page, as the same places look there.
        for (name, model) in Self.cards() {
            guard let page = model.page else { continue }
            let view = PageView(page: page, open: { _ in }, terminal: page.kind == "session" ? {} : nil)
                .padding(16).frame(width: 402).background(Color(uiColor: LensPalette.ground))
            let renderer = ImageRenderer(content: view)
            renderer.scale = 3
            try renderer.uiImage?.pngData()?.write(to: URL(fileURLWithPath: folder).appendingPathComponent("phone-\(name).png"))
        }
        session.disconnect()
        await MockDeviceKit.shared.unpairDevice(glasses)
        await MockDeviceKit.shared.disable()
    }

    /// The pages, as the Mac would send them.
    static func cards() -> [(String, LensModel)] {
        func lens(_ page: LensPage, focus: Int = -1) -> LensModel {
            var model = LensModel()
            model.follow(page, opened: false)
            for _ in 0..<(focus + 1) { _ = model.handle(.down, speaking: false) }
            return model
        }
        let session = LensPage(kind: "session", place: "55de6efd", eyebrow: "Session · glasscode", title: "Why the finished state and orange icon lag", status: "Working", tone: .busy,
                               body: "Found it: the orange dot waits for Claude Code's 60-second idle reminder. Building the fix now: a turn end wakes the manager, and the voice says finished at once.",
                               items: [.init(title: "Needs your OK", detail: "run cargo test -p glasscode", tone: .warn, open: "approval:12"),
                                       .init(title: "Pull request #349", detail: "checks pass · Finish states stay true", tone: .ok, open: "pr:349")])
        let tickets = LensPage(JSON_tickets)!
        let usage = LensPage(kind: "usage", place: "usage", eyebrow: "Claude and Codex", title: "Usage", status: "78% at most", tone: .warn, meter: (value: 0.78, label: "Claude · My login: 78% of the week"),
                             items: [.init(title: "Claude · My login", detail: "31% of 5 hours · 78% of the week", tone: .warn, open: nil),
                                     .init(title: "Codex · Work", detail: "12% of 5 hours · 20% of the week", tone: .ok, open: nil)])
        let ticket = LensPage(kind: "ticket", place: "ticket:ENG-2", eyebrow: "Ticket ENG-2 · Glasscode", title: "Lens pages for every place", status: "Todo", tone: .plain,
                              body: "Each place of the Mac gets a lens version: less on it, the band moves through it, and what opens on the lens opens on the Mac.", prev: "ticket:ENG-1", next: "ticket:ENG-3")
        let today = LensPage(kind: "today", place: "today", eyebrow: "Today · MacBook", title: "Today", status: "2 need you", tone: .warn, body: "2 things are due today.",
                             items: [.init(title: "Needs your OK", detail: "push glasscode/lens-everywhere", tone: .warn, open: "approval:3"),
                                     .init(title: "Prepare the project for publishing", detail: "asks which video to keep", tone: .warn, open: "2c23ab93")])
        return [("session", lens(session)), ("session-row", lens(session, focus: 1)), ("tickets", lens(tickets, focus: 0)), ("ticket", lens(ticket)), ("usage", lens(usage)), ("today", lens(today))]
    }
}
