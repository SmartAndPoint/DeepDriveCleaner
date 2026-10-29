import Foundation

/// Which rows of the Whole disk view offer an action, and what the sheet says
/// about it (story "Whole disk: free space with the owning tool"; menu bar
/// ADR §6 item 11). The CLI checks everything again right before it runs:
/// this only decides which buttons show.
public enum AreaActions {
    /// One button: an action on one area, or on one runtime, conda
    /// environment or conda install of it.
    public struct Offer: Sendable, Equatable, Hashable, Identifiable {
        public let action: DiskReport.ActionId
        /// The runtime's asset id, the environment's path or the install's
        /// prefix; nil for Homebrew.
        public let identifier: String?
        /// "iOS 27.0 (24A100)", "Homebrew", "ml", "/opt/miniconda3".
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
            case .condaEnvRemove: "Remove Environment…"
            case .condaClean: "Clean Package Cache…"
            }
        }

        /// The sheet's title.
        public var question: String {
            switch action {
            case .simctlRuntimeDelete: "Delete the \(name) simulator runtime?"
            case .brewCleanup: "Clean up Homebrew?"
            case .condaEnvRemove: "Remove the conda environment \(name)?"
            case .condaClean: "Clean the package cache of \(name)?"
            }
        }

        /// What it removes, and what cannot be undone.
        public var consequence: String {
            switch action {
            case .simctlRuntimeDelete:
                "simctl deletes the runtime and its disk image. It does not go to the Trash and cannot be undone. If it is the runtime of an installed Xcode's SDK, Xcode downloads it again (several GB) the next time you build or run for it; any other runtime comes back only when you download it."
            case .brewCleanup:
                "brew cleanup removes old versions of installed formulae and casks from /opt/homebrew/Cellar and old downloads from ~/Library/Caches/Homebrew. They do not go to the Trash. The versions you use stay, and nothing is uninstalled. HOMEBREW_ settings in your shell profile do not reach a run started by the app; Homebrew's brew.env files do."
            case .condaEnvRemove:
                "conda env remove deletes the environment's folder \(identifier ?? name) with the install's own conda. It does not go to the Trash and cannot be undone. Its packages stay in the install's package cache: the files it shares with the cache are freed only when Clean Package Cache… removes packages no environment uses."
            case .condaClean:
                "conda clean removes the packages no environment uses, downloaded package archives, the index cache and logs from \(name)/pkgs, with the install's own conda. They do not go to the Trash. Packages your environments use stay, so less may be freed than the folder's size."
            }
        }

        /// The tool the sheet asks for its dry run.
        public var tool: String {
            switch action {
            case .simctlRuntimeDelete: "simctl"
            case .brewCleanup: "brew"
            case .condaEnvRemove, .condaClean: "conda"
            }
        }

        /// The sheet's line while it runs.
        public var runningText: String {
            switch action {
            case .simctlRuntimeDelete: "Deleting the runtime…"
            case .brewCleanup: "Running brew cleanup…"
            case .condaEnvRemove: "Removing the environment…"
            case .condaClean: "Running conda clean…"
            }
        }

        /// The dry run's estimate, as the sheet states it.
        public func estimateText(_ bytes: UInt64) -> String {
            let size = ByteSize.human(bytes)
            return switch action {
            case .simctlRuntimeDelete: "Its folder holds \(size)."
            case .brewCleanup: "brew says this frees about \(size)."
            case .condaEnvRemove: "Only this environment holds \(size): that is what removing it frees."
            case .condaClean: "conda says this frees about \(size)."
            }
        }

        /// The confirm button.
        public var confirmTitle: String {
            switch action {
            case .simctlRuntimeDelete: "Delete Runtime"
            case .brewCleanup: "Clean Up"
            case .condaEnvRemove: "Remove Environment"
            case .condaClean: "Clean Package Cache"
            }
        }
    }

    /// A row's way out where DeepDriveCleaner does not act itself (story
    /// "Whole disk: Show in Finder and the honest action for each row").
    public enum Reveal: Sendable, Equatable {
        /// The folders to select, by the paths the user knows.
        case finder([String])
        case softwareUpdate

        public var title: String {
            switch self {
            case .finder: "Show in Finder"
            case .softwareUpdate: "Open Software Update"
            }
        }
    }

    /// The Software Update pane of System Settings.
    public static let softwareUpdateURL = URL(string: "x-apple.systempreferences:com.apple.Software-Update-Settings.extension")!

    /// The area's reveal, its paths through the firmlinks.
    public static func reveal(_ area: DiskReport.Area) -> Reveal? {
        switch area.reveal {
        case .finder: .finder(area.paths.map(userPath))
        case .softwareUpdate: .softwareUpdate
        case nil: nil
        }
    }

    /// `/System/Volumes/Data/opt/miniconda3` → `/opt/miniconda3`: the
    /// disk report spells areas through the Data volume, and Finder would
    /// open inside it. Only at a path boundary.
    public static func userPath(_ path: String) -> String {
        let data = "/System/Volumes/Data"
        if path == data { return "/" }
        return path.hasPrefix(data + "/") ? String(path.dropFirst(data.count)) : path
    }

    /// The paths that exist now: a folder may be gone since the report.
    public static func existing(_ paths: [String], exists: (String) -> Bool = { FileManager.default.fileExists(atPath: $0) }) -> [String] {
        paths.filter(exists)
    }

    /// The offers of `report`, by the id of the row they belong to: the
    /// Homebrew area's row, one row per runtime (`runtimeRowID`), and the
    /// conda rows (`condaOffers`).
    public static func offers(_ report: DiskReport) -> [String: Offer] {
        var out: [String: Offer] = [:]
        if let brew = report.areas.first(where: { $0.id == "homebrew" && $0.action == .brewCleanup }) {
            out[brew.id] = Offer(action: .brewCleanup, identifier: nil, name: "Homebrew", bytes: brew.bytes)
        }
        out.merge(condaOffers(report)) { first, _ in first }
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

    /// The conda offers (story "Whole disk: Conda environments and its
    /// package cache, with conda"): Remove Environment… on each `/opt`
    /// environment's row, and Clean Package Cache… on the Conda area's row,
    /// or with more than one install on each install's row. A folder the
    /// user cannot write gets none: conda would need an administrator.
    static func condaOffers(_ report: DiskReport) -> [String: Offer] {
        guard let area = report.areas.first(where: { $0.action == .condaClean }), let conda = report.conda else { return [:] }
        var out: [String: Offer] = [:]
        for install in conda.installs {
            if let pkgs = install.packages, pkgs.writable {
                let row = conda.installs.count == 1 ? area.id : condaInstallRowID(install)
                out[row] = Offer(action: .condaClean, identifier: install.prefix, name: install.prefix, bytes: pkgs.bytes)
            }
            for env in install.environments where env.writable {
                out[condaEnvironmentRowID(env)] = Offer(
                    action: .condaEnvRemove, identifier: env.path, name: condaName(env.path), bytes: env.bytes
                )
            }
        }
        return out
    }

    public static func condaInstallRowID(_ install: DiskReport.CondaInstall) -> String { "conda-install \(install.prefix)" }

    public static func condaEnvironmentRowID(_ env: DiskReport.CondaFolder) -> String { "conda-env \(env.path)" }

    /// `/opt/miniconda3/envs/ml` → "ml": its folder's name, as `conda env
    /// list` shows it.
    public static func condaName(_ path: String) -> String {
        (path as NSString).lastPathComponent
    }

    /// "iOS 27.0 (24A100)", or the asset id when simctl named nothing else.
    public static func name(_ runtime: DiskReport.Runtime) -> String {
        let head = [runtime.platform, runtime.version].compactMap { $0 }.joined(separator: " ")
        guard !head.isEmpty else { return runtime.identifier }
        return runtime.build.map { "\(head) (\($0))" } ?? head
    }

    static let assets = "/System/Library/AssetsV2/"
}
