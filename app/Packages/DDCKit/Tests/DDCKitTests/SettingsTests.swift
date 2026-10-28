import Foundation
import Testing
@testable import DDCKit

/// Story "Settings: folders, types, notifications": the file the Settings
/// window writes, the panes' logic, and what the popover does with them.
/// Temporary data directories, homes and defaults suites only.
@Suite("Settings", .timeLimit(.minutes(1)))
@MainActor
struct SettingsTests {
    let data: TempDataDirectory
    let defaults: TempDefaults

    init() throws {
        data = try TempDataDirectory()
        defaults = try TempDefaults()
    }

    /// The settings model over a popover model; `changed` is the store's hook.
    private func settings(
        _ popover: PopoverModel,
        home: String = "/Users/example",
        changed: @escaping @MainActor () -> Void = {},
        clock: @escaping @MainActor () -> Date = { Date(timeIntervalSince1970: 1_800_000_000) }
    ) -> SettingsModel {
        SettingsModel(
            store: SettingsStore(url: data.directory.settings, changed: changed),
            popover: popover,
            preferences: AppPreferences(defaults: defaults.defaults),
            home: home,
            clock: clock
        )
    }

    /// `settings.json` as a JSON object.
    private func json() throws -> [String: Any] {
        try #require(try JSONSerialization.jsonObject(with: Data(contentsOf: data.directory.settings)) as? [String: Any])
    }

    private func inode(_ url: URL) throws -> UInt64 {
        try #require((try FileManager.default.attributesOfItem(atPath: url.path))[.systemFileNumber] as? UInt64)
    }

    // MARK: - The file

    @Test("an edit writes the file atomically, keeps every key it did not edit, the scheduler record included, and passes SharedSettings.read")
    func keepsUneditedKeys() throws {
        try data.write(settings: """
            {"schema_version": 1, "roots": ["/Users/example", "/Volumes/Work"], "excludes": ["/Users/example/keep"],
             "skips": ["/Users/example/huge"], "min_age_days": 30, "min_size": 5, "notify_threshold": 1048576,
             "notify_low_disk": false, "scheduler": {"by": "app", "app": "/Applications/DeepDriveCleaner.app"}}
            """)
        let before = try json()
        let oldInode = try inode(data.directory.settings)
        var changes = 0
        let store = SettingsStore(url: data.directory.settings, changed: { changes += 1 })
        #expect(store.update { $0.disabledRules = ["cache.npm"] })
        #expect(changes == 1)

        let after = try json()
        #expect(after["disabled_rules"] as? [String] == ["cache.npm"])
        for (key, value) in before {
            #expect((after[key] as? NSObject) == (value as? NSObject), "\(key) kept")
        }
        #expect(Set(after.keys) == Set(before.keys).union(["disabled_rules"]))
        #expect(try inode(data.directory.settings) != oldInode, "written to a new file and renamed over the old one")
        let read = try SharedSettings.read(at: data.directory.settings)
        #expect(read.disabledRules == ["cache.npm"] && read.minSize == 5 && read.roots == ["/Users/example", "/Volumes/Work"])

        // An edit that changes nothing writes nothing.
        let unchanged = try inode(data.directory.settings)
        #expect(store.update { $0.disabledRules = ["cache.npm"] })
        #expect(try inode(data.directory.settings) == unchanged && changes == 1)
    }

    @Test("empty lists and absent values are never written: no skips key, no null")
    func omitsEmpty() throws {
        let store = SettingsStore(url: data.directory.settings)
        #expect(store.update { $0.skips = ["/Users/example/huge"]; $0.excludes = ["/Users/example/keep"] })
        #expect(try json()["skips"] as? [String] == ["/Users/example/huge"])
        #expect(store.update { $0.skips = []; $0.excludes = [] })
        let text = try String(contentsOf: data.directory.settings, encoding: .utf8)
        #expect(try json().keys.sorted() == ["schema_version"])
        #expect(!text.contains("null") && !text.contains("[]"))
        #expect(throws: Never.self) { try SharedSettings.read(at: data.directory.settings) }
    }

    @Test("a malformed or newer file is never overwritten, and the pane says why")
    func neverOverwritesUnreadable() throws {
        let popover = try makeModel(runner: nil, data: data, defaults: defaults)
        for text in [#"{"schema_version": 1, "show_badge": true}"#, #"{"schema_version": 1, "roots": ["Work"]}"#, "not json", #"{"schema_version": 2, "skips": 7}"#] {
            try data.write(settings: text)
            let model = settings(popover)
            #expect(model.problem != nil, "\(text)")
            model.setLowDisk(false)
            model.setType(TypesRow(label: "x", help: nil, classes: [.safe], ids: ["cache.npm"], isOn: true), on: false)
            model.addExclusion("/Users/example/keep", mode: .skip)
            #expect(try String(contentsOf: data.directory.settings, encoding: .utf8) == text, "left as it was")
        }
        #expect(settings(popover).problem?.contains("another version (2)") == true)
        try data.write(settings: "not json")
        #expect(settings(popover).problem?.contains("could not be read (not a JSON object)") == true)
    }

    // MARK: - Types

    @Test("a Types row's chips are the distinct classes of its switchable rules, in class order")
    func typesChips() throws {
        let rows = TypesRows.make(rules: try Fixture.rules(), disabled: [])
        #expect(rows.count == 14, "every group with a switchable rule; not Interrupted cleanups or Your data")
        let row = { (label: String) in rows.first { $0.label == label } }
        #expect(row("Toolchain caches")?.classes == [.safe] && row("Toolchain caches")?.chips == "Safe")
        #expect(row("node_modules")?.classes == [.rebuildable, .review] && row("node_modules")?.chips == "Rebuildable · Review")
        #expect(row("IDE caches")?.chips == "Safe · Review")
        #expect(row("Python environments")?.chips == "Safe · Rebuildable")
        #expect(row("VM and container images")?.ids == ["vm.docker-desktop", "vm.podman", "cache.colima"], "switchable rules only")
        #expect(row("Your data") == nil && row("Interrupted cleanups") == nil)
        #expect(rows.allSatisfy { $0.isOn })
    }

    @Test("switching a group off writes exactly its switchable ids, and on removes them; a partly switched group reads on")
    func typesSwitch() throws {
        let rules = try Fixture.rules()
        let store = SettingsStore(url: data.directory.settings)
        #expect(store.update { $0.disabledRules = ["cache.pip"] })
        let vm = try #require(TypesRows.make(rules: rules, disabled: store.file.disabledRules).first { $0.label == "VM and container images" })
        store.update { TypesRows.set(vm, on: false, in: &$0) }
        #expect(try json()["disabled_rules"] as? [String] == ["cache.pip", "vm.docker-desktop", "vm.podman", "cache.colima"])
        #expect(TypesRows.make(rules: rules, disabled: store.file.disabledRules).first { $0.label == "VM and container images" }?.isOn == false)
        #expect(TypesRows.make(rules: rules, disabled: store.file.disabledRules).first { $0.label == "Package downloads" }?.isOn == true)
        store.update { TypesRows.set(vm, on: true, in: &$0) }
        #expect(try json()["disabled_rules"] as? [String] == ["cache.pip"])
    }

    @Test("after the store's change hook the popover marks a switched-off type's findings ignored, and the minimum size too, with no scan")
    func typesApplyAtOnce() async throws {
        try data.write(lastScan: try Fixture.data("scan-report-groups"))
        let runner = FakeRunner(scan: FakeRunner.done(Data()), rules: FakeRunner.done(try Fixture.data("rules")))
        let popover = try makeModel(runner: runner, data: data, defaults: defaults)
        await popover.start()
        var refreshed: Task<Void, Never>?
        let model = settings(popover, changed: { refreshed = Task { await popover.refresh() } })
        #expect(popover.summary?.ignoredCount == 0)

        let modules = try #require(model.types.first { $0.label == "node_modules" })
        model.setType(modules, on: false)
        await refreshed?.value
        #expect(popover.summary?.ignoredCount == 2, "both node_modules")
        #expect(popover.summary?.topGroups.contains { $0.name == "node_modules" } == false)
        #expect(model.types.first { $0.label == "node_modules" }?.isOn == false)

        model.setType(modules, on: true)
        await refreshed?.value
        model.setMinSizeMB(1)
        await refreshed?.value
        #expect(try json()["min_size"] as? Int == 1 << 20)
        #expect(popover.summary?.ignoredCount == 11, "every finding is under 1 MiB")
        #expect(model.minSizeMB == 1)
        #expect(await runner.count(.scan) == 0)
    }

    // MARK: - Folders

    /// A temporary home with a `Projects` folder, and a folder outside it with a link to it.
    private func home() throws -> (home: String, outside: String, link: String) {
        let home = data.url.appending(path: "home/me")
        let outside = data.url.appending(path: "Work")
        for dir in [home.appending(path: "Projects"), outside] {
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        }
        let link = data.url.appending(path: "work-link")
        try FileManager.default.createSymbolicLink(at: link, withDestinationURL: outside)
        return (home.path, outside.path, link.path)
    }

    @Test("adding an external disk to the default writes the home folder and the disk")
    func addRoot() throws {
        let popover = try makeModel(runner: nil, data: data, defaults: defaults)
        let model = settings(popover)
        #expect(model.folders.roots.map(\.detail) == ["Home folder · default"])
        model.addRoot("/Volumes/Work")
        #expect(model.folderMessage == nil)
        #expect(try json()["roots"] as? [String] == ["/Users/example", "/Volumes/Work"])
        #expect(model.folders.roots.map(\.shown) == ["~", "/Volumes/Work"])
        #expect(model.folders.roots.map(\.detail) == ["Home folder", "External disk"])
    }

    @Test("a folder inside a root, a root around one, the same folder twice or through a symlink, and an exclusion at or above a root are refused with a message")
    func refusals() throws {
        let (home, outside, link) = try home()
        let popover = try makeModel(runner: nil, data: data, defaults: defaults)
        let model = settings(popover, home: home)
        func refused(_ expected: FolderList.Refusal, _ action: () -> Void) {
            let before = try? Data(contentsOf: data.directory.settings)
            action()
            #expect(model.folderMessage == expected.message)
            #expect((try? Data(contentsOf: data.directory.settings)) == before, "nothing written")
        }
        refused(.insideRoot("~/Projects", root: "~")) { model.addRoot("\(home)/Projects") }
        model.addRoot(outside)
        #expect(model.folderMessage == nil)
        refused(.alreadyListed(outside)) { model.addRoot(outside) }
        refused(.sameFolder(link, as: outside)) { model.addRoot(link) }
        refused(.insideRoot("\(outside)/sub", root: outside)) { model.addRoot("\(outside)/sub") }
        refused(.aroundRoot(data.url.path, root: "~")) { model.addRoot(data.url.path) }
        refused(.coversRoot("~", root: "~")) { model.addExclusion(home, mode: .count) }
        refused(.coversRoot(link, root: outside)) { model.addExclusion(link, mode: .skip) }
        refused(.coversRoot(data.url.appending(path: "home").path, root: "~")) { model.addExclusion(data.url.appending(path: "home").path, mode: .skip) }

        model.addExclusion("\(home)/Projects", mode: .skip)
        #expect(model.folderMessage == nil)
        refused(.alreadyListed("~/Projects")) { model.addExclusion("\(home)/Projects", mode: .count) }
        // An exclusion outside every root: a root inside it is refused too.
        let other = data.url.appending(path: "Other").path
        model.addExclusion(other, mode: .count)
        refused(.insideExclusion("\(other)/x", exclusion: other)) { model.addRoot("\(other)/x") }
        #expect(!FolderList.Refusal.lastRoot.message.isEmpty)
    }

    @Test("the last root cannot be removed; removing the disk returns to the default")
    func lastRoot() throws {
        let popover = try makeModel(runner: nil, data: data, defaults: defaults)
        let model = settings(popover)
        #expect(!model.folders.canRemoveRoot)
        model.removeRoot("/Users/example")
        #expect(model.folderMessage == FolderList.Refusal.lastRoot.message)
        #expect(!FileManager.default.fileExists(atPath: data.directory.settings.path))
        model.addRoot("/Volumes/Work")
        #expect(model.folders.canRemoveRoot)
        model.removeRoot("/Volumes/Work")
        #expect(try json()["roots"] == nil, "home only: the default")
        model.addRoot("/Volumes/Work")
        model.removeRoot("/Users/example")
        #expect(try json()["roots"] as? [String] == ["/Volumes/Work"])
        model.removeRoot("/Volumes/Work")
        #expect(model.folderMessage == FolderList.Refusal.lastRoot.message)
    }

    @Test("an exclusion's mode moves its path between excludes and skips")
    func exclusionMode() throws {
        let popover = try makeModel(runner: nil, data: data, defaults: defaults)
        let model = settings(popover)
        model.addExclusion("/Users/example/Projects/client-archive", mode: .count)
        model.addExclusion("/Users/example/Movies/Raw footage", mode: .skip)
        #expect(try json()["excludes"] as? [String] == ["/Users/example/Projects/client-archive"])
        #expect(try json()["skips"] as? [String] == ["/Users/example/Movies/Raw footage"])
        #expect(model.folders.exclusions.map(\.shown) == ["~/Movies/Raw footage", "~/Projects/client-archive"])
        #expect(model.folders.exclusions.map(\.mode) == [.skip, .count])

        model.setMode(.skip, of: "/Users/example/Projects/client-archive")
        #expect(try json()["excludes"] == nil)
        #expect(try json()["skips"] as? [String] == ["/Users/example/Movies/Raw footage", "/Users/example/Projects/client-archive"])
        model.setMode(.count, of: "/Users/example/Movies/Raw footage")
        #expect(try json()["excludes"] as? [String] == ["/Users/example/Movies/Raw footage"])
        model.removeExclusion("/Users/example/Movies/Raw footage")
        model.removeExclusion("/Users/example/Projects/client-archive")
        #expect(try json().keys.sorted() == ["schema_version"])
    }

    @Test("after roots, excludes and skips are edited, Scan now runs the plain scan: no paths, no --exclude, no --skip")
    func scanNowTakesNoFlags() async throws {
        let runner = FakeRunner(scan: FakeRunner.done(try Fixture.data("scan-report-groups")))
        let popover = try makeModel(runner: runner, data: data, defaults: defaults)
        await popover.start()
        let model = settings(popover)
        model.addRoot("/Volumes/Work")
        model.addExclusion("/Users/example/keep", mode: .count)
        model.addExclusion("/Users/example/huge", mode: .skip)
        #expect(try json()["skips"] as? [String] == ["/Users/example/huge"])
        await popover.scanNow()
        #expect(await runner.arguments.filter { $0.first == "scan" } == [["scan", "--json", "--progress", "json"]])
    }

    // MARK: - App preferences

    @Test("the app's own preferences persist in UserDefaults and never in settings.json; the icon switches act at once; Pause is the popover's")
    func preferences() async throws {
        try data.write(lastScan: try Fixture.data("scan-report-groups"))
        try data.write(settings: String(decoding: try Fixture.data("scan-report-groups.settings"), as: UTF8.self))
        let popover = try makeModel(runner: FakeRunner(scan: FakeRunner.done(Data())), data: data, defaults: defaults)
        await popover.start()
        let model = settings(popover)
        #expect(popover.iconState.label == nil, "the size is off by default")

        model.setShowSize(true)
        #expect(popover.iconState.label != nil, "over the 1 MiB threshold: the size shows at once")
        popover.notificationPosted()
        #expect(popover.iconState.mark == .badge)
        model.setShowBadge(false)
        #expect(popover.iconState.mark == .none)
        model.setWorkingHoursOnly(true)
        model.setAfterScheduledScan(.menuBarOnly)
        model.setDeletePermanently(true)
        model.setSchedule(Schedule(enabled: true, repeats: .weekly, weekday: 6, hour: 7))
        model.setPaused(true)
        #expect(popover.paused && defaults.defaults.bool(forKey: "ScansPaused"))
        #expect(model.paused)

        let stored = AppPreferences(defaults: defaults.defaults)
        #expect(stored.showSize && !stored.showBadge && stored.workingHoursOnly && stored.deleteReviewPermanently && stored.scansPaused)
        #expect(stored.afterScheduledScan == .menuBarOnly)
        #expect(stored.schedule == Schedule(enabled: true, repeats: .weekly, weekday: 6, hour: 7))
        for key in ["ShowSizeNextToIcon", "ShowMenuBarBadge", "NotifyWorkingHoursOnly", "AfterScheduledScan", "DeleteReviewItemsPermanently", "ScheduleEnabled", "ScheduleRepeat", "ScheduleWeekday", "ScheduleHour"] {
            #expect(defaults.defaults.object(forKey: key) != nil, "\(key)")
        }
        // A write of the file after all of them holds only the CLI's keys.
        model.setLowDisk(false)
        #expect(Set(try json().keys).isSubset(of: SharedSettings.keys))
        #expect(try json()["notify_low_disk"] as? Bool == false)
    }

    // MARK: - Schedule

    @Test("Scan automatically defaults to on, daily at 10:00; only whole hours of a day can be chosen")
    func scheduleDefaults() throws {
        let schedule = AppPreferences(defaults: defaults.defaults).schedule
        #expect(schedule == Schedule())
        #expect(schedule.enabled && schedule.repeats == .daily && schedule.hour == 10 && schedule.weekday == 2)
        #expect(Schedule.weekdayName(schedule.weekday) == "Monday")
        #expect(Schedule.hours == Array(0...23))
        #expect(Schedule.hours.map(Schedule.hourText).first == "00:00" && Schedule.hourText(10) == "10:00")
        var other = Schedule()
        other.setHour(24)
        #expect(other.hour == 23)
        other.setHour(-1)
        #expect(other.hour == 0)
        // A stored value out of range reads as a valid one.
        defaults.defaults.set(99, forKey: "ScheduleHour")
        defaults.defaults.set(0, forKey: "ScheduleWeekday")
        defaults.defaults.set("hourly", forKey: "ScheduleRepeat")
        let read = AppPreferences(defaults: defaults.defaults).schedule
        #expect(read.hour == 23 && read.weekday == 2 && read.repeats == .daily)
    }

    @Test("the Next scan line is the popover footer's seam; off reads Off…, paused reads Paused…, no date hides it")
    func nextScan() throws {
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        var next: Date? = now.addingTimeInterval(3 * 3600)
        let popover = try makeModel(runner: nil, data: data, defaults: defaults, nextScan: { next }, clock: { now })
        let model = settings(popover, clock: { now })
        #expect(popover.nextScanDate == next)
        #expect(model.nextScan == Schedule().nextScanLine(paused: false, next: next, now: now))
        next = nil
        #expect(model.nextScan == nil, "hidden until the scheduler fills the seam")
        model.setPaused(true)
        #expect(model.nextScan?.hasPrefix("Paused") == true)
        var off = Schedule()
        off.enabled = false
        model.setSchedule(off)
        #expect(model.nextScan?.hasPrefix("Off") == true, "Off wins over Paused")

        let gb = Locale(identifier: "en_GB")
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        calendar.locale = gb
        let at = { (hours: Double) in Schedule().nextScanLine(paused: false, next: now.addingTimeInterval(hours * 3600), now: now, calendar: calendar, locale: gb) }
        // 1_800_000_000 is a Friday, 08:00 UTC.
        #expect(at(2) == "Today at 10:00")
        #expect(at(26) == "Tomorrow at 10:00")
        #expect(at(74) == "Monday at 10:00")
        #expect(at(0) == "Within the hour" && at(-1) == "Within the hour")
    }

    @Test("a schedule change goes to the scheduler seam")
    func schedulerSeam() throws {
        final class Recorder: SchedulerControl {
            var changes: [Schedule] = []
            func scheduleChanged(_ schedule: Schedule) { changes.append(schedule) }
            var registration: SchedulerRegistration? { .requiresApproval }
            var legacyAgentFound: Bool { true }
            func replaceLegacyAgent() {}
        }
        let recorder = Recorder()
        let popover = try makeModel(runner: nil, data: data, defaults: defaults)
        let model = SettingsModel(
            store: SettingsStore(url: data.directory.settings), popover: popover,
            preferences: AppPreferences(defaults: defaults.defaults), scheduler: recorder, home: "/Users/example"
        )
        #expect(model.registration == .requiresApproval && model.legacyAgentFound)
        var weekly = Schedule()
        weekly.repeats = .weekly
        model.setSchedule(weekly)
        model.setSchedule(weekly)
        #expect(recorder.changes == [weekly], "once per change")

        let none = settings(popover)
        #expect(none.registration == nil && !none.legacyAgentFound, "no rows until the scheduler lands")
    }

    // MARK: - Automation

    @Test("the threshold and the low-disk switch write the file; with scheduled scans off the pane says so; the automatic choice is never stored")
    func automation() throws {
        let popover = try makeModel(runner: nil, data: data, defaults: defaults)
        let model = settings(popover)
        #expect(model.thresholdGiB == 10 && model.lowDisk)
        model.setThresholdGiB(25)
        model.setLowDisk(false)
        #expect(try json()["notify_threshold"] as? Int == 25 << 30)
        #expect(try json()["notify_low_disk"] as? Bool == false)
        #expect(model.thresholdGiB == 25 && !model.lowDisk)

        #expect(model.automationNote == nil)
        var off = Schedule()
        off.enabled = false
        model.setSchedule(off)
        #expect(model.automationNote == SettingsModel.automationOff)

        #expect(AfterScheduledScan.allCases == [.notify, .menuBarOnly], "the automatic choice cannot be picked")
        for choice in AfterScheduledScan.allCases {
            model.setAfterScheduledScan(choice)
            #expect(defaults.defaults.string(forKey: "AfterScheduledScan") == choice.rawValue)
        }
        #expect(!(try String(contentsOf: data.directory.settings, encoding: .utf8)).contains("auto"))
        defaults.defaults.set("auto", forKey: "AfterScheduledScan")
        #expect(AppPreferences(defaults: defaults.defaults).afterScheduledScan == .notify)
    }

    // MARK: - Full Disk Access and Not readable

    @Test("the Folders banner and the Permissions row follow the popover's probe")
    func fullDiskAccess() async throws {
        let popover = try makeModel(runner: nil, data: data, defaults: defaults, probeGranted: false)
        await popover.start()
        let model = settings(popover)
        #expect(!model.fullDiskAccess)
        _ = try data.probe(granted: true)
        await popover.refresh()
        #expect(model.fullDiskAccess)
    }

    /// The groups report with `inaccessible` folders under its home.
    private func unreadable(_ paths: [String]) throws -> Data {
        try Fixture.edited("scan-report-groups") { json in
            json["inaccessible"] = paths.map { ["path": "/Users/example/\($0)", "error": "Operation not permitted (os error 1)"] }
        }
    }

    @Test("Not readable is the report's inaccessible folders, one row per prompting area")
    func notReadableAreas() throws {
        let report = try ScanReport.read(unreadable(["Documents/a", "Library/Safari", "Documents/b/c", "Library/Containers/com.x/Data", "Downloads", "Desktopper"]))
        #expect(report.inaccessible.count == 6)
        #expect(NotReadable.areas(of: report) == ["/Users/example/Documents", "/Users/example/Downloads", "/Users/example/Library/Containers"])
        #expect(NotReadable.areas(of: try Fixture.groupsReport()).isEmpty)
    }

    @Test("Not readable changes only with the app's own Scan now; a report read from disk leaves it as it was")
    func notReadableOwnScanOnly() async throws {
        try data.write(lastScan: try unreadable(["Documents/a"]))
        let runner = FakeRunner(scan: FakeRunner.done(try unreadable(["Pictures/x", "Library/Mail/V10"])))
        let popover = try makeModel(runner: runner, data: data, defaults: defaults)
        await popover.start()
        let model = settings(popover)
        #expect(model.notReadable.isEmpty, "the report on disk may be a background scan's")

        await popover.scanNow()
        #expect(model.notReadable.map(\.shown) == ["~/Pictures", "~/Library/Mail"])
        #expect(AppPreferences(defaults: defaults.defaults).notReadable == ["/Users/example/Pictures", "/Users/example/Library/Mail"])

        try data.write(lastScan: try unreadable(["Music/y"]))
        try FileManager.default.setAttributes([.modificationDate: Date.now.addingTimeInterval(60)], ofItemAtPath: data.directory.lastScan.path)
        await popover.refresh()
        #expect(popover.loadedReport?.inaccessible.map(\.path) == ["/Users/example/Music/y"])
        #expect(model.notReadable.map(\.shown) == ["~/Pictures", "~/Library/Mail"])
        let relaunched = try makeModel(runner: nil, data: data, defaults: defaults)
        #expect(relaunched.notReadable == ["/Users/example/Pictures", "/Users/example/Library/Mail"], "kept across launches")
    }

    // MARK: - Review

    @Test("a report with not_scanned gives Review its Not scanned line, and a report without it gives none")
    func notScannedLine() throws {
        #expect(try ReviewFixture.items().notScanned == nil)
        #expect(try Fixture.groupsReport().notScanned.isEmpty)
        let skipped = try Fixture.edited("scan-report-groups") { json in
            json["not_scanned"] = ["/Users/example/Projects/client-archive", "/Users/example/Movies/Raw footage"]
            json["skipped"] = [["path": "/Users/example/Projects/client-archive", "reason": "excluded"]]
        }
        let items = try ReviewFixture.items(report: skipped)
        #expect(items.notScanned == "Not scanned: ~/Projects/client-archive and ~/Movies/Raw footage (Skip entirely in Settings)")
        #expect(!items.backgroundSkipped, "the user's skip is not a background scan's")
    }

    // MARK: - General

    @Test("the command-line tool row names the Homebrew link into this bundle, else the bundled helper")
    func commandLineTool() throws {
        let bundle = data.url.appending(path: "DeepDriveCleaner.app")
        let helper = bundle.appending(path: HelperLocator.relativePath)
        try FileManager.default.createDirectory(at: helper.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data().write(to: helper)
        let elsewhere = data.url.appending(path: "other-ddc")
        try Data().write(to: elsewhere)
        let foreign = data.url.appending(path: "foreign").path
        let ours = data.url.appending(path: "ours").path
        try FileManager.default.createSymbolicLink(atPath: foreign, withDestinationPath: elsewhere.path)
        try FileManager.default.createSymbolicLink(atPath: ours, withDestinationPath: helper.path)

        #expect(HelperLocator.commandLineTool(bundleURL: bundle, candidates: [foreign, ours]) == ours)
        #expect(HelperLocator.commandLineTool(bundleURL: bundle, candidates: [foreign, "/nonexistent/ddc"]) == helper.path)
        #expect(HelperLocator.linkCandidates == ["/opt/homebrew/bin/ddc", "/usr/local/bin/ddc"])
    }
}
