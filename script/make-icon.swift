// Draws Glasscode's app icon: an amber crescent on a dark rounded tile.
// Usage: swift script/make-icon.swift out.png
import AppKit

let size: CGFloat = 1024
let image = NSImage(size: NSSize(width: size, height: size))
image.lockFocus()
let tile = NSRect(x: 100, y: 100, width: 824, height: 824)
let path = NSBezierPath(roundedRect: tile, xRadius: 185, yRadius: 185)
NSGradient(starting: NSColor(red: 0.09, green: 0.11, blue: 0.16, alpha: 1),
           ending: NSColor(red: 0.04, green: 0.05, blue: 0.08, alpha: 1))!.draw(in: path, angle: -90)
NSColor(white: 1, alpha: 0.08).setStroke()
path.lineWidth = 4
path.stroke()
// Crescent: a full disc minus an offset disc.
let moon = NSBezierPath(ovalIn: NSRect(x: 292, y: 292, width: 440, height: 440))
let cut = NSBezierPath(ovalIn: NSRect(x: 412, y: 380, width: 400, height: 400))
NSGraphicsContext.current?.saveGraphicsState()
let clip = NSBezierPath(rect: NSRect(x: 0, y: 0, width: size, height: size))
clip.append(cut.reversed)
clip.addClip()
NSColor(red: 0.95, green: 0.71, blue: 0.35, alpha: 1).setFill()
moon.fill()
NSGraphicsContext.current?.restoreGraphicsState()
image.unlockFocus()
let rep = NSBitmapImageRep(data: image.tiffRepresentation!)!
try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: CommandLine.arguments[1]))
