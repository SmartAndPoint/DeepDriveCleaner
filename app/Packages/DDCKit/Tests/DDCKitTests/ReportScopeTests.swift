import Foundation
import Testing
@testable import DDCKit

/// Story "Review window: say when the saved scan is not of the home folder":
/// a report of other roots in the app's data folder gets a notice, and one
/// outside the app's roots no Clean and no Protect. Temporary directories and
/// defaults suites only; nothing is scanned.
@Suite("A report not of the app's roots")
@MainActor
struct ReportScopeTests {
    let data: TempDataDirectory
    let defaults: TempDefaults
    /// A real folder: `/var/folders/…`, whose canonical path is `/private/var/…`.
    let root: URL

    init() throws {
        data = try TempDataDirectory()
        defaults = try TempDefaults()
        root = data.url.appending(path: "roots")
        try FileManager.default.createDirectory(at: root.appending(path: "home/Projects"), withIntermediateDirectories: true)
    }

    private var home: String { root.appending(path: "home").path }
    private var real: String { ReportScope.canonical(home) }

    @Test("realpath as Rust's canonicalize: /tmp is /private/tmp, a missing path stays as written")
    func canonical() {
        #expect(ReportScope.canonical("/tmp") == "/private/tmp")
        #expect(ReportScope.canonical("/private/tmp") == "/private/tmp")
        #expect(ReportScope.canonical(root.appending(path: "never-made").path) == root.appending(path: "never-made").path)
        #expect(real.hasPrefix("/private/var/"), "\(real)")
    }

    @Test("outside, inside and the same list, with unplugged Settings roots left out")
    func verdicts() throws {
        func of(_ roots: [String], settings: [String] = [], reportHome: String? = nil) -> ReportScope {
            ReportScope.of(reportRoots: roots, reportHome: reportHome ?? real, settingsRoots: settings, home: home)
        }
        #expect(of(["/private"]) == .outside)
        #expect(of([real + "/Projects"]) == .inside)
        #expect(of([real]) == .same)
        #expect(of([real], reportHome: "/Users/someone") == .outside, "equal roots, another home")

        let unplugged = root.appending(path: "vol-never-made").path
        #expect(of([real], settings: [home, unplugged]) == .same, "Settings has an extra root that is unplugged and the report lacks it")
        #expect(of([real, unplugged], settings: [home, unplugged]) == .same, "the report has it, made while it was plugged in")
        #expect(of([real, "/Volumes/Other"], settings: [home, unplugged]) == .outside)

        // A Settings root through a symlink and the home through /var compare as the CLI compares them.
        let link = root.appending(path: "link")
        try FileManager.default.createSymbolicLink(atPath: link.path, withDestinationPath: home)
        #expect(of([real], settings: [link.path]) == .same)
        #expect(ReportScope.of(reportRoots: [real], reportHome: real, settingsRoots: [], home: home) == .same)
        #expect(of([real], settings: [home, home + "/Projects"]) == .same, "a nested Settings root is dropped, as the walk drops it")
    }

    @Test("the notice for one root and for several, in both cases; none for the same list")
    func wording() {
        let home = "/Users/example"
        #expect(Wording.reportScope(.outside, roots: ["/private"], home: home)
            == "This report is of /private, not of your home folder and the folders in Settings. Scan Now replaces it.")
        #expect(Wording.reportScope(.outside, roots: ["/private", "/Volumes/X"], home: home)
            == "This report is of 2 folders, not of your home folder and the folders in Settings. Scan Now replaces it.")
        #expect(Wording.reportScope(.inside, roots: ["/Users/example/Projects"], home: home)
            == "This report is of ~/Projects only. Scan Now covers your home folder and the folders in Settings.")
        #expect(Wording.reportScope(.inside, roots: ["/Users/example/a", "/Users/example/b"], home: home)
            == "This report is of 2 folders only. Scan Now covers your home folder and the folders in Settings.")
        #expect(Wording.reportScope(.same, roots: [home], home: home) == nil)
        #expect(Wording.reportScope(.outside, roots: [home], home: home, reportHome: "/Users/someone")
            == "This report is of another home folder, /Users/someone, not of yours. Scan Now replaces it.")
    }

    private func report(roots: [String], home: String = "/Users/example") throws -> Data {
        try Fixture.edited("scan-report-groups") {
            $0["scan_roots"] = roots
            $0["home"] = home
        }
    }

    @Test("the Review window: outside offers no Clean and no Protect and opens no sheet; inside keeps both")
    func reviewGates() throws {
        let outside = try ReviewFixture.snapshot(report: try report(roots: ["/private"]), settingsIn: data)
        let review = makeReview(runner: nil, data: data, defaults: defaults) { outside }
        #expect(review.open(ReviewRequest(group: nil, classes: [.safe, .rebuildable], confirm: true)))
        #expect(review.reportScope == .outside)
        #expect(review.scopeNotice?.hasPrefix("This report is of /private, not of") == true)
        #expect(!review.selection.isEmpty)
        #expect(review.sheet == nil, "the popover's Clean selected… opens no sheet")
        #expect(!review.canClean(scope: nil))
        let items = try #require(review.items)
        #expect(!items.rows.contains { review.canProtect($0.id) })
        review.openSheet()
        #expect(review.sheet == nil)

        let inside = try ReviewFixture.snapshot(report: try report(roots: ["/Users/example/Projects"]), settingsIn: data)
        let other = makeReview(runner: nil, data: data, defaults: defaults) { inside }
        _ = other.open(ReviewRequest(group: nil, classes: [.safe, .rebuildable], confirm: false))
        #expect(other.reportScope == .inside)
        #expect(other.scopeNotice == "This report is of ~/Projects only. Scan Now covers your home folder and the folders in Settings.")
        #expect(other.canClean(scope: nil))
        #expect(try #require(other.items).rows.contains { other.canProtect($0.id) })

        let same = try ReviewFixture.snapshot(settingsIn: data)
        let own = makeReview(runner: nil, data: data, defaults: defaults) { same }
        _ = own.open(ReviewRequest(group: nil, classes: [.safe], confirm: false))
        #expect(own.reportScope == .same && own.scopeNotice == nil)
    }

    @Test("the notice goes with the report: reopened without one, the window shows none")
    func noticeGoesWithTheReport() throws {
        var snapshot: ReviewSnapshot? = try ReviewFixture.snapshot(report: try report(roots: ["/private"]), settingsIn: data)
        let review = makeReview(runner: nil, data: data, defaults: defaults) { snapshot }
        _ = review.open(ReviewRequest(group: nil, classes: [.safe], confirm: false))
        #expect(review.scopeNotice != nil)
        snapshot = nil
        _ = review.open(ReviewRequest(group: nil, classes: [.safe], confirm: false))
        #expect(review.reportScope == nil && review.scopeNotice == nil)
    }

    @Test("with settings.json unreadable, a report outside the home still offers no Protect; one of the home is not judged")
    func unreadableSettings() throws {
        try data.write(settings: "{ not json")
        let outside = ReviewSnapshot(report: try ScanReport.read(try report(roots: ["/private"])), rules: try Fixture.rules())
        let review = makeReview(runner: nil, data: data, defaults: defaults) { outside }
        _ = review.open(ReviewRequest(group: nil, classes: [.safe], confirm: false))
        #expect(review.reportScope == .outside)
        #expect(!(try #require(review.items)).rows.contains { review.canProtect($0.id) })

        let own = ReviewSnapshot(report: try ScanReport.read(try report(roots: ["/Users/example"])), rules: try Fixture.rules())
        let other = makeReview(runner: nil, data: data, defaults: defaults) { own }
        _ = other.open(ReviewRequest(group: nil, classes: [.safe], confirm: false))
        #expect(other.reportScope == nil && other.scopeNotice == nil)
    }

    @Test("the popover: outside turns Clean selected… off and sends nothing; inside and another home as the story says")
    func popoverGates() async throws {
        try data.write(lastScan: try report(roots: ["/private"]))
        let router = FakeRouter()
        let model = try makeModel(runner: nil, data: data, defaults: defaults, router: router)
        await model.start()
        #expect(model.reportScope == .outside)
        #expect(model.selectedToClean > 0)
        #expect(!model.canCleanSelected)
        #expect(model.scopeNotice?.hasPrefix("This report is of /private, not of") == true)
        model.cleanSelected()
        #expect(router.requests.isEmpty)

        try data.write(lastScan: try report(roots: ["/Users/example/Projects"]))
        let inside = try makeModel(runner: nil, data: data, defaults: defaults, router: router)
        await inside.start()
        #expect(inside.reportScope == .inside && inside.canCleanSelected)

        try data.write(lastScan: try report(roots: ["/Users/example"], home: "/Users/someone"))
        let foreign = try makeModel(runner: nil, data: data, defaults: defaults, router: router)
        await foreign.start()
        #expect(foreign.reportScope == .outside && !foreign.canCleanSelected)
    }
}
