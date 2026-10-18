// Writes the app's AppIcon set: ten PNGs and Contents.json.
//
//   swiftc -o build/icons app/DeepDriveCleaner/DriveGlyph.swift scripts/icons/main.swift
//   build/icons app/DeepDriveCleaner/Assets.xcassets/AppIcon.appiconset
//
// `make icons` runs both. The drive is the status item's glyph (DriveGlyph.swift),
// white on a blue tile. The tile fills Apple's icon shape on the 824/1024 grid, so
// macOS 26 does not put the icon on a grey tile of its own.
import CoreGraphics
import Foundation
import ImageIO
import SwiftUI
import UniformTypeIdentifiers

guard CommandLine.arguments.count == 2 else {
    FileHandle.standardError.write(Data("usage: icons <AppIcon.appiconset>\n".utf8))
    exit(2)
}
let output = URL(filePath: CommandLine.arguments[1], directoryHint: .isDirectory)
try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)

let space = CGColorSpace(name: CGColorSpace.sRGB)!
func color(_ hex: UInt32) -> CGColor {
    CGColor(
        srgbRed: CGFloat(hex >> 16 & 0xFF) / 255, green: CGFloat(hex >> 8 & 0xFF) / 255,
        blue: CGFloat(hex & 0xFF) / 255, alpha: 1)
}

func icon(pixels: Int) -> CGImage {
    let context = CGContext(
        data: nil, width: pixels, height: pixels, bitsPerComponent: 8, bytesPerRow: 0, space: space,
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    let scale = CGFloat(pixels) / 1024
    context.scaleBy(x: scale, y: scale)

    // The tile: 824 x 824 with continuous corners, centred, with the grid's shadow.
    let tile = CGRect(x: 100, y: 100, width: 824, height: 824)
    let shape = RoundedRectangle(cornerRadius: 185.4, style: .continuous).path(in: tile).cgPath
    context.saveGState()
    context.setShadow(offset: CGSize(width: 0, height: -10), blur: 20, color: CGColor(gray: 0, alpha: 0.3))
    context.addPath(shape)
    context.setFillColor(color(0x2459B8))
    context.fillPath()
    context.restoreGState()
    context.saveGState()
    context.addPath(shape)
    context.clip()
    let gradient = CGGradient(colorsSpace: space, colors: [color(0x3D7BE0), color(0x1B4596)] as CFArray, locations: [0, 1])!
    context.drawLinearGradient(gradient, start: CGPoint(x: 512, y: 924), end: CGPoint(x: 512, y: 100), options: [])
    context.restoreGState()

    // The glyph: 600 pt wide; thicker strokes at small sizes keep it visible.
    let glyph = CGRect(x: 212, y: 212, width: 600, height: 600)
    let unitPixels = 600 / 16 * scale
    DriveGlyph.draw(
        in: context, rect: glyph, color: CGColor(gray: 1, alpha: 1),
        lineWidth: max(DriveGlyph.lineWidth, 1.6 / unitPixels))
    return context.makeImage()!
}

var images: [[String: String]] = []
for points in [16, 32, 128, 256, 512] {
    for scale in [1, 2] {
        let name = scale == 1 ? "icon_\(points)x\(points).png" : "icon_\(points)x\(points)@2x.png"
        let url = output.appending(path: name)
        let destination = CGImageDestinationCreateWithURL(url as CFURL, UTType.png.identifier as CFString, 1, nil)!
        CGImageDestinationAddImage(destination, icon(pixels: points * scale), nil)
        guard CGImageDestinationFinalize(destination) else { fatalError("cannot write \(url.path)") }
        images.append(["filename": name, "idiom": "mac", "scale": "\(scale)x", "size": "\(points)x\(points)"])
    }
}
let contents: [String: Any] = ["images": images, "info": ["author": "xcode", "version": 1]]
let json = try JSONSerialization.data(withJSONObject: contents, options: [.prettyPrinted, .sortedKeys])
try json.write(to: output.appending(path: "Contents.json"))
print("wrote \(images.count) icons to \(output.path)")
