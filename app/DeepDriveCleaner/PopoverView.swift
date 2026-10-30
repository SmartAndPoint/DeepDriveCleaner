import DDCKit
import SwiftUI

/// The popover (artboard 1 of the design canvas), drawn from `PopoverModel`.
/// Text and backgrounds use semantic colours, so it reads in dark mode; the
/// class colours are the canvas's. The accent is the user's, and the brand
/// blue of the `AccentColor` asset under Multicolor.
struct PopoverView: View {
    @Bindable var model: PopoverModel
    let openSettings: () -> Void
    /// The Next scan line: Settings on the pane that can change it.
    let openNextScan: (SettingsDestination) -> Void
    let openPrivacy: () -> Void
    /// The Whole disk window (story "Whole disk in the app").
    let openWholeDisk: () -> Void
    /// The update line's model, and the runner it waits for.
    let update: UpdateModel?
    let activity: RunnerActivity?
    /// With keyboard navigation on, the first focus is Review…, not the gear.
    @FocusState private var focus: PopoverFocus?

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            header
            content
            // Next to the buttons and Scan now, whose results they report.
            ForEach(model.notices) { notice in
                NoticeRow(text: notice.text, action: notice.action.map { action in (action.title, { model.perform(action) }) }) {
                    model.dismiss(notice)
                }
            }
            if let update {
                UpdateLineView(model: update, activity: activity)
            }
            footer
        }
        .padding(16)
        .frame(width: 400, alignment: .leading)
        .defaultFocus($focus, .review)
    }

    private var header: some View {
        HStack(alignment: .center, spacing: 10) {
            Image(nsImage: NSApp.applicationIconImage)
                .resizable()
                .frame(width: 32, height: 32)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text("DeepDriveCleaner").font(.system(size: 15, weight: .semibold))
                if !model.subtitle.isEmpty {
                    Text(model.subtitle).font(.system(size: 12)).foregroundStyle(.secondary)
                }
            }
            Spacer()
            Button(action: openSettings) {
                Image(systemName: "gearshape").font(.system(size: 15)).frame(width: 32, height: 32)
            }
            .buttonStyle(.plain)
            .help("Settings")
            .accessibilityLabel("Settings")
        }
    }

    @ViewBuilder private var content: some View {
        switch model.content {
        case .loading:
            EmptyView()
        case .scanning:
            ScanningView(model: model)
        case .firstScan:
            FirstScanView(model: model, openPrivacy: openPrivacy)
        case .unreadable(let reason):
            StateBlock(title: "The last report could not be read", text: "\(reason) A new scan replaces it.") {
                Button("Scan Now") { Task { await model.scanNow() } }.disabled(!model.canScan)
            }
        case .nothingFound:
            StateBlock(
                title: "Nothing found to clean",
                text: model.scopeNotice ?? "The last scan found nothing to free \(model.scope).",
                note: model.withoutAccessNote
            ) {
                if model.scopeNotice != nil {
                    Button("Scan Now") { Task { await model.scanNow() } }.disabled(!model.canScan)
                }
                if (model.summary?.foundBytes ?? 0) > 0 {
                    Button("See What Was Found") { model.review() }.buttonStyle(.link)
                }
                WholeDiskLink(open: openWholeDisk)
            }
        case .nothingWorthCleaning:
            let summary = model.summary
            StateBlock(
                title: "Nothing worth cleaning",
                text: "\(ByteSize.human(summary?.reclaimable ?? 0)) could be freed, under your \(ByteSize.human(model.threshold)) notification threshold.",
                note: model.scopeNotice ?? model.withoutAccessNote
            ) {
                if model.scopeNotice != nil {
                    Button("Scan Now") { Task { await model.scanNow() } }.disabled(!model.canScan)
                }
                Button("See What Was Found") { model.review() }.buttonStyle(.link)
                WholeDiskLink(open: openWholeDisk)
            }
        case .found:
            if let summary = model.summary {
                FoundView(model: model, summary: summary, focus: $focus, openWholeDisk: openWholeDisk)
            }
        }
    }

    private var footer: some View {
        VStack(spacing: 10) {
            Divider()
            HStack(alignment: .center) {
                if let scan = model.scan {
                    TimelineView(.periodic(from: scan.started, by: 1)) { context in
                        Text("Scanning… \(Wording.elapsed(context.date.timeIntervalSince(scan.started)))").monospacedDigit()
                    }
                } else if model.updateWaiting {
                    // Scan Now, the clean and the free start nothing until the relaunch.
                    Text("Updating after this run")
                } else if model.paused {
                    HStack(spacing: 4) {
                        Text("Scans paused")
                        Button("Resume") { model.setPaused(false) }.buttonStyle(.link)
                    }
                } else {
                    Button("Scan Now") { Task { await model.scanNow() } }
                        .buttonStyle(.link)
                        .disabled(!model.canScan)
                }
                Spacer()
                if model.fullDiskAccess.isGranted {
                    Label {
                        Text("Full Disk Access")
                    } icon: {
                        Image(systemName: "checkmark").foregroundStyle(ReportClassStyle.safe)
                    }
                    .labelStyle(.titleAndIcon)
                } else {
                    Button(action: openPrivacy) {
                        Label("Full Disk Access Needed", systemImage: "exclamationmark.circle.fill")
                    }
                    .buttonStyle(.link)
                    .foregroundStyle(ReportClassStyle.ink(.review))
                    .help("Open Privacy & Security → Full Disk Access")
                }
                Spacer()
                // While paused the left says so, with Resume.
                if !model.paused {
                    let destination = model.nextScanValue.settingsDestination
                    // Opens a window and changes nothing.
                    Button(model.nextScanText) { openNextScan(model.nextScanValue.settingsDestination) }
                        .buttonStyle(.link)
                        .foregroundStyle(.secondary)
                        .pointerStyle(.link)
                        .help(destination.help)
                        .accessibilityHint(destination.hint)
                }
            }
            .font(.system(size: 12))
            .foregroundStyle(.secondary)
        }
    }
}

// MARK: - States

private struct FoundView: View {
    @Bindable var model: PopoverModel
    let summary: ReportSummary
    var focus: FocusState<PopoverFocus?>.Binding
    let openWholeDisk: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            VStack(alignment: .leading, spacing: 4) {
                Caption("Selected to clean")
                Text(ByteSize.human(model.selectedToClean))
                    .font(.system(size: 34, weight: .bold))
                    .monospacedDigit()
                if let scope = model.scopeNotice {
                    Text("of \(ByteSize.human(summary.foundBytes)) found.\(model.reportScope == .inside ? " Nothing is removed until you confirm." : "")")
                        .font(.system(size: 13))
                        .foregroundStyle(.secondary)
                    // A report of other roots (`ddc scan <path>` in the terminal).
                    Text(scope).font(.system(size: 12)).foregroundStyle(ReportClassStyle.ink(.review))
                        .fixedSize(horizontal: false, vertical: true)
                    Button("Scan Now") { Task { await model.scanNow() } }.disabled(!model.canScan).controlSize(.small)
                } else {
                    Text("of \(ByteSize.human(summary.foundBytes)) found \(model.scope). Nothing is removed until you confirm.")
                        .font(.system(size: 13))
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                if let frees = model.freesAbout {
                    Text("Frees about \(frees): the rest is shared with copies that stay on disk.")
                        .font(.system(size: 12))
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                if let note = model.withoutAccessNote {
                    Text(note).font(.system(size: 12)).foregroundStyle(ReportClassStyle.ink(.review))
                        .fixedSize(horizontal: false, vertical: true)
                }
                WholeDiskLink(open: openWholeDisk)
            }

            ClassBar(summary: summary)

            VStack(alignment: .leading, spacing: 2) {
                ForEach(summary.classes, id: \.riskClass) { row in
                    ClassRowView(row: row, model: model)
                }
            }

            if !summary.topGroups.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    Divider()
                    Caption("Biggest finds").padding(.top, 6)
                    ForEach(summary.topGroups, id: \.name) { group in
                        FindRow(group: group) { model.review(group: group.name) }
                    }
                }
            }

            HStack(spacing: 8) {
                Button { model.review() } label: {
                    Text("Review…").frame(maxWidth: .infinity, minHeight: 28)
                }
                .controlSize(.large)
                .focused(focus, equals: .review)
                Button { model.cleanSelected() } label: {
                    Text("Clean Selected…").fontWeight(.semibold).frame(maxWidth: .infinity, minHeight: 28)
                }
                .controlSize(.large)
                .buttonStyle(.borderedProminent)
                .disabled(!model.canCleanSelected)
            }
        }
    }
}

private struct ScanningView: View {
    let model: PopoverModel

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Scanning \(model.scope.dropFirst("in ".count))…").font(.system(size: 20, weight: .semibold))
            ProgressView().progressViewStyle(.linear)
            if let scan = model.scan {
                TimelineView(.periodic(from: scan.started, by: 1)) { context in
                    let elapsed = Wording.elapsed(context.date.timeIntervalSince(scan.started))
                    let read = scan.files.map { "\(Wording.count($0)) files read · " } ?? ""
                    Text("\(read)\(elapsed). The last results stay available until this scan ends.")
                        .font(.system(size: 13))
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                        .monospacedDigit()
                }
            }
        }
        .padding(.vertical, 4)
    }
}

private struct FirstScanView: View {
    let model: PopoverModel
    let openPrivacy: () -> Void

    var body: some View {
        if model.fullDiskAccess.isGranted {
            StateBlock(title: "No scan yet", text: "A scan reads your home folder and changes nothing.") {
                Button("Scan Now") { Task { await model.scanNow() } }
                    .buttonStyle(.borderedProminent)
                    .disabled(!model.canScan)
            }
        } else {
            StateBlock(
                title: "Give DeepDriveCleaner Full Disk Access first",
                text: "Without it, a scan asks for Desktop, Documents, Downloads and other apps' data, one dialog per folder, and part of ~/Library stays unread."
            ) {
                HStack(spacing: 12) {
                    Button("Open Privacy Settings", action: openPrivacy)
                        .buttonStyle(.borderedProminent)
                    Button("Scan Anyway") { Task { await model.scanNow() } }
                        .buttonStyle(.link)
                        .disabled(!model.canScan)
                }
            }
        }
    }
}

/// Under the totals: the home folder is only part of the disk.
private struct WholeDiskLink: View {
    let open: () -> Void

    var body: some View {
        Button("Where Does the Rest Go?", action: open)
            .buttonStyle(.link)
            .font(.system(size: 12))
            .help("What takes the rest of the startup disk, outside the home folder")
    }
}

/// A title, a sentence and actions: the states that are not the summary.
private struct StateBlock<Actions: View>: View {
    let title: String
    let text: String
    var note: String? = nil
    @ViewBuilder let actions: () -> Actions

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title).font(.system(size: 20, weight: .semibold)).fixedSize(horizontal: false, vertical: true)
            Text(text).font(.system(size: 13)).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            if let note {
                Text(note).font(.system(size: 12)).foregroundStyle(ReportClassStyle.ink(.review)).fixedSize(horizontal: false, vertical: true)
            }
            actions().font(.system(size: 13)).padding(.top, 4)
        }
        .padding(.vertical, 4)
    }
}

// MARK: - Parts

private struct Caption: View {
    let text: String
    init(_ text: String) { self.text = text }

    var body: some View {
        Text(text.uppercased()).font(.system(size: 12)).tracking(0.5).foregroundStyle(.secondary)
    }
}

private struct ClassBar: View {
    let summary: ReportSummary

    var body: some View {
        GeometryReader { geometry in
            let shown = summary.classes.filter { $0.bytes > 0 }
            let gaps = CGFloat(max(shown.count - 1, 0)) * 2
            HStack(spacing: 2) {
                ForEach(shown, id: \.riskClass) { row in
                    ReportClassStyle.color(row.riskClass)
                        .frame(width: max(2, (geometry.size.width - gaps) * CGFloat(row.bytes) / CGFloat(max(summary.foundBytes, 1))))
                }
            }
        }
        .frame(height: 10)
        .clipShape(RoundedRectangle(cornerRadius: 5))
        .accessibilityElement()
        .accessibilityLabel(
            "Found space by class: "
                + summary.classes.map { "\(ReportClassStyle.name($0.riskClass)) \(ByteSize.human($0.bytes))" }.joined(separator: ", ")
        )
    }
}

private struct ClassRowView: View {
    let row: ReportSummary.ClassRow
    @Bindable var model: PopoverModel

    var body: some View {
        HStack(spacing: 10) {
            RoundedRectangle(cornerRadius: 3).fill(ReportClassStyle.color(row.riskClass)).frame(width: 10, height: 10)
            VStack(alignment: .leading, spacing: 1) {
                Text(ReportClassStyle.name(row.riskClass)).font(.system(size: 13, weight: .semibold))
                Text(detail).font(.system(size: 12)).foregroundStyle(.secondary)
            }
            Spacer(minLength: 8)
            Text(size).font(.system(size: 13)).monospacedDigit()
            switch row.riskClass {
            case .safe:
                Toggle("Include Safe", isOn: $model.includeSafe).switchStyle()
            case .rebuildable:
                Toggle("Include Rebuildable", isOn: $model.includeRebuildable).switchStyle()
            case .review, .userData:
                Color.clear.frame(width: 38, height: 1)
            }
        }
        .frame(minHeight: 44)
    }

    private var detail: String {
        switch row.riskClass {
        case .safe: "Caches that refill themselves"
        case .rebuildable: "\(row.selectedCount) idle \(row.selectedCount == 1 ? "project" : "projects") with a lock file"
        case .review: "VMs, models, projects without a lock file"
        case .userData: "Downloads, media, backups · pick by hand"
        }
    }

    /// "24.2 of 35.1 GiB" when the default selection leaves part of a switchable class out.
    private var size: String {
        let switchable = row.riskClass == .safe || row.riskClass == .rebuildable
        guard switchable, row.selectedBytes != row.bytes else { return ByteSize.human(row.bytes) }
        return "\(ByteSize.human(row.selectedBytes)) of \(ByteSize.human(row.bytes))"
    }
}

private struct FindRow: View {
    let group: ReportSummary.Group
    let open: () -> Void

    var body: some View {
        Button(action: open) {
            HStack(spacing: 10) {
                VStack(alignment: .leading, spacing: 0) {
                    Text(group.name).font(.system(size: 13))
                    Text(detail).font(.system(size: 12)).foregroundStyle(.secondary)
                }
                Spacer()
                Text(ByteSize.human(group.selectedBytes)).font(.system(size: 13)).monospacedDigit()
                Image(systemName: "chevron.right").font(.system(size: 10, weight: .semibold)).foregroundStyle(.tertiary)
            }
            .frame(minHeight: 40)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("\(group.name), \(ByteSize.human(group.selectedBytes)), \(detail)")
    }

    private var detail: String {
        if group.projects {
            return "\(group.selectedCount) idle \(group.selectedCount == 1 ? "project" : "projects") of \(group.count)"
        }
        return "\(group.selectedCount) \(group.selectedCount == 1 ? "item" : "items")"
    }
}

private struct NoticeRow: View {
    let text: String
    /// A button's title and what it does: Allow Notifications, Replace It.
    var action: (title: String, run: () -> Void)?
    let dismiss: () -> Void

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: "info.circle").foregroundStyle(.secondary)
            VStack(alignment: .leading, spacing: 6) {
                Text(text).font(.system(size: 12)).fixedSize(horizontal: false, vertical: true)
                if let action {
                    Button(action.title, action: action.run).controlSize(.small)
                }
            }
            Spacer(minLength: 0)
            Button(action: dismiss) { Image(systemName: "xmark").font(.system(size: 10, weight: .semibold)) }
                .buttonStyle(.plain)
                .foregroundStyle(.secondary)
                .accessibilityLabel("Dismiss")
        }
        .padding(10)
        .background(Color.secondary.opacity(0.1), in: RoundedRectangle(cornerRadius: 8))
    }
}

/// The canvas's class colours and names.
enum ReportClassStyle {
    static let safe = Color(red: 0x1F / 255, green: 0x7A / 255, blue: 0x4D / 255)
    static let rebuildable = Color(red: 0x2A / 255, green: 0x64 / 255, blue: 0xC5 / 255)
    static let review = Color(red: 0xB4 / 255, green: 0x5F / 255, blue: 0x06 / 255)
    static let userData = Color(red: 0xA3 / 255, green: 0xA3 / 255, blue: 0xA8 / 255)

    static func color(_ riskClass: ScanReport.RiskClass) -> Color {
        switch riskClass {
        case .safe: safe
        case .rebuildable: rebuildable
        case .review: review
        case .userData: userData
        }
    }

    static func name(_ riskClass: ScanReport.RiskClass) -> String {
        CleanWording.className(riskClass)
    }

    /// The Review sidebar's class section (board 9, `tint`): its background,
    /// its picked entry and its heading, each with a dark-mode value.
    static func sectionTint(_ riskClass: ScanReport.RiskClass) -> Color {
        let (light, dark) = bases(riskClass)
        return dynamic(light.withAlphaComponent(riskClass == .userData ? 0.08 : 0.07), dark.withAlphaComponent(0.14))
    }

    static func selectedTint(_ riskClass: ScanReport.RiskClass) -> Color {
        let (light, dark) = bases(riskClass)
        return dynamic(light.withAlphaComponent(riskClass == .userData ? 0.18 : 0.16), dark.withAlphaComponent(0.28))
    }

    static func ink(_ riskClass: ScanReport.RiskClass) -> Color {
        switch riskClass {
        case .safe: dynamic(hex(0x1A6A43), hex(0x86E0B2))
        case .rebuildable: dynamic(hex(0x2457AD), hex(0xAFC9F7))
        case .review: dynamic(hex(0x8A4700), hex(0xF6C38F))
        case .userData: dynamic(hex(0x55555A), hex(0xC7C7CC))
        }
    }

    private static func bases(_ riskClass: ScanReport.RiskClass) -> (light: NSColor, dark: NSColor) {
        switch riskClass {
        case .safe: (hex(0x1F7A4D), hex(0x3DBF7F))
        case .rebuildable: (hex(0x2A64C5), hex(0x5B8FEA))
        case .review: (hex(0xB45F06), hex(0xE08A2E))
        case .userData: (hex(0x787880), hex(0x8E8E93))
        }
    }

    static func hex(_ value: Int) -> NSColor {
        NSColor(srgbRed: CGFloat(value >> 16 & 0xFF) / 255, green: CGFloat(value >> 8 & 0xFF) / 255, blue: CGFloat(value & 0xFF) / 255, alpha: 1)
    }

    static func dynamic(_ light: NSColor, _ dark: NSColor) -> Color {
        Color(nsColor: NSColor(name: nil) { appearance in
            appearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua ? dark : light
        })
    }
}

private extension Toggle {
    /// The canvas's switch: small, accent when on, its label for VoiceOver only.
    func switchStyle() -> some View {
        toggleStyle(.switch).controlSize(.small).labelsHidden().frame(width: 38)
    }
}

/// Where the popover's keyboard focus can start.
enum PopoverFocus: Hashable {
    case review
}
