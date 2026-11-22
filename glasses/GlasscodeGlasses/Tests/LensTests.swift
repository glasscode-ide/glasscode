import XCTest
@testable import GlasscodeGlasses

/// The band's moves on the lens (G12).
final class LensTests: XCTestCase {
    func testAPinchOnTheGlanceTalks() {
        var lens = LensModel()
        XCTAssertEqual(lens.handle(.pinch, speaking: false), .talk)
        XCTAssertEqual(lens.handle(.left, speaking: false), .none, "the glance never goes back")
    }

    func testAPinchWhileSpeakingStops() {
        var lens = LensModel()
        XCTAssertEqual(lens.handle(.pinch, speaking: true), .stopSpeaking)
    }

    func testListeningSendsOrCancels() {
        var lens = LensModel()
        lens.show(.listening("what needs me"))
        XCTAssertEqual(lens.handle(.pinch, speaking: false), .sendNow)
        XCTAssertEqual(lens.handle(.left, speaking: false), .cancelListening)
        XCTAssertEqual(lens.card, .glance)
        // A swipe down while it listens makes it silent (the talk stays open).
        lens.show(.listening(""))
        XCTAssertEqual(lens.handle(.down, speaking: false), .mute)
        XCTAssertEqual(lens.card, .glance)
    }

    func testSwipingDownFocusesTheLinksAndPinchOpens() {
        var lens = LensModel()
        lens.show(.answer("Two things.", [.init(label: "#42", target: .pr(42, session: nil)), .init(label: "checkout-fix", target: .session("s1"))]))
        XCTAssertEqual(lens.handle(.pinch, speaking: false), .talk, "nothing focused: a pinch talks")
        _ = lens.handle(.down, speaking: false)
        _ = lens.handle(.down, speaking: false)
        XCTAssertEqual(lens.handle(.pinch, speaking: false), .open(.session("s1")))
        _ = lens.handle(.up, speaking: false)
        XCTAssertEqual(lens.handle(.right, speaking: false), .open(.pr(42, session: nil)))
        _ = lens.handle(.up, speaking: false)
        XCTAssertEqual(lens.handle(.pinch, speaking: false), .talk, "back up to nothing")
    }

    func testAnApprovalIsAllowedOnlyWhenItFits() {
        var lens = LensModel()
        let long = LensModel.ApprovalCard(id: 7, launch: "L", who: "Claude", what: "send a message", payload: String(repeating: "x", count: 400), fits: false)
        lens.show(.approval(long))
        XCTAssertEqual(lens.handle(.pinch, speaking: false), .deny(long), "focus starts on Deny")
        _ = lens.handle(.up, speaking: false)
        XCTAssertEqual(lens.handle(.pinch, speaking: false), .none, "Allow is not on the lens")
        let short = LensModel.ApprovalCard(id: 8, launch: "L", who: "Claude", what: "run the tests", payload: "npm test", fits: true)
        lens.show(.approval(short))
        XCTAssertEqual(lens.handle(.pinch, speaking: false), .allow(short))
    }

    func testAQuestionAnswersItsOption() {
        var lens = LensModel()
        lens.show(.question(.init(session: "s1", title: "checkout-fix", question: "Run npm test?", options: [(number: 1, label: "Yes"), (number: 3, label: "No")])))
        _ = lens.handle(.down, speaking: false)
        XCTAssertEqual(lens.handle(.pinch, speaking: false), .answer(session: "s1", option: 3))
    }

    func testAnExplanationMovesByPoint() {
        var lens = LensModel()
        lens.show(.points(title: "#42", points: ["one", "two", "three"], at: 0))
        _ = lens.handle(.down, speaking: false)
        _ = lens.handle(.down, speaking: false)
        _ = lens.handle(.down, speaking: false)
        XCTAssertEqual(lens.card, .points(title: "#42", points: ["one", "two", "three"], at: 2))
        XCTAssertEqual(lens.handle(.left, speaking: false), .back)
        XCTAssertEqual(lens.card, .glance)
    }

    func testListeningGivesWayToWhatFollows() {
        var lens = LensModel()
        lens.show(.listening(""))
        lens.show(.working(["asking"]))
        lens.show(.answer("Done.", []))
        XCTAssertEqual(lens.stack, [.glance, .answer("Done.", [])])
    }
}
