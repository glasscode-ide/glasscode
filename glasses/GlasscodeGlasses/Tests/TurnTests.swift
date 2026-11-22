import XCTest
@testable import GlasscodeGlasses

/// Turn-taking and sentences for the voice (G13).
final class TurnTests: XCTestCase {
    func testAPauseEndsTheTurnOnce() {
        var turn = TurnTaking()
        let t0 = Date()
        turn.talk(now: t0)
        turn.heard("what needs", now: t0.addingTimeInterval(0.3))
        XCTAssertNil(turn.tick(now: t0.addingTimeInterval(0.6)), "still talking")
        turn.heard("what needs me", now: t0.addingTimeInterval(0.8))
        XCTAssertNil(turn.tick(now: t0.addingTimeInterval(1.2)))
        XCTAssertEqual(turn.tick(now: t0.addingTimeInterval(1.6)), "what needs me")
        XCTAssertNil(turn.tick(now: t0.addingTimeInterval(2.0)), "once")
        XCTAssertEqual(turn.state, .thinking(asked: "what needs me"))
    }

    func testSilenceGoesBackToRest() {
        var turn = TurnTaking()
        let t0 = Date()
        turn.talk(now: t0)
        XCTAssertNil(turn.tick(now: t0.addingTimeInterval(9)))
        XCTAssertEqual(turn.state, .idle)
    }

    func testSentencesComeOnceEach() {
        var s = Sentences()
        XCTAssertEqual(s.next("Two things. Check", final: false), ["Two things."])
        XCTAssertEqual(s.next("Two things. Checkout-fix asks to run npm test. And", final: false), ["Checkout-fix asks to run npm test."])
        XCTAssertEqual(s.next("Two things. Checkout-fix asks to run npm test. And CI failed on #42", final: true), ["And CI failed on #42"])
        XCTAssertEqual(s.next("Two things. Checkout-fix asks to run npm test. And CI failed on #42", final: true), [])
    }

    func testDecimalsDoNotEndASentence() {
        var s = Sentences()
        XCTAssertEqual(s.next("It costs 19.99 now", final: false), [])
        XCTAssertEqual(s.next("It costs 19.99 now.", final: true), ["It costs 19.99 now."])
    }
}
