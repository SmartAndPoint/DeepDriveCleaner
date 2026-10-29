import Foundation

/// Whether the saved report is of the folders the app scans (story "Review
/// window: say when the saved scan is not of the home folder"). A terminal
/// `ddc scan <path>` saves into the app's data folder like any scan, and the
/// app would show it as its own.
public enum ReportScope: Sendable, Equatable {
    /// The app's own roots: no notice.
    case same
    /// Every root lies inside the app's roots (`ddc scan ~/Projects`): a
    /// notice, and Clean works as today.
    case inside
    /// A root lies outside them (`ddc scan /private`), or the report is of
    /// another home: a notice, and no Clean or Protect.
    case outside

    /// The app's roots computed as the CLI computes them (`settings.rs`,
    /// `scan/walker.rs::normalized_roots`): the Settings roots, or the home
    /// when there are none; those that exist through `realpath(3)`; nested
    /// ones dropped. A Settings root that does not exist now (an unplugged
    /// disk) is left out of the comparison: matched as written, it neither
    /// makes a report foreign nor is it required.
    public static func of(reportRoots: [String], reportHome: String, settingsRoots: [String], home: String) -> ReportScope {
        if canonical(reportHome) != canonical(home) { return .outside }
        var plugged: Set<String> = []
        var unplugged: Set<String> = []
        if settingsRoots.isEmpty {
            plugged.insert(canonical(home))
        } else {
            for root in settingsRoots {
                if FileManager.default.fileExists(atPath: root) {
                    plugged.insert(canonical(root))
                } else {
                    unplugged.insert(root)
                }
            }
        }
        plugged = plugged.filter { root in !plugged.contains { FolderList.isInside(root, $0) } }
        let report = Set(reportRoots.map(canonical)).subtracting(unplugged)
        if report == plugged { return .same }
        let within = report.allSatisfy { root in
            (plugged.union(unplugged)).contains { root == $0 || FolderList.isInside(root, $0) }
        }
        return within ? .inside : .outside
    }

    public static func of(report: ScanReport, settingsRoots: [String], home: String) -> ReportScope {
        of(reportRoots: report.scanRoots, reportHome: report.home, settingsRoots: settingsRoots, home: home)
    }

    /// With `settings.json` unreadable (nil settings) the Settings roots are
    /// not known. Another home, or a root outside the home, counts as outside,
    /// so Protect writes no marker into a tree the user may not have chosen;
    /// any other report is nil. Clean needs no gate there: the CLI refuses
    /// that file.
    public static func of(report: ScanReport, settings: SharedSettings?, home: String) -> ReportScope? {
        if let settings { return of(report: report, settingsRoots: settings.roots, home: home) }
        let real = canonical(home)
        let foreign = canonical(report.home) != real
            || report.scanRoots.map(canonical).contains { $0 != real && !FolderList.isInside($0, real) }
        return foreign ? .outside : nil
    }

    /// `realpath(3)`, as Rust's `canonicalize`: `/tmp` is `/private/tmp`.
    /// Foundation's `resolvingSymlinksInPath` strips a leading `/private`
    /// instead. A path that does not resolve is kept as written.
    static func canonical(_ path: String) -> String {
        guard let real = realpath(path, nil) else { return path }
        defer { free(real) }
        return String(cString: real)
    }
}
