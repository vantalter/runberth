// Renders the app icon at every size the asset catalog lists, from the
// square artwork in app/assets/runberth-icon.png:
//   swift app/scripts/generate-app-icon.swift app/assets/runberth-icon.png \
//       app/macos/Runberth/Assets.xcassets/AppIcon.appiconset
// The artwork fills a rounded square in Apple's icon grid, with its shadow.
import AppKit

let artwork = NSImage(contentsOf: URL(fileURLWithPath: CommandLine.arguments[1]))!
let output = URL(fileURLWithPath: CommandLine.arguments[2])

func icon(pixels: Int) -> Data {
    let rep = NSBitmapImageRep(
        bitmapDataPlanes: nil, pixelsWide: pixels, pixelsHigh: pixels, bitsPerSample: 8,
        samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
        bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.saveGraphicsState()
    let context = NSGraphicsContext(bitmapImageRep: rep)!
    context.imageInterpolation = .high
    NSGraphicsContext.current = context
    let size = CGFloat(pixels)
    // Apple's grid: an 824-point body with a 185-point corner radius on 1024.
    let inset = size * 100 / 1024
    let body = NSRect(x: inset, y: inset, width: size - 2 * inset, height: size - 2 * inset)
    let shape = NSBezierPath(roundedRect: body, xRadius: size * 185 / 1024, yRadius: size * 185 / 1024)
    NSGraphicsContext.saveGraphicsState()
    let shadow = NSShadow()
    shadow.shadowColor = NSColor.black.withAlphaComponent(0.3)
    shadow.shadowOffset = NSSize(width: 0, height: -size * 10 / 1024)
    shadow.shadowBlurRadius = size * 20 / 1024
    shadow.set()
    NSColor.black.setFill()
    shape.fill()
    NSGraphicsContext.restoreGraphicsState()
    shape.addClip()
    artwork.draw(in: body, from: .zero, operation: .copy, fraction: 1)
    NSGraphicsContext.restoreGraphicsState()
    return rep.representation(using: .png, properties: [:])!
}

for points in [16, 32, 128, 256, 512] {
    for scale in [1, 2] {
        let name = scale == 1 ? "icon_\(points)x\(points).png" : "icon_\(points)x\(points)@2x.png"
        try! icon(pixels: points * scale).write(to: output.appendingPathComponent(name))
    }
}
