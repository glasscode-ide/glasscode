import XCTest
import MWDATCore
import MWDATMockDevice
@testable import GlasscodeGlasses

/// The glasses stay on Glasscode: whatever ends the session on the glasses, the app starts a
/// new one by itself, and only the person's own Disconnect leaves them to Meta. Run against
/// the toolkit's simulated Display glasses.
@MainActor
final class StayOnTests: XCTestCase {
    private var glasses: (any MockGlasses)!
    private var session: GlassesSession!

    override func setUp() async throws {
        MockDeviceKit.shared.enable()
        glasses = try MockDeviceKit.shared.pairGlasses(model: .metaRayBanDisplay)
        glasses.powerOn()
        glasses.unfold()
        glasses.don()
        session = GlassesSession()
        session.show(GlasscodeScreens.blank())
        try await until("the lens starts", 20) { self.session.isConnected }
    }

    override func tearDown() async throws {
        session.disconnect()
        session = nil
        await MockDeviceKit.shared.unpairDevice(glasses)
        await MockDeviceKit.shared.disable()
    }

    /// A tap and hold on the touchpad ends the session on the glasses. We come back.
    func testComesBackAfterTheGlassesEndTheSession() async throws {
        glasses.services.captouch.tapAndHold()
        try await until("the session ends", 10) { !self.session.isConnected }
        try await until("a new session starts by itself", 20) { self.session.isConnected }
    }

    /// It keeps coming back, not just once.
    func testComesBackEveryTime() async throws {
        for round in 1...3 {
            glasses.services.captouch.tapAndHold()
            try await until("the session ends (round \(round))", 10) { !self.session.isConnected }
            try await until("a new session starts (round \(round))", 20) { self.session.isConnected }
        }
    }

    /// Disconnect on the phone hands the glasses back: nothing reconnects until Connect.
    func testDisconnectStaysDisconnected() async throws {
        session.disconnect()
        XCTAssertFalse(session.stayOn)
        try await Task.sleep(for: .seconds(8))
        XCTAssertFalse(session.isConnected, "reconnected after the person disconnected")
        session.show(GlasscodeScreens.blank())          // drawing a screen must not reconnect either
        try await Task.sleep(for: .seconds(4))
        XCTAssertFalse(session.isConnected, "a redraw reconnected after the person disconnected")

        session.connect()
        try await until("Connect brings the lens back", 20) { self.session.isConnected }
    }

    /// Folding the glasses ends the session; opening them brings Glasscode back.
    func testComesBackWhenTheGlassesAreOpenedAgain() async throws {
        glasses.fold()
        try await until("the session ends", 10) { !self.session.isConnected }
        try await Task.sleep(for: .seconds(5))
        glasses.unfold()
        glasses.don()
        try await until("a new session starts by itself", 90) { self.session.isConnected }
    }

    /// One tap pauses the session on the glasses, another resumes it. The session is kept.
    func testPauseAndResumeKeepTheSession() async throws {
        glasses.services.captouch.tap()
        try await until("the session pauses", 10) { self.session.paused }
        glasses.services.captouch.tap()
        try await until("the session resumes", 10) { !self.session.paused }
        try await until("the lens is on", 10) { self.session.isConnected }
    }

    private func until(_ what: String, _ seconds: Double, _ done: @escaping () -> Bool) async throws {
        let end = Date().addingTimeInterval(seconds)
        while Date() < end {
            if done() { return }
            try await Task.sleep(for: .milliseconds(100))
        }
        XCTFail("Timed out waiting until \(what) (status: \(session?.statusText ?? "—"))")
        throw CancellationError()
    }
}
