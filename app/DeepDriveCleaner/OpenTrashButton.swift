import AppKit
import SwiftUI

/// Opens the Trash in Finder (story "The Trash: its own row, not Your data").
/// DeepDriveCleaner never empties it: there is no Empty button.
struct OpenTrashButton: View {
    var body: some View {
        Button("Open the Trash") {
            let trash = FileManager.default.homeDirectoryForCurrentUser.appending(path: ".Trash", directoryHint: .isDirectory)
            NSWorkspace.shared.open(trash)
        }
    }
}
