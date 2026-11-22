// Draws the app icon (a kitchen scale on the app's green) and writes a 1024x1024 PNG.
// Usage: swift make_icon.swift out.png
import AppKit

let size: CGFloat = 1024
let img = NSImage(size: NSSize(width: size, height: size))
img.lockFocus()

let green = NSColor(red: 0x2f/255, green: 0x6f/255, blue: 0x5e/255, alpha: 1)
let cream = NSColor(red: 0xf5/255, green: 0xf1/255, blue: 0xea/255, alpha: 1)

// macOS icon grid: ~824px rounded square centered in 1024.
let tile = NSRect(x: 100, y: 100, width: 824, height: 824)
let tilePath = NSBezierPath(roundedRect: tile, xRadius: 185, yRadius: 185)
let gradient = NSGradient(starting: NSColor(red: 0x3a/255, green: 0x80/255, blue: 0x6c/255, alpha: 1), ending: green)!
gradient.draw(in: tilePath, angle: -90)

// Platform (ellipse) on top.
cream.setFill()
NSBezierPath(ovalIn: NSRect(x: 230, y: 600, width: 564, height: 130)).fill()
// Body.
NSBezierPath(roundedRect: NSRect(x: 200, y: 260, width: 624, height: 300), xRadius: 70, yRadius: 70).fill()
// Display.
green.setFill()
NSBezierPath(roundedRect: NSRect(x: 352, y: 350, width: 320, height: 120), xRadius: 22, yRadius: 22).fill()
// Digits.
let para = NSMutableParagraphStyle(); para.alignment = .center
let attrs: [NSAttributedString.Key: Any] = [
    .font: NSFont.monospacedDigitSystemFont(ofSize: 92, weight: .bold),
    .foregroundColor: cream,
    .paragraphStyle: para,
]
NSAttributedString(string: "0.0", attributes: attrs).draw(in: NSRect(x: 352, y: 352, width: 320, height: 112))

img.unlockFocus()
let rep = NSBitmapImageRep(data: img.tiffRepresentation!)!
try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: CommandLine.arguments[1]))
