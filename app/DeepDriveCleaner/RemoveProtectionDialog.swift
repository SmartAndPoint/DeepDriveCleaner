import DDCKit
import SwiftUI

/// Remove Protection… on a kept row (story "Protected by you: say what it
/// is, and Remove Protection"; menu bar ADR §7): the confirmation, and the
/// one place in the app that takes a protection away. It removes the
/// protection, never the folder; the scan after it lists the folder in its
/// own group. A structural test holds that its confirm button is the only
/// caller.
struct RemoveProtectionDialog: ViewModifier {
    let model: ReviewModel
    /// What the row asked for; nil while no confirmation is shown.
    @Binding var confirmation: RemoveProtection.Confirmation?

    func body(content: Content) -> some View {
        content.confirmationDialog(
            confirmation?.question ?? "",
            isPresented: Binding(get: { confirmation != nil }, set: { if !$0 { confirmation = nil } }),
            presenting: confirmation
        ) { confirmed in
            Button(confirmed.confirmTitle, role: .destructive) {
                Task { await model.removeProtection(confirmed) }
            }
            Button("Cancel", role: .cancel) {}
        } message: { confirmed in
            Text(Self.message(confirmed))
        }
    }

    /// The kept findings it unprotects, what happens next, and git's warning
    /// when git tracks the marker.
    static func message(_ confirmed: RemoveProtection.Confirmation) -> String {
        var parts = [confirmed.findings.joined(separator: "\n"), confirmed.message]
        if let warning = confirmed.gitWarning { parts.append(warning) }
        return parts.filter { !$0.isEmpty }.joined(separator: "\n\n")
    }
}

/// The row's Remove Protection… button: shown on a kept row with one
/// removable protection, enabled when the window can run it now.
struct RemoveProtectionButton: View {
    let model: ReviewModel
    let row: ReviewItems.Row
    /// The row's path as shown, for VoiceOver.
    let path: String
    let ask: @MainActor () -> Void

    var body: some View {
        Button("Remove Protection…") { ask() }
            .controlSize(.small)
            .disabled(!model.canRemoveProtection(row.id))
            .help("Takes the protection away, never the folder. The next scan lists the folder in its own group.")
            .accessibilityLabel("Remove protection from \(path)")
    }

    /// A kept finding with exactly one protection the app can remove: a
    /// marker or a Settings entry. A flag's, or several, get none.
    static func offered(_ row: ReviewItems.Row) -> Bool {
        row.userEntry == nil && row.finding.rule == ReviewItems.keptRule && RemoveProtection.target(row.finding.protection) != nil
    }
}
