import CoreGraphics

/// The drive of the design canvas: a rounded outline, a line and a dot on a
/// 16-unit grid. The status item and the app icon are both drawn from it;
/// `scripts/icons/main.swift` compiles this file too, so it uses CoreGraphics only.
enum DriveGlyph {
    /// The canvas's stroke weight, in grid units.
    static let lineWidth: CGFloat = 1.4

    /// What sits inside the drive (artboard 3): its dot, a turning arc while
    /// the CLI runs, or nothing when a mark sits on the corner.
    enum Inner {
        case dot
        /// `angle` in radians turns the arc about its centre.
        case arc(angle: CGFloat)
        case none
    }

    /// Strokes the glyph centred in the largest square that fits `rect`.
    /// The default `inner` draws the app icon's glyph.
    static func draw(in context: CGContext, rect: CGRect, color: CGColor, lineWidth: CGFloat = lineWidth, inner: Inner = .dot) {
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
        switch inner {
        case .dot:
            context.addLine(to: CGPoint(x: 8.5, y: 8))
            context.addEllipse(in: dot)
            context.strokePath()
            context.fillEllipse(in: dot)
        case .arc(let angle):
            // The canvas's `M9.6 6.2a2.4 2.4 0 1 1-0.1 3.4`: most of a circle, open to the left.
            context.addLine(to: CGPoint(x: 7, y: 8))
            context.strokePath()
            let open = CGFloat.pi * 2 / 9
            context.addArc(
                center: CGPoint(x: 11.5, y: 8), radius: 2.4,
                startAngle: .pi + open + angle, endAngle: .pi - open + angle + 2 * .pi, clockwise: false
            )
            context.strokePath()
        case .none:
            context.addLine(to: CGPoint(x: 8.5, y: 8))
            context.strokePath()
        }
    }
}
