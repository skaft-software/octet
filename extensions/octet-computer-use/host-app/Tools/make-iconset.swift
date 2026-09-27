// Build the octet computer-use app icon set from supplied artwork.
//
// The source is the designer's exported PNG: a rounded-square plate carrying
// the glass glyph, with real transparency outside the plate. That transparency
// is what makes this reliable - the plate's extent is read from the alpha
// channel rather than guessed from colour, so there is no background to detect,
// trim, or accidentally bake into the icon.
//
// macOS additionally expects an icon's artwork to occupy a fixed, smaller
// portion of a transparent canvas, because the system applies its own shadow and
// rounded mask. So this tool finds the plate, places it on a correctly inset
// transparent canvas, and writes every size macOS may ask for.
//
// Usage:
//   swift make-iconset.swift <source.png> <output.icns> [work-directory]

import AppKit
import Foundation

// Apple's app icon grid: the artwork occupies this fraction of the canvas on
// each edge, leaving room for the system's own shadow and mask.
private let artworkInsetFraction: CGFloat = 0.0996

/// Alpha considered "opaque enough" to be artwork. Low enough to keep the
/// antialiased rim, high enough to ignore stray near-zero noise.
private let alphaThreshold: UInt8 = 8

/// Axis-aligned bounds of the artwork, in image space (top-left origin).
private struct Bounds {
    var minX = Int.max
    var minY = Int.max
    var maxX = -1
    var maxY = -1

    var isEmpty: Bool { maxX < minX }
    var width: Int { maxX - minX + 1 }
    var height: Int { maxY - minY + 1 }
}

/// Find the artwork's extent from the alpha channel.
///
/// Requires a source with transparency. A fully opaque image has no such
/// channel, so the tool refuses rather than silently cropping by colour and
/// baking a background halo into the icon.
private func artworkBounds(of image: CGImage) -> Bounds? {
    let width = image.width
    let height = image.height
    let channels = 4
    var pixels = [UInt8](repeating: 0, count: width * height * channels)
    guard let context = CGContext(
        data: &pixels,
        width: width,
        height: height,
        bitsPerComponent: 8,
        bytesPerRow: width * channels,
        space: CGColorSpaceCreateDeviceRGB(),
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
    ) else { return nil }
    context.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))

    var bounds = Bounds()
    var anyOpaque = false
    for y in 0..<height {
        for x in 0..<width {
            guard pixels[(y * width + x) * channels + 3] > alphaThreshold else { continue }
            anyOpaque = true
            if x < bounds.minX { bounds.minX = x }
            if x > bounds.maxX { bounds.maxX = x }
            // The context is bottom-left origin; bounds are tracked top-left.
            let flipped = height - 1 - y
            if flipped < bounds.minY { bounds.minY = flipped }
            if flipped > bounds.maxY { bounds.maxY = flipped }
        }
    }
    if !anyOpaque {
        FileHandle.standardError.write(
            "source has no transparency: expected an alpha channel\n".data(using: .utf8)!
        )
        return nil
    }
    return bounds.isEmpty ? nil : bounds
}

/// Render one icon size: the plate centred on a transparent canvas.
private func renderIcon(artwork: CGImage, bounds: Bounds, size: Int) -> CGImage? {
    // A real bitmap context, not NSImage.lockFocus: lockFocus draws through a
    // window backing store, which at icon sizes leaves uninitialised pixels
    // (observed as solid magenta) and cannot be trusted to clear transparent.
    guard let context = CGContext(
        data: nil,
        width: size,
        height: size,
        bitsPerComponent: 8,
        bytesPerRow: 0,
        space: CGColorSpaceCreateDeviceRGB(),
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
    ) else { return nil }
    context.clear(CGRect(x: 0, y: 0, width: size, height: size))
    context.interpolationQuality = .high

    let inset = artworkInsetFraction * CGFloat(size)
    let drawSize = CGFloat(size) - inset * 2
    // Preserve the plate's aspect ratio, then centre it.
    let scale = min(drawSize / CGFloat(bounds.width), drawSize / CGFloat(bounds.height))
    let scaled = CGSize(width: CGFloat(bounds.width) * scale, height: CGFloat(bounds.height) * scale)
    let target = CGRect(
        x: (CGFloat(size) - scaled.width) / 2,
        y: (CGFloat(size) - scaled.height) / 2,
        width: scaled.width,
        height: scaled.height
    )

    // Bounds are top-left; CoreGraphics is bottom-left, so flip the y origin.
    let crop = CGRect(
        x: CGFloat(bounds.minX),
        y: CGFloat(artwork.height - bounds.maxY - 1),
        width: CGFloat(bounds.width),
        height: CGFloat(bounds.height)
    )

    // Map the crop onto the target with a transform rather than a plain draw, so
    // the source rect is honoured exactly and is never resampled twice. The clip
    // keeps the rest of the source from spilling outside the target.
    context.saveGState()
    context.clip(to: target)
    context.translateBy(x: target.minX, y: target.minY)
    context.scaleBy(x: target.width / crop.width, y: target.height / crop.height)
    context.translateBy(x: -crop.minX, y: -crop.minY)
    context.draw(
        artwork,
        in: CGRect(x: 0, y: 0, width: CGFloat(artwork.width), height: CGFloat(artwork.height))
    )
    context.restoreGState()
    return context.makeImage()
}

private func writePNG(_ image: CGImage, to url: URL) throws {
    let rep = NSBitmapImageRep(cgImage: image)
    rep.size = NSSize(width: image.width, height: image.height)
    guard let data = rep.representation(using: .png, properties: [:]) else {
        throw NSError(domain: "make-iconset", code: 1)
    }
    try data.write(to: url)
}

let arguments = CommandLine.arguments
guard arguments.count >= 3 else {
    FileHandle.standardError.write(
        "usage: make-iconset.swift <source.png> <output.icns> [work-directory]\n".data(using: .utf8)!
    )
    exit(2)
}
let sourceURL = URL(fileURLWithPath: arguments[1])
let outputURL = URL(fileURLWithPath: arguments[2])
let workDirectory = URL(fileURLWithPath: arguments.count >= 4 ? arguments[3] : NSTemporaryDirectory())
    .appendingPathComponent("octet-iconset", isDirectory: true)

guard let source = NSImage(contentsOf: sourceURL),
    let artwork = source.cgImage(forProposedRect: nil, context: nil, hints: nil)
else {
    FileHandle.standardError.write("could not read \(sourceURL.path)\n".data(using: .utf8)!)
    exit(1)
}
guard let bounds = artworkBounds(of: artwork) else {
    FileHandle.standardError.write("no artwork found in \(sourceURL.path)\n".data(using: .utf8)!)
    exit(1)
}

let iconset = workDirectory.appendingPathComponent("AppIcon.iconset", isDirectory: true)
try? FileManager.default.removeItem(at: workDirectory)
try FileManager.default.createDirectory(at: iconset, withIntermediateDirectories: true)

// Every size macOS may ask for, each rendered from the source so small sizes
// stay sharp instead of being downsampled from one master.
let variants: [(points: Int, scale: Int, name: String)] = [
    (16, 1, "icon_16x16.png"),
    (16, 2, "icon_16x16@2x.png"),
    (32, 1, "icon_32x32.png"),
    (32, 2, "icon_32x32@2x.png"),
    (128, 1, "icon_128x128.png"),
    (128, 2, "icon_128x128@2x.png"),
    (256, 1, "icon_256x256.png"),
    (256, 2, "icon_256x256@2x.png"),
    (512, 1, "icon_512x512.png"),
    (512, 2, "icon_512x512@2x.png"),
]
for variant in variants {
    guard let image = renderIcon(artwork: artwork, bounds: bounds, size: variant.points * variant.scale) else {
        FileHandle.standardError.write("failed to render \(variant.name)\n".data(using: .utf8)!)
        exit(1)
    }
    try writePNG(image, to: iconset.appendingPathComponent(variant.name))
}

let task = Process()
task.executableURL = URL(fileURLWithPath: "/usr/bin/iconutil")
task.arguments = ["-c", "icns", iconset.path, "-o", outputURL.path]
try task.run()
task.waitUntilExit()
guard task.terminationStatus == 0 else {
    FileHandle.standardError.write("iconutil failed\n".data(using: .utf8)!)
    exit(1)
}
print("wrote \(outputURL.path)")
