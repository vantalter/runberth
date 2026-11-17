// Generates the tray icons from one geometry, on a 24-unit grid:
//   swift app/scripts/generate-tray-icons.swift app/assets/tray
//
// macOS (40-pixel PNGs, drawn at 20 points): the glyph as a template, a copy
// cut out for a badge, and the coloured badges; the app tints the glyph with
// the menu bar's text colour and rotates the running badge.
//
// Linux (linux/*.svg): symbolic icons, one per state and eight frames for the
// running spinner. GTK recolours symbolic icons by forcing every shape's fill,
// so each icon is one filled path, with cut-outs already subtracted: no
// strokes and no masks.
import AppKit

let output = URL(fileURLWithPath: CommandLine.arguments[1])

/// An SVG path with absolute M, L, C, and Z commands, on the 24-unit grid.
func svgPath(_ data: String) -> CGPath {
    let path = CGMutablePath()
    let scanner = Scanner(string: data)
    scanner.charactersToBeSkipped = CharacterSet(charactersIn: " ,")
    var command = "M"
    func number() -> CGFloat { CGFloat(scanner.scanDouble()!) }
    func point() -> CGPoint { CGPoint(x: number(), y: number()) }
    while !scanner.isAtEnd {
        if let letter = scanner.scanCharacters(from: CharacterSet(charactersIn: "MLCZ")) {
            command = letter
        }
        switch command {
        case "M": path.move(to: point()); command = "L"
        case "L": path.addLine(to: point())
        case "C":
            let (c1, c2, end) = (point(), point(), point())
            path.addCurve(to: end, control1: c1, control2: c2)
        default: path.closeSubpath()
        }
    }
    return path
}

func stroke(_ path: CGPath, width: CGFloat) -> CGPath {
    path.copy(strokingWithWidth: width, lineCap: .round, lineJoin: .round, miterLimit: 10)
}

func polyline(_ points: [CGPoint]) -> CGPath {
    let path = CGMutablePath()
    path.addLines(between: points)
    return path
}

func circle(_ center: CGPoint, _ radius: CGFloat) -> CGPath {
    CGPath(ellipseIn: CGRect(x: center.x - radius, y: center.y - radius, width: 2 * radius, height: 2 * radius),
           transform: nil)
}

// The berth: an open hexagon, its entrance at the lower right.
let frame = stroke(polyline([
    CGPoint(x: 20.6, y: 13.2), CGPoint(x: 20.6, y: 7.2), CGPoint(x: 12, y: 2.2), CGPoint(x: 3.4, y: 7.2),
    CGPoint(x: 3.4, y: 16.8), CGPoint(x: 12, y: 21.8), CGPoint(x: 15.6, y: 19.7),
]), width: 2)
let hull = svgPath("M4.8 13.4 L17 10.7 C17.8 10.5 18.4 11.2 18.2 12 L17.1 15.8 C16.9 16.6 16.3 17.1 15.5 17.2 L9.2 18.1 C8.5 18.2 7.8 17.9 7.4 17.3 Z")
let cabin = svgPath("M9.7 8.9 L14.6 7.9 C15.1 7.8 15.5 8.1 15.5 8.6 L15.7 11 L10.2 12.1 Z")
let window = svgPath("M10.8 9.7 L14.3 9 L14.4 10.3 L11 11 Z")
let glyph = frame.union(hull).union(cabin.subtracting(window))

// The badge sits in the berth's entrance, with a gap around it. Its symbols
// are drawn for a 4.1-unit disc and scaled to the badge's radius.
let center = CGPoint(x: 19, y: 18.9)
let radius: CGFloat = 4.9
let scale = radius / 4.1
/// A point `dx`, `dy` units from the badge's centre, at the 4.1-unit design size.
func at(_ dx: CGFloat, _ dy: CGFloat) -> CGPoint {
    CGPoint(x: center.x + dx * scale, y: center.y + dy * scale)
}
let cut = glyph.subtracting(circle(center, radius + 1))
let disc = circle(center, radius)
let check = stroke(polyline([at(-1.9, 0.1), at(-0.5, 1.5), at(2, -1.4)]), width: 1.4 * scale)
let exclamation = CGPath(roundedRect: CGRect(origin: at(-0.68, -2.7), size: CGSize(width: 1.36 * scale, height: 3.5 * scale)),
                         cornerWidth: 0.68 * scale, cornerHeight: 0.68 * scale, transform: nil)
    .union(circle(at(0, 2.35), 0.78 * scale))
let cross = stroke(polyline([at(-1.5, -1.5), at(1.5, 1.5)]), width: 1.4 * scale)
    .union(stroke(polyline([at(1.5, -1.5), at(-1.5, 1.5)]), width: 1.4 * scale))
/// Three quarters of a ring, starting at the top, rotated by `degrees`.
func spinner(_ degrees: CGFloat) -> CGPath {
    let arc = CGMutablePath()
    let start = (-90 + degrees) * .pi / 180
    arc.addArc(center: center, radius: 3.3 * scale, startAngle: start, endAngle: start + 1.5 * .pi, clockwise: false)
    return stroke(arc, width: 1.6 * scale)
}

// MARK: macOS

func png(_ name: String, draw: (CGContext) -> Void) {
    let pixels = 40
    let rep = NSBitmapImageRep(
        bitmapDataPlanes: nil, pixelsWide: pixels, pixelsHigh: pixels, bitsPerSample: 8,
        samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
        bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.saveGraphicsState()
    let context = NSGraphicsContext(bitmapImageRep: rep)!.cgContext
    NSGraphicsContext.current = NSGraphicsContext(cgContext: context, flipped: true)
    // The grid's y axis points down, as in SVG.
    context.translateBy(x: 0, y: CGFloat(pixels))
    context.scaleBy(x: CGFloat(pixels) / 24, y: -CGFloat(pixels) / 24)
    draw(context)
    NSGraphicsContext.restoreGraphicsState()
    try! rep.representation(using: .png, properties: [:])!.write(to: output.appendingPathComponent("\(name).png"))
}

func fill(_ context: CGContext, _ path: CGPath, _ color: NSColor) {
    context.addPath(path)
    context.setFillColor(color.cgColor)
    context.fillPath()
}

func badge(_ name: String, _ color: NSColor, _ symbol: CGPath) {
    png(name) { context in
        fill(context, disc, color)
        fill(context, symbol, .white)
    }
}

try! FileManager.default.createDirectory(at: output.appendingPathComponent("linux"), withIntermediateDirectories: true)
png("glyph") { fill($0, glyph, .black) }
png("glyph-cut") { fill($0, cut, .black) }
badge("badge-success", NSColor(srgbRed: 0.20, green: 0.70, blue: 0.33, alpha: 1), check)
badge("badge-failed", NSColor(srgbRed: 0.92, green: 0.23, blue: 0.20, alpha: 1), exclamation)
badge("badge-offline", NSColor(srgbRed: 0.56, green: 0.56, blue: 0.58, alpha: 1), cross)
// Unrotated; the app turns it around the badge's centre.
// Orange like the boat's cabin, and unlike blue, visible on a tinted menu bar.
png("badge-running") { fill($0, spinner(0), NSColor(srgbRed: 1, green: 0.58, blue: 0.05, alpha: 1)) }

// MARK: Linux

func svgData(_ path: CGPath) -> String {
    var data = ""
    let format = { (point: CGPoint) in String(format: "%.2f %.2f", point.x, point.y) }
    path.applyWithBlock { element in
        let points = element.pointee.points
        switch element.pointee.type {
        case .moveToPoint: data += "M\(format(points[0]))"
        case .addLineToPoint: data += "L\(format(points[0]))"
        case .addQuadCurveToPoint: data += "Q\(format(points[0])) \(format(points[1]))"
        case .addCurveToPoint: data += "C\(format(points[0])) \(format(points[1])) \(format(points[2]))"
        case .closeSubpath: data += "Z"
        @unknown default: break
        }
    }
    return data
}

/// KDE recolours through ColorScheme-Text; GTK replaces the fill outright.
func symbolic(_ name: String, _ path: CGPath) {
    let svg = """
        <svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24">\
        <style>.ColorScheme-Text{color:#2e3436}</style>\
        <path class="ColorScheme-Text" fill="currentColor" fill-rule="evenodd" d="\(svgData(path))"/></svg>

        """
    try! svg.write(to: output.appendingPathComponent("linux/runberth-tray\(name)-symbolic.svg"),
                   atomically: true, encoding: .utf8)
}

symbolic("", glyph)
symbolic("-success", cut.union(disc.subtracting(check)))
symbolic("-failed", cut.union(disc.subtracting(exclamation)))
symbolic("-offline", cut.union(disc.subtracting(cross)))
for frame in 0..<8 {
    symbolic("-running-\(frame)", cut.union(spinner(CGFloat(frame) * 45)))
}
