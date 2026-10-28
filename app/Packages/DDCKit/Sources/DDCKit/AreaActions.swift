import Foundation

/// Which rows of the Whole disk view offer an action, and what the sheet says
/// about it (story "Whole disk: free space with the owning tool"; menu bar
/// ADR §6 item 11). The CLI checks everything again right before it runs:
/// this only decides which buttons show.
public enum AreaActions {
    /// One button: an action on one area, or on one runtime of it.
    public struct Offer: Sendable, Equatable, Hashable, Identifiable {
        public let action: DiskReport.ActionId
        /// The runtime's asset id; nil for Homebrew.
        public let identifier: String?
        /// "iOS 27.0 (24A100)", "Homebrew".
        public let name: String
        /// The size the last disk report gave it.
        public let bytes: UInt64?

        public var id: String { identifier.map { "\(action.rawValue) \($0)" } ?? action.rawValue }
        public var listed: ListedAreaAction { ListedAreaAction(action: action, identifier: identifier) }

        /// The row's button.
        public var buttonTitle: String {
            switch action {
            case .simctlRuntimeDelete: "Delete Runtime…"
            case .brewCleanup: "Clean Up Homebrew…"
            }
        }

        /// The sheet's title.
        public var question: String {
            switch action {
            case .simctlRuntimeDelete: "Delete the \(name) simulator runtime?"
            case .brewCleanup: "Clean up Homebrew?"
            }
        }

        /// What it removes, and what cannot be undone.
        public var consequence: String {
            switch action {
            case .simctlRuntimeDelete:
                "simctl deletes the runtime and its disk image. It does not go to the Trash and cannot be undone. If it is the runtime of an installed Xcode's SDK, Xcode downloads it again (several GB) the next time you build or run for it; any other runtime comes back only when you download it."
            case .brewCleanup:
                "brew cleanup removes old versions of installed formulae and casks from /opt/homebrew/Cellar and old downloads from ~/Library/Caches/Homebrew. They do not go to the Trash. The versions you use stay, and nothing is uninstalled. HOMEBREW_ settings in your shell profile do not reach a run started by the app; Homebrew's brew.env files do."
            }
        }

        /// The confirm button.
        public var confirmTitle: String {
            switch action {
            case .simctlRuntimeDelete: "Delete Runtime"
            case .brewCleanup: "Clean Up"
            }
        }
    }

    /// The offers of `report`, by the id of the row they belong to: the
    /// Homebrew area's row, and one row per runtime (`runtimeRowID`).
    public static func offers(_ report: DiskReport) -> [String: Offer] {
        var out: [String: Offer] = [:]
        if let brew = report.areas.first(where: { $0.id == "homebrew" && $0.action == .brewCleanup }) {
            out[brew.id] = Offer(action: .brewCleanup, identifier: nil, name: "Homebrew", bytes: brew.bytes)
        }
        guard report.areas.contains(where: { $0.action == .simctlRuntimeDelete }),
              let sims = report.simulators, sims.status == .listed
        else { return out }
        for runtime in sims.runtimes where offersDelete(runtime) {
            out[runtimeRowID(runtime)] = Offer(
                action: .simctlRuntimeDelete, identifier: runtime.identifier, name: name(runtime), bytes: runtime.sizeBytes
            )
        }
        return out
    }

    /// The safety ADR's rule (amendment of 2026-10-28): under AssetsV2, not
    /// `deletable: false`, with a runtime identifier, no simulator booted on
    /// it, and not already being deleted.
    public static func offersDelete(_ runtime: DiskReport.Runtime) -> Bool {
        guard let mount = runtime.parentMountPath, mount.hasPrefix(assets), !mount.contains("/../") else { return false }
        return runtime.deletable != false && runtime.runtimeIdentifier != nil && !runtime.booted && runtime.state != "Deleting"
    }

    public static func runtimeRowID(_ runtime: DiskReport.Runtime) -> String { "runtime \(runtime.identifier)" }

    /// "iOS 27.0 (24A100)", or the asset id when simctl named nothing else.
    public static func name(_ runtime: DiskReport.Runtime) -> String {
        let head = [runtime.platform, runtime.version].compactMap { $0 }.joined(separator: " ")
        guard !head.isEmpty else { return runtime.identifier }
        return runtime.build.map { "\(head) (\($0))" } ?? head
    }

    static let assets = "/System/Library/AssetsV2/"
}
