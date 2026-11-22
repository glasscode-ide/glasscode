import UIKit

/// The look of the lens and the phone (`docs/lens-everywhere.md`, Colours). The Ray-Ban Display
/// is additive: black is clear glass, so the ground stays black and the colour is in thin strokes
/// and small shapes, never in filled panels. Dark and low-luminance colours vanish against a
/// bright world, and cyan and magenta wash out in sunlight: the accents are bright and warm-cool
/// (mint, ice, amber, coral), and text is off-white, never pure white. Glows, not shadows.
enum LensPalette {
    static let text = UIColor(hex: 0xEDEFF2)
    static let soft = UIColor(hex: 0x9AA4AE)
    /// The phone's ground: near black, so it reads like the lens.
    static let ground = UIColor(hex: 0x05070A)
    static let panel = UIColor(hex: 0x0D1117)

    static func color(_ tone: LensModel.Tone) -> UIColor {
        switch tone {
        case .ok: return UIColor(hex: 0x5CFFB0)
        case .busy: return UIColor(hex: 0x9BE7FF)
        case .warn: return UIColor(hex: 0xFFC24D)
        case .bad: return UIColor(hex: 0xFF7A66)
        case .plain: return UIColor(hex: 0x8A94A0)
        }
    }
}

extension UIColor {
    convenience init(hex: UInt32) {
        self.init(red: CGFloat((hex >> 16) & 0xFF) / 255, green: CGFloat((hex >> 8) & 0xFF) / 255, blue: CGFloat(hex & 0xFF) / 255, alpha: 1)
    }
}

/// The small images the lens is coloured with: the toolkit draws text in its own two colours
/// only, so a status's colour comes as an image the phone draws.
enum LensArt {
    /// Drawn once each (the lens is redrawn whole on every change). Only the main thread draws.
    nonisolated(unsafe) private static var cache: [String: UIImage] = [:]

    private static func drawn(_ key: String, _ size: CGSize, _ draw: (CGContext, CGSize) -> Void) -> UIImage {
        if let image = cache[key] { return image }
        let format = UIGraphicsImageRendererFormat()
        format.scale = 1
        format.opaque = false
        let image = UIGraphicsImageRenderer(size: size, format: format).image { context in
            draw(context.cgContext, size)
        }
        if cache.count > 64 { cache.removeAll() }
        cache[key] = image
        return image
    }

    /// The status orb: a bright core in a soft halo and a thin ring, in the tone's colour.
    static func orb(_ tone: LensModel.Tone) -> UIImage {
        drawn("orb-\(tone)", CGSize(width: 48, height: 48)) { cg, size in
            let color = LensPalette.color(tone)
            let center = CGPoint(x: size.width / 2, y: size.height / 2)
            let halo = CGGradient(colorsSpace: CGColorSpaceCreateDeviceRGB(), colors: [color.withAlphaComponent(0.55).cgColor, color.withAlphaComponent(0).cgColor] as CFArray, locations: [0, 1])!
            cg.drawRadialGradient(halo, startCenter: center, startRadius: 4, endCenter: center, endRadius: 24, options: [])
            cg.setStrokeColor(color.cgColor)
            cg.setLineWidth(2.5)
            cg.strokeEllipse(in: CGRect(x: 9, y: 9, width: 30, height: 30))
            cg.setFillColor(color.cgColor)
            cg.fillEllipse(in: CGRect(x: 17, y: 17, width: 14, height: 14))
        }
    }

    /// A row's mark: a small dot in the tone's colour, a ring when it has nothing to say.
    static func dot(_ tone: LensModel.Tone) -> UIImage {
        drawn("dot-\(tone)", CGSize(width: 28, height: 28)) { cg, _ in
            let color = LensPalette.color(tone)
            if tone == .plain {
                cg.setStrokeColor(color.cgColor)
                cg.setLineWidth(2)
                cg.strokeEllipse(in: CGRect(x: 8, y: 8, width: 12, height: 12))
            } else {
                cg.setFillColor(color.withAlphaComponent(0.3).cgColor)
                cg.fillEllipse(in: CGRect(x: 4, y: 4, width: 20, height: 20))
                cg.setFillColor(color.cgColor)
                cg.fillEllipse(in: CGRect(x: 8, y: 8, width: 12, height: 12))
            }
        }
    }

    /// The page's accent: a thin line that glows at its start and fades out.
    static func rule(_ tone: LensModel.Tone) -> UIImage {
        drawn("rule-\(tone)", CGSize(width: 560, height: 8)) { cg, size in
            let color = LensPalette.color(tone)
            let fade = CGGradient(colorsSpace: CGColorSpaceCreateDeviceRGB(), colors: [color.cgColor, color.withAlphaComponent(0.35).cgColor, color.withAlphaComponent(0).cgColor] as CFArray, locations: [0, 0.45, 1])!
            cg.addRect(CGRect(x: 0, y: 3, width: size.width, height: 2))
            cg.clip()
            cg.drawLinearGradient(fade, start: .zero, end: CGPoint(x: size.width, y: 0), options: [])
        }
    }

    /// The meter: a thin track and its glowing fill, `value` of 0 to 1.
    static func meter(_ value: Double, _ tone: LensModel.Tone) -> UIImage {
        let steps = Int((min(max(value, 0), 1) * 100).rounded())
        return drawn("meter-\(tone)-\(steps)", CGSize(width: 560, height: 18)) { cg, size in
            let color = LensPalette.color(tone)
            let track = CGRect(x: 0, y: 7, width: size.width, height: 4)
            cg.setFillColor(LensPalette.soft.withAlphaComponent(0.35).cgColor)
            cg.fill(track)
            let filled = CGRect(x: 0, y: 5, width: max(8, size.width * CGFloat(steps) / 100), height: 8)
            cg.setShadow(offset: .zero, blur: 6, color: color.withAlphaComponent(0.8).cgColor)
            cg.setFillColor(color.cgColor)
            cg.fill(filled)
        }
    }
}
