import Foundation
import Testing
@testable import DDCKit

/// Story "Background scan and notifications in the app": the due function,
/// the tick, the notifications and their answers, the scheduler agent and
/// the migration from 0.1.0. Fakes only: no `SMAppService`, no
/// `UNUserNotificationCenter`, no real `notify ack|snooze` or `schedule
/// uninstall`; temporary data directories, homes and defaults suites; the
/// calendar is UTC.
@Suite("Scheduled scans", .timeLimit(.minutes(1)))
@MainActor
struct ScheduledScanTests {
    let data: TempDataDirectory
    let defaults: TempDefaults
    let calendar: Calendar

    init() throws {
        data = try TempDataDirectory()
        defaults = try TempDefaults()
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = try #require(TimeZone(identifier: "UTC"))
        calendar.locale = Locale(identifier: "en_GB")
        self.calendar = calendar
    }

    /// 2026-10-19 is a Monday.
    private func at(_ day: Int, _ hour: Int, _ minute: Int = 0) -> Date {
        calendar.date(from: DateComponents(year: 2026, month: 10, day: day, hour: hour, minute: minute))!
    }

    private func decision(notify: Bool = true, reason: String = "threshold", bytes: UInt64 = 3_000_000) -> Data {
        let reasonJSON = notify ? "\"\(reason)\"" : "null"
        return Data(#"{"schema_version": 1, "status": "scanned", "notify": \#(notify), "reason": \#(reasonJSON), "volume": "/System/Volumes/Data", "reclaimable": \#(bytes), "free": 20000000000, "total": 500000000000}"#.utf8)
    }

    /// The popover, the tick and its fakes over the groups report.
    @MainActor
    private final class Rig {
        let runner: FakeRunner
        let popover: PopoverModel
        let preferences: AppPreferences
        let notifications = FakeNotifications()
        let clock: Clock
        var legacy = false
        var updateWaiting = false
        var work: ScheduledWork!

        init(_ test: ScheduledScanTests, now: Date) throws {
            try test.data.write(lastScan: try Fixture.data("scan-report-groups"))
            runner = FakeRunner(scan: FakeRunner.done(try Fixture.data("scan-report-groups")), rules: FakeRunner.done(try Fixture.data("rules")))
            let clock = Clock(now)
            self.clock = clock
            preferences = AppPreferences(defaults: test.defaults.defaults)
            var work: ScheduledWork?
            popover = try makeModel(runner: runner, data: test.data, defaults: test.defaults, nextScan: { work?.next() ?? .off }, clock: { clock.now })
            self.work = ScheduledWork(
                popover: popover, preferences: preferences, runner: runner, notifications: notifications,
                legacyAgentFound: { [unowned self] in self.legacy }, updateWaiting: { [unowned self] in self.updateWaiting },
                clock: { clock.now }, calendar: test.calendar
            )
            work = self.work
        }

        func reply(_ scan: Data) async {
            await runner.reply(.backgroundScan, FakeRunner.done(scan))
        }

        func acks() async -> [[String]] {
            await runner.arguments(of: .notify).filter { $0.prefix(2) == ["notify", "ack"] }
        }
    }

    // MARK: - The due function

    @Test("daily and weekly: due at and after the slot, not before; a slot missed while asleep is due; a new install's first tick is due")
    func dueFunction() throws {
        var inputs = ScanDue.Inputs(schedule: Schedule(), lastScheduled: at(19, 10), reminder: nil, paused: false, legacyAgentFound: false)
        let due = { (now: Date) in ScanDue.due(inputs, now: now, calendar: calendar) }
        #expect(due(at(20, 9, 59)) == nil)
        #expect(ScanDue.next(inputs, now: at(20, 9), calendar: calendar) == .at(at(20, 10)))
        #expect(due(at(20, 10)) == .slot)
        #expect(due(at(20, 15)) == .slot)
        #expect(due(at(22, 8)) == .slot, "asleep from Tuesday to Thursday morning: Wednesday's slot is due")
        #expect(ScanDue.next(inputs, now: at(22, 8), calendar: calendar) == .at(at(21, 10)), "a date in the past: within the hour")
        inputs.lastScheduled = at(20, 10, 2)
        #expect(due(at(20, 11)) == nil, "today's slot was scanned")

        inputs.lastScheduled = nil
        #expect(due(at(20, 3)) == .slot, "a new install: the first tick is due")

        inputs.schedule = Schedule(enabled: true, repeats: .weekly, weekday: 2, hour: 10)
        inputs.lastScheduled = at(19, 10)
        #expect(due(at(21, 12)) == nil)
        #expect(ScanDue.next(inputs, now: at(21, 12), calendar: calendar) == .at(at(26, 10)))
        #expect(due(at(26, 9)) == nil)
        #expect(due(at(26, 10)) == .slot)
        #expect(due(at(26, 15)) == .slot, "weekly: after the slot, still due")
        #expect(due(at(28, 8)) == .slot, "weekly: a slot missed while asleep")

        inputs.paused = true
        #expect(due(at(26, 10)) == nil, "nothing while paused")
        inputs.paused = false
        inputs.schedule.enabled = false
        #expect(due(at(26, 10)) == nil && ScanDue.next(inputs, now: at(26, 10), calendar: calendar) == .off)
    }

    @Test("a manual scan moves neither the slot nor today's scheduled scan; the footer shows the due function's next slot")
    func manualScanKeepsTheSlot() async throws {
        let rig = try Rig(self, now: at(20, 9))
        rig.preferences.setLastScheduledScan(at(19, 10))
        #expect(rig.popover.nextScanValue == .at(at(20, 10)))
        #expect(rig.popover.nextScanText == Wording.nextScan(.at(at(20, 10)), now: at(20, 9)))
        await rig.popover.start()
        await rig.popover.scanNow()
        #expect(await rig.runner.count(.scan) == 1)
        #expect(rig.preferences.lastScheduledScan == at(19, 10), "a Scan now is not a scheduled scan")
        #expect(rig.popover.nextScanValue == .at(at(20, 10)))

        rig.clock.now = at(20, 10)
        await rig.reply(decision())
        #expect(rig.work.due() == .slot, "today's scheduled scan still runs")
        #expect(await rig.work.tick() == .scanned)
        #expect(rig.notifications.posted.count == 1, "and may notify")
        #expect(await rig.runner.arguments(of: .backgroundScan) == [["scan", "--background", "--notify", "json"]])
    }

    @Test("with a weekly schedule, Remind Me Tomorrow adds one reminder scan at the first tick after the snooze; the weekly slot stays")
    func weeklyReminder() async throws {
        let rig = try Rig(self, now: at(19, 10, 5))
        rig.preferences.setSchedule(Schedule(enabled: true, repeats: .weekly, weekday: 2, hour: 10))
        rig.preferences.setLastScheduledScan(at(19, 10))
        await rig.work.remindTomorrow()
        #expect(rig.preferences.reminderScan == at(20, 10))
        #expect(await rig.runner.arguments(of: .notify) == [["notify", "snooze", "--until", "2026-10-20T10:00:00Z"]])

        rig.clock.now = at(20, 9)
        #expect(rig.work.due() == nil)
        #expect(rig.work.next() == .at(at(20, 10)), "the reminder comes before next Monday")
        rig.clock.now = at(20, 10)
        #expect(rig.work.due() == .reminder)
        await rig.reply(decision(notify: false))
        #expect(await rig.work.tick() == .scanned)
        #expect(rig.preferences.reminderScan == nil)
        rig.clock.now = at(20, 11)
        #expect(rig.work.due() == nil, "one reminder scan only")
        #expect(rig.work.next() == .at(at(26, 10)), "the weekly slot stays where it was")
    }

    // MARK: - Notifications

    @Test("the notification names the amount and at most three groups; a low-disk one names the volume")
    func notificationText() throws {
        let threshold = NotifyDecision(notify: true, reason: .threshold, volume: "/System/Volumes/Data", reclaimable: 14_495_514_624)
        let groups = ["node_modules", "Package downloads", "Xcode builds", "IDE caches", "ML models"]
        let text = NotificationText.make(threshold, groups: groups)
        #expect(text.title == "13.5 GiB can be freed")
        #expect(text.body == "node_modules, Package downloads and Xcode builds. Nothing is removed until you review them.")
        #expect(!text.body.contains("IDE caches"))
        #expect(NotificationText.make(threshold, groups: []).body == "Caches and idle project folders. Nothing is removed until you review them.")

        let low = NotifyDecision(notify: true, reason: .lowDisk, volume: "/System/Volumes/Data", reclaimable: 14_495_514_624, free: 40 << 30, total: 500 << 30)
        let lowText = NotificationText.make(low, groups: ["node_modules"])
        #expect(lowText.title == "The startup disk is 92% full")
        #expect(lowText.body == "40.0 GiB free. 13.5 GiB in node_modules can be freed.")
        let work = NotificationText.make(NotifyDecision(notify: true, reason: .lowDisk, volume: "/Volumes/Work", reclaimable: 1 << 30), groups: [])
        #expect(work.title == "Work is almost full")
        for text in [text, lowText, work] {
            #expect(!text.body.localizedCaseInsensitiveContains("clean"), "never an offer to clean")
        }

        // From a recorded decision, as the tick builds it.
        let report = try NotifyReport.read(decision(bytes: 2_666_496))
        guard case .scanned(let recorded) = report.status else { Issue.record("not scanned"); return }
        let summary = ReportSummary.make(report: try Fixture.groupsReport(), settings: try Fixture.groupsSettings(), rules: try Fixture.rules(), top: 3)
        let built = NotificationText.make(recorded, groups: summary.topGroups.map(\.name))
        #expect(built.title == "2.5 MiB can be freed")
        #expect(built.body.hasPrefix("Package downloads, node_modules and Python environments."))
    }

    @Test("a posted notification acknowledges the decision's amount and sets the badge; denied permission drops it with no ack, badge set (B8)")
    func ackAndDenied() async throws {
        let rig = try Rig(self, now: at(20, 10))
        await rig.reply(decision(bytes: 7_340_032))
        rig.preferences.setLastScheduledScan(at(19, 10))
        #expect(await rig.work.tick() == .scanned)
        #expect(rig.notifications.posted.map(\.title) == ["7.0 MiB can be freed"])
        #expect(await rig.acks() == [["notify", "ack", "--bytes", "7340032"]])
        #expect(rig.popover.badgePending)
        #expect(rig.preferences.lastScheduledScan == at(20, 10))
        #expect(rig.preferences.pendingNotification == nil)

        let denied = try Rig(self, now: at(21, 10))
        denied.preferences.clearBadge()
        denied.notifications.status = .denied
        await denied.reply(decision())
        #expect(await denied.work.tick() == .scanned)
        #expect(denied.notifications.posted.isEmpty && denied.notifications.requests == 0)
        #expect(await denied.acks().isEmpty)
        #expect(denied.preferences.pendingNotification == nil, "dropped: the next scan decides again")
        #expect(denied.popover.badgePending && denied.preferences.badgePending)
    }

    @Test("a decision at 22:00 with working hours on waits, a newer one replaces it, and it posts and acknowledges at 09:00 on the next working day")
    func workingHours() async throws {
        let rig = try Rig(self, now: at(19, 22))
        rig.preferences.setWorkingHoursOnly(true)
        rig.preferences.setSchedule(Schedule(enabled: true, repeats: .daily, weekday: 2, hour: 22))
        rig.preferences.setLastScheduledScan(at(18, 22))
        await rig.reply(decision(bytes: 5 << 20))
        #expect(await rig.work.tick() == .scanned)
        #expect(rig.notifications.posted.isEmpty)
        #expect(await rig.acks().isEmpty)
        #expect(rig.preferences.pendingNotification?.bytes == 5 << 20, "kept as pending")

        // The user moves the schedule to 02:00: a scan before 09:00 decides again.
        rig.preferences.setSchedule(Schedule(enabled: true, repeats: .daily, weekday: 2, hour: 2))
        rig.clock.now = at(20, 2)
        await rig.reply(decision(bytes: 6 << 20))
        #expect(await rig.work.tick() == .scanned)
        #expect(rig.preferences.pendingNotification?.bytes == 6 << 20, "a newer decision replaces it")
        #expect(rig.notifications.posted.isEmpty)

        rig.clock.now = at(20, 8)
        #expect(await rig.work.tick() == .notDue)
        #expect(rig.notifications.posted.isEmpty)
        rig.clock.now = at(20, 9)
        #expect(await rig.work.tick() == .notDue, "no scan: the pending one posts on its own")
        #expect(rig.notifications.posted.map(\.title) == ["6.0 MiB can be freed"])
        #expect(await rig.acks() == [["notify", "ack", "--bytes", String(6 << 20)]])
        #expect(rig.preferences.pendingNotification == nil)

        #expect(ScanDue.isWorkingHours(at(23, 18, 59), calendar: calendar))
        #expect(!ScanDue.isWorkingHours(at(23, 19), calendar: calendar))
        #expect(!ScanDue.isWorkingHours(at(24, 10), calendar: calendar), "Saturday")
    }

    @Test("a scheduled run that finds permission not asked requests nothing, sets the flag, drops the decision and sends no ack; the popover then offers it; a normal launch after the first scan asks once")
    func permissionNotDetermined() async throws {
        let rig = try Rig(self, now: at(20, 10))
        rig.notifications.status = .notDetermined
        await rig.reply(decision())
        #expect(await rig.work.tick() == .scanned)
        #expect(rig.notifications.requests == 0 && rig.notifications.posted.isEmpty)
        #expect(await rig.acks().isEmpty)
        #expect(rig.preferences.notificationPermissionPending)
        #expect(rig.preferences.pendingNotification == nil, "dropped")

        await rig.popover.refresh()
        let offer = try #require(rig.popover.notices.first { $0.id == PopoverModel.permissionNotice })
        #expect(offer.action == .allowNotifications)
        var performed: [PopoverModel.Notice.Action] = []
        rig.popover.onNoticeAction = { performed.append($0) }
        rig.popover.perform(.allowNotifications)
        #expect(performed == [.allowNotifications])
        await rig.work.allowNotifications()
        #expect(rig.notifications.requests == 1)
        #expect(!rig.popover.notices.contains { $0.id == PopoverModel.permissionNotice })

        // A normal launch: never before the first scan, then once.
        let fresh = try TempDefaults()
        let other = try TempDataDirectory()
        let notifications = FakeNotifications()
        notifications.status = .notDetermined
        let popover = try makeModel(runner: nil, data: other, defaults: fresh)
        await popover.start()
        let work = ScheduledWork(popover: popover, preferences: AppPreferences(defaults: fresh.defaults), runner: nil, notifications: notifications, legacyAgentFound: { false })
        await work.askPermissionOnce()
        #expect(notifications.requests == 0, "no scan yet")
        try other.write(lastScan: try Fixture.data("scan-report-groups"))
        _ = await popover.reloadReport()
        notifications.status = .notDetermined
        await work.askPermissionOnce()
        notifications.status = .notDetermined
        await work.askPermissionOnce()
        #expect(notifications.requests == 1, "once")
    }

    @Test("with Only show it in the menu bar, a scan over the threshold posts nothing, sends no ack and sets the badge (B3)")
    func menuBarOnly() async throws {
        let rig = try Rig(self, now: at(20, 10))
        rig.preferences.setAfterScheduledScan(.menuBarOnly)
        await rig.reply(decision())
        #expect(await rig.work.tick() == .scanned)
        #expect(rig.notifications.posted.isEmpty)
        #expect(await rig.acks().isEmpty)
        #expect(rig.popover.badgePending)
        #expect(rig.popover.iconState.mark == .badge)
        rig.popover.setShowBadge(false)
        #expect(rig.popover.iconState.mark == .none, "subject to Show a badge")
    }

    @Test("a background run answered skipped/busy posts nothing, and the scan stays due")
    func busy() async throws {
        let rig = try Rig(self, now: at(20, 10))
        rig.preferences.setLastScheduledScan(at(19, 10))
        await rig.reply(Data(#"{"schema_version": 1, "status": "skipped", "reason": "busy"}"#.utf8))
        #expect(await rig.work.tick() == .busy)
        #expect(rig.notifications.posted.isEmpty)
        #expect(await rig.acks().isEmpty)
        #expect(rig.preferences.lastScheduledScan == at(19, 10))
        #expect(rig.work.due() == .slot)
        #expect(rig.popover.scan == nil)

        await rig.runner.reply(.backgroundScan, CLIRunner.Result(outcome: .busy, stdout: Data(), stderrTail: []))
        #expect(await rig.work.tick() == .busy)
        #expect(rig.work.due() == .slot)
    }

    /// Story "Sparkle updates: check daily, Update and Relaunch, and the cask's auto_updates".
    @Test("a tick starts no scan while an update's install waits, and scans again after")
    func updateWaiting() async throws {
        let rig = try Rig(self, now: at(20, 10))
        rig.preferences.setLastScheduledScan(at(19, 10))
        rig.updateWaiting = true
        #expect(await rig.work.tick() == .updateWaiting)
        #expect(await rig.runner.count(.backgroundScan) == 0)
        #expect(rig.preferences.lastScheduledScan == at(19, 10))
        #expect(rig.work.due() == .slot)

        rig.updateWaiting = false
        await rig.reply(decision())
        #expect(await rig.work.tick() == .scanned)
        #expect(await rig.runner.count(.backgroundScan) == 1)
        #expect(rig.preferences.lastScheduledScan == at(20, 10))
    }

    @Test("a due date at or before now reads within the hour; paused, nothing is due and Resume reads within the hour; the legacy plist makes nothing due")
    func footerWording() async throws {
        #expect(Wording.nextScan(.at(at(20, 10)), now: at(20, 10)) == "Next scan within the hour")
        #expect(Wording.nextScan(.at(at(20, 8)), now: at(20, 10)) == "Next scan within the hour")
        #expect(Wording.nextScan(.replaceOldScanFirst, now: at(20, 10)) == "Replace the old background scan first")

        let rig = try Rig(self, now: at(20, 12))
        rig.preferences.setLastScheduledScan(at(19, 10))
        rig.popover.setPaused(true)
        #expect(rig.work.due() == nil)
        #expect(await rig.work.tick() == .notDue)
        #expect(await rig.runner.count(.backgroundScan) == 0)
        rig.popover.setPaused(false)
        #expect(rig.popover.nextScanText == "Next scan within the hour", "never a slot in the past")
        #expect(rig.work.due() == .slot, "at the next tick, not at Resume")

        rig.legacy = true
        #expect(rig.work.due() == nil)
        #expect(rig.popover.nextScanText == "Replace the old background scan first")
        #expect(await rig.work.tick() == .notDue)
        #expect(await rig.runner.count(.backgroundScan) == 0)
    }

    @Test("turning scheduled scans off clears the reminder and the pending notification (B9)")
    func offClears() throws {
        let rig = try Rig(self, now: at(20, 10))
        rig.preferences.setReminderScan(at(21, 10))
        rig.preferences.setPendingNotification(PendingNotification(text: NotificationText(title: "t", body: "b"), bytes: 1, decided: at(20, 10)))
        let settings = SettingsModel(store: SettingsStore(url: data.directory.settings), popover: rig.popover, preferences: rig.preferences, home: "/Users/example")
        var weekly = Schedule()
        weekly.repeats = .weekly
        settings.setSchedule(weekly)
        #expect(rig.preferences.reminderScan != nil, "only off clears them")
        weekly.enabled = false
        settings.setSchedule(weekly)
        #expect(rig.preferences.reminderScan == nil && rig.preferences.pendingNotification == nil)
    }

    @Test("two concurrent ticks give one background scan, one post and one ack")
    func concurrentTicks() async throws {
        let rig = try Rig(self, now: at(20, 10))
        await rig.reply(decision(bytes: 4 << 20))
        async let first = rig.work.tick()
        async let second = rig.work.tick()
        let results = await [first, second]
        #expect(results == [.scanned, .scanned], "the second joins the first")
        #expect(await rig.runner.count(.backgroundScan) == 1)
        #expect(rig.notifications.posted.count == 1)
        #expect(await rig.acks() == [["notify", "ack", "--bytes", String(4 << 20)]])
        #expect(await rig.work.tick() == .notDue, "once it ended, a tick decides again")
    }

    @Test("a catch-up slot scan before a future reminder keeps the reminder")
    func catchUpKeepsReminder() async throws {
        let rig = try Rig(self, now: at(19, 15))
        rig.preferences.setSchedule(Schedule(enabled: true, repeats: .weekly, weekday: 2, hour: 10))
        rig.preferences.setLastScheduledScan(at(12, 10))
        rig.preferences.setReminderScan(at(20, 10))
        #expect(rig.work.due() == .slot)
        await rig.reply(decision(notify: false))
        #expect(await rig.work.tick() == .scanned)
        #expect(rig.preferences.reminderScan == at(20, 10), "not due yet: kept")
        rig.clock.now = at(20, 10)
        #expect(rig.work.due() == .reminder)
    }

    @Test("a failed scheduled scan's warning ends when the next scheduled scan starts")
    func failureClears() async throws {
        let rig = try Rig(self, now: at(20, 10))
        await rig.runner.reply(.backgroundScan, CLIRunner.Result(outcome: .error(status: 1), stdout: Data(), stderrTail: ["ddc: none of the folders to scan exists"]))
        #expect(await rig.work.tick() == .failed)
        #expect(rig.popover.attention.contains(.scanFailed))
        #expect(rig.popover.notices.contains { $0.id == "scan" })
        await rig.reply(decision(notify: false))
        #expect(await rig.work.tick() == .scanned)
        #expect(!rig.popover.attention.contains(.scanFailed))
        #expect(!rig.popover.notices.contains { $0.id == "scan" })
    }

    @Test("a pending working-hours notification still posts while paused: it reports a scan already made")
    func pendingPostsWhilePaused() async throws {
        let rig = try Rig(self, now: at(20, 9))
        rig.preferences.setLastScheduledScan(at(19, 22))
        rig.preferences.setWorkingHoursOnly(true)
        rig.preferences.setPendingNotification(PendingNotification(text: NotificationText(title: "3.0 MiB can be freed", body: "b"), bytes: 3 << 20, decided: at(19, 22)))
        rig.popover.setPaused(true)
        #expect(await rig.work.tick() == .notDue)
        #expect(await rig.runner.count(.backgroundScan) == 0)
        #expect(rig.notifications.posted.map(\.title) == ["3.0 MiB can be freed"])
        #expect(await rig.acks() == [["notify", "ack", "--bytes", String(3 << 20)]])
    }

    @Test("Allow Notifications after a denial opens System Settings instead of asking")
    func allowWhenDenied() async throws {
        let rig = try Rig(self, now: at(20, 10))
        let opened = Box(0)
        let work = ScheduledWork(
            popover: rig.popover, preferences: rig.preferences, runner: rig.runner, notifications: rig.notifications,
            legacyAgentFound: { false }, openNotificationSettings: { opened.value += 1 }
        )
        rig.notifications.status = .denied
        await work.allowNotifications()
        #expect(opened.value == 1 && rig.notifications.requests == 0)
        rig.notifications.status = .notDetermined
        await work.allowNotifications()
        #expect(opened.value == 1 && rig.notifications.requests == 1)
    }

    @Test("Remind Me Tomorrow anchors on the tap (B7)")
    func remindAnchorsOnTheTap() async throws {
        let rig = try Rig(self, now: at(19, 10, 5))
        rig.preferences.notificationPosted()
        await rig.work.remindTomorrow()
        #expect(!rig.popover.badgePending && !rig.preferences.badgePending, "the badge clears")
        // Held by working hours, posted Monday 09:00, tapped 09:05.
        rig.clock.now = at(19, 9, 5)
        await rig.work.remindTomorrow()
        // Tapped three days after it was posted.
        rig.clock.now = at(22, 14)
        await rig.work.remindTomorrow()
        #expect(await rig.runner.arguments(of: .notify).map { $0.last ?? "" } == ["2026-10-20T10:00:00Z", "2026-10-20T10:00:00Z", "2026-10-23T10:00:00Z"])
        #expect(rig.preferences.reminderScan == nil, "a daily schedule needs no reminder scan")
        #expect(ScanDue.remindAt(Schedule(), tappedAt: at(31, 23, 59), calendar: calendar) == calendar.date(from: DateComponents(year: 2026, month: 11, day: 1, hour: 10)))
    }

    @Test("Review loads the report first, then opens the Review window")
    func reviewAnswer() async throws {
        let rig = try Rig(self, now: at(20, 10))
        let router = FakeRouter()
        router.opens = true
        let popover = try makeModel(runner: rig.runner, data: data, defaults: defaults, router: router)
        let work = ScheduledWork(popover: popover, preferences: rig.preferences, runner: rig.runner, notifications: rig.notifications, legacyAgentFound: { false })
        await work.review()
        #expect(popover.loadedReport != nil)
        #expect(router.requests.count == 1)
    }

    // MARK: - The scheduler agent

    private func agent(_ service: FakeAgents, home: URL? = nil, runner: FakeRunner? = nil, notices: Box<[String?]> = Box([]), asked: Box<Int> = Box(0)) -> (SchedulerAgent, SettingsStore) {
        let store = SettingsStore(url: data.directory.settings)
        let agent = SchedulerAgent(
            service: service,
            store: store,
            preferences: AppPreferences(defaults: defaults.defaults),
            runner: runner,
            legacyPlist: LegacyAgent.plist(home: home ?? data.url.appending(path: "home")),
            bundlePath: "/Applications/DeepDriveCleaner.app",
            notice: { notices.value.append($0) },
            askPermission: { asked.value += 1 }
        )
        return (agent, store)
    }

    @Test("every launch with scheduled scans on registers; approval pending leaves the record; enabled writes it; a failing register clears it; off unregisters and clears it")
    func agentHeals() async throws {
        let service = FakeAgents()
        service.after[SchedulerAgent.primaryPlist] = .requiresApproval
        try data.write(settings: #"{"schema_version": 1, "scheduler": {"by": "app", "app": "/Applications/Old.app"}}"#)
        let (agent, store) = agent(service)
        await agent.heal()
        #expect(service.registered == [SchedulerAgent.primaryPlist])
        #expect(store.file.scheduler?.app == "/Applications/Old.app", "requires approval: left as it is")
        #expect(agent.registration == .requiresApproval)

        // Approved later: the next launch finds it enabled.
        service.after[SchedulerAgent.primaryPlist] = .enabled
        await agent.heal()
        #expect(service.registered.count == 2, "every launch registers")
        #expect(store.file.scheduler == SettingsFile.Scheduler(by: "app", app: "/Applications/DeepDriveCleaner.app"))
        #expect(agent.registration == .allowed)
        let written = DataDirectory.modified(data.directory.settings)
        await agent.heal()
        #expect(DataDirectory.modified(data.directory.settings) == written, "written only when the record changes")

        // Dropped and refused, both plists: cleared.
        service.after = [SchedulerAgent.primaryPlist: .notRegistered, SchedulerAgent.fallbackPlist: .notRegistered]
        service.throwing = [SchedulerAgent.primaryPlist, SchedulerAgent.fallbackPlist]
        await agent.heal()
        #expect(store.file.scheduler == nil)

        service.throwing = []
        service.after = [:]
        await agent.heal()
        #expect(store.file.scheduler != nil)
        let preferences = AppPreferences(defaults: defaults.defaults)
        var off = Schedule()
        off.enabled = false
        preferences.setSchedule(off)
        agent.scheduleChanged(off)
        await agent.change?.value
        #expect(service.unregistered.contains(SchedulerAgent.primaryPlist))
        #expect(service.status(SchedulerAgent.primaryPlist) == .notRegistered)
        #expect(store.file.scheduler == nil)

        // Off then on at once: each heal waits for the one before, and on wins.
        agent.scheduleChanged(off)
        var on = Schedule()
        on.enabled = true
        preferences.setSchedule(on)
        agent.scheduleChanged(on)
        await agent.change?.value
        #expect(service.status(SchedulerAgent.primaryPlist) == .enabled)
        #expect(store.file.scheduler != nil)
    }

    @Test("a refused primary plist leads to the fallback; both refused clears the record and the notice names Login Items and ddc schedule install (B6)")
    func fallback() async throws {
        let service = FakeAgents()
        service.after[SchedulerAgent.primaryPlist] = .notFound
        let notices = Box<[String?]>([])
        let (agent, store) = agent(service, notices: notices)
        await agent.heal()
        #expect(service.registered == [SchedulerAgent.primaryPlist, SchedulerAgent.fallbackPlist])
        #expect(service.unregistered == [SchedulerAgent.primaryPlist])
        #expect(store.file.scheduler?.by == "app")
        #expect(agent.registration == .allowed)

        service.after[SchedulerAgent.fallbackPlist] = .notFound
        await agent.heal()
        #expect(store.file.scheduler == nil)
        let notice = try #require(notices.value.last ?? nil)
        #expect(notice.contains("Login Items") && notice.contains("ddc schedule install"))
    }

    // MARK: - Migration

    /// A 0.1.0 agent's plist, as `launchd.rs` writes it, in a temporary home.
    private func legacyPlist(weekly: Bool, threshold: String? = "5368709120") throws -> URL {
        let home = data.url.appending(path: "home")
        let url = LegacyAgent.plist(home: home)
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        var arguments = ["/opt/homebrew/bin/deepdrivecleaner", "scan", "--background"]
        if let threshold { arguments += ["--threshold", threshold] }
        var interval: [String: Int] = ["Hour": 11, "Minute": 30]
        if weekly { interval["Weekday"] = 1 }
        let plist: [String: Any] = ["Label": LegacyAgent.label, "ProgramArguments": arguments, "StartCalendarInterval": interval]
        try PropertyListSerialization.data(fromPropertyList: plist, format: .xml, options: 0).write(to: url)
        return url
    }

    @Test("Replace It: the schedule becomes 11:00 daily or on Mondays, the threshold is written, the old agent is uninstalled, the app's registered and permission asked")
    func replaceIt() async throws {
        for weekly in [false, true] {
            let fresh = try TempDefaults()
            let plist = try legacyPlist(weekly: weekly)
            try? FileManager.default.removeItem(at: data.directory.settings)
            let runner = FakeRunner(scan: FakeRunner.done(Data()))
            // The fake uninstall deletes the plist, as the real one does.
            await runner.reply(.schedule, FakeRunner.done(Data())) { try? FileManager.default.removeItem(at: plist) }
            let service = FakeAgents()
            let asked = Box(0)
            let store = SettingsStore(url: data.directory.settings)
            let preferences = AppPreferences(defaults: fresh.defaults)
            let agent = SchedulerAgent(service: service, store: store, preferences: preferences, runner: runner, legacyPlist: plist, bundlePath: "/Applications/DeepDriveCleaner.app", askPermission: { asked.value += 1 })
            #expect(agent.legacyAgentFound)
            await agent.heal()
            #expect(service.registered.isEmpty, "no launch registers while the old agent exists (B1)")

            #expect(await agent.replaceLegacy())
            #expect(await runner.arguments(of: .schedule) == [["schedule", "uninstall"]])
            #expect(!agent.legacyAgentFound)
            #expect(preferences.schedule == Schedule(enabled: true, repeats: weekly ? .weekly : .daily, weekday: 2, hour: 11))
            #expect(store.file.notifyThreshold == 5 << 30)
            #expect(service.registered == [SchedulerAgent.primaryPlist])
            #expect(store.file.scheduler != nil)
            #expect(asked.value == 1)
        }
    }

    @Test("a failed uninstall changes nothing and keeps the offer; declining hides the popover's offer and Settings keeps it; while the plist exists a reopen scans nothing")
    func migrationRefusals() async throws {
        let plist = try legacyPlist(weekly: true)
        let runner = FakeRunner(scan: FakeRunner.done(Data()), rules: FakeRunner.done(try Fixture.data("rules")))
        await runner.reply(.schedule, CLIRunner.Result(outcome: .error(status: 1), stdout: Data(), stderrTail: ["ddc: launchctl bootout failed"]))
        let service = FakeAgents()
        let asked = Box(0)
        let preferences = AppPreferences(defaults: defaults.defaults)
        let store = SettingsStore(url: data.directory.settings)
        let agent = SchedulerAgent(service: service, store: store, preferences: preferences, runner: runner, legacyPlist: plist, bundlePath: "/Applications/DeepDriveCleaner.app", askPermission: { asked.value += 1 })
        let popover = try makeModel(runner: runner, data: data, defaults: defaults, legacyAgentFound: { agent.legacyAgentFound })
        await popover.start()
        #expect(popover.notices.first { $0.id == PopoverModel.oldScanNotice }?.action == .replaceOldScan)

        #expect(!(await agent.replaceLegacy()))
        #expect(preferences.schedule == Schedule(), "no schedule change")
        #expect(store.file.notifyThreshold == nil && store.file.scheduler == nil, "no threshold, no record")
        #expect(service.registered.isEmpty && asked.value == 0)
        #expect(FileManager.default.fileExists(atPath: plist.path))
        await popover.refresh()
        #expect(popover.notices.contains { $0.id == PopoverModel.oldScanNotice }, "the offer stays")

        // A reopen while the plist exists scans nothing (B1).
        try data.write(lastScan: try Fixture.data("scan-report-groups"))
        let work = ScheduledWork(popover: popover, preferences: preferences, runner: runner, notifications: FakeNotifications(), legacyAgentFound: { agent.legacyAgentFound })
        #expect(await work.tick() == .notDue)
        #expect(await runner.count(.backgroundScan) == 0)

        // Declining: the popover stops offering; Settings keeps the row.
        let offer = try #require(popover.notices.first { $0.id == PopoverModel.oldScanNotice })
        popover.dismiss(offer)
        await popover.refresh()
        #expect(!popover.notices.contains { $0.id == PopoverModel.oldScanNotice })
        #expect(preferences.oldScanOfferDismissed)
        let settings = SettingsModel(store: store, popover: popover, preferences: preferences, scheduler: agent, home: "/Users/example")
        #expect(settings.legacyAgentFound)
        // Replace It from Settings: the pane says why it failed.
        await settings.replaceLegacyAgent()
        #expect(settings.replaceFailure?.hasPrefix("The old background scan could not be removed.") == true)
        #expect(settings.replaceFailure?.contains("launchctl bootout failed") == true)
        #expect(preferences.schedule == Schedule() && store.file == SettingsFile(), "declining changes nothing else")
    }

    @Test("the legacy plist is read as launchd.rs writes it: interval, threshold in bytes or with a unit")
    func legacyRead() throws {
        let daily = try #require(LegacyAgent.read(try legacyPlist(weekly: false, threshold: nil)))
        #expect(!daily.weekly && daily.threshold == nil)
        let weekly = try #require(LegacyAgent.read(try legacyPlist(weekly: true, threshold: "12G")))
        #expect(weekly.weekly && weekly.threshold == 12 << 30)
        #expect(weekly.schedule.weekday == 2, "launchd's Monday is Calendar's 2 (B5)")
        #expect(LegacyAgent.read(data.url.appending(path: "none.plist")) == nil)
        // A unit whose capital is two letters ("ß" is "SS") is refused, never a trap.
        #expect(LegacyAgent.size("5ß") == nil)
        #expect(LegacyAgent.size("5g") == 5 << 30 && LegacyAgent.size("1.5M") == 3 << 19 && LegacyAgent.size("2048") == 2048)
    }

    // MARK: - The tick and the bundled agent

    @Test("--tick opens the app by its own bundle path, hidden, with --scheduled; main.swift uses that function")
    func tickArguments() throws {
        let bundle = URL(filePath: "/Applications/DeepDriveCleaner.app", directoryHint: .isDirectory)
        #expect(LaunchMode.tickOpenArguments(bundleURL: bundle) == ["-gj", "/Applications/DeepDriveCleaner.app", "--args", "--scheduled"])
        let main = try String(contentsOf: repositoryRoot().appending(path: "app/DeepDriveCleaner/main.swift"), encoding: .utf8)
        #expect(main.contains("LaunchMode.tickOpenArguments(bundleURL: Bundle.main.bundleURL)"))
        #expect(main.contains("CommandLine.arguments.contains(LaunchMode.tickArgument)"))
    }

    @Test("the bundled scheduler plist runs the app's binary with --tick under the label the app registers; the fallback runs open; both are copied")
    func bundledPlists() throws {
        func plist(_ name: String) throws -> [String: Any] {
            let data = try Data(contentsOf: repositoryRoot().appending(path: "app/LaunchAgents/\(name)"))
            return try #require(try PropertyListSerialization.propertyList(from: data, format: nil) as? [String: Any])
        }
        let primary = try plist(SchedulerAgent.primaryPlist)
        #expect(primary["BundleProgram"] as? String == "Contents/MacOS/DeepDriveCleaner")
        #expect(primary["ProgramArguments"] as? [String] == ["DeepDriveCleaner", "--tick"])
        #expect(primary["Label"] as? String == String(SchedulerAgent.primaryPlist.dropLast(".plist".count)))
        #expect((primary["StartCalendarInterval"] as? [String: Int]) == ["Minute": 0])
        let fallback = try plist(SchedulerAgent.fallbackPlist)
        #expect(fallback["Label"] as? String == String(SchedulerAgent.fallbackPlist.dropLast(".plist".count)))
        #expect(fallback["ProgramArguments"] as? [String] == ["/usr/bin/open", "-gjb", "com.smartandpoint.deepdrivecleaner", "--args", "--scheduled"])
        #expect(fallback["BundleProgram"] == nil)
        let project = try String(contentsOf: repositoryRoot().appending(path: "app/DeepDriveCleaner.xcodeproj/project.pbxproj"), encoding: .utf8)
        for name in [SchedulerAgent.primaryPlist, SchedulerAgent.fallbackPlist] {
            #expect(project.contains("\(name) in Copy Launch Agents */,"), "\(name) is copied into Contents/Library/LaunchAgents")
        }
    }

    @Test("Scan now during a scheduled scan waits for it, then runs its own scan")
    func scanNowWaits() async throws {
        let rig = try Rig(self, now: at(20, 10))
        await rig.reply(decision(notify: false))
        let tick = Task { await rig.work.tick() }
        let scan = Task { await rig.popover.scanNow() }
        _ = await tick.value
        await scan.value
        #expect(await rig.runner.count(.backgroundScan) == 1)
        #expect(await rig.runner.count(.scan) == 1)
    }
}

// MARK: - Fakes

/// `UNUserNotificationCenter` stand-in.
@MainActor
final class FakeNotifications: NotificationPosting {
    var status: NotificationAuthorization = .authorized
    private(set) var posted: [NotificationText] = []
    private(set) var requests = 0

    func authorization() async -> NotificationAuthorization { status }

    func requestAuthorization() async -> Bool {
        requests += 1
        status = .authorized
        return true
    }

    func post(_ text: NotificationText) async -> Bool {
        posted.append(text)
        return true
    }
}

/// `SMAppService.agent` stand-in: `after` is the status a register leaves (enabled by default).
@MainActor
final class FakeAgents: AgentService {
    struct Refused: Error {}
    var after: [String: AgentStatus] = [:]
    var throwing: Set<String> = []
    private(set) var registered: [String] = []
    private(set) var unregistered: [String] = []
    private var statuses: [String: AgentStatus] = [:]

    func status(_ plist: String) -> AgentStatus { statuses[plist] ?? .notRegistered }

    func register(_ plist: String) throws {
        registered.append(plist)
        statuses[plist] = after[plist] ?? .enabled
        if throwing.contains(plist) { throw Refused() }
    }

    func unregister(_ plist: String) async throws {
        unregistered.append(plist)
        statuses[plist] = .notRegistered
    }
}

/// The tests' clock.
@MainActor
final class Clock {
    var now: Date
    init(_ now: Date) { self.now = now }
}

/// A value the fakes' closures share.
@MainActor
final class Box<Value> {
    var value: Value
    init(_ value: Value) { self.value = value }
}
