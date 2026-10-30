import Foundation

/// The part of `deepdrivecleaner scan --json` the app reads (the contract is
/// `crates/ddc-core/src/model.rs`, schema in `docs/scan.schema.json`).
///
/// The schema version is read first: a report of another version is
/// unreadable as a whole, never half-decoded. Fields the CLI leaves out when
/// empty (`volumes`, `ignored`, the ignored totals, `not_scanned`) decode as
/// their defaults, so a report of this version without them still reads.
public struct ScanReport: Decodable, Sendable, Equatable {
    /// `SCHEMA_VERSION` of the CLI this app embeds.
    public static let schemaVersion = 3

    public enum ReadError: Error, Equatable {
        case otherSchema(Int)
    }

    public let schemaVersion: Int
    public let ddcVersion: String
    /// The platform the CLI was built for.
    public let platform: Platform
    /// What the CLI can do on its platform, in declaration order.
    public let capabilities: [Capability]
    /// When the scan started (RFC 3339).
    public let generatedAt: String
    public let durationMs: UInt64
    public let home: String
    public let scanRoots: [String]
    /// True unless a permission system withheld protected data. On macOS,
    /// whether the CLI that wrote the report could read TCC.db: a fact about
    /// that scan, not about the app now (the app probes for itself).
    public let fullDiskAccess: Bool
    public let volumes: [Volume]
    public let minAgeDays: UInt64
    public let totals: Totals
    public let findings: [Finding]
    /// Folders that could not be read: without Full Disk Access, or after
    /// the user said no to macOS (at most 10 000; see `errorsTotal`).
    public let inaccessible: [Inaccessible]
    public let errorsTotal: UInt64
    /// Areas the scan did not enter: the cloud folders, what a background
    /// scan skips to avoid a permission prompt, and the user's skips.
    public let skipped: [Skipped]
    public let cancelled: Bool
    /// The user's skips ("Skip entirely") the walk stopped at, canonical.
    public let notScanned: [String]

    private enum CodingKeys: String, CodingKey {
        case schemaVersion, ddcVersion, platform, capabilities, generatedAt, durationMs, home, scanRoots, fullDiskAccess, volumes
        case minAgeDays, totals, findings, inaccessible, errorsTotal, skipped, cancelled, notScanned
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schemaVersion = try c.decode(Int.self, forKey: .schemaVersion)
        ddcVersion = try c.decode(String.self, forKey: .ddcVersion)
        platform = try c.decode(Platform.self, forKey: .platform)
        capabilities = try c.decode([Capability].self, forKey: .capabilities)
        generatedAt = try c.decode(String.self, forKey: .generatedAt)
        durationMs = try c.decode(UInt64.self, forKey: .durationMs)
        home = try c.decode(String.self, forKey: .home)
        scanRoots = try c.decode([String].self, forKey: .scanRoots)
        fullDiskAccess = try c.decode(Bool.self, forKey: .fullDiskAccess)
        volumes = try c.decodeIfPresent([Volume].self, forKey: .volumes) ?? []
        minAgeDays = try c.decode(UInt64.self, forKey: .minAgeDays)
        totals = try c.decode(Totals.self, forKey: .totals)
        findings = try c.decode([Finding].self, forKey: .findings)
        inaccessible = try c.decodeIfPresent([Inaccessible].self, forKey: .inaccessible) ?? []
        errorsTotal = try c.decode(UInt64.self, forKey: .errorsTotal)
        skipped = try c.decodeIfPresent([Skipped].self, forKey: .skipped) ?? []
        cancelled = try c.decode(Bool.self, forKey: .cancelled)
        notScanned = try c.decodeIfPresent([String].self, forKey: .notScanned) ?? []
    }

    public struct Inaccessible: Decodable, Sendable, Equatable {
        public let path: String
        public let error: String
    }

    public struct Skipped: Decodable, Sendable, Equatable {
        public let path: String
        public let reason: SkipArea
    }

    /// The scan's `SkipReason`. An unknown value is a decoding error.
    public enum SkipArea: String, Decodable, Sendable, CaseIterable {
        case excluded, dataless, changed
        case otherFilesystem = "other-filesystem"
    }

    public struct Totals: Decodable, Sendable, Equatable {
        public let scannedBytes: UInt64
        public let files: UInt64
        public let dirs: UInt64
        public let classes: [ClassTotal]
        public let selectedBytes: UInt64
        public let selectedReclaimableBytes: UInt64
        public let ignoredCount: UInt64
        public let ignoredBytes: UInt64

        private enum CodingKeys: String, CodingKey {
            case scannedBytes, files, dirs, classes, selectedBytes, selectedReclaimableBytes, ignoredCount, ignoredBytes
        }

        public init(from decoder: any Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            scannedBytes = try c.decode(UInt64.self, forKey: .scannedBytes)
            files = try c.decode(UInt64.self, forKey: .files)
            dirs = try c.decode(UInt64.self, forKey: .dirs)
            classes = try c.decode([ClassTotal].self, forKey: .classes)
            selectedBytes = try c.decode(UInt64.self, forKey: .selectedBytes)
            selectedReclaimableBytes = try c.decode(UInt64.self, forKey: .selectedReclaimableBytes)
            ignoredCount = try c.decodeIfPresent(UInt64.self, forKey: .ignoredCount) ?? 0
            ignoredBytes = try c.decodeIfPresent(UInt64.self, forKey: .ignoredBytes) ?? 0
        }
    }

    public struct ClassTotal: Decodable, Sendable, Equatable {
        public let `class`: RiskClass
        public let count: UInt64
        public let bytes: UInt64
        public let selectedCount: UInt64
        public let selectedBytes: UInt64
    }

    public struct Volume: Decodable, Sendable, Equatable {
        public let mountPoint: String
        public let roots: [String]
        public let freeBytes: UInt64
        public let totalBytes: UInt64
    }

    public struct Finding: Decodable, Sendable, Equatable {
        public let path: String
        public let rule: String
        /// The effective class: lower than `ruleClass` when a note downgrades it.
        public let `class`: RiskClass
        public let ruleClass: RiskClass
        /// The effective method: a `delete` rule downgraded to review reads `trash`.
        public let removal: Removal
        /// The directory's identity at scan time; `clean --items` matches it.
        public let dev: UInt64
        public let ino: UInt64
        public let bytes: UInt64
        /// APFS clones shared with nothing outside the finding; `bytes` when absent.
        public let privateBytes: UInt64?
        public let project: Project?
        public let selected: Bool
        /// Why the default selection leaves it out; kept for `ignored` findings.
        public let notSelected: NotSelected?
        /// Its type is switched off, or it is under the minimum size (`settings.json`).
        public let ignored: Bool
        public let notes: [Note]
        /// How to get it back.
        public let hint: String
        /// Part of it could not be read; its sizes are lower bounds.
        public let incomplete: Bool
        /// What one of the user's own folders holds, to pick from.
        public let contents: Contents?
        /// What keeps a `user.kept` finding; nil on any other, and on one
        /// from a CLI older than the field.
        public let protection: Protection?
        /// The newest change inside it (RFC 3339, fixed-width UTC); nil when
        /// the scan read none. A picked entry carries its own change.
        public let modified: String?

        /// What deleting it frees, as the CLI's `Finding::reclaimable_bytes`.
        public var reclaimableBytes: UInt64 { privateBytes ?? bytes }

        private enum CodingKeys: String, CodingKey {
            case path, rule, `class`, ruleClass, removal, dev, ino, bytes, privateBytes, project, selected, notSelected, ignored
            case notes, hint, incomplete, contents, protection, modified
        }

        public init(from decoder: any Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            path = try c.decode(String.self, forKey: .path)
            rule = try c.decode(String.self, forKey: .rule)
            `class` = try c.decode(RiskClass.self, forKey: .class)
            ruleClass = try c.decode(RiskClass.self, forKey: .ruleClass)
            removal = try c.decode(Removal.self, forKey: .removal)
            dev = try c.decode(UInt64.self, forKey: .dev)
            ino = try c.decode(UInt64.self, forKey: .ino)
            bytes = try c.decode(UInt64.self, forKey: .bytes)
            privateBytes = try c.decodeIfPresent(UInt64.self, forKey: .privateBytes)
            project = try c.decodeIfPresent(Project.self, forKey: .project)
            selected = try c.decode(Bool.self, forKey: .selected)
            notSelected = try c.decodeIfPresent(NotSelected.self, forKey: .notSelected)
            ignored = try c.decodeIfPresent(Bool.self, forKey: .ignored) ?? false
            notes = try c.decode([Note].self, forKey: .notes)
            hint = try c.decode(String.self, forKey: .hint)
            incomplete = try c.decode(Bool.self, forKey: .incomplete)
            contents = try c.decodeIfPresent(Contents.self, forKey: .contents)
            protection = try c.decodeIfPresent(Protection.self, forKey: .protection)
            // Absent before the CLI read it, and on what has no date: both nil.
            modified = try c.decodeIfPresent(String.self, forKey: .modified)
        }

        /// A picked entry as `plan_listed` synthesizes it: its folder's rule,
        /// class `user-data`, to the Trash.
        init(entry: FolderEntry, folder: Finding) {
            path = entry.path
            rule = folder.rule
            `class` = .userData
            ruleClass = folder.ruleClass
            removal = .trash
            dev = entry.dev
            ino = entry.ino
            bytes = entry.bytes
            privateBytes = nil
            project = nil
            selected = false
            notSelected = .userData
            ignored = false
            notes = []
            hint = folder.hint
            incomplete = entry.incomplete
            contents = nil
            protection = nil
            modified = entry.modified
        }
    }

    /// What protects a `user.kept` finding (story "Protected by you: say what
    /// it is, and Remove Protection"): its keep markers, the exclusions and
    /// skips at or above it or inside it, and the rule it displaced.
    public struct Protection: Decodable, Sendable, Equatable {
        /// Each folder holding a `.ddc-keep`, the finding's own or one inside it.
        public let markers: [MarkerFolder]
        public let exclusions: [Exclusion]
        /// The rule that would have claimed the folder, when the walk knows it.
        public let rule: String?
    }

    /// A folder holding a `.ddc-keep`, with its identity at scan time:
    /// `keep remove` checks it again.
    public struct MarkerFolder: Decodable, Sendable, Equatable, Hashable {
        public let folder: String
        public let dev: UInt64
        public let ino: UInt64
    }

    /// An exclusion or a skip that protects a finding; its path is canonical.
    public struct Exclusion: Decodable, Sendable, Equatable, Hashable {
        public let path: String
        public let kind: ExclusionKind
        public let origin: ExclusionOrigin
        /// The path lies inside the finding; otherwise it is the finding's
        /// own folder or above it.
        public let inside: Bool
    }

    /// The CLI's `ExclusionKind`. An unknown value is a decoding error: a new
    /// kind comes with a new schema version.
    public enum ExclusionKind: String, Decodable, Sendable, CaseIterable {
        /// `excludes`, `--exclude`: read and sized, never touched.
        case exclude
        /// `skips`, `--skip`: never read.
        case skip
    }

    /// The CLI's `ExclusionOrigin`. An unknown value is a decoding error.
    public enum ExclusionOrigin: String, Decodable, Sendable, CaseIterable {
        /// `settings.json`.
        case settings
        /// A command-line flag.
        case flag
    }

    /// The direct children of a folder with entries: the largest listed, the
    /// rest summed. A coding agent's session folder lists its sessions, with
    /// no size floor.
    public struct Contents: Decodable, Sendable, Equatable {
        /// Largest first.
        public let entries: [FolderEntry]
        /// The other children, findings of their own left out.
        public let restCount: UInt64
        public let restBytes: UInt64
    }

    public struct FolderEntry: Decodable, Sendable, Equatable {
        public enum Kind: String, Decodable, Sendable {
            case file, folder
        }

        public let path: String
        public let kind: Kind
        /// Allocated bytes, findings inside it included; a session's, its
        /// companions' too.
        public let bytes: UInt64
        public let dev: UInt64
        public let ino: UInt64
        /// Last modification (RFC 3339).
        public let modified: String?
        /// Why it cannot be picked.
        public let lock: EntryLock?
        /// Parts of it could not be read; it can still be picked.
        public let incomplete: Bool
        /// Findings of other rules inside it.
        public let findings: UInt64
        /// A folder with `.git`: a project.
        public let git: Bool
        public let backup: Backup?

        private enum CodingKeys: String, CodingKey {
            case path, kind, bytes, dev, ino, modified, lock, incomplete, findings, git, backup
        }

        public init(from decoder: any Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            path = try c.decode(String.self, forKey: .path)
            kind = try c.decode(Kind.self, forKey: .kind)
            bytes = try c.decode(UInt64.self, forKey: .bytes)
            dev = try c.decode(UInt64.self, forKey: .dev)
            ino = try c.decode(UInt64.self, forKey: .ino)
            modified = try c.decodeIfPresent(String.self, forKey: .modified)
            lock = try c.decodeIfPresent(EntryLock.self, forKey: .lock)
            incomplete = try c.decodeIfPresent(Bool.self, forKey: .incomplete) ?? false
            findings = try c.decodeIfPresent(UInt64.self, forKey: .findings) ?? 0
            git = try c.decodeIfPresent(Bool.self, forKey: .git) ?? false
            backup = try c.decodeIfPresent(Backup.self, forKey: .backup)
        }
    }

    /// A device backup's `Info.plist`, where it could be read.
    public struct Backup: Decodable, Sendable, Equatable {
        public let deviceName: String?
        /// RFC 3339.
        public let lastBackup: String?
    }

    public struct Project: Decodable, Sendable, Equatable {
        public let root: String
        public let idleDays: UInt64?
        public let hasLockfile: Bool?
        /// The latest activity (RFC 3339): git, manifests, lock files.
        public let lastActivity: String?
    }

    /// The CLI's `RemovalKind`. An unknown value is a decoding error.
    public enum Removal: String, Decodable, Sendable, CaseIterable {
        case delete, trash, native
        case nativeOnly = "native-only"
        case none
    }

    /// The CLI's `Note`. An unknown value is a decoding error.
    public enum Note: String, Decodable, Sendable, CaseIterable {
        case noLockfile = "no-lockfile"
        case appOwned = "app-owned"
        case keptByMarker = "kept-by-marker"
        case excluded
    }

    /// The CLI's `NotSelected`, tagged by `reason`. A reason this app does not
    /// know is a decoding error: a new one comes with a new schema version.
    public enum NotSelected: Decodable, Sendable, Equatable {
        case review
        case userData
        case activeProject(idleDays: UInt64)
        case unknownActivity

        private enum CodingKeys: String, CodingKey {
            case reason, idleDays
        }

        public init(from decoder: any Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            switch try c.decode(String.self, forKey: .reason) {
            case "review": self = .review
            case "user-data": self = .userData
            case "active-project": self = .activeProject(idleDays: try c.decode(UInt64.self, forKey: .idleDays))
            case "unknown-activity": self = .unknownActivity
            case let other:
                throw DecodingError.dataCorruptedError(forKey: .reason, in: c, debugDescription: "unknown reason \(other)")
            }
        }
    }

    public enum RiskClass: String, Codable, Sendable, CaseIterable {
        case safe, rebuildable, review
        case userData = "user-data"
    }

    public static func read(_ data: Data) throws -> ScanReport {
        let decoder = JSONDecoder.snakeCase
        let version = try decoder.decode(VersionHeader.self, from: data).schemaVersion
        guard version == schemaVersion else { throw ReadError.otherSchema(version) }
        return try decoder.decode(ScanReport.self, from: data)
    }
}

/// Why an entry of the user's own folders, or a coding agent's session,
/// cannot be picked (`EntryLock` in `crates/ddc-core/src/rules/entries.rs`),
/// in the scan report and the clean report alike. Tagged by `reason`; an
/// unknown one, or an unknown `by` or `cause`, is a decoding error.
public enum EntryLock: Decodable, Sendable, Equatable, Hashable {
    case appManaged(app: String)
    case kept
    case protected
    case otherVolume
    case recent
    case partialDownload
    case unfinishedBackup
    /// A coding agent's session that is live; in place of `recent` for the
    /// session folders.
    case liveSession(session: String, by: LiveBy)
    /// Whether the session is live could not be told.
    case livenessUnknown(cause: LivenessCause)

    /// What shows a session live (`LiveBy` in `crates/ddc-core/src/agents/liveness.rs`).
    public enum LiveBy: String, Decodable, Sendable, Hashable, CaseIterable {
        /// A running Claude Code's record names it.
        case record
        /// A file of it changed in the last 10 minutes.
        case recent
        /// A process holds a file of it open.
        case openFile = "open-file"
    }

    /// Why a session's liveness could not be told (`UnknownCause` in
    /// `crates/ddc-core/src/agents/liveness.rs`).
    public enum LivenessCause: String, Decodable, Sendable, Hashable, CaseIterable {
        case recordsMissing = "records-missing"
        case recordsUnreadable = "records-unreadable"
        case recordsSkipped = "records-skipped"
        case unparsableRecord = "unparsable-record"
        case undecidableRecord = "undecidable-record"
        case liveRecordWithoutSession = "live-record-without-session"
        case undecidableSessionRecord = "undecidable-session-record"
        case sessionWithoutFiles = "session-without-files"
    }

    /// Every `reason` value, as the schemas list them.
    public static let tags = [
        "app-managed", "kept", "protected", "other-volume", "recent", "partial-download", "unfinished-backup", "live-session",
        "liveness-unknown",
    ]

    private enum CodingKeys: String, CodingKey {
        case reason, app, session, by, cause
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        switch try c.decode(String.self, forKey: .reason) {
        case "app-managed": self = .appManaged(app: try c.decode(String.self, forKey: .app))
        case "kept": self = .kept
        case "protected": self = .protected
        case "other-volume": self = .otherVolume
        case "recent": self = .recent
        case "partial-download": self = .partialDownload
        case "unfinished-backup": self = .unfinishedBackup
        case "live-session":
            self = .liveSession(session: try c.decode(String.self, forKey: .session), by: try c.decode(LiveBy.self, forKey: .by))
        case "liveness-unknown": self = .livenessUnknown(cause: try c.decode(LivenessCause.self, forKey: .cause))
        case let other:
            throw DecodingError.dataCorruptedError(forKey: .reason, in: c, debugDescription: "unknown lock \(other)")
        }
    }
}

/// The first thing read from every versioned JSON the CLI writes.
struct VersionHeader: Decodable {
    let schemaVersion: Int
}

extension JSONDecoder {
    /// The CLI writes snake_case keys.
    static var snakeCase: JSONDecoder {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return decoder
    }
}
