import Foundation

/// The Folders pane's lists (artboard 10): Always scan (`roots`) and Exclude,
/// whose rows are "Count, never clean" (`excludes`) or "Skip entirely"
/// (`skips`).
///
/// Folders are compared by resolved path, so the same folder given through
/// a symlink is the same folder. The file keeps the path the user picked; the
/// CLI resolves it again. An empty `roots` means the home folder.
public struct FolderList: Sendable, Equatable {
    public enum Mode: String, CaseIterable, Sendable {
        /// `excludes`: still read and sized, marked as the user's.
        case count
        /// `skips`: never read; Review says it was not scanned.
        case skip

        public var title: String {
            switch self {
            case .count: "Count, never clean"
            case .skip: "Skip entirely"
            }
        }
    }

    public struct Root: Sendable, Equatable, Identifiable {
        public let path: String
        /// "~", "/Volumes/Work".
        public let shown: String
        /// "Home folder · default", "External disk", or nothing.
        public let detail: String?
        public var id: String { path }
    }

    public struct Exclusion: Sendable, Equatable, Identifiable {
        public let path: String
        public let shown: String
        public let mode: Mode
        public var id: String { path }
    }

    /// Why a change was refused, as the pane says it.
    public enum Refusal: Error, Sendable, Equatable {
        case alreadyListed(String)
        case sameFolder(String, as: String)
        case insideRoot(String, root: String)
        case aroundRoot(String, root: String)
        case insideExclusion(String, exclusion: String)
        case coversRoot(String, root: String)
        case lastRoot

        public var message: String {
            switch self {
            case .alreadyListed(let path):
                "\(path) is already listed."
            case .sameFolder(let path, let other):
                "\(path) is the same folder as \(other), which is already listed."
            case .insideRoot(let path, let root):
                "\(path) is inside \(root), which is already scanned."
            case .aroundRoot(let path, let root):
                "\(path) holds \(root), which is already listed. Remove \(root) first."
            case .insideExclusion(let path, let exclusion):
                "\(path) is inside \(exclusion), which is excluded."
            case .coversRoot(let path, let root):
                "Excluding \(path) would leave out \(root), a folder you scan."
            case .lastRoot:
                "At least one folder is always scanned. Add another before removing this one."
            }
        }
    }

    public let home: String
    public let roots: [Root]
    /// Sorted by path, whatever their mode.
    public let exclusions: [Exclusion]

    public init(file: SettingsFile, home: String) {
        self.home = home
        let isDefault = file.roots.isEmpty
        roots = Self.effectiveRoots(file, home: home).map { path in
            let detail: String? = if path == home {
                isDefault ? "Home folder · default" : "Home folder"
            } else if path.split(separator: "/").first == "Volumes" {
                "External disk"
            } else {
                nil
            }
            return Root(path: path, shown: Wording.tilde(path, home: home), detail: detail)
        }
        let rows = file.excludes.map { ($0, Mode.count) } + file.skips.map { ($0, Mode.skip) }
        exclusions = rows.map { Exclusion(path: $0.0, shown: Wording.tilde($0.0, home: home), mode: $0.1) }
            .sorted { $0.path < $1.path }
    }

    /// The last root cannot be removed.
    public var canRemoveRoot: Bool { roots.count > 1 }

    // MARK: - Edits

    /// The first added root keeps the home folder: `[home, new]`.
    public static func addRoot(_ path: String, home: String, to file: inout SettingsFile) throws(Refusal) {
        let roots = effectiveRoots(file, home: home)
        let new = resolved(path)
        let shown = Wording.tilde(path, home: home)
        for root in roots {
            let existing = resolved(root)
            let rootShown = Wording.tilde(root, home: home)
            if existing == new { throw root == path ? .alreadyListed(shown) : .sameFolder(shown, as: rootShown) }
            if isInside(new, existing) { throw .insideRoot(shown, root: rootShown) }
            if isInside(existing, new) { throw .aroundRoot(shown, root: rootShown) }
        }
        if let exclusion = (file.excludes + file.skips).first(where: { isInside(new, resolved($0)) || resolved($0) == new }) {
            throw .insideExclusion(shown, exclusion: Wording.tilde(exclusion, home: home))
        }
        file.roots = roots + [path]
    }

    /// Back to the default (`[]`) when only the home folder is left.
    public static func removeRoot(_ path: String, home: String, from file: inout SettingsFile) throws(Refusal) {
        let roots = effectiveRoots(file, home: home)
        guard roots.count > 1 else { throw .lastRoot }
        let left = roots.filter { $0 != path }
        file.roots = left == [home] ? [] : left
    }

    /// Refused at or above a root, the home folder of the default included.
    public static func addExclusion(_ path: String, mode: Mode, home: String, to file: inout SettingsFile) throws(Refusal) {
        let new = resolved(path)
        let shown = Wording.tilde(path, home: home)
        if let root = effectiveRoots(file, home: home).first(where: { resolved($0) == new || isInside(resolved($0), new) }) {
            throw .coversRoot(shown, root: Wording.tilde(root, home: home))
        }
        if let existing = (file.excludes + file.skips).first(where: { resolved($0) == new }) {
            throw existing == path ? .alreadyListed(shown) : .sameFolder(shown, as: Wording.tilde(existing, home: home))
        }
        switch mode {
        case .count: file.excludes.append(path)
        case .skip: file.skips.append(path)
        }
    }

    public static func removeExclusion(_ path: String, from file: inout SettingsFile) {
        file.excludes.removeAll { $0 == path }
        file.skips.removeAll { $0 == path }
    }

    /// The entry, as the file writes it, that names the folder the scan
    /// report calls `path` (story "Protected by you: say what it is, and
    /// Remove Protection"). The report's path is canonical and
    /// `removeExclusion` matches by exact string, so the entry is the one of
    /// `mode`'s list whose `realpath(3)` is `path`, or that is `path` as written.
    public static func entry(for path: String, mode: Mode, in file: SettingsFile) -> String? {
        let list = mode == .count ? file.excludes : file.skips
        return list.first { $0 == path } ?? list.first { resolved($0) == path }
    }

    /// Moves the path between `excludes` and `skips`.
    public static func setMode(_ mode: Mode, of path: String, in file: inout SettingsFile) {
        guard file.excludes.contains(path) || file.skips.contains(path) else { return }
        removeExclusion(path, from: &file)
        switch mode {
        case .count: file.excludes.append(path)
        case .skip: file.skips.append(path)
        }
    }

    // MARK: - Paths

    static func effectiveRoots(_ file: SettingsFile, home: String) -> [String] {
        file.roots.isEmpty ? [home] : file.roots
    }

    /// The canonical path (`/tmp` is `/private/tmp`). Of a folder that does
    /// not exist (an unplugged disk), its deepest existing parent is
    /// resolved and the rest kept as written.
    static func resolved(_ path: String) -> String {
        var head = URL(filePath: path).standardized.path
        var rest: [String] = []
        while true {
            if let real = realpath(head, nil) {
                defer { free(real) }
                let base = String(cString: real)
                guard !rest.isEmpty else { return base }
                return (base == "/" ? "" : base) + "/" + rest.reversed().joined(separator: "/")
            }
            let parent = (head as NSString).deletingLastPathComponent
            guard parent != head, !parent.isEmpty else { return path }
            rest.append((head as NSString).lastPathComponent)
            head = parent
        }
    }

    /// Strictly under `folder`.
    static func isInside(_ path: String, _ folder: String) -> Bool {
        path != folder && path.hasPrefix(folder.hasSuffix("/") ? folder : folder + "/")
    }
}
