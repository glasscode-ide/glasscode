import XCTest
@testable import GlasscodeGlasses

/// A place's page on the lens (`docs/lens-everywhere.md`): what the Mac's window shows, the band's
/// moves through it, and how it follows the Mac.
final class PageTests: XCTestCase {
    private func tickets() -> LensPage {
        LensPage(JSON_tickets)!
    }

    func testThePageComesFromTheMacsJSON() {
        let page = tickets()
        XCTAssertEqual(page.kind, "tickets")
        XCTAssertEqual(page.place, "tickets")
        XCTAssertEqual(page.status, "3 open · 1 to review")
        XCTAssertEqual(page.tone, .plain)
        XCTAssertEqual(page.items.count, 3)
        XCTAssertEqual(page.items[0], .init(title: "Voice finds sessions", detail: "ENG-1 · In Progress", tone: .busy, open: "ticket:ENG-1"))
        XCTAssertEqual(page.total, 4)
        XCTAssertNil(LensPage(["title": "no kind"]))
        let usage = LensPage(["kind": "usage", "place": "usage", "title": "Usage", "meter": ["value": 1.4, "label": "Claude: 98%"], "status": ["label": "98% at most", "tone": "bad"]])!
        XCTAssertEqual(usage.meter?.value, 1)
        XCTAssertEqual(usage.tone, .bad)
    }

    func testTheBandMovesThroughTheRowsAndOpensOne() {
        var lens = LensModel()
        lens.follow(tickets(), opened: false)
        XCTAssertEqual(lens.handle(.pinch, speaking: false), .talk, "nothing chosen: a pinch talks about it")
        _ = lens.handle(.down, speaking: false)
        _ = lens.handle(.down, speaking: false)
        XCTAssertEqual(lens.handle(.right, speaking: false), .place("ticket:ENG-2"))
        XCTAssertEqual(lens.handle(.pinch, speaking: false), .place("ticket:ENG-2"))
        _ = lens.handle(.down, speaking: false)
        _ = lens.handle(.down, speaking: false)
        XCTAssertEqual(lens.handle(.right, speaking: false), .none, "the last row opens nothing")
        XCTAssertEqual(lens.handle(.pinch, speaking: false), .talk, "a pinch on a row that opens nothing talks about it")
    }

    func testATicketGoesToTheNextOneByOne() {
        var lens = LensModel()
        let ticket = LensPage(kind: "ticket", place: "ticket:ENG-2", title: "Lens pages", prev: "ticket:ENG-1", next: "ticket:ENG-3")
        lens.follow(ticket, opened: true)
        XCTAssertEqual(lens.handle(.down, speaking: false), .place("ticket:ENG-3"))
        XCTAssertEqual(lens.handle(.up, speaking: false), .place("ticket:ENG-1"))
        let last = LensPage(kind: "ticket", place: "ticket:ENG-3", title: "Colours", prev: "ticket:ENG-2")
        lens.follow(last, opened: true)
        XCTAssertEqual(lens.handle(.down, speaking: false), .none)
        XCTAssertEqual(lens.handle(.left, speaking: false), .back)
    }

    func testTheLensFollowsTheMacAndBackReturns() {
        var lens = LensModel()
        lens.follow(tickets(), opened: false)
        // The Mac went somewhere else by itself: that page takes this one's place.
        let usage = LensPage(kind: "usage", place: "usage", title: "Usage")
        lens.follow(usage, opened: false)
        XCTAssertEqual(lens.stack, [.glance, .page(usage)])
        // Opened from the lens: on top, and back returns to where the person was.
        let ticket = LensPage(kind: "ticket", place: "ticket:ENG-1", title: "Voice finds sessions")
        lens.follow(ticket, opened: true)
        XCTAssertEqual(lens.stack, [.glance, .page(usage), .page(ticket)])
        XCTAssertEqual(lens.handle(.left, speaking: false), .back)
        XCTAssertEqual(lens.card, .page(usage))
        // The same place again: updated in place, the focus kept.
        _ = lens.handle(.down, speaking: false)
        lens.follow(LensPage(kind: "usage", place: "usage", title: "Usage", status: "40% at most"), opened: false)
        XCTAssertEqual(lens.stack.count, 2)
        XCTAssertEqual(lens.page?.status, "40% at most")
    }

    func testWhatThePersonDoesIsNotInterrupted() {
        var lens = LensModel()
        lens.show(.listening("open the"))
        let usage = LensPage(kind: "usage", place: "usage", title: "Usage")
        lens.follow(usage, opened: false)
        XCTAssertEqual(lens.card, .listening("open the"), "listening keeps the lens")
        XCTAssertEqual(lens.stack, [.glance, .page(usage), .listening("open the")])
        // A request being worked on gives way to the place it opened.
        lens.show(.working(["opening"]))
        let ticket = LensPage(kind: "ticket", place: "ticket:ENG-1", title: "Voice finds sessions")
        lens.follow(ticket, opened: false)
        XCTAssertEqual(lens.card, .page(ticket))
    }

    func testAnUpdateMovesNothing() {
        var lens = LensModel()
        let tickets = tickets()
        lens.follow(tickets, opened: false)
        // Swiped back to the glance: an update of the page does not bring it back.
        _ = lens.handle(.left, speaking: false)
        var updated = tickets
        updated.status = "4 open · 1 to review"
        lens.refresh(updated)
        XCTAssertEqual(lens.card, .glance)
        XCTAssertEqual(lens.stack, [.glance])
        // An answer being read stays on top; the page under it is updated.
        lens.follow(tickets, opened: false)
        lens.show(.answer("Two tickets moved.", []))
        lens.refresh(updated)
        XCTAssertEqual(lens.card, .answer("Two tickets moved.", []))
        XCTAssertEqual(lens.stack[lens.stack.count - 2], .page(updated))
    }

    func testUpdatesUnderACardAreNotStacked() {
        var lens = LensModel()
        lens.show(.approval(.init(id: 1, launch: "L", who: "Claude", what: "run tests", payload: "npm test", fits: true)))
        let usage = LensPage(kind: "usage", place: "usage", title: "Usage")
        for _ in 0..<5 { lens.follow(usage, opened: false) }
        XCTAssertEqual(lens.stack.filter { if case .page = $0 { return true } else { return false } }.count, 1)
        XCTAssertEqual(lens.stack.count, 3)
    }

    func testTheLensShowsFourRowsAroundTheChosenOne() {
        let items = (1...10).map { LensPage.Item(title: "row \($0)", detail: "", tone: .plain, open: nil) }
        let page = LensPage(kind: "tickets", place: "tickets", title: "Many", items: items)
        XCTAssertEqual(page.window(focus: 0, count: 4).start, 0)
        XCTAssertEqual(page.window(focus: 5, count: 4).start, 3)
        XCTAssertEqual(page.window(focus: 9, count: 4).start, 6)
        XCTAssertEqual(page.window(focus: 9, count: 4).rows.count, 4)
        XCTAssertEqual(LensPage(kind: "x", place: "x", title: "none").window(focus: 0, count: 4).rows.count, 0)
    }
}

/// The board as the Mac sends it.
let JSON_tickets: JSON = [
    "kind": "tickets", "place": "tickets", "eyebrow": "Board", "title": "Your tickets",
    "status": ["label": "3 open · 1 to review", "tone": "plain"],
    "items": [
        ["title": "Voice finds sessions", "detail": "ENG-1 · In Progress", "tone": "busy", "open": "ticket:ENG-1"],
        ["title": "Lens pages", "detail": "ENG-2 · Todo", "tone": "plain", "open": "ticket:ENG-2"],
        ["title": "Colours", "detail": "ENG-3 · Todo", "tone": "warn"],
    ],
    "total": 4,
]
