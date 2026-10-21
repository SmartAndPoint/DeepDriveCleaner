import AppKit
import DDCKit

/// Draws an `IconState` as the status item's image (artboard 3 of the canvas,
/// with the owner's changes of 2026-10-19).
///
/// Without a mark the image is a template, tinted by the menu bar as before;
/// paused, the template is drawn at low opacity, so the menu bar shows it
/// grey, as Google Drive does. A mark is coloured, so those states are plain
/// images whose glyph is drawn in `labelColor` (paused: `tertiaryLabelColor`),
/// resolved when the image draws: it follows a light or dark menu bar. Every
/// state has the same size, so the icon never shifts.
@MainActor
enum StatusIcon {
    /// The glyph's square, plus room on the right for a mark that overhangs it.
    private static let size = NSSize(width: 22, height: 18)
    private static let glyph = NSRect(x: 0, y: 0, width: 18, height: 18)
    /// A paused template's opacity: the menu bar's grey.
    private static let pausedAlpha: CGFloat = 0.4

    static func image(_ state: IconState, arcAngle: CGFloat) -> NSImage {
        let inner: DriveGlyph.Inner =
            switch state.inner {
            case .dot: .dot
            case .arc: .arc(angle: arcAngle)
            case .none: .none
            }
        let template = state.mark == .none
        let mark = state.mark
        let dimmed = state.dimmed
        let image = NSImage(size: size, flipped: false) { _ in
            guard let context = NSGraphicsContext.current?.cgContext else { return false }
            let color: CGColor =
                if template {
                    NSColor.black.withAlphaComponent(dimmed ? pausedAlpha : 1).cgColor
                } else {
                    (dimmed ? NSColor.tertiaryLabelColor : NSColor.labelColor).cgColor
                }
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
        switch mark {
        case .none:
            return
        case .badge:
            // A ring of menu bar around the mark, as the canvas's 1.5 px outline.
            context.saveGState()
            context.setBlendMode(.clear)
            context.fillEllipse(in: circle(6))
            context.restoreGState()
            // The user's accent, resolved when the image draws; under
            // Multicolor, the brand blue of the AccentColor asset.
            context.setFillColor(NSColor.controlAccentColor.cgColor)
            context.fillEllipse(in: circle(4.5))
            context.setFillColor(NSColor.white.cgColor)
            context.fillEllipse(in: circle(1.5))
        case .warning:
            // A small warning sign: a rounded triangle with "!" in it, set low
            // enough that its rounded tip stays inside the image.
            let mid = CGPoint(x: center.x, y: center.y - 0.6 * unit)
            let triangle = CGMutablePath()
            triangle.move(to: CGPoint(x: mid.x, y: mid.y + 4.6 * unit))
            triangle.addLine(to: CGPoint(x: mid.x + 4.9 * unit, y: mid.y - 3.8 * unit))
            triangle.addLine(to: CGPoint(x: mid.x - 4.9 * unit, y: mid.y - 3.8 * unit))
            triangle.closeSubpath()
            context.saveGState()
            context.setBlendMode(.clear)
            context.setLineJoin(.round)
            context.setLineWidth(3 * unit)
            context.addPath(triangle)
            context.drawPath(using: .fillStroke)
            context.restoreGState()
            context.saveGState()
            context.setFillColor(NSColor.systemOrange.cgColor)
            context.setStrokeColor(NSColor.systemOrange.cgColor)
            context.setLineJoin(.round)
            context.setLineWidth(1 * unit)
            context.addPath(triangle)
            context.drawPath(using: .fillStroke)
            context.restoreGState()
            context.setStrokeColor(NSColor.white.cgColor)
            context.setFillColor(NSColor.white.cgColor)
            context.setLineWidth(1.2 * unit)
            context.setLineCap(.round)
            context.move(to: CGPoint(x: mid.x, y: mid.y + 1.6 * unit))
            context.addLine(to: CGPoint(x: mid.x, y: mid.y - 0.9 * unit))
            context.strokePath()
            let dot = 0.65 * unit
            context.fillEllipse(in: CGRect(x: mid.x - dot, y: mid.y - 2.6 * unit - dot, width: 2 * dot, height: 2 * dot))
        }
    }
}
