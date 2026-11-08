import Foundation

/// What `ddc disk docker --json` prints (`docs/docker.schema.json`; story
/// "Docker Desktop: what is inside its disk image, and what a clean
/// frees"): Docker Desktop's records by kind, and what the clean's two
/// prunes would free, about. It names no image, container or volume.
public struct DockerContents: Decodable, Sendable, Equatable {
    /// `DOCKER_SCHEMA_VERSION` of the CLI this app embeds.
    public static let schemaVersion = 1

    public let schemaVersion: Int
    public let ddcVersion: String
    public let result: Result

    /// A kind of record: how many, and their size together; nil when one
    /// has none (volumes without one are left out of it instead).
    public struct Group: Decodable, Sendable, Equatable {
        public let count: UInt64
        public let bytes: UInt64?

        public init(count: UInt64, bytes: UInt64?) {
            self.count = count
            self.bytes = bytes
        }
    }

    /// What `docker builder prune -f` and `docker image prune -f` would free.
    public struct Estimate: Decodable, Sendable, Equatable {
        public let buildCache: UInt64
        public let danglingImages: UInt64?
        /// The two together; nil while a part is.
        public let bytes: UInt64?

        public init(buildCache: UInt64, danglingImages: UInt64?, bytes: UInt64?) {
            self.buildCache = buildCache
            self.danglingImages = danglingImages
            self.bytes = bytes
        }
    }

    public struct Listed: Sendable, Equatable {
        public let images: Group
        public let containers: Group
        public let volumes: Group
        public let buildCache: Group
        public let estimate: Estimate

        public init(images: Group, containers: Group, volumes: Group, buildCache: Group, estimate: Estimate) {
            self.images = images
            self.containers = containers
            self.volumes = volumes
            self.buildCache = buildCache
            self.estimate = estimate
        }
    }

    /// The CLI's `status`, strict: an unknown one is a read error.
    public enum Result: Decodable, Sendable, Equatable {
        /// Every `status`, as `docs/docker.schema.json` names them.
        public static let tags = ["listed", "not-installed", "not-ready", "failed", "timed-out", "unreadable"]

        case listed(Listed)
        case notInstalled
        /// The readiness probe's own words, as in a clean's `tool-not-ready` skip.
        case notReady(detail: String)
        case failed(detail: String)
        case timedOut(seconds: UInt64)
        case unreadable(detail: String)

        private enum CodingKeys: String, CodingKey {
            case status, images, containers, volumes, buildCache, estimate, detail, seconds
        }

        public init(from decoder: any Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            switch try c.decode(String.self, forKey: .status) {
            case "listed":
                self = .listed(Listed(
                    images: try c.decode(Group.self, forKey: .images),
                    containers: try c.decode(Group.self, forKey: .containers),
                    volumes: try c.decode(Group.self, forKey: .volumes),
                    buildCache: try c.decode(Group.self, forKey: .buildCache),
                    estimate: try c.decode(Estimate.self, forKey: .estimate)
                ))
            case "not-installed": self = .notInstalled
            case "not-ready": self = .notReady(detail: try c.decode(String.self, forKey: .detail))
            case "failed": self = .failed(detail: try c.decode(String.self, forKey: .detail))
            case "timed-out": self = .timedOut(seconds: try c.decode(UInt64.self, forKey: .seconds))
            case "unreadable": self = .unreadable(detail: try c.decode(String.self, forKey: .detail))
            case let other:
                throw DecodingError.dataCorruptedError(forKey: .status, in: c, debugDescription: "unknown status \(other)")
            }
        }
    }

    /// The JSON, strictly: another schema version is `otherSchema`.
    public static func read(_ data: Data) throws -> DockerContents {
        let decoder = JSONDecoder.snakeCase
        let version = try decoder.decode(VersionHeader.self, from: data).schemaVersion
        guard version == schemaVersion else { throw ScanReport.ReadError.otherSchema(version) }
        return try decoder.decode(DockerContents.self, from: data)
    }
}

extension CleanWording {
    /// A size as docker prints it (`units.HumanSize`: decimal units, four
    /// significant digits), so the row reads like `docker system df`.
    public static func dockerSize(_ bytes: UInt64) -> String {
        let units = ["B", "kB", "MB", "GB", "TB", "PB", "EB"]
        var value = Double(bytes)
        var unit = 0
        while value >= 1000, unit < units.count - 1 {
            value /= 1000
            unit += 1
        }
        return String(format: "%.4g%@", value, units[unit])
    }

    /// The Docker Desktop item's lines: what is inside, what a clean frees,
    /// what it leaves with the command to run, and why the sizes do not add
    /// up to the folder's.
    public static func dockerLines(_ listed: DockerContents.Listed) -> [String] {
        func group(_ g: DockerContents.Group, _ one: String, _ many: String, note: String = "") -> String {
            let count = "\(Wording.number(Int(clamping: g.count))) \(g.count == 1 ? one : many)"
            guard let bytes = g.bytes else { return "\(count), size not known" }
            return "\(count) · \(dockerSize(bytes))\(note)"
        }
        let inside = [
            group(listed.images, "image", "images", note: " unique to each"),
            group(listed.containers, "container", "containers"),
            group(listed.volumes, "volume", "volumes"),
            group(listed.buildCache, "build-cache record", "build-cache records"),
        ]
        let estimate = listed.estimate
        let frees: String
        if let bytes = estimate.bytes, let dangling = estimate.danglingImages {
            frees = "A clean frees about \(dockerSize(bytes)): unused build cache \(dockerSize(estimate.buildCache)) and dangling images \(dockerSize(dangling))."
        } else {
            frees = "What a clean frees is not known: Docker gave no size for a dangling image. It stays not counted."
        }
        return [
            "Inside: \(Wording.list(inside)).",
            frees,
            "It leaves unused tagged images (docker image prune -a), stopped containers (docker container prune), volumes with their data (docker volume prune) and the rest of the build cache (docker builder prune --all).",
            "These sizes leave out shared layers, and the disk image keeps freed space until Docker Desktop shrinks it, so they do not add up to the folder's size.",
        ]
    }
}
