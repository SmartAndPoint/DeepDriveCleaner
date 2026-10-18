import CoreGraphics

/// The drive of the design canvas: a rounded outline, a line and a dot on a
/// 16-unit grid. The status item and the app icon are both drawn from it;
/// `scripts/icons/main.swift` compiles this file too, so it uses CoreGraphics only.
enum DriveGlyph {
    /// The canvas's stroke weight, in grid units.
    static let lineWidth: CGFloat = 1.4

    /// Strokes the glyph centred in the largest square that fits `rect`.
    static func draw(in context: CGContext, rect: CGRect, color: CGColor, lineWidth: CGFloat = lineWidth) {
        let unit = min(rect.width, rect.height) / 16
        context.saveGState()
        defer { context.restoreGState() }
        context.translateBy(x: rect.midX - 8 * unit, y: rect.midY - 8 * unit)
        context.scaleBy(x: unit, y: unit)
        context.setStrokeColor(color)
        context.setFillColor(color)
        context.setLineWidth(lineWidth)
        context.setLineCap(.round)

        // Symmetric about y = 8, so the same in flipped and unflipped contexts.
        let dot = CGRect(x: 11.5 - 0.9, y: 8 - 0.9, width: 1.8, height: 1.8)
        context.addPath(CGPath(roundedRect: CGRect(x: 1.5, y: 4, width: 13, height: 8), cornerWidth: 2, cornerHeight: 2, transform: nil))
        context.move(to: CGPoint(x: 4.5, y: 8))
        context.addLine(to: CGPoint(x: 8.5, y: 8))
        context.addEllipse(in: dot)
        context.strokePath()
        context.fillEllipse(in: dot)
    }
}
