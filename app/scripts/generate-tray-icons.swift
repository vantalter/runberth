// Generates the tray icons from one geometry, on a 24-unit grid:
//   swift app/scripts/generate-tray-icons.swift app/assets/tray
//
// macOS (40-pixel PNGs, drawn at 20 points): the glyph as a template, copies
// cut out for the round badges and for the attention triangle, and the
// coloured badges; the app tints the glyph with
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

// The glyph is drawn on a 1024-unit square, as the artwork it follows,
// then scaled onto the 24-unit grid.
func rounded(_ path: CGPath, _ radius: CGFloat) -> CGPath {
    path.union(stroke(path, width: 2 * radius))
}
// The berth: a hexagon whose lower-right side is a solid ledge, with a step
// at its left end where the berth opens.
let frame = stroke(svgPath("M512 160 L840 345 L840 675 L512 862 L182 675 L182 345 Z"), width: 58)
let ledge = rounded(svgPath("M462 750 L770 590 L840 555 L840 690 L512 878 L462 848 Z"), 10)
// The boat: a hull whose rim passes behind the cabin, around an open deck,
// and a cabin with a lid and two windows.
let hull = rounded(svgPath("M258 572 C268 500 370 440 470 418 L690 405 C735 408 752 428 750 460 L748 590 C700 635 600 678 380 706 C350 709 330 700 320 690 Z"), 6)
let deck = svgPath("M292 558 C306 506 392 460 462 446 L462 470 L566 506 L690 466 L690 446 C716 446 728 452 728 470 L726 556 C600 574 452 578 292 558 Z")
let cabin = rounded(svgPath("M462 344 L572 292 L688 330 L688 446 L566 506 L462 466 Z"), 14)
let windows = svgPath("M480 384 L556 412 L556 464 L480 434 Z M594 414 L668 384 L668 434 L594 464 Z")
let lid = stroke(svgPath("M456 364 L566 404 L694 360"), width: 13)
let behindCabin = cabin.union(stroke(cabin, width: 22))
let artwork = frame.union(ledge)
    .union(hull.subtracting(deck).subtracting(behindCabin))
    .union(cabin.subtracting(windows).subtracting(lid))
// The hexagon's outer edge, x 153 to 869 and y 131 to 891, fills the grid
// from 1.2 to 22.8 down, centred across.
let unit: CGFloat = 21.6 / 760
var toGrid = CGAffineTransform(a: unit, b: 0, c: 0, d: unit, tx: 12 - 511 * unit, ty: 1.2 - 131 * unit)
let glyph = artwork.copy(using: &toGrid)!

// The badge sits at the top left, clear of the boat and the ledge, with a
// gap around it. Its symbols
// are drawn for a 4.1-unit disc and scaled to the badge's radius.
let center = CGPoint(x: 4.9, y: 4.9)
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
// The attention badge is a warning triangle, so it differs from the round
// badges by its shape alone, as in the one-colour Linux icons. It fills the
// same corner, and the glyph is cut around the triangle instead of a disc.
let triangle = rounded(svgPath("M4.9 0.9 L9.05 8.1 L0.75 8.1 Z"), 0.7)
let triangleCut = glyph.subtracting(triangle.union(stroke(triangle, width: 2)))
let mark = CGPath(roundedRect: CGRect(x: 4.3, y: 3.2, width: 1.2, height: 2.75),
                  cornerWidth: 0.6, cornerHeight: 0.6, transform: nil)
    .union(circle(CGPoint(x: 4.9, y: 7.0), 0.65))
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
png("glyph-cut-attention") { fill($0, triangleCut, .black) }
// Amber with a dark mark, as macOS draws its warning triangles.
png("badge-attention") { context in
    fill(context, triangle, NSColor(srgbRed: 1, green: 0.77, blue: 0.04, alpha: 1))
    fill(context, mark, NSColor(white: 0.12, alpha: 1))
}
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
symbolic("-attention", triangleCut.union(triangle.subtracting(mark)))
for frame in 0..<8 {
    symbolic("-running-\(frame)", cut.union(spinner(CGFloat(frame) * 45)))
}
