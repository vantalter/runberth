// Renders the menu bar template images from the tray SVGs:
//   swift app/scripts/generate-tray-icons.swift app/assets
// Writes tray*.png at 36 pixels, shown at 18 points on Retina displays.
import AppKit

let assets = URL(fileURLWithPath: CommandLine.arguments[1])
for name in ["tray", "tray-running", "tray-failed"] {
    let svg = NSImage(contentsOf: assets.appendingPathComponent("\(name).svg"))!
    let pixels = 36
    let rep = NSBitmapImageRep(
        bitmapDataPlanes: nil, pixelsWide: pixels, pixelsHigh: pixels, bitsPerSample: 8,
        samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
        bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    svg.draw(in: NSRect(x: 0, y: 0, width: pixels, height: pixels))
    NSGraphicsContext.restoreGraphicsState()
    try! rep.representation(using: .png, properties: [:])!
        .write(to: assets.appendingPathComponent("\(name).png"))
}
