import MWDATDisplay

// Builds the single root FlexBox Glasscode shows on the glasses lens.
// One screen, redrawn whenever Glasscode's state changes. No taps needed — the Neural
// Band / phone drives Glasscode; the lens is a heads-up readout.
enum GlassesHUD {
    static func screen(_ hud: GlasscodeHUD) -> FlexBox {
        FlexBox(direction: .column, spacing: 12) {
            // Glasscode status band
            FlexBox(direction: .column, spacing: 4) {
                Text(hud.live ? "Glasscode" : "Glasscode · asleep", style: .heading)
                Text(hud.status, style: .body)
            }
            .padding(24)
            .background(.card)

            // Current session
            if let cur = hud.currentTitle {
                FlexBox(direction: .column, spacing: 4) {
                    Text("Current session", style: .meta, color: .secondary)
                    Text(cur, style: .body)
                }
                .padding(24)
                .background(.card)
            }

            // Suggestion Glasscode is pointing at
            if let sug = hud.suggestion, sug != hud.currentTitle {
                FlexBox(direction: .column, spacing: 4) {
                    Text("Suggested", style: .meta, color: .secondary)
                    Text(sug, style: .body)
                }
                .padding(24)
                .background(.card)
            }

            // Needs-you count
            if hud.needs > 0 {
                FlexBox(direction: .column) {
                    Text("\(hud.needs) session\(hud.needs == 1 ? "" : "s") need you", style: .meta, color: .secondary)
                }
                .padding(24)
                .background(.card)
            }
        }
    }
}
