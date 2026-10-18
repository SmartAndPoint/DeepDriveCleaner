import Observation
import ServiceManagement

/// Open at login, through `SMAppService.mainApp`.
///
/// The state shown is always the system's: `status` is re-read whenever the
/// settings appear or the app becomes active, because the user can change it
/// in System Settings at any time. Nothing is kept in `UserDefaults`
/// (GrammarHelper's stored bool drifts from the system).
@MainActor
@Observable
final class LoginItem {
    private(set) var status = SMAppService.mainApp.status
    private(set) var failure: String?

    /// On also while the system waits for the user's approval.
    var isOn: Bool { status == .enabled || status == .requiresApproval }

    func refresh() {
        status = SMAppService.mainApp.status
    }

    func set(_ on: Bool) {
        do {
            if on {
                try SMAppService.mainApp.register()
            } else {
                try SMAppService.mainApp.unregister()
            }
            failure = nil
        } catch {
            failure = error.localizedDescription
            AppLog.write("login item \(on ? "register" : "unregister") failed: \(error)")
        }
        refresh()
        AppLog.write("login item: \(status.name)")
    }

    func openLoginItemsSettings() {
        SMAppService.openSystemSettingsLoginItems()
    }
}

extension SMAppService.Status {
    var name: String {
        switch self {
        case .notRegistered: "not registered"
        case .enabled: "enabled"
        case .requiresApproval: "requires approval"
        case .notFound: "not found"
        @unknown default: "unknown (\(rawValue))"
        }
    }
}
