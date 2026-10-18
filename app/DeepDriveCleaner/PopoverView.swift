import SwiftUI

/// The popover's placeholder; the story "Menu bar popover" fills it.
struct PopoverView: View {
    let scanNow: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                Text("DeepDriveCleaner").font(.headline)
                Text("Scan results will show here.").font(.callout).foregroundStyle(.secondary)
            }
            Divider()
            Button("Scan now", action: scanNow)
        }
        .padding(16)
        .frame(width: 400, alignment: .leading)
    }
}
