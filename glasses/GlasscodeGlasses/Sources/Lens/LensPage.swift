import Foundation

/// A place as the lens and the phone show it (`docs/lens-everywhere.md`): the Mac builds it for
/// the place its window shows (`glasscode_core::lens::Page`), and both draw the same page. The
/// lens shows a few rows at a time; the band moves through them.
struct LensPage: Equatable {
    struct Item: Equatable {
        var title: String
        var detail: String
        var tone: LensModel.Tone
        /// The place it opens (`place` of that page), when it opens one.
        var open: String?
    }

    /// session, today, tickets, ticket, prs, pr, usage, sessions, settings.
    var kind: String
    /// The word that opens it again (`place.open`).
    var place: String
    var eyebrow: String
    var title: String
    var status: String
    var tone: LensModel.Tone
    /// 0 to 1, with what it is: the usage.
    var meter: (value: Double, label: String)?
    var body: String
    var items: [Item]
    /// How many rows there are in all (`items` holds the first ones).
    var total: Int
    /// The places before and after it in its list: the tickets one by one.
    var prev: String?
    var next: String?

    static func == (a: LensPage, b: LensPage) -> Bool {
        a.kind == b.kind && a.place == b.place && a.eyebrow == b.eyebrow && a.title == b.title && a.status == b.status && a.tone == b.tone
            && a.meter?.value == b.meter?.value && a.meter?.label == b.meter?.label && a.body == b.body && a.items == b.items && a.total == b.total && a.prev == b.prev && a.next == b.next
    }

    init(kind: String, place: String, eyebrow: String = "", title: String, status: String = "", tone: LensModel.Tone = .plain, meter: (value: Double, label: String)? = nil, body: String = "", items: [Item] = [], total: Int? = nil, prev: String? = nil, next: String? = nil) {
        self.kind = kind
        self.place = place
        self.eyebrow = eyebrow
        self.title = title
        self.status = status
        self.tone = tone
        self.meter = meter
        self.body = body
        self.items = items
        self.total = total ?? items.count
        self.prev = prev
        self.next = next
    }

    /// From the Mac's JSON (the door's `place` event, `place.open`'s answer).
    init?(_ json: JSON) {
        guard let kind = json.string("kind"), let place = json.string("place") else { return nil }
        let status = json.object("status")
        let meter = json.object("meter").flatMap { m -> (value: Double, label: String)? in
            guard let value = (m["value"] as? NSNumber)?.doubleValue else { return nil }
            return (value: min(max(value, 0), 1), label: m.string("label") ?? "")
        }
        let items = json.array("items").map { i in
            Item(title: i.string("title") ?? "", detail: i.string("detail") ?? "", tone: LensModel.Tone(word: i.string("tone")), open: i.string("open"))
        }
        self.init(
            kind: kind,
            place: place,
            eyebrow: json.string("eyebrow") ?? "",
            title: json.string("title") ?? "",
            status: status?.string("label") ?? "",
            tone: LensModel.Tone(word: status?.string("tone")),
            meter: meter,
            body: json.string("body") ?? "",
            items: items,
            total: json.int("total") ?? items.count,
            prev: json.string("prev"),
            next: json.string("next")
        )
    }

    /// The rows the lens shows around the focused one (at most `count`), and where they start.
    func window(focus: Int, count: Int) -> (start: Int, rows: ArraySlice<Item>) {
        guard !items.isEmpty else { return (0, items[0..<0]) }
        let start = max(0, min(items.count - count, focus - count / 2))
        let end = min(items.count, start + count)
        return (start, items[start..<end])
    }
}

extension LensModel.Tone {
    /// A tone's word in the Mac's JSON.
    init(word: String?) {
        switch word {
        case "ok": self = .ok
        case "busy": self = .busy
        case "warn": self = .warn
        case "bad": self = .bad
        default: self = .plain
        }
    }
}
