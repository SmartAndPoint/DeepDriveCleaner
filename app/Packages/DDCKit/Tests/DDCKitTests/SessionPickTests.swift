import Foundation
import Testing
@testable import DDCKit

/// Story "Review window: pick a coding agent's sessions one by one" (epic
/// DDC-002): under Review → Coding agents each session folder lists its
/// sessions, picked one by one and moved to the Trash through `clean --items`.
/// Fixture paths under `/Users/example` and fake session ids only; no
/// harness folder is read.
@Suite("Coding agents: pick sessions one by one")
@MainActor
struct SessionPickTests {
    let data: TempDataDirectory
    let defaults: TempDefaults

    init() throws {
        data = try TempDataDirectory()
        defaults = try TempDefaults()
    }

    static let home = "/Users/example"
    static let project = "\(home)/.claude/projects/-Users-example-proj"
    static let fileHistory = "\(home)/.claude/file-history"
    /// `scan-report-origin`'s two sessions, one live and one free, and a
    /// third whose liveness is unknown.
    static let live = "\(project)/0a1b2c3d-0000-4000-8000-00000000000a.jsonl"
    static let free = "\(project)/0a1b2c3d-0000-4000-8000-00000000000b.jsonl"
    static let unsure = "\(project)/0a1b2c3d-0000-4000-8000-00000000000e.jsonl"
    /// The file-history folder's two sessions: liveness unknown, and free.
    static let unknown = "\(fileHistory)/0a1b2c3d-0000-4000-8000-00000000000c"
    static let checkpoints = "\(fileHistory)/0a1b2c3d-0000-4000-8000-00000000000d"

    static let transcriptsHint = "This session's transcript. --resume of it fails until it is put back from the Trash, before it is resumed."
    static let fileHistoryHint = "Checkpoints of this session. /rewind cannot use them once moved."

    /// The Coding agents report of `AgentsReviewTests`, with a third session in
    /// the transcripts folder, and Claude Code's file-history folder with two.
    static func report() throws -> Data {
        try AgentsReviewTests.report { findings in
            guard let index = findings.firstIndex(where: { $0["rule"] as? String == "agent.claude-code-transcripts" }),
                var contents = findings[index]["contents"] as? [String: Any],
                let entries = contents["entries"] as? [[String: Any]]
            else { return }
            contents["entries"] = entries + [[
                "path": unsure, "kind": "file", "bytes": 4096, "dev": 16_777_232, "ino": 339_569_813,
                "modified": "2026-10-19T09:00:00Z",
                "lock": ["reason": "liveness-unknown", "cause": "records-missing"],
            ]]
            findings[index]["contents"] = contents
            var history = findings[index]
            history["path"] = fileHistory
            history["rule"] = "agent.claude-code-file-history"
            history["hint"] = fileHistoryHint
            history["bytes"] = 20480
            history["ino"] = 339_569_820
            history["contents"] = [
                "entries": [
                    [
                        "path": unknown, "kind": "folder", "bytes": 12288, "dev": 16_777_232, "ino": 339_569_821,
                        "modified": "2026-10-29T08:00:00Z",
                        "lock": ["reason": "liveness-unknown", "cause": "records-unreadable"],
                    ],
                    [
                        "path": checkpoints, "kind": "folder", "bytes": 8192, "dev": 16_777_232, "ino": 339_569_822,
                        "modified": "2026-10-18T08:00:00Z",
                    ],
                ],
                "rest_count": 0,
                "rest_bytes": 0,
            ]
            findings.append(history)
        }
    }

    private func items() throws -> ReviewItems {
        ReviewItems.make(report: try ScanReport.read(try Self.report()), settings: nil, rules: try Fixture.rules())
    }

    /// The Review window on this report, opened on Coding agents.
    private func window() throws -> ReviewModel {
        let snapshot = ReviewSnapshot(report: try ScanReport.read(try Self.report()), rules: try Fixture.rules())
        let review = makeReview(runner: nil, data: data, defaults: defaults) { snapshot }
        #expect(review.open(ReviewRequest(group: AgentsSummary.group, classes: [], confirm: false)))
        return review
    }

    private static let entry = ReviewItems.Entry(riskClass: .review, group: AgentsSummary.group)

    private func id(_ items: ReviewItems, _ path: String) throws -> Int {
        try #require(items.rows.first { $0.finding.path == path }).id
    }

    private func folder(_ items: ReviewItems, _ rule: String) throws -> ReviewItems.Row {
        try #require(items.rows.first { $0.finding.rule == rule && $0.userEntry == nil })
    }

    private func subgroups(_ layout: GroupLayout) -> [GroupLayout.Subgroup] {
        layout.blocks.compactMap { if case .subgroup(let subgroup) = $0 { subgroup } else { nil } }
    }

    // MARK: - The rows

    @Test("Review → Coding agents lists the transcripts folder's three sessions: the live and the unknown one say why and cannot be picked, the free one can")
    func listsSessions() throws {
        let items = try items()
        let layout = items.layout(Self.entry, matching: "")
        #expect(layout.shape == .rules)
        let transcripts = try #require(subgroups(layout).first { $0.title == "~/.claude/projects/-Users-example-proj" })
        #expect(!transcripts.checkable, "sessions are picked one by one")
        #expect(transcripts.isFolder && transcripts.kind == .sessions)
        #expect(transcripts.hint == Self.transcriptsHint)
        #expect(transcripts.subtitle == "agent.claude-code-transcripts · 3 sessions · 1 other item (\(ByteSize.human(4096))) is not listed")
        // Its rows are named by their session id, largest first.
        #expect(transcripts.items.map(\.shown) == [
            "0a1b2c3d-0000-4000-8000-00000000000a", "0a1b2c3d-0000-4000-8000-00000000000b", "0a1b2c3d-0000-4000-8000-00000000000e",
        ])
        #expect(transcripts.items.map(\.row.finding.bytes) == [36864, 8192, 4096])

        let live = try #require(transcripts.items.first { $0.row.finding.path == Self.live })
        #expect(!live.row.selectable)
        #expect(live.row.lock == .entry(.liveSession(session: "0a1b2c3d-0000-4000-8000-00000000000a", by: .record)))
        #expect(live.detail == "A running Claude Code has this session open")
        let unsure = try #require(transcripts.items.first { $0.row.finding.path == Self.unsure })
        #expect(!unsure.row.selectable)
        #expect(unsure.row.lock == .entry(.livenessUnknown(cause: .recordsMissing)))
        #expect(unsure.detail == "Could not tell whether the session is in use · Claude Code keeps no session records here")
        let free = try #require(transcripts.items.first { $0.row.finding.path == Self.free })
        #expect(free.row.selectable && free.row.lock == nil)
        #expect(free.detail.isEmpty, "a free session's size and change are its columns")
        #expect(free.row.finding.modified == "2026-10-20T09:00:00Z")

        // All three are rows of the Review entry, never of User data.
        #expect([live, free, unsure].allSatisfy { $0.row.riskClass == .review && $0.row.userEntry?.isSession == true })
        #expect(items.rows(in: Self.entry).contains { $0.id == free.id })
        #expect(items.sidebar.first { $0.riskClass == .userData }?.entries.contains { $0.group == AgentsSummary.group } != true)

        // The picks are the free sessions; Select all keeps the other rows only.
        let history = try folder(items, "agent.claude-code-history").id
        #expect(layout.selectAll == [history])
        #expect(Set(layout.pickScope) == [free.id, try id(items, Self.checkpoints)])
        // Of this folder, only the free session is in the picks.
        #expect(Set(layout.pickScope).intersection(transcripts.ids) == [free.id])
    }

    @Test("Select all's total counts its own rows only, never the sessions beside it")
    func selectAllTotal() throws {
        let items = try items()
        let layout = items.layout(Self.entry, matching: "")
        // The history, then three transcripts sessions and two file-history ones.
        #expect(layout.count == 6)
        #expect(layout.selectAllBytes == 16384)
        #expect(layout.selectAllTotal == "1 folder · \(ByteSize.human(16384))")
        // A group with no sessions: its total is the group's, as before.
        let safe = items.layout(ReviewItems.Entry(riskClass: .safe, group: AgentsSummary.group), matching: "")
        #expect(safe.selectAllTotal == "\(Wording.items(safe.count, "folder")) · \(ByteSize.human(safe.bytes))")
    }

    @Test("a session subgroup's fold button says it shows or hides its sessions")
    func foldHint() throws {
        let layout = try items().layout(Self.entry, matching: "")
        let transcripts = try #require(subgroups(layout).first { $0.kind == .sessions })
        #expect(transcripts.foldHint(expanded: false) == "Shows its sessions")
        #expect(transcripts.foldHint(expanded: true) == "Hides its sessions")
        let folders = try #require(subgroups(items().layout(ReviewItems.Entry(riskClass: .safe, group: AgentsSummary.group), matching: "")).first)
        #expect(folders.foldHint(expanded: false) == "Shows its folders")
    }

    @Test("a session folder the scan could not list: no rows, nothing to pick, still locked as a session folder")
    func notListed() throws {
        let report = try AgentsReviewTests.report { findings in
            for index in findings.indices where findings[index]["rule"] as? String == "agent.claude-code-transcripts" {
                findings[index].removeValue(forKey: "contents")
            }
        }
        let items = ReviewItems.make(report: try ScanReport.read(report), settings: nil, rules: try Fixture.rules())
        let row = try folder(items, "agent.claude-code-transcripts")
        #expect(row.lock == .sessions)
        #expect(!row.selectable)
        #expect(!items.rows.contains { $0.userEntry != nil })
        let layout = items.layout(Self.entry, matching: "")
        let transcripts = try #require(subgroups(layout).first { $0.kind == .sessions })
        #expect(transcripts.subtitle == "agent.claude-code-transcripts · Its sessions could not be listed")
        #expect(transcripts.items.isEmpty)
        #expect(layout.pickScope.isEmpty)
    }

    @Test("a session whose liveness is unknown says why and cannot be picked; a file-history session can")
    func fileHistory() throws {
        let items = try items()
        let layout = items.layout(Self.entry, matching: "")
        let folder = try #require(subgroups(layout).first { $0.title == "~/.claude/file-history" })
        #expect(folder.hint == Self.fileHistoryHint)
        let unknown = try #require(folder.items.first { $0.row.finding.path == Self.unknown })
        #expect(!unknown.row.selectable)
        #expect(unknown.detail == "Could not tell whether the session is in use · its session records could not be read")
        let checkpoints = try #require(folder.items.first { $0.row.finding.path == Self.checkpoints })
        #expect(checkpoints.row.selectable)
        #expect(checkpoints.shown == "0a1b2c3d-0000-4000-8000-00000000000d")
    }

    @Test("the session folder rows are locked, never selectable, never cleaned whole; no row of the group reads Manual only")
    func folderLocked() throws {
        let items = try items()
        for rule in ["agent.claude-code-transcripts", "agent.claude-code-file-history"] {
            let row = try folder(items, rule)
            #expect(row.lock == .sessions, "\(rule)")
            #expect(!row.selectable && row.action == nil, "\(rule)")
            // Even forced into a selection, the folder is never listed.
            let confirmation = Confirmation(items, selection: [row.id], deletePermanently: false)
            #expect(confirmation.items.isEmpty)
            #expect(confirmation.itemList(checkedAgainst: items).items.isEmpty)
        }
        let manual = ReviewItems.Lock.noAction.text
        #expect(manual == "Manual only: ddc has no command for it")
        let layout = items.layout(Self.entry, matching: "")
        let texts = layout.lines(collapsed: []).flatMap { line -> [String] in
            switch line {
            case .heading(let subgroup, _): [subgroup.title, subgroup.subtitle, subgroup.hint ?? ""]
            case .item(let item), .entry(let item): [item.shown, item.detail]
            case .single(let single): [single.title, single.subtitle, single.detail]
            case .note(let text): [text]
            }
        } + [layout.facts ?? "", layout.hint ?? ""]
        #expect(!texts.contains { $0.contains(manual) })
        #expect(!items.rows(in: Self.entry).contains { $0.lock == .noAction })
        // The Items view shows the folder with its own lock, and no session.
        let shown = items.items(matching: "", sort: .default)
        #expect(!shown.contains { $0.userEntry != nil })
        #expect(shown.first { $0.finding.rule == "agent.claude-code-transcripts" }?.lock?.text == "Not cleaned whole · pick its sessions one by one")
    }

    @Test("a session is never offered Protect this folder")
    func noProtect() throws {
        let items = try items()
        let checkpoints = try #require(items.row(try id(items, Self.checkpoints)))
        #expect(checkpoints.userEntry?.kind == .folder)
        #expect(!items.canProtect(checkpoints))
        let review = try window()
        #expect(!review.canProtect(try id(try #require(review.items), Self.checkpoints)))
    }

    // MARK: - The sheet and the payload

    @Test("a picked session is listed as user-data to the Trash, never permanent; the sheet counts it under Review and names the Trash and --resume")
    func payload() throws {
        let items = try items()
        let free = try id(items, Self.free)
        let row = try #require(items.row(free))
        let confirmation = Confirmation(items, selection: [free], deletePermanently: true)
        let item = try #require(confirmation.items.first)
        #expect(confirmation.items.count == 1)
        #expect(!item.canBePermanent && !item.permanent, "the sheet offers no Delete permanently")
        #expect(item.effectiveAction == .trash)
        #expect(confirmation.itemList(checkedAgainst: items).items == [
            ListedItem(
                path: Self.free, dev: 16_777_232, ino: 339_569_812, rule: "agent.claude-code-transcripts",
                class: .userData, action: .trash, permanent: false
            ),
        ])
        #expect(row.finding.dev == 16_777_232)
        #expect(confirmation.classRows.map(\.riskClass) == [.review])
        #expect(CleanWording.sheetTitle(confirmation) == "Move 1 item to the Trash?")
        #expect(CleanWording.sheetAction(confirmation) == "Move to the Trash")
        let note = try #require(CleanWording.sessions(confirmation))
        #expect(note == "1 session goes to the Trash with its companion files; nothing is deleted permanently. Claude Code transcripts: \(Self.transcriptsHint)")
        #expect(note.contains("Trash") && note.contains("--resume"))
        #expect(!note.contains("User data") && !note.contains("agent."))
        // A sheet without a session says nothing of sessions.
        let history = try folder(items, "agent.claude-code-history").id
        #expect(CleanWording.sessions(Confirmation(items, selection: [history], deletePermanently: false)) == nil)
    }

    @Test("a file-history session's sheet says /rewind; two folders' sessions say both hints once each")
    func fileHistorySheet() throws {
        let items = try items()
        let checkpoints = try id(items, Self.checkpoints)
        let one = Confirmation(items, selection: [checkpoints], deletePermanently: false)
        #expect(one.itemList(checkedAgainst: items).items.map(\.class) == [.userData])
        let note = try #require(CleanWording.sessions(one))
        #expect(note.contains("/rewind") && !note.contains("--resume"))
        #expect(note.contains("Claude Code file history: ") && !note.contains("agent."))

        let both = Confirmation(items, selection: [checkpoints, try id(items, Self.free)], deletePermanently: false)
        let text = try #require(CleanWording.sessions(both))
        #expect(text.hasPrefix("2 sessions go to the Trash"))
        #expect(text.contains("--resume") && text.contains("/rewind"))
    }

    // MARK: - The window

    @Test("in the window: the live session never enters the selection; the picked row says sessions")
    func picking() throws {
        let review = try window()
        let items = try #require(review.items)
        let scope = items.layout(Self.entry, matching: "").pickScope
        #expect(review.pickedSummary(scope) == "Pick sessions below to move them to the Trash")
        review.toggle(try id(items, Self.live))
        #expect(review.selection.isEmpty)
        let free = try id(items, Self.free)
        review.toggle(free)
        #expect(review.selection == [free])
        #expect(review.pickedSummary(scope) == "1 picked · \(ByteSize.human(8192))")
        #expect(review.amount(of: Self.entry) == Wording.amount(selected: 8192, total: 49152 + 16384 + 20480))
    }

    @Test("picking a transcript session and a file-history session sends one clean --items with those two paths only")
    func clean() async throws {
        let report = try Self.report()
        try data.write(lastScan: report)
        let fake = try FakeCLI(lastScan: data.directory.lastScan)
        try fake.put("dry-run.jsonl", Data("""
            {"type":"item","path":"\(Self.free)","rule":"agent.claude-code-transcripts","class":"user-data","action":"trash","bytes":8192,"prediction":"would-trash"}
            {"type":"item","path":"\(Self.checkpoints)","rule":"agent.claude-code-file-history","class":"user-data","action":"trash","bytes":8192,"prediction":"would-trash"}
            {"type":"summary","items":2,"would_delete":0,"would_trash":2,"would_run":0,"would_skip":0,"bytes":16384}

            """.utf8))
        try fake.put("clean-report.json", Data("""
            {"schema_version": 6, "ddc_version": "0.11.0", "platform": "macos", "capabilities": ["private-size", "in-use-check", "dataless-files", "full-disk-access", "local-snapshots", "whole-disk", "coding-agents", "homebrew", "schedule", "notifications"], "cancelled": false, "failed": 0, "local_snapshots": 0,
             "journal": "/Users/example/Library/Logs/DeepDriveCleaner/actions.jsonl", "volumes": [],
             "items": [{"path": "\(Self.free)", "dev": 16777232, "ino": 339569812, "rule": "agent.claude-code-transcripts",
                        "class": "user-data", "action": "trash", "bytes": 8192, "outcome": "trashed"},
                       {"path": "\(Self.checkpoints)", "dev": 16777232, "ino": 339569822, "rule": "agent.claude-code-file-history",
                        "class": "user-data", "action": "trash", "bytes": 8192, "outcome": "trashed"}]}
            """.utf8))
        try fake.put("saved-scan.json", report)
        let events = ReviewEvents()
        let snapshot = ReviewSnapshot(report: try ScanReport.read(report), rules: try Fixture.rules())
        let review = makeReview(runner: CLIRunner(executable: fake.executable), data: data, defaults: defaults, events: events) { snapshot }
        #expect(review.open(ReviewRequest(group: AgentsSummary.group, classes: [], confirm: false)))
        let items = try #require(review.items)
        let free = try id(items, Self.free)
        let checkpoints = try id(items, Self.checkpoints)
        review.toggle(free)
        review.toggle(checkpoints)
        review.toggle(try id(items, Self.live))
        review.toggle(try id(items, Self.unsure))
        #expect(review.selection == [free, checkpoints])

        review.openSheet(scope: items.layout(Self.entry, matching: "").pickScope)
        let confirmation = try #require(review.sheet?.confirmation)
        #expect(confirmation.items.map(\.id) == [free, checkpoints])
        #expect(confirmation.items.allSatisfy { !$0.canBePermanent && !$0.permanent && $0.isSession })
        try await until("the dry run") { review.sheet?.warnings == .checked }
        review.cleanConfirmedItems()
        try fake.open("clean-start")
        try fake.open("clean-go")
        try await until("the result") { review.sheet?.stage.result != nil }

        let runs = try fake.runs().filter { $0.event == "start" }.map(\.args)
        #expect(runs.filter { $0.hasPrefix("clean --items ") && !$0.contains("--dry-run") }.count == 1, "one clean")
        let lists = try fake.lists()
        #expect(lists.count == 2, "the dry run's list, then the clean's")
        for list in lists {
            let json = try #require(try JSONSerialization.jsonObject(with: list) as? [String: Any])
            let sent = try #require(json["items"] as? [[String: Any]])
            #expect(sent.compactMap { $0["path"] as? String } == [Self.free, Self.checkpoints])
            #expect(sent.allSatisfy { $0["class"] as? String == "user-data" && $0["action"] as? String == "trash" && $0["permanent"] == nil })
        }
        try await until("the rescan") { events.busy == [true, false] }
        review.closeSheet()
    }
}
