import Foundation

public enum FullDiskAccess: Sendable, Equatable {
    case granted
    /// Nothing the grant opens could be read: the errno of the last step
    /// (`EPERM` without the grant, `ENOENT` when it does not exist).
    case missing(errno: Int32)

    public var isGranted: Bool { self == .granted }
}

/// Whether this app has Full Disk Access, checked live the way the CLI's
/// `report::full_disk_access` checks it: it tries, in order, what only the
/// grant opens, and any one read means granted (story "Full Disk Access check
/// on macOS 27", DDC-002). The per-user TCC.db opens with the grant on every
/// macOS that has it, so older versions keep their answer; macOS 27 has none,
/// or refuses it even with the grant, and the Safari folder answers there.
/// Without the grant neither opens, on any version.
///
/// The popover and Settings follow this probe, never a report's
/// `full_disk_access`: a report may come from Terminal's grant or from the
/// 0.1.0 agent. The real home is given in the app target only; tests give
/// temporary folders.
public struct FullDiskAccessProbe: Sendable {
    public enum Kind: Sendable, Equatable {
        /// Opened for reading.
        case file
        /// Opened and one entry read: an open alone may pass where the
        /// listing is refused.
        case folder
    }

    public struct Location: Sendable, Equatable {
        public let url: URL
        public let kind: Kind

        public init(url: URL, kind: Kind) {
            self.url = url
            self.kind = kind
        }
    }

    /// One step tried: `errno` nil when it was read.
    public struct Step: Sendable, Equatable {
        public let location: Location
        public let errno: Int32?
    }

    /// The core's `FULL_DISK_ACCESS_PROBES`, relative to the home folder, in
    /// order (a contract test compares them).
    public static let homeRelative: [(path: String, kind: Kind)] = [
        ("Library/Application Support/com.apple.TCC/TCC.db", .file),
        ("Library/Safari", .folder),
    ]

    public static func locations(home: URL) -> [Location] {
        homeRelative.map { Location(url: home.appending(path: $0.path), kind: $0.kind) }
    }

    public let locations: [Location]

    public init(locations: [Location]) {
        self.locations = locations
    }

    /// One file, as the tests of the popover give it.
    public init(path: URL) {
        self.init(locations: [Location(url: path, kind: .file)])
    }

    /// The steps tried, up to the first read; an error of one, whatever it
    /// is, only sends the check to the next.
    public func steps() -> [Step] {
        var steps: [Step] = []
        for location in locations {
            let code = Self.read(location)
            steps.append(Step(location: location, errno: code))
            if code == nil { break }
        }
        return steps
    }

    public func check() -> FullDiskAccess {
        guard let last = steps().last else { return .missing(errno: ENOENT) }
        if let code = last.errno { return .missing(errno: code) }
        return .granted
    }

    /// nil when read, else the errno.
    private static func read(_ location: Location) -> Int32? {
        switch location.kind {
        case .file:
            let fd = open(location.url.path, O_RDONLY)
            guard fd >= 0 else { return errno }
            close(fd)
            return nil
        case .folder:
            guard let dir = opendir(location.url.path) else { return errno }
            defer { closedir(dir) }
            errno = 0
            if readdir(dir) == nil, errno != 0 { return errno }
            return nil
        }
    }
}
