import Foundation

public enum FullDiskAccess: Sendable, Equatable {
    case granted
    /// The file could not be opened: `EPERM` without the grant, `ENOENT` when
    /// there is no file (which then proves nothing).
    case missing(errno: Int32)

    public var isGranted: Bool { self == .granted }
}

/// Whether this app has Full Disk Access, checked live the way the CLI's
/// `report::full_disk_access` checks it: TCC.db opens only with the grant.
///
/// The popover and Settings follow this probe, never a report's
/// `full_disk_access`: a report may come from Terminal's grant or from the
/// 0.1.0 agent. The real TCC.db path is built in the app target only; tests
/// give it temporary files.
public struct FullDiskAccessProbe: Sendable {
    public let path: URL

    public init(path: URL) {
        self.path = path
    }

    public func check() -> FullDiskAccess {
        let fd = open(path.path, O_RDONLY)
        guard fd >= 0 else { return .missing(errno: errno) }
        close(fd)
        return .granted
    }
}
