import AppKit
import DDCKit

/// Draws an `IconState` as the status item's image (artboard 3 of the canvas).
///
/// Idle and busy are template images, tinted by the menu bar as before. A mark
/// is coloured, so those states are plain images whose glyph is drawn in
/// `labelColor`, resolved when the image draws: it follows a light or dark
/// menu bar. Every state has the same size, so the icon never shifts.
@MainActor
enum StatusIcon {
    static let accent = NSColor(srgbRed: 0x24 / 255, green: 0x59 / 255, blue: 0xB8 / 255, alpha: 1)
    static let alert = NSColor(srgbRed: 0xB4 / 255, green: 0x5F / 255, blue: 0x06 / 255, alpha: 1)

    /// The glyph's square, plus room on the right for a mark that overhangs it.
    private static let size = NSSize(width: 22, height: 18)
    private static let glyph = NSRect(x: 0, y: 0, width: 18, height: 18)

    static func image(_ state: IconState, arcAngle: CGFloat) -> NSImage {
        let inner: DriveGlyph.Inner =
            switch state.inner {
            case .dot: .dot
            case .arc: .arc(angle: arcAngle)
            case .none: .none
            }
        let template = state.mark == .none
        let mark = state.mark
        let image = NSImage(size: size, flipped: false) { _ in
            guard let context = NSGraphicsContext.current?.cgContext else { return false }
            let color = template ? NSColor.black.cgColor : NSColor.labelColor.cgColor
            DriveGlyph.draw(in: context, rect: glyph, color: color, inner: inner)
            drawMark(mark, in: context)
            return true
        }
        image.isTemplate = template
        image.accessibilityDescription = state.accessibility
        return image
    }

    /// On the drive's top-right corner, in the glyph's 16-unit grid (y up).
    private static func drawMark(_ mark: IconState.Mark, in context: CGContext) {
        let unit = glyph.width / 16
        let center = CGPoint(x: 13.5 * unit, y: 11.5 * unit)
        func circle(_ radius: CGFloat) -> CGRect {
            CGRect(x: center.x - radius * unit, y: center.y - radius * unit, width: 2 * radius * unit, height: 2 * radius * unit)
        }
        func punch(_ radius: CGFloat) {
            // A ring of menu bar around the mark, as the canvas's 1.5 px outline.
            context.saveGState()
            context.setBlendMode(.clear)
            context.fillEllipse(in: circle(radius + 1.5))
            context.restoreGState()
        }
        switch mark {
        case .none:
            return
        case .dot:
            punch(3.5)
            context.setFillColor(accent.cgColor)
            context.fillEllipse(in: circle(3.5))
        case .badge:
            punch(4.5)
            context.setFillColor(accent.cgColor)
            context.fillEllipse(in: circle(4.5))
            context.setFillColor(NSColor.white.cgColor)
            context.fillEllipse(in: circle(1.5))
        case .alert:
            punch(4.5)
            context.setFillColor(alert.cgColor)
            context.fillEllipse(in: circle(4.5))
            context.setStrokeColor(NSColor.white.cgColor)
            context.setFillColor(NSColor.white.cgColor)
            context.setLineWidth(1.3 * unit)
            context.setLineCap(.round)
            context.move(to: CGPoint(x: center.x, y: center.y + 2.4 * unit))
            context.addLine(to: CGPoint(x: center.x, y: center.y - 0.4 * unit))
            context.strokePath()
            let dot = 0.75 * unit
            context.fillEllipse(in: CGRect(x: center.x - dot, y: center.y - 2.4 * unit - dot, width: 2 * dot, height: 2 * dot))
        }
    }
}
