import DDCKit
import SwiftUI

/// The Whole disk view (artboard 13; story "Whole disk in the app"): the
/// startup container as one bar, then a row per area. Homebrew and each
/// downloaded simulator runtime offer their owning tool's cleanup, through
/// `AreaActionSheet` (story "Whole disk: free space with the owning tool").
/// Used as the Review window's detail and in its own window; both show the
/// one `DiskModel` and the one `AgentsModel`.
struct DiskView: View {
    let model: DiskModel
    /// The Coding agents section: read again each time the view appears.
    let agents: AgentsModel
    /// The popover's report, summary and switched-on classes: the home
    /// folder's coloured part is the popover's number.
    let popover: PopoverModel

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            Divider()
            if let message = model.message {
                Banner(systemImage: "info.circle", text: message)
            }
            content
        }
        .task { await model.load() }
        .task { await agents.refresh() }
        .sheet(isPresented: Binding(
            get: { model.freeSheet != nil },
            set: { shown in if !shown { Task { await model.closeFree() } } }
        )) {
            AreaActionSheet(model: model)
        }
    }

    private var header: some View {
        HStack(alignment: .center, spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                Text("Whole disk").font(.system(size: 17, weight: .semibold))
                Text(subtitle).font(.system(size: 12)).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            }
            Spacer()
            if let progress = model.progress {
                ProgressView().controlSize(.small)
                Text(measuring(progress)).font(.system(size: 12)).foregroundStyle(.secondary).monospacedDigit()
                Button("Stop") { Task { await model.stop() } }
            } else {
                Button("Scan Whole Disk") { Task { await model.scanDisk() } }.disabled(!model.canScan)
            }
        }
        .padding(16)
    }

    private var subtitle: String {
        var parts: [String] = []
        if let report = model.loadedReport {
            let name = report.container?.volumes.first { $0.role == "system" }?.name
            let usage = DiskSummary.usage(report)
            parts.append([name, usage].compactMap { $0 }.joined(separator: " · "))
            if let measured = model.subtitle { parts.append(measured) }
        }
        let head = parts.filter { !$0.isEmpty }.joined(separator: " · ")
        return head
    }

    private func measuring(_ progress: DiskModel.Progress) -> String {
        guard let files = progress.files else { return "Measuring…" }
        let bytes = progress.bytes.map { " · \(ByteSize.human($0))" } ?? ""
        return "Measuring… \(Wording.count(files)) files\(bytes)"
    }

    @ViewBuilder private var content: some View {
        switch model.report {
        case .loading:
            Spacer()
        case .none:
            withAgents {
                ContentUnavailableView {
                    Label("No Whole-Disk Scan Yet", systemImage: "internaldrive")
                } description: {
                    Text("Scan Whole Disk measures what lies outside the home folder: apps, system data, Homebrew, simulator runtimes. It reads sizes only and takes a few minutes.")
                }
            }
        case .unreadable(let reason):
            withAgents {
                ContentUnavailableView("The Whole-Disk Report Could Not Be Read", systemImage: "exclamationmark.triangle", description: Text("\(reason) A new whole-disk scan replaces it."))
            }
        case .loaded:
            if let summary = model.summary(scan: popover.loadedReport, summary: popover.summary, included: popover.included) {
                ScrollView {
                    VStack(alignment: .leading, spacing: 12) {
                        if summary.stopped {
                            Text("This scan was stopped: the sizes are what it measured before it stopped.")
                                .font(.system(size: 12))
                                .foregroundStyle(ReportClassStyle.ink(.review))
                        }
                        DiskBar(summary: summary)
                        Legend(summary: summary)
                        VStack(spacing: 0) {
                            ForEach(summary.rows) { row in
                                DiskRow(row: row, model: model)
                                Divider()
                            }
                        }
                        AgentsSection(agents: agents).padding(.top, 12)
                    }
                    .padding(16)
                }
            }
        }
    }

    /// Without a whole-disk report, the Coding agents section still shows.
    private func withAgents(@ViewBuilder _ empty: () -> some View) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                empty().frame(minHeight: 220)
                AgentsSection(agents: agents)
            }
            .padding(16)
        }
    }
}

/// A tone's colour: the class colours for what the home folder frees, and
/// greys from the canvas for the rest, each with a dark-mode value.
enum DiskTone {
    static func color(_ tone: DiskSummary.Tone) -> Color {
        switch tone {
        case .frees(let riskClass): ReportClassStyle.color(riskClass)
        case .home: ReportClassStyle.dynamic(ReportClassStyle.hex(0x8E8E93), ReportClassStyle.hex(0x8E8E93))
        case .outside: ReportClassStyle.dynamic(ReportClassStyle.hex(0xB4B4BA), ReportClassStyle.hex(0x6C6C72))
        case .notCounted: ReportClassStyle.dynamic(ReportClassStyle.hex(0x5E5E63), ReportClassStyle.hex(0xC7C7CC))
        case .otherVolumes: ReportClassStyle.dynamic(ReportClassStyle.hex(0xD0D0D5), ReportClassStyle.hex(0x4A4A4F))
        case .free: ReportClassStyle.dynamic(ReportClassStyle.hex(0xEDEDF0), ReportClassStyle.hex(0x2C2C2E))
        }
    }

    static func name(_ tone: DiskSummary.Tone) -> String {
        switch tone {
        case .frees(let riskClass): ReportClassStyle.name(riskClass)
        case .home: "Rest of the home folder"
        case .outside: "Outside the home folder"
        case .notCounted: "Not counted"
        case .otherVolumes: "System and other volumes"
        case .free: "Free"
        }
    }
}

private struct DiskBar: View {
    let summary: DiskSummary

    var body: some View {
        GeometryReader { geometry in
            let total = CGFloat(max(summary.barTotal, 1))
            HStack(spacing: 0) {
                ForEach(Array(summary.segments.enumerated()), id: \.offset) { _, segment in
                    DiskTone.color(segment.tone)
                        .frame(width: geometry.size.width * CGFloat(segment.bytes) / total)
                }
            }
        }
        .frame(height: 18)
        .clipShape(RoundedRectangle(cornerRadius: 5))
        .accessibilityElement()
        .accessibilityLabel(
            "The startup disk: "
                + summary.segments.map { "\(DiskTone.name($0.tone)) \(ByteSize.human($0.bytes))" }.joined(separator: ", ")
        )
    }
}

private struct Legend: View {
    let summary: DiskSummary

    var body: some View {
        let tones = summary.segments.map(\.tone)
        HStack(spacing: 14) {
            ForEach(Array(tones.enumerated()), id: \.offset) { _, tone in
                HStack(spacing: 5) {
                    RoundedRectangle(cornerRadius: 2).fill(DiskTone.color(tone)).frame(width: 9, height: 9)
                    Text(DiskTone.name(tone))
                }
            }
        }
        .font(.system(size: 11))
        .foregroundStyle(.secondary)
        .accessibilityHidden(true)
    }
}

private struct DiskRow: View {
    let row: DiskSummary.Row
    let model: DiskModel

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            RoundedRectangle(cornerRadius: 2).fill(DiskTone.color(row.tone)).frame(width: 9, height: 9)
            VStack(alignment: .leading, spacing: 2) {
                Text(row.title).font(.system(size: 13, weight: row.nested ? .regular : .semibold))
                if !row.note.isEmpty {
                    Text(row.note).font(.system(size: 12)).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                }
                if let offer = row.offer, let result = model.freeResults[offer.id] {
                    Text(FreeWording.result(result, name: offer.name))
                        .font(.system(size: 12))
                        .foregroundStyle(result.result == .freed ? Color.secondary : ReportClassStyle.ink(.review))
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            Spacer(minLength: 12)
            if let offer = row.offer {
                Button(offer.buttonTitle) { Task { await model.previewAction(offer) } }
                    .controlSize(.small)
                    .disabled(!model.canFree)
            }
            Text(row.size).font(.system(size: 13)).monospacedDigit().fixedSize()
        }
        .padding(.leading, row.nested ? 24 : 0)
        .padding(.vertical, 8)
        .accessibilityElement(children: .combine)
    }
}
