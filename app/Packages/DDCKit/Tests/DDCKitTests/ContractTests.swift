import Foundation
import Testing
@testable import DDCKit

/// The Swift side mirrors constants of the Rust core; these read the Rust
/// sources so a change there fails here instead of drifting silently.
@Suite("Contracts with the CLI")
struct ContractTests {
    @Test("the tombstone rule id is the core's")
    func tombstone() throws {
        let source = try rustSource("crates/ddc-core/src/rules/mod.rs")
        let match = try #require(source.firstMatch(of: /pub const TOMBSTONE: &str = "([^"]+)";/))
        #expect(String(match.1) == ReportSummary.tombstone)
    }

    @Test("the rules list version is the CLI's")
    func rulesVersion() throws {
        let source = try rustSource("crates/ddc-core/src/rules/listing.rs")
        let match = try #require(source.firstMatch(of: /pub const RULES_SCHEMA_VERSION: u32 = (\d+);/))
        #expect(Int(match.1) == RuleList.schemaVersion)
    }

    @Test("the settings version is the CLI's")
    func settingsVersion() throws {
        let source = try rustSource("crates/ddc-core/src/settings.rs")
        let match = try #require(source.firstMatch(of: /pub const SETTINGS_SCHEMA_VERSION: u32 = (\d+);/))
        #expect(Int(match.1) == SharedSettings.schemaVersion)
    }

    @Test("the default notification threshold is the background scan's")
    func defaultThreshold() throws {
        let source = try rustSource("crates/ddc-cli/src/cmd/background.rs")
        let match = try #require(source.firstMatch(of: /const DEFAULT_THRESHOLD: u64 = (\d+) << (\d+);/))
        #expect(try #require(UInt64(match.1)) << (try #require(UInt64(match.2))) == SharedSettings.defaultThreshold)
    }

    @Test("the startup disk is named as the CLI names it")
    func startupDisk() throws {
        let source = try rustSource("crates/ddc-cli/src/notify.rs")
        let match = try #require(source.firstMatch(of: /Some\("\/" \| "\/System\/Volumes\/Data"\) => "([^"]+)"/))
        #expect(Wording.volume("/") == String(match.1))
        #expect(Wording.volume("/System/Volumes/Data") == String(match.1))
        #expect(Wording.volume("/Volumes/Work") == "Work")
    }

    @Test("sizes are written as the CLI writes them")
    func sizes() {
        #expect(ByteSize.human(0) == "0 B")
        #expect(ByteSize.human(1023) == "1023 B")
        #expect(ByteSize.human(1536) == "1.5 KiB")
        #expect(ByteSize.human(10 << 30) == "10.0 GiB")
        #expect(ByteSize.human(1_048_575) == "1024.0 KiB")
    }

    @Test("the rules list and settings decode; a missing settings file gives the defaults")
    func decoding() throws {
        let rules = try Fixture.rules()
        #expect(rules.group(of: "node.node_modules") == "node_modules")
        #expect(rules.groups.first == "Python environments")
        let none = try SharedSettings.read(at: URL(filePath: "/nonexistent/settings.json"))
        #expect(none == SharedSettings())
        #expect(none.threshold == SharedSettings.defaultThreshold)
        let groups = try Fixture.groupsReport()
        #expect(groups.volumes.map(\.mountPoint) == ["/System/Volumes/Data"])
        #expect(groups.findings.contains { $0.notSelected == .activeProject(idleDays: 0) })
    }

    @Test("a settings file the CLI would refuse is refused, a newer one reads as newer")
    func strictSettings() throws {
        let data = try TempDataDirectory()
        func read(_ text: String) throws(SharedSettings.ReadError) -> SharedSettings {
            try? data.write(settings: text)
            return try SharedSettings.read(at: data.directory.settings)
        }
        let path = data.directory.settings.path
        #expect(throws: SharedSettings.ReadError.malformed(path: path, reason: "unknown key show_badge")) {
            try read(#"{"schema_version": 1, "show_badge": true}"#)
        }
        #expect(throws: SharedSettings.ReadError.otherSchema(2)) { try read(#"{"schema_version": 2, "min_size": "50M"}"#) }
        #expect(throws: SharedSettings.ReadError.malformed(path: path, reason: "Work is not an absolute path")) {
            try read(#"{"schema_version": 1, "roots": ["Work"]}"#)
        }
        #expect(throws: SharedSettings.ReadError.malformed(path: path, reason: "min_age_days is over 3650")) {
            try read(#"{"schema_version": 1, "min_age_days": 4000}"#)
        }
        #expect(throws: SharedSettings.ReadError.malformed(path: path, reason: "a field has the wrong type")) {
            try read(#"{"schema_version": 1, "notify_low_disk": "yes"}"#)
        }
        let full = try read(#"{"schema_version": 1, "roots": ["/Volumes/Work"], "excludes": ["/Volumes/Work/keep"], "min_age_days": 30, "notify_low_disk": false, "scheduler": {"by": "app", "app": "/Applications/DeepDriveCleaner.app"}, "min_size": 5}"#)
        #expect(full.roots == ["/Volumes/Work"] && full.minSize == 5)
    }

    @Test("the settings keys are the CLI's")
    func settingsKeys() throws {
        let source = try rustSource("crates/ddc-core/src/settings.rs")
        let body = try #require(source.firstMatch(of: /pub struct Settings \{([^}]*)\}/))
        let fields = Set(String(body.1).matches(of: /pub (\w+):/).map { String($0.1) })
        #expect(fields == SharedSettings.keys)
        let max = try #require(source.firstMatch(of: /const MAX_MIN_AGE_DAYS: u64 = (\d+);/))
        #expect(UInt64(max.1) == SharedSettings.maxMinAgeDays)
    }

    @Test("only CleanRun.swift runs a clean or its dry run, and only the sheet's Clean action starts one")
    func cleanHasOneOwner() throws {
        let sources = try appSources()
        func files(_ pattern: Regex<Substring>) -> Set<String> {
            Set(sources.filter { $0.value.contains(pattern) }.map(\.key))
        }
        func count(_ pattern: Regex<Substring>, in file: String) -> Int {
            sources[file]?.matches(of: pattern).count ?? 0
        }
        #expect(files(/\.clean\b/).isSubset(of: ["CLIRunner.swift", "CleanRun.swift"]), "the popover never cleans (ADR §7)")
        #expect(files(/\.dryRun\b/).isSubset(of: ["CLIRunner.swift", "CleanRun.swift"]))
        #expect(count(/\.clean\b/, in: "CleanRun.swift") == 1)
        #expect(files(/"--items"/) == ["CleanRun.swift"])
        #expect(files(/cleanConfirmedItems\(/) == ["CleanRun.swift", "ConfirmSheet.swift"])
        #expect(count(/func cleanConfirmedItems\(/, in: "CleanRun.swift") == 1)
        #expect(count(/cleanConfirmedItems\(/, in: "CleanRun.swift") == 1, "its definition only")
        #expect(count(/cleanConfirmedItems\(/, in: "ConfirmSheet.swift") == 1, "the sheet's Clean action")
    }

    @Test("the item list, the clean report and the keep marker are the CLI's")
    func cleanContracts() throws {
        let plan = try rustSource("crates/ddc-core/src/plan.rs")
        let listed = try #require(plan.firstMatch(of: /pub const LISTED_SCHEMA_VERSION: u32 = (\d+);/))
        #expect(Int(listed.1) == ItemList.schemaVersion)
        let report = try rustSource("crates/ddc-core/src/exec/report.rs")
        let clean = try #require(report.firstMatch(of: /pub const CLEAN_SCHEMA_VERSION: u32 = (\d+);/))
        #expect(Int(clean.1) == CleanReport.schemaVersion)
        let visitor = try rustSource("crates/ddc-core/src/rules/visitor.rs")
        let marker = try #require(visitor.firstMatch(of: /pub const KEEP_MARKER: &str = "([^"]+)";/))
        #expect(String(marker.1) == KeepMarker.name)
    }

    @Test("every enum value the schemas list is one the app decodes")
    func schemaEnums() throws {
        let clean = try schema("clean")
        #expect(Set(try values(clean, "SkipReason", key: "reason")) == Set(SkipReason.tags))
        #expect(SkipReason.tags.count == 18)
        #expect(Set(try values(clean, "ActionKind")) == Set(ListedAction.allCases.map(\.rawValue)))
        let scan = try schema("scan")
        #expect(Set(try values(scan, "RemovalKind")) == Set(ScanReport.Removal.allCases.map(\.rawValue)))
        #expect(Set(try values(scan, "Note")) == Set(ScanReport.Note.allCases.map(\.rawValue)))
        #expect(Set(try values(scan, "RiskClass")) == Set(ScanReport.RiskClass.allCases.map(\.rawValue)))
    }

    @Test("every skip reason decodes and reads as text")
    func skipReasonsHaveText() throws {
        for tag in SkipReason.tags {
            let line = #"{"type":"item","path":"/Users/example/x","rule":"cache.npm","class":"safe","action":"delete","prediction":"would-skip","reason":"\#(tag)","pid":7,"name":"node","program":"go"}"#
            guard case .item(let item) = try PreflightLine.read(line), case .wouldSkip(let reason) = item.prediction else {
                Issue.record("\(tag) did not decode as a skip")
                continue
            }
            #expect(!CleanWording.reason(reason).isEmpty)
            #expect(CleanWording.willSkip(reason).hasSuffix("will be skipped"))
        }
        #expect(throws: (any Error).self) {
            try PreflightLine.read(#"{"type":"item","path":"/x","rule":"r","class":"safe","action":"delete","prediction":"would-skip","reason":"new-reason"}"#)
        }
    }

    // MARK: - Parts

    /// The app's Swift sources, comment lines stripped, by file name.
    private func appSources() throws -> [String: String] {
        let root = repositoryRoot()
        var sources: [String: String] = [:]
        for dir in ["app/DeepDriveCleaner", "app/Packages/DDCKit/Sources/DDCKit"] {
            for file in try FileManager.default.contentsOfDirectory(atPath: root.appending(path: dir).path) where file.hasSuffix(".swift") {
                let text = try String(contentsOf: root.appending(path: "\(dir)/\(file)"), encoding: .utf8)
                sources[file] = text.split(separator: "\n", omittingEmptySubsequences: false)
                    .filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("//") }
                    .joined(separator: "\n")
            }
        }
        return sources
    }

    private func schema(_ name: String) throws -> [String: Any] {
        let data = try Data(contentsOf: repositoryRoot().appending(path: "docs/\(name).schema.json"))
        return try #require(try JSONSerialization.jsonObject(with: data) as? [String: Any])
    }

    /// The string values a `$defs` entry allows: its `enum`s and `const`s, or
    /// with `key`, the `const` of that property in each `oneOf` branch.
    private func values(_ schema: [String: Any], _ name: String, key: String? = nil) throws -> [String] {
        let defs = try #require(schema["$defs"] as? [String: Any])
        let def = try #require(defs[name] as? [String: Any])
        let branches = (def["oneOf"] as? [[String: Any]]) ?? [def]
        return branches.flatMap { branch -> [String] in
            if let key {
                let property = (branch["properties"] as? [String: Any])?[key] as? [String: Any]
                return (property?["const"] as? String).map { [$0] } ?? []
            }
            return (branch["enum"] as? [String] ?? []) + ((branch["const"] as? String).map { [$0] } ?? [])
        }
    }
}
