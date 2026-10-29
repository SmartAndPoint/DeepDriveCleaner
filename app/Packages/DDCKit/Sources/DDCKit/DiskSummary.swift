import Foundation

/// What the Whole disk view draws from a `DiskReport`: the bar of the
/// startup container and the rows under it, built together so the bar and
/// the rows cannot disagree (story "Whole disk in the app", artboard 13).
public struct DiskSummary: Sendable, Equatable {
    /// What a segment of the bar or a row's swatch stands for.
    public enum Tone: Sendable, Equatable, Hashable {
        /// What cleaning the last scan's default selection frees, by class.
        case frees(ScanReport.RiskClass)
        /// The home folder, or its part the scan found nothing in.
        case home
        /// The areas outside the home folder.
        case outside
        case notCounted
        /// The system, VM, Preboot and the other volumes of the container.
        case otherVolumes
        case free
    }

    public struct Segment: Sendable, Equatable {
        public let tone: Tone
        public let bytes: UInt64
    }

    public struct Row: Sendable, Equatable, Identifiable {
        public let id: String
        public let title: String
        /// "80.0 GiB", "at least 2.0 GiB", or a text such as "unknown".
        public let size: String
        public let note: String
        /// How many rows it lies inside: 1 for Homebrew inside /opt or a
        /// runtime inside Simulator runtimes, 2 for a conda environment
        /// inside Conda inside /opt.
        public let depth: Int
        public let tone: Tone
        /// The row's button: Delete Runtime…, Clean Up Homebrew…, Remove
        /// Environment…, Clean Package Cache….
        public var offer: AreaActions.Offer? = nil
        /// Show in Finder, Open Software Update; nil for a row with nothing
        /// to press.
        public var reveal: AreaActions.Reveal? = nil

        /// Lies inside the row above it.
        public var nested: Bool { depth > 0 }
    }

    /// "749.3 GiB used of 926.4 GiB", or nil without the container.
    public let usage: String?
    /// Back to front; `free` last. Their sum is the bar's whole length.
    public let segments: [Segment]
    public let rows: [Row]
    /// Set when the run was stopped: rows read "at least".
    public let stopped: Bool

    public var barTotal: UInt64 { segments.reduce(0) { $0 + $1.bytes } }

    /// The `usage` line of `report`, without the rest of the summary.
    public static func usage(_ report: DiskReport) -> String? {
        report.container.map { "\(ByteSize.human($0.inUseBytes)) used of \(ByteSize.human($0.capacityBytes))" }
    }

    /// `scan` is the report the popover loaded, `summary` its summary with
    /// `settings.json` applied, and `included` the classes the popover has
    /// switched on. The home segment shows by class what cleaning their
    /// default selection frees, the popover's number, only when the disk
    /// report's home row is that very scan; otherwise it is drawn whole.
    public static func make(
        _ report: DiskReport,
        scan: ScanReport?,
        summary: ReportSummary?,
        included: Set<ScanReport.RiskClass>,
        now: Date,
        calendar: Calendar = .current,
        locale: Locale = .autoupdatingCurrent
    ) -> DiskSummary {
        var segments: [Segment] = []
        var rows: [Row] = []

        // Home.
        let home = report.home
        var homeLeft = home.bytes
        var frees = home.reclaimableBytes
        if home.source == .lastScan, let scan, let summary, scan.generatedAt == home.asOf {
            // The popover offers Safe and Rebuildable; Review is never default-selected.
            for cls in [ScanReport.RiskClass.safe, .rebuildable] where included.contains(cls) {
                let bytes = min(homeLeft, summary.row(cls)?.selectedReclaimable ?? 0)
                guard bytes > 0 else { continue }
                segments.append(Segment(tone: .frees(cls), bytes: bytes))
                homeLeft -= bytes
            }
            frees = home.bytes - homeLeft
        }
        if homeLeft > 0 { segments.append(Segment(tone: .home, bytes: homeLeft)) }
        let when = Wording.date(rfc3339: home.asOf).map { Wording.moment($0, now: now, calendar: calendar, locale: locale) }
        var homeNote = switch home.source {
        case .lastScan: when.map { "From the scan \($0)" } ?? "From the last scan"
        case .walk: "Measured now"
        }
        if let frees, frees > 0 {
            homeNote += " · \(ByteSize.human(frees)) can be freed in Review and clean"
        }
        rows.append(Row(
            id: "home", title: "Home folder", size: size(home.bytes, partial: home.partial),
            note: homeNote, depth: 0, tone: .home
        ))

        // The areas: each nested one right under the one it lies in, when listed.
        let ids = Set(report.areas.map(\.id))
        let listed = { (parent: String?) in parent.map(ids.contains) ?? false }
        let ordered = report.areas.filter { !listed($0.parent) }.flatMap { top in
            [top] + report.areas.filter { $0.parent == top.id }
        }
        let offers = AreaActions.offers(report)
        var outside: UInt64 = 0
        for area in ordered {
            outside += area.bytes
            let measured = area.measuredBeforeAction ? " before cleanup" : ""
            let depth = listed(area.parent) ? 1 : 0
            rows.append(Row(
                id: area.id, title: area.title, size: size(area.bytes, partial: area.partial) + measured,
                note: note(area, simulators: report.simulators, conda: report.conda), depth: depth, tone: .outside,
                offer: offers[area.id], reveal: AreaActions.reveal(area)
            ))
            if area.action == .condaClean, let conda = report.conda {
                rows += condaRows(conda, depth: depth + 1, offers: offers)
            }
            // Each listed runtime under its area; their sizes are in the area's.
            if area.action == .simctlRuntimeDelete, let sims = report.simulators, sims.status == .listed {
                for runtime in sims.runtimes {
                    let id = AreaActions.runtimeRowID(runtime)
                    rows.append(Row(
                        id: id, title: AreaActions.name(runtime),
                        size: runtime.sizeBytes.map(ByteSize.human) ?? "unknown",
                        note: runtimeNote(runtime), depth: 1, tone: .outside, offer: offers[id]
                    ))
                }
            }
        }
        if outside > 0 { segments.append(Segment(tone: .outside, bytes: outside)) }

        // Not counted: never a negative segment.
        let nc = report.notCounted
        var ncNote: [String] = []
        let ncSize: String
        switch nc.bytes {
        case let bytes? where bytes < 0:
            ncSize = "−" + ByteSize.human(bytes.magnitude)
            ncNote.append(inconsistency(nc.causes))
        case let bytes?:
            ncSize = ByteSize.human(UInt64(bytes))
            if bytes > 0 { segments.append(Segment(tone: .notCounted, bytes: UInt64(bytes))) }
            ncNote.append("The Spotlight index, folders only root can read, APFS metadata")
        case nil:
            ncSize = "unknown"
            ncNote.append("The Data volume's size could not be read")
        }
        let clouds = nc.holds.filter { $0.kind == .cloud }.count
        let skips = nc.holds.count - clouds
        if clouds > 0 { ncNote.append("\(Wording.items(clouds, "cloud folder")) not read") }
        if skips > 0 { ncNote.append("\(Wording.items(skips, "skipped folder")) not read") }
        if nc.unreadable > 0 {
            ncNote.append("\(Wording.items(Int(clamping: nc.unreadable), "folder")) could not be read")
        }
        rows.append(Row(
            id: "not-counted", title: "Not counted", size: ncSize,
            note: ncNote.joined(separator: " · "), depth: 0, tone: .notCounted
        ))

        // The rest of the container: its other volumes, then the free space.
        if let c = report.container {
            if let data = nc.dataVolumeBytes, c.inUseBytes > data {
                segments.append(Segment(tone: .otherVolumes, bytes: c.inUseBytes - data))
            }
            let drawn = segments.reduce(0) { $0 + $1.bytes }
            if c.capacityBytes > drawn { segments.append(Segment(tone: .free, bytes: c.capacityBytes - drawn)) }
        }
        return DiskSummary(usage: usage(report), segments: segments, rows: rows, stopped: report.cancelled)
    }

    private static func size(_ bytes: UInt64, partial: Bool) -> String {
        partial ? "at least \(ByteSize.human(bytes))" : ByteSize.human(bytes)
    }

    /// The `/opt` conda installs under the Conda row (story "Whole disk:
    /// Conda environments and its package cache, with conda"): each
    /// environment, and with more than one install each install with its
    /// environments under it. Their sizes are in the area's.
    private static func condaRows(_ conda: DiskReport.Conda, depth: Int, offers: [String: AreaActions.Offer]) -> [Row] {
        var rows: [Row] = []
        let several = conda.installs.count > 1
        for install in conda.installs {
            if several {
                let id = AreaActions.condaInstallRowID(install)
                let pkgs = install.packages
                rows.append(Row(
                    id: id, title: install.prefix,
                    size: pkgs.map { size($0.bytes + $0.sharedBytes, partial: $0.partial) } ?? "no package cache",
                    note: installNote(install), depth: depth, tone: .outside, offer: offers[id]
                ))
            }
            for env in install.environments {
                let id = AreaActions.condaEnvironmentRowID(env)
                rows.append(Row(
                    id: id, title: AreaActions.condaName(env.path), size: size(env.bytes, partial: env.partial),
                    note: environmentNote(env), depth: several ? depth + 1 : depth, tone: .outside, offer: offers[id]
                ))
            }
        }
        return rows
    }

    private static func installNote(_ install: DiskReport.CondaInstall) -> String {
        guard let pkgs = install.packages else { return Wording.items(install.environments.count, "environment") }
        var parts = ["Its package cache", Wording.items(install.environments.count, "environment")]
        if pkgs.unreadable > 0 { parts.append("\(Wording.count(pkgs.unreadable)) unreadable") }
        if !pkgs.writable { parts.append(cacheNeedsAdmin) }
        return parts.joined(separator: " · ")
    }

    private static func environmentNote(_ env: DiskReport.CondaFolder) -> String {
        var parts = ["Conda environment"]
        if env.sharedBytes > 0 {
            parts.append("\(ByteSize.human(env.sharedBytes)) also held elsewhere, such as the package cache")
        }
        if env.unreadable > 0 { parts.append("\(Wording.count(env.unreadable)) unreadable") }
        if !env.writable { parts.append("Your user cannot write it: conda needs an administrator, so it is not removed from here") }
        return parts.joined(separator: " · ")
    }

    /// The cache an `/opt` install keeps where the user cannot write.
    static let cacheNeedsAdmin =
        "Its package cache needs an administrator: your own packages live in ~/.conda/pkgs, a finding in the Review window"

    private static func note(_ area: DiskReport.Area, simulators: DiskReport.Simulators?, conda: DiskReport.Conda?) -> String {
        var parts: [String] = []
        if area.unreadable > 0 { parts.append("\(Wording.count(area.unreadable)) unreadable") }
        if area.partial, let read = area.entriesRead { parts.append("\(Wording.count(read)) entries read") }
        if area.id == "simulator-runtimes", let s = simulators {
            switch s.status {
            case .listed:
                let kept = area.bytes > s.listedBytes ? area.bytes - s.listedBytes : 0
                parts.append(kept > 0
                    ? "Listed runtimes \(ByteSize.human(s.listedBytes)); \(ByteSize.human(kept)) more in their folders"
                    : "Listed runtimes \(ByteSize.human(s.listedBytes))")
            case .firstLaunchPending, .failed:
                if let message = s.message { parts.append(message) }
            }
        }
        if area.action == .condaClean, let installs = conda?.installs, installs.count == 1,
           let pkgs = installs[0].packages, !pkgs.writable {
            parts.append(cacheNeedsAdmin)
        }
        parts.append(area.frees)
        return parts.joined(separator: " · ")
    }

    private static func runtimeNote(_ runtime: DiskReport.Runtime) -> String {
        var parts = [Wording.items(runtime.devices.count, "simulator")]
        if runtime.booted {
            parts.append("A simulator on it is running")
        } else if runtime.state == "Deleting" {
            parts.append("Being deleted")
        } else if runtime.deletable == false {
            parts.append("macOS keeps it: simctl lists it as not deletable")
        } else if !AreaActions.offersDelete(runtime) {
            parts.append("Not a downloaded runtime, so it is not deleted from here")
        }
        return parts.joined(separator: " · ")
    }

    private static func inconsistency(_ causes: [DiskReport.InconsistentCause]) -> String {
        let why = causes.map { cause in
            switch cause {
            case .staleHomeRow: "the home folder's size is from an older scan"
            case .clonesCountedTwice: "APFS clones were counted twice"
            }
        }
        let reason = why.isEmpty ? "" : ": " + Wording.list(why)
        return "More was counted than the volume holds\(reason). Scan the home folder and the whole disk again."
    }
}

extension Wording {
    /// "today at 10:00", "yesterday at 10:00", or "on 19 Oct at 10:00".
    public static func moment(_ date: Date, now: Date, calendar: Calendar = .current, locale: Locale = .autoupdatingCurrent) -> String {
        let time = date.formatted(Date.FormatStyle(date: .omitted, time: .shortened, locale: locale, calendar: calendar, timeZone: calendar.timeZone))
        if calendar.isDate(date, inSameDayAs: now) { return "today at \(time)" }
        if let yesterday = calendar.date(byAdding: .day, value: -1, to: now), calendar.isDate(date, inSameDayAs: yesterday) {
            return "yesterday at \(time)"
        }
        let day = date.formatted(Date.FormatStyle(locale: locale, calendar: calendar, timeZone: calendar.timeZone).day().month(.abbreviated))
        return "on \(day) at \(time)"
    }
}
