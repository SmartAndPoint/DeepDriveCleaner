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
        #expect(throws: SharedSettings.ReadError.malformed(path: path, reason: "huge is not an absolute path")) {
            try read(#"{"schema_version": 1, "skips": ["/Users/example/x", "huge"]}"#)
        }
        #expect(throws: SharedSettings.ReadError.malformed(path: path, reason: "a field has the wrong type")) {
            try read(#"{"schema_version": 1, "skips": "/Users/example/x"}"#)
        }
        for key in ["roots", "excludes", "skips", "disabled_rules"] {
            #expect(throws: SharedSettings.ReadError.self, "serde refuses a null \(key)") {
                try read(#"{"schema_version": 1, "\#(key)": null}"#)
            }
        }
        #expect(throws: SharedSettings.ReadError.malformed(path: path, reason: "min_age_days is over 3650")) {
            try read(#"{"schema_version": 1, "min_age_days": 4000}"#)
        }
        #expect(throws: SharedSettings.ReadError.malformed(path: path, reason: "a field has the wrong type")) {
            try read(#"{"schema_version": 1, "notify_low_disk": "yes"}"#)
        }
        let full = try read(#"{"schema_version": 1, "roots": ["/Volumes/Work"], "excludes": ["/Volumes/Work/keep"], "skips": ["/Volumes/Work/huge"], "min_age_days": 30, "notify_low_disk": false, "scheduler": {"by": "app", "app": "/Applications/DeepDriveCleaner.app"}, "min_size": 5}"#)
        #expect(full.roots == ["/Volumes/Work"] && full.minSize == 5)
    }

    @Test("the settings keys are the CLI's")
    func settingsKeys() throws {
        let source = try rustSource("crates/ddc-core/src/settings.rs")
        let body = try #require(source.firstMatch(of: /pub struct Settings \{([^}]*)\}/))
        let fields = Set(String(body.1).matches(of: /pub (\w+):/).map { String($0.1) })
        #expect(fields == SharedSettings.keys)
        #expect(fields.contains("skips"))
        let max = try #require(source.firstMatch(of: /const MAX_MIN_AGE_DAYS: u64 = (\d+);/))
        #expect(UInt64(max.1) == SharedSettings.maxMinAgeDays)
        let report = try rustSource("crates/ddc-core/src/report/mod.rs")
        let age = try #require(report.firstMatch(of: /pub const DEFAULT_MIN_AGE: Duration = Duration::from_secs\((\d+) \* 86_400\);/))
        #expect(UInt64(age.1) == SettingsModel.defaultMinAgeDays)
    }

    @Test("the prompting areas are the background scan's")
    func promptingAreas() throws {
        let source = try rustSource("crates/ddc-cli/src/cmd/background.rs")
        let body = try #require(source.firstMatch(of: /const PROMPTING_AREAS: &\[&str\] = &\[([^\]]*)\];/))
        #expect(String(body.1).matches(of: /"([^"]+)"/).map { String($0.1) } == NotReadable.promptingAreas)
    }

    @Test("every Types row has help text, and the groups are the core's")
    func typesHelp() throws {
        let rules = try Fixture.rules()
        let rows = TypesRows.make(rules: rules, disabled: [])
        #expect(!rows.isEmpty)
        for row in rows { #expect(row.help?.isEmpty == false, "\(row.label) has no help") }
        #expect(Set(TypesRows.help.keys) == Set(rows.map(\.label)), "no help for a group that is not a row")
        // The fixture is `rules --json`; its groups are `GROUPS` of the core, in order.
        let source = try rustSource("crates/ddc-core/src/rules/listing.rs")
        let groups = try #require(source.firstMatch(of: /pub const GROUPS: &\[\(&str, &\[&str\]\)\] = &\[(.*?)\n\];/.dotMatchesNewlines()))
        let labels = String(groups.1).matches(of: /\(\s*"([^"]+)",\s*&\[/).map { String($0.1) }
        #expect(labels == rules.groups)
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
        // Story "Review window: Clean is not offered when every item would be
        // skipped": the button is off, and the action itself runs nothing,
        // while the check runs or when every item would be skipped.
        let run = try #require(sources["CleanRun.swift"])
        let confirmed = try #require(run.firstMatch(of: /func cleanConfirmedItems\(\)[\s\S]*?\n    }\n/))
        #expect(confirmed.output.contains(/guard let sheet, sheet\.cleanAllowed,/), "cleanConfirmedItems() guards on cleanAllowed")
        let sheet = try #require(sources["ConfirmSheet.swift"])
        // The label is computed ("Clean 2 items", "Move to the Trash"): the
        // button is found by its call.
        let button = try #require(sheet.firstMatch(of: /Button\([^\n]*\{ model\.cleanConfirmedItems\(\) \}\n(?:\s*\.[^\n]*\n)*/))
        #expect(button.output.contains("role: .destructive"))
        #expect(button.output.contains(/\.disabled\([^\n]*!sheet\.cleanAllowed/), "the Clean button is off unless the clean is allowed")
        let summary = try #require(sources["SkipSummary.swift"])
        #expect(summary.contains(/stage == \.confirm && warnings != \.checking && !skipSummary\.nothingToClean/))
    }

    @Test("a User data folder draws no Select all; its Move row has no checkbox and only opens the scoped sheet")
    func userDataPickedRow() throws {
        // Story "User data: move a folder's picked files to the Trash from that folder".
        let view = try #require(try appSources()["ReviewGroupsView.swift"])
        #expect(view.matches(of: /SelectAllRow\(/).count == 1, "drawn in one place")
        #expect(view.contains(/if let all = layout\.selectAll[^\n]*\{\n\s*SelectAllRow\(/), "only where the layout has Select all")
        let row = try #require(view.firstMatch(of: /struct PickedRow[\s\S]*?\n}\n/)).output
        for forbidden in ["CheckBox(", "toggleAll(", "set(", "selection", "cleanConfirmedItems"] {
            #expect(!row.contains(forbidden), "PickedRow uses \(forbidden)")
        }
        #expect(row.matches(of: /Button\s*[({]/).count == 1, "one button")
        let actions = row.matches(of: /Button\s*\{([^}]*)\}/).map { $0.1.trimmingCharacters(in: .whitespaces) }
        #expect(actions == ["model.openSheet(scope: ids)"])
    }

    @Test("the Next scan line is one link button that only opens Settings, hidden while paused; the popover closes first")
    func nextScanLink() throws {
        // Story "Popover: the Next scan line opens the Schedule settings".
        let sources = try appSources()
        let popover = try #require(sources["PopoverView.swift"])
        #expect(popover.matches(of: /Button\(model\.nextScanText\)/).count == 1)
        let button = try #require(popover.firstMatch(of: /if !model\.paused \{\n[^\n]*\n\s*Button\(model\.nextScanText\) \{([^}]*)\}\n((?:\s*\.[^\n]*\n)*)/))
        #expect(button.1.trimmingCharacters(in: .whitespaces) == "openNextScan(model.nextScanValue.settingsDestination)")
        for modifier in [".buttonStyle(.link)", ".foregroundStyle(.secondary)", ".pointerStyle(.link)", ".help(", ".accessibilityHint("] {
            #expect(button.2.contains(modifier), "\(modifier)")
        }
        #expect(popover.matches(of: /openNextScan\(/).count == 1, "the one call")
        let status = try #require(sources["StatusItemController.swift"])
        #expect(status.contains(/openNextScan: \{ \[weak self\] destination in\n\s*self\?\.popover\.performClose\(nil\)\n\s*openNextScan\(destination\)/))
        #expect(status.contains(#"ClosureMenuItem("Settings…", key: ",", handler: openSettings)"#), "the menu's Settings… stays pane-less")
        let delegate = try #require(sources["AppDelegate.swift"])
        #expect(delegate.contains(/openNextScan: \{[^\n]*show\(pane:/))
    }

    @Test("the result's Scan Again closes the sheet and runs only the window's scanAgain(); the closing sheet draws its last stage")
    func resultScanAgain() throws {
        let sources = try appSources()
        // Story "Review window: after a clean, only skips that can clear stay selected".
        let view = try #require(sources["CleanResultView.swift"])
        let button = try #require(view.firstMatch(of: /Button\("Scan Again"\) \{\n([\s\S]*?)\n\s*\}\n(?:\s*\.[^\n]*\n)*/))
        let calls = button.1.matches(of: /model\.(\w+)\(/).map { String($0.1) }
        #expect(calls == ["closeSheet", "scanAgain"], "\(calls)")
        #expect(button.1.contains("Task { await model.scanAgain() }"))
        #expect(!button.1.contains(/rescan\b|reloadReport|runner|\.run\(/))
        #expect(button.0.contains(/\.disabled\(model\.isCleaning \|\| model\.rescanning != nil/), "off while cleaning or rescanning")
        #expect(view.contains(/if result\.needsRescan > 0 \{\n\s*Button\("Scan Again"\)/), "offered only for items that changed")
        let sheet = try #require(sources["ConfirmSheet.swift"])
        #expect(sheet.contains("if let sheet = model.sheet ?? model.lastSheet {"))
        let review = try #require(sources["ReviewView.swift"])
        #expect(review.contains(".sheet(isPresented: Binding(get: { model.sheet != nil }"), "presented on the sheet itself, not the last one")
    }

    @Test("the Docker details the sheet advises on are the catalog's")
    func dockerDetails() throws {
        let source = try rustSource("crates/ddc-core/src/rules/catalog.rs")
        let down = try #require(source.firstMatch(of: /pub const DOCKER_DOWN: &str = "([^"]+)";/))
        #expect(String(down.1) == CleanWording.dockerDown)
        let missing = try #require(source.firstMatch(of: /no_context: "([^"]+)",/))
        #expect(String(missing.1) == CleanWording.dockerNoContext)
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
        // The field that spares the scan after a clean, and its default for an older CLI.
        let saved = try #require((try schema("clean")["properties"] as? [String: Any])?["last_scan_saved"] as? [String: Any])
        #expect(saved["type"] as? String == "boolean" && saved["default"] as? Bool == false)
    }

    @Test("the progress events are the CLI's: version, event names, phases, walk steps, reasons and actions")
    func progressContracts() throws {
        let source = try rustSource("crates/ddc-core/src/progress.rs")
        let version = try #require(source.firstMatch(of: /pub const PROGRESS_SCHEMA_VERSION: u32 = (\d+);/))
        #expect(Int(version.1) == ProgressEvent.schemaVersion)
        let progress = try schema("progress")
        let branches = try #require(progress["oneOf"] as? [[String: Any]])
        let events = branches.compactMap { (($0["properties"] as? [String: Any])?["event"] as? [String: Any])?["const"] as? String }
        #expect(events.count == branches.count)
        #expect(Set(events) == Set(ProgressEvent.names))
        #expect(Set(try values(progress, "Phase")) == Set(ProgressEvent.Phase.allCases.map(\.rawValue)))
        #expect(Set(try values(progress, "WalkStep")) == Set(ProgressEvent.WalkStep.allCases.map(\.rawValue)))
        #expect(Set(try values(progress, "SkipReason", key: "reason")) == Set(SkipReason.tags))
        #expect(Set(try values(progress, "ActionKind")) == Set(ListedAction.allCases.map(\.rawValue)))
    }

    @Test("every enum value the schemas list is one the app decodes")
    func schemaEnums() throws {
        let clean = try schema("clean")
        #expect(Set(try values(clean, "SkipReason", key: "reason")) == Set(SkipReason.tags))
        #expect(SkipReason.tags.count == 22)
        #expect(Set(try values(clean, "EntryLock", key: "reason")) == Set(EntryLock.tags))
        #expect(Set(try values(clean, "ActionKind")) == Set(ListedAction.allCases.map(\.rawValue)))
        let scan = try schema("scan")
        #expect(Set(try values(scan, "RemovalKind")) == Set(ScanReport.Removal.allCases.map(\.rawValue)))
        #expect(Set(try values(scan, "Note")) == Set(ScanReport.Note.allCases.map(\.rawValue)))
        #expect(Set(try values(scan, "RiskClass")) == Set(ScanReport.RiskClass.allCases.map(\.rawValue)))
        #expect(Set(try values(scan, "EntryLock", key: "reason")) == Set(EntryLock.tags))
        #expect(Set(try values(scan, "SkipReason")) == Set(ScanReport.SkipArea.allCases.map(\.rawValue)))
        #expect(Set(try values(scan, "FolderEntryKind")) == ["file", "folder"])
    }

    /// ADR "Platform seam: one platform module per OS with declared
    /// capabilities", §4: the scan, clean and disk reports carry both
    /// fields, required, decoded strictly, in the order the CLI declares.
    @Test("the platform and its capabilities are the CLI's, in every report that carries them")
    func platformContracts() throws {
        for name in ["scan", "clean", "disk"] {
            let s = try schema(name)
            #expect(try values(s, "Platform") == Platform.allCases.map(\.rawValue), "\(name)")
            #expect(try values(s, "Capability") == Capability.allCases.map(\.rawValue), "\(name)")
            let required = try #require(s["required"] as? [String])
            #expect(required.contains("platform") && required.contains("capabilities"), "\(name)")
        }
        let scan = try ScanReport.read(try Fixture.data("scan-report"))
        #expect(scan.platform == .macos && scan.capabilities == Capability.allCases)
        let disk = try DiskReport.read(try Fixture.data("disk-report"))
        #expect(disk.platform == .macos && disk.capabilities == Capability.allCases)
        let clean = try CleanReport.read(try Fixture.data("clean-report"))
        #expect(clean.platform == .macos && clean.capabilities == Capability.allCases)
        let unknown = try Fixture.edited("disk-report") { $0["capabilities"] = ["private-size", "teleport"] }
        #expect(throws: DecodingError.self) { try DiskReport.read(unknown) }
        let other = try Fixture.edited("disk-report") { $0["platform"] = "freebsd" }
        #expect(throws: DecodingError.self) { try DiskReport.read(other) }
        let missing = try Fixture.edited("disk-report") { $0.removeValue(forKey: "capabilities") }
        #expect(throws: DecodingError.self) { try DiskReport.read(missing) }
    }

    @Test("the whole-disk report is the CLI's: version and every enum")
    func diskContracts() throws {
        let source = try rustSource("crates/ddc-core/src/disk/model.rs")
        let version = try #require(source.firstMatch(of: /pub const DISK_SCHEMA_VERSION: u32 = (\d+);/))
        #expect(Int(version.1) == DiskReport.schemaVersion)
        let disk = try schema("disk")
        #expect(Set(try values(disk, "ContainerSource")) == Set(DiskReport.ContainerSource.allCases.map(\.rawValue)))
        #expect(Set(try values(disk, "HomeSource")) == Set(DiskReport.HomeSource.allCases.map(\.rawValue)))
        #expect(Set(try values(disk, "ActionId")) == Set(DiskReport.ActionId.allCases.map(\.rawValue)))
        #expect(Set(try values(disk, "AreaReveal")) == Set(DiskReport.AreaReveal.allCases.map(\.rawValue)))
        #expect(Set(try values(disk, "SimulatorStatus")) == Set(DiskReport.SimulatorStatus.allCases.map(\.rawValue)))
        #expect(Set(try values(disk, "InconsistentCause")) == Set(DiskReport.InconsistentCause.allCases.map(\.rawValue)))
        #expect(Set(try values(disk, "HeldKind")) == Set(DiskReport.HeldKind.allCases.map(\.rawValue)))
    }

    @Test("every user-data rule of the catalog has a sidebar name")
    func userDataNames() throws {
        let userData = try Fixture.rules().rules.filter { $0.class == .userData }.map(\.id)
        #expect(userData.contains("user.downloads"))
        for id in userData { #expect(CleanWording.userDataName(id) != nil, "\(id)") }
        #expect(Set(ReviewItems.userDataOrder).isSuperset(of: userData))
    }

    @Test("every skip reason decodes and reads as text")
    func skipReasonsHaveText() throws {
        for tag in SkipReason.tags {
            let line = #"{"type":"item","path":"/Users/example/x","rule":"cache.npm","class":"safe","action":"delete","prediction":"would-skip","reason":"\#(tag)","pid":7,"name":"node","program":"go","detail":"Docker Desktop is not running or not answering","lock":{"reason":"recent"}}"#
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
        #expect(throws: (any Error).self) {
            try PreflightLine.read(#"{"type":"item","path":"/x","rule":"r","class":"safe","action":"delete","prediction":"would-skip","reason":"entry-locked","lock":{"reason":"new-lock"}}"#)
        }
    }

    // MARK: - Scheduled scans ("Background scan and notifications in the app")

    @Test("the notify decision is the CLI's: version, statuses, reasons and skip causes")
    func notifyContract() throws {
        let source = try rustSource("crates/ddc-core/src/notify.rs")
        let version = try #require(source.firstMatch(of: /pub const NOTIFY_SCHEMA_VERSION: u32 = (\d+);/))
        #expect(Int(version.1) == NotifyReport.schemaVersion)
        let notify = try schema("notify")
        #expect(Set(try values(notify, "NotifyReason")) == Set(NotifyDecision.Reason.allCases.map(\.rawValue)))
        #expect(Set(try values(notify, "SkipCause")) == ["busy"])
        let branches = try #require(notify["oneOf"] as? [[String: Any]])
        let statuses = branches.compactMap { (($0["properties"] as? [String: Any])?["status"] as? [String: Any])?["const"] as? String }
        #expect(Set(statuses) == ["scanned", "skipped"])
    }

    @Test("the 0.1.0 agent is the CLI's: its label, 11 o'clock, and Monday as launchd's Weekday 1")
    func legacyAgentContract() throws {
        let source = try rustSource("crates/ddc-cli/src/launchd.rs")
        let label = try #require(source.firstMatch(of: /pub const LABEL: &str = "([^"]+)";/))
        #expect(String(label.1) == LegacyAgent.label)
        #expect(source.contains("<key>Hour</key><integer>\(LegacyAgent.hour)</integer><key>Minute</key><integer>30</integer>"))
        #expect(source.contains("<key>Weekday</key><integer>1</integer>"))
        let plist = try rustSource("crates/ddc-cli/src/launchd.rs").firstMatch(of: /home\.join\("Library\/LaunchAgents"\)\.join\(format!\("\{LABEL\}\.plist"\)\)/)
        #expect(plist != nil, "the plist path is ~/Library/LaunchAgents/<label>.plist")
        #expect(LegacyAgent.plist(home: URL(filePath: "/Users/example")).path == "/Users/example/Library/LaunchAgents/com.smartandpoint.deepdrivecleaner.plist")
    }

    @Test("no DDCKit source imports UserNotifications or ServiceManagement")
    func noSystemFrameworks() throws {
        let dir = repositoryRoot().appending(path: "app/Packages/DDCKit/Sources/DDCKit")
        let files = try FileManager.default.contentsOfDirectory(atPath: dir.path).filter { $0.hasSuffix(".swift") }
        #expect(!files.isEmpty)
        for file in files {
            let text = try String(contentsOf: dir.appending(path: file), encoding: .utf8)
            #expect(!text.contains(/import\s+(UserNotifications|ServiceManagement)\b/), "\(file)")
        }
    }

    // MARK: - HIG ("HIG pass: keyboard, colour, empty states and menus")

    @Test("buttons, menu items and window titles use title-style capitalization")
    func titleCase() throws {
        var literals: [String] = []
        for text in try appSources().values {
            for match in text.matches(of: /(?:Button|ClosureMenuItem)\("([^"]*)"|withTitle: "([^"]*)"/) {
                literals.append(String(match.1 ?? match.2 ?? ""))
            }
            for line in text.matches(of: /\.title = ([^\n]*)/) {
                literals += line.1.matches(of: /"([^"]*)"/).map { String($0.1) }
            }
        }
        // Words that stay lowercase inside a title.
        let minor: Set<String> = ["a", "an", "and", "as", "at", "but", "by", "for", "from", "in", "into", "nor", "of", "on", "or", "the", "to", "with"]
        // A file name, not a title.
        let exceptions: Set<String> = ["Show app.log in Finder"]
        let failing = literals.filter { literal in
            guard !exceptions.contains(literal) else { return false }
            let words = literal.replacing(/\\\(.*\)/, with: "")
                .split(separator: " ")
                .map { $0.trimmingCharacters(in: CharacterSet(charactersIn: "…()").union(.punctuationCharacters)) }
                .filter { !$0.isEmpty }
            return words.enumerated().contains { index, word in
                guard word.first?.isLowercase == true else { return false }
                return index == 0 || index == words.count - 1 || !minor.contains(word)
            }
        }
        #expect(!literals.isEmpty)
        #expect(failing.isEmpty, "not in title case: \(failing)")
    }

    @Test("the accent is the system's, and warnings use the class ink")
    func colours() throws {
        let sources = try appSources()
        func files(_ pattern: Regex<Substring>) -> Set<String> {
            Set(sources.filter { $0.value.contains(pattern) }.map(\.key))
        }
        #expect(files(/PopoverView\.accent\b|StatusIcon\.accent\b/).isEmpty, "the AccentColor asset is the accent")
        #expect(files(/srgbRed: 0x24\b|red: 0x24 \/ 255/).isEmpty, "the brand blue lives in the AccentColor asset only")
        #expect(files(/foregroundStyle\([^\n]*ReportClassStyle\.review\b/).isEmpty, "warning text uses ReportClassStyle.ink(.review)")
        let sheet = try #require(sources["ConfirmSheet.swift"])
        #expect(sheet.contains(/Button\("Stop"\)[^\n]*\n\s*\.keyboardShortcut\(\.cancelAction\)/), "Esc stops a clean")
    }

    // MARK: - Parts

    @Test("only the Whole disk view runs ddc disk: never the scheduled path, never on quit")
    func diskCallers() throws {
        let sources = try appSources()
        func files(_ pattern: Regex<Substring>) -> Set<String> {
            Set(sources.filter { $0.value.contains(pattern) }.map(\.key))
        }
        // `WalkStep.disk` exists too: the patterns name a run and its argument.
        #expect(files(/run\(\s*\.disk\b/) == ["DiskModel.swift"])
        #expect(files(/"disk"/) == ["DiskModel.swift", "AgentsModel.swift", "DiskFreeRun.swift", "BrewPackagesModel.swift"])
        #expect(files(/\.scanDisk\(/) == ["DiskView.swift"], "the Scan Whole Disk button is the one caller")
        for file in ["ScheduledWork.swift", "PopoverModel.swift"] {
            #expect(!(try #require(sources[file])).contains(/DiskModel|scanDisk|\.disk\b/), "\(file) never runs a whole-disk scan")
        }
        let delegate = try #require(sources["AppDelegate.swift"])
        #expect(!delegate.contains(/scanDisk|disk\??\.stop\(/), "the app never starts a whole-disk scan on its own, nor stops one on quit")
        for name in ["applicationShouldTerminate", "applicationWillTerminate"] {
            let body = try #require(delegate.firstMatch(of: Regex("func \(name)[\\s\\S]*?\n    }\n")))
            #expect(!String(body.0).localizedCaseInsensitiveContains("disk"), "\(name) leaves a whole-disk scan running")
        }
    }

    @Test("only the Whole disk view lists the coding agents, and nothing in the section removes anything")
    func agentsCallers() throws {
        let sources = try appSources()
        func files(_ pattern: Regex<Substring>) -> Set<String> {
            Set(sources.filter { $0.value.contains(pattern) }.map(\.key))
        }
        #expect(files(/run\(\s*\.diskAgents\b/) == ["AgentsModel.swift"])
        #expect(files(/"agents"/) == ["AgentsModel.swift"])
        #expect(files(/agents\??\.refresh\(/) == ["DiskView.swift", "DiskWindowController.swift", "AgentsSection.swift"], "the view appearing, the window shown again, and the Refresh button")
        for file in ["ScheduledWork.swift", "PopoverModel.swift", "AppDelegate.swift"] {
            #expect(!(try #require(sources[file])).contains(/diskAgents|agents\??\.refresh|AgentsModel\.(fast|sizes)Arguments/), "\(file) never lists the coding agents")
        }
        for file in ["AgentsModel.swift", "AgentsSummary.swift", "AgentsReport.swift", "AgentsSection.swift"] {
            let source = try #require(sources[file])
            #expect(!source.contains(/\.clean\b|"clean"|\.trash|removeItem|cleanConfirmedItems/), "\(file) removes nothing")
        }
    }

    @Test("only the sheet's confirm button runs disk free, and nothing scheduled does")
    func freeCallers() throws {
        let sources = try appSources()
        func files(_ pattern: Regex<Substring>) -> Set<String> {
            Set(sources.filter { $0.value.contains(pattern) }.map(\.key))
        }
        #expect(files(/func freeConfirmedActions\(/) == ["DiskFreeRun.swift"])
        #expect(files(/\.freeConfirmedActions\(/) == ["AreaActionSheet.swift"], "the sheet's confirm button is the one caller")
        #expect(files(/\.diskFree(?:DryRun)?\b/) == ["DiskFreeRun.swift"])
        #expect(files(/"free"/) == ["DiskFreeRun.swift"])
        #expect(files(/\.previewAction\(/) == ["DiskView.swift", "BrewPackagesSheet.swift"], "a row's button, or a formula's, opens the sheet")
        #expect(files(/func previewChecked\(/) == ["DiskFreeRun.swift"])
        #expect(files(/\.previewChecked\(/) == ["DiskView.swift"], "the checked bar's button is the one caller")
        let view = try #require(sources["DiskView.swift"])
        #expect(view.matches(of: /\.previewChecked\(/).count == 1)
        #expect(view.matches(of: /\.sheet\(/).count == 1, "one sheet: the list uses the area action sheet")
        // The one real run is the confirm button's.
        let run = try #require(sources["DiskFreeRun.swift"])
        #expect(run.matches(of: /runFree\([^)]*dryRun: false\)/).count == 1)
        let confirmed = try #require(run.firstMatch(of: /func freeConfirmedActions\(\)[\s\S]*?\n    }\n/))
        #expect(confirmed.output.contains(/runFree\([^)]*dryRun: false\)/))
        for file in ["ScheduledWork.swift", "PopoverModel.swift", "AppDelegate.swift", "SchedulerAgent.swift", "ScanDue.swift"] {
            #expect(!(try #require(sources[file])).contains(/diskFree|previewAction|previewChecked|freeConfirmedActions|"free"/), "\(file) never frees an area")
        }
    }

    @Test("the disk free report and list are the CLI's: version and every enum")
    func freeContracts() throws {
        let source = try rustSource("crates/ddc-core/src/free/model.rs")
        let version = try #require(source.firstMatch(of: /pub const FREE_SCHEMA_VERSION: u32 = (\d+);/))
        #expect(Int(version.1) == FreeReport.schemaVersion)
        #expect(Int(version.1) == AreaActionList.schemaVersion)
        let free = try schema("disk-free")
        #expect(Set(try values(free, "ReportEdit")) == Set(FreeReport.ReportEdit.allCases.map(\.rawValue)))
        #expect(Set(try values(free, "Refusal")) == Set(FreeReport.Refusal.allCases.map(\.rawValue)))
        #expect(Set(try values(free, "ActionResult", key: "status")) == Set(FreeReport.Outcome.tags))
        #expect(Set(try values(free, "ActionId")) == Set(DiskReport.ActionId.allCases.map(\.rawValue)))
        #expect(try rustSource("crates/ddc-core/src/free/mod.rs").contains("pub const STOPPED: &str = \"\(FreeWording.stopped)\";"))
        #expect(try rustSource("crates/ddc-core/src/free/mod.rs").contains("pub const NOT_STARTED: &str = \"\(FreeWording.notStarted)\";"))
    }

    @Test("only the Homebrew row lists the packages, and the listing removes nothing itself")
    func brewPackagesCallers() throws {
        let sources = try appSources()
        func files(_ pattern: Regex<Substring>) -> Set<String> {
            Set(sources.filter { $0.value.contains(pattern) }.map(\.key))
        }
        #expect(files(/run\(\s*\.brewPackages\b/) == ["BrewPackagesModel.swift"])
        #expect(files(/"brew-packages"/) == ["BrewPackagesModel.swift"])
        #expect(files(/brewPackages\.open\(/) == ["DiskView.swift"], "the Homebrew row's button is the one caller")
        for file in ["ScheduledWork.swift", "PopoverModel.swift", "AppDelegate.swift", "SchedulerAgent.swift", "ScanDue.swift"] {
            #expect(!(try #require(sources[file])).contains(/brewPackages|BrewPackages/), "\(file) never lists the Homebrew packages")
        }
        for file in ["BrewPackagesModel.swift", "BrewPackages.swift", "BrewPackagesSheet.swift"] {
            let source = try #require(sources[file])
            #expect(!source.contains(/\.diskFree|"free"|freeConfirmedActions|\.trash|removeItem/), "\(file) removes nothing itself")
        }
    }

    @Test("the Homebrew packages listing is the CLI's: version and every enum")
    func brewPackagesContracts() throws {
        let source = try rustSource("crates/ddc-core/src/brew/model.rs")
        let version = try #require(source.firstMatch(of: /pub const BREW_PACKAGES_SCHEMA_VERSION: u32 = (\d+);/))
        #expect(Int(version.1) == BrewPackages.schemaVersion)
        let packages = try schema("brew-packages")
        #expect(Set(try values(packages, "ServicesState")) == Set(BrewPackages.ServicesState.allCases.map(\.rawValue)))
        #expect(Set(try values(packages, "Service")) == Set(BrewPackages.Service.allCases.map(\.rawValue)))
        #expect(Set(try values(packages, "NotOffered")) == Set(BrewPackages.NotOffered.allCases.map(\.rawValue)))
    }

    @Test("the coding agents report is the CLI's: version and every enum")
    func agentsContracts() throws {
        let source = try rustSource("crates/ddc-core/src/agents/model.rs")
        let version = try #require(source.firstMatch(of: /pub const AGENTS_SCHEMA_VERSION: u32 = (\d+);/))
        #expect(Int(version.1) == AgentsReport.schemaVersion)
        let agents = try schema("agents")
        #expect(Set(try values(agents, "HarnessId")) == Set(AgentsReport.HarnessID.allCases.map(\.rawValue)))
        #expect(Set(try values(agents, "RunningKind")) == Set(AgentsReport.RunningKind.allCases.map(\.rawValue)))
        #expect(Set(try values(agents, "RecordsState")) == Set(AgentsReport.RecordsState.allCases.map(\.rawValue)))
        #expect(Set(try values(agents, "UnknownReason")) == Set(AgentsReport.UnknownReason.allCases.map(\.rawValue)))
        #expect(Set(try values(agents, "SessionState")) == Set(AgentsReport.SessionState.allCases.map(\.rawValue)))
    }

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
