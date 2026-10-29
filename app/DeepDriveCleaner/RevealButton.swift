import AppKit
import DDCKit
import SwiftUI

/// Show in Finder or Open Software Update for a Whole disk row (story "Whole
/// disk: Show in Finder and the honest action for each row"). It removes
/// nothing: the user deletes in Finder, which asks for a password itself.
struct RevealButton: View {
    let reveal: AreaActions.Reveal

    var body: some View {
        switch reveal {
        case .finder(let paths):
            Button(reveal.title) {
                NSWorkspace.shared.activateFileViewerSelecting(AreaActions.existing(paths).map { URL(filePath: $0) })
            }
            .disabled(AreaActions.existing(paths).isEmpty)
        case .softwareUpdate:
            Button(reveal.title) { NSWorkspace.shared.open(AreaActions.softwareUpdateURL) }
        }
    }
}
