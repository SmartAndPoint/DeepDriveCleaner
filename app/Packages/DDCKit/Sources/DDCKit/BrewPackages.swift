import Foundation

/// The listing of `deepdrivecleaner disk brew-packages --json` (the contract
/// is `crates/ddc-core/src/brew/model.rs`, schema in
/// `docs/brew-packages.schema.json`; story "Whole disk: Homebrew packages,
/// their sizes, and brew uninstall"). Strict like `DiskReport`: another
/// schema version is unreadable, and an unknown enum value is a decoding
/// error. Names and versions are brew's strings.
public struct BrewPackages: Decodable, Sendable, Equatable {
    /// `BREW_PACKAGES_SCHEMA_VERSION` of the CLI this app embeds.
    public static let schemaVersion = 1

    public let schemaVersion: Int
    public let ddcVersion: String
    public let prefix: String
    public let cellar: String
    public let services: ServicesState
    /// Installed on request, largest first.
    public let formulae: [Package]
    /// Needed by nothing now, by `full_name`.
    public let orphans: [String]
    /// Formulae brew listed without a field ddc needs: nothing is offered.
    public let incomplete: [String]

    private enum CodingKeys: String, CodingKey {
        case schemaVersion, ddcVersion, prefix, cellar, services, formulae, orphans, incomplete
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schemaVersion = try c.decode(Int.self, forKey: .schemaVersion)
        ddcVersion = try c.decode(String.self, forKey: .ddcVersion)
        prefix = try c.decode(String.self, forKey: .prefix)
        cellar = try c.decode(String.self, forKey: .cellar)
        services = try c.decode(ServicesState.self, forKey: .services)
        formulae = try c.decode([Package].self, forKey: .formulae)
        orphans = try c.decode([String].self, forKey: .orphans)
        incomplete = try c.decodeIfPresent([String].self, forKey: .incomplete) ?? []
    }

    public static func read(_ data: Data) throws -> BrewPackages {
        let decoder = JSONDecoder.snakeCase
        let version = try decoder.decode(VersionHeader.self, from: data).schemaVersion
        guard version == schemaVersion else { throw ScanReport.ReadError.otherSchema(version) }
        return try decoder.decode(BrewPackages.self, from: data)
    }

    public enum ServicesState: String, Decodable, Sendable, CaseIterable {
        case listed
        case notKnown = "not-known"
        case unreadable
    }

    /// A formula's service, as the listing judged it.
    public enum Service: String, Decodable, Sendable, CaseIterable {
        case none
        case notKnown = "not-known"
        case unreadable, running, root, registered
    }

    public enum NotOffered: String, Decodable, Sendable, CaseIterable {
        case needed, pinned, service
        case servicesUnreadable = "services-unreadable"
        case fieldMissing = "field-missing"
    }

    public struct Package: Decodable, Sendable, Equatable, Identifiable {
        /// The Cellar rack's name.
        public let name: String
        /// What an uninstall names: `user/tap/foo` for a tap formula.
        public let fullName: String
        public let pinned: Bool
        public let kegs: [Keg]
        /// The kegs together; nil when one was not measured.
        public let bytes: UInt64?
        public let service: Service
        public let neededBy: [String]
        public let neededByCasks: [String]
        public let dependencies: [Dependency]
        /// What its removal would leave needed by nothing.
        public let wouldOrphan: [String]
        public let wouldOrphanBytes: UInt64?
        public let keptPinned: [String]
        public let notOffered: NotOffered?

        public var id: String { fullName }

        private enum CodingKeys: String, CodingKey {
            case name, fullName, pinned, kegs, bytes, service, neededBy, neededByCasks, dependencies
            case wouldOrphan, wouldOrphanBytes, keptPinned, notOffered
        }

        public init(from decoder: any Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            name = try c.decode(String.self, forKey: .name)
            fullName = try c.decode(String.self, forKey: .fullName)
            pinned = try c.decode(Bool.self, forKey: .pinned)
            kegs = try c.decode([Keg].self, forKey: .kegs)
            bytes = try c.decodeIfPresent(UInt64.self, forKey: .bytes)
            service = try c.decode(Service.self, forKey: .service)
            neededBy = try c.decodeIfPresent([String].self, forKey: .neededBy) ?? []
            neededByCasks = try c.decodeIfPresent([String].self, forKey: .neededByCasks) ?? []
            dependencies = try c.decode([Dependency].self, forKey: .dependencies)
            wouldOrphan = try c.decode([String].self, forKey: .wouldOrphan)
            wouldOrphanBytes = try c.decodeIfPresent(UInt64.self, forKey: .wouldOrphanBytes)
            keptPinned = try c.decodeIfPresent([String].self, forKey: .keptPinned) ?? []
            notOffered = try c.decodeIfPresent(NotOffered.self, forKey: .notOffered)
        }
    }

    public struct Keg: Decodable, Sendable, Equatable {
        public let version: String
        public let path: String
        /// nil under a skip of `settings.json`, or when it could not be read.
        public let bytes: UInt64?
    }

    public struct Dependency: Decodable, Sendable, Equatable {
        public let fullName: String
        public let bytes: UInt64?
        /// Installed on request: it stays.
        public let onRequest: Bool
        public let pinned: Bool
        /// Only this formula holds it: its removal orphans it.
        public let alone: Bool
    }
}

/// The packages sheet's sentences (story "Whole disk: Homebrew packages,
/// their sizes, and brew uninstall").
public enum BrewWording {
    /// Why a formula's row has no Uninstall button; nil when it has one.
    public static func notOffered(_ package: BrewPackages.Package) -> String? {
        switch package.notOffered {
        case nil: nil
        case .needed: "Needed by \(dependents(formulae: package.neededBy, casks: package.neededByCasks))."
        case .pinned: "Pinned: brew unpin it first."
        case .service: "\(capitalized(service(package.service)))."
        case .servicesUnreadable: "Its service state could not be read."
        case .fieldMissing: "brew listed a formula without a field DeepDriveCleaner needs."
        }
    }

    /// A note under an offered formula: brew could not say whether it runs
    /// a service.
    public static func note(_ package: BrewPackages.Package) -> String? {
        guard package.notOffered == nil, package.service == .notKnown else { return nil }
        return "Whether a service is registered for it is not known: this brew has no services command."
    }

    /// "curl, wget and the cask foo".
    public static func dependents(formulae: [String], casks: [String]) -> String {
        let all = formulae + casks.map { "the cask \($0)" }
        guard all.count > 1 else { return all.first ?? "nothing" }
        return all.dropLast().joined(separator: ", ") + " and " + all.last!
    }

    /// What the refusal's `needed_by` names, one sentence per formula.
    public static func neededBy(_ action: FreeReport.ActionReport) -> String? {
        let lines = action.neededBy.filter { !$0.formulae.isEmpty || !$0.casks.isEmpty }.map {
            "\($0.formula) is needed by \(dependents(formulae: $0.formulae, casks: $0.casks))."
        }
        return lines.isEmpty ? nil : lines.joined(separator: " ")
    }

    static func service(_ service: BrewPackages.Service) -> String {
        switch service {
        case .running: "a service of yours is loaded for it; stop it with brew services first"
        case .root: "a root service is registered for it; its state is not known"
        case .registered: "a service of yours is registered for it but not running; remove it with brew services first"
        case .unreadable: "its service state could not be read"
        case .notKnown: "whether a service is registered for it is not known"
        case .none: "no service of yours is registered with launchd"
        }
    }

    private static func capitalized(_ text: String) -> String {
        text.prefix(1).uppercased() + text.dropFirst()
    }
}
