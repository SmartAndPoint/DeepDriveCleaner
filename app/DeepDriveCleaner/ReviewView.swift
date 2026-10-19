import AppKit
import DDCKit
import SwiftUI

/// The Review window (artboard 4): classes and groups on the left, the
/// findings on the right, the selection and Clean N items… at the bottom.
/// Nothing here cleans; Clean N items… opens the confirmation sheet.
struct ReviewView: View {
    @Bindable var model: ReviewModel
    let close: () -> Void

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            if let notice = model.notice {
                Banner(systemImage: "info.circle", text: notice)
            }
            if let rescan = model.rescanning, model.sheet == nil {
                Banner(
                    systemImage: nil,
                    text: rescan == .afterClean ? "Scanning again after the clean… The list updates when the scan ends." : "Scanning… The list updates when the scan ends."
                )
            }
            if let items = model.items {
                HStack(spacing: 0) {
                    Sidebar(model: model, items: items).frame(width: 260)
                    Divider()
                    content(items)
                }
            } else {
                VStack(spacing: 8) {
                    Text("No scan yet").font(.system(size: 20, weight: .semibold))
                    Text("Scan from the menu bar first; the findings show here.").foregroundStyle(.secondary)
                    if model.scansPaused {
                        Text("Scans are paused. Resume them from the menu bar icon.").foregroundStyle(.secondary)
                    } else {
                        Button("Scan now") { Task { await model.scanAgain() } }.disabled(model.rescanning != nil)
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            Divider()
            footer
        }
        .frame(minWidth: 820, minHeight: 520)
        .sheet(isPresented: Binding(get: { model.sheet != nil }, set: { if !$0 { model.closeSheet() } })) {
            ConfirmSheet(model: model)
        }
    }

    private var header: some View {
        HStack(spacing: 12) {
            Picker("View", selection: $model.mode) {
                Text("Groups").tag(ReviewModel.Mode.groups)
                Text("Items").tag(ReviewModel.Mode.items)
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .frame(width: 180)
            Spacer()
            if model.mode == .items {
                Picker("Sort", selection: $model.sort) {
                    Text("Biggest first").tag(ReviewItems.Sort.size)
                    Text("By path").tag(ReviewItems.Sort.path)
                    Text("Most idle first").tag(ReviewItems.Sort.idle)
                }
                .frame(width: 170)
            }
            HStack(spacing: 6) {
                Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
                TextField("Filter by path or rule", text: $model.query).textFieldStyle(.plain)
                if !model.query.isEmpty {
                    Button { model.query = "" } label: { Image(systemName: "xmark.circle.fill") }
                        .buttonStyle(.plain)
                        .foregroundStyle(.secondary)
                        .accessibilityLabel("Clear the filter")
                }
            }
            .padding(.horizontal, 8)
            .padding(.vertical, 5)
            .frame(width: 260)
            .background(Color.secondary.opacity(0.1), in: RoundedRectangle(cornerRadius: 7))
            if model.scansPaused {
                Text("Scans paused")
                    .foregroundStyle(.secondary)
                    .help("Resume them from the menu bar icon.")
            } else {
                Button("Scan again") { Task { await model.scanAgain() } }
                    .disabled(model.rescanning != nil || model.isCleaning)
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
    }

    @ViewBuilder private func content(_ items: ReviewItems) -> some View {
        switch model.mode {
        case .groups:
            if let entry = model.entry {
                GroupView(model: model, items: items, entry: entry)
            } else {
                Text("Nothing was found.").foregroundStyle(.secondary).frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        case .items:
            ItemsView(model: model, items: items)
        }
    }

    private var footer: some View {
        let rows = model.selectedRows
        let review = rows.filter { $0.riskClass == .review }.count
        var sub = ["frees about \(ByteSize.human(model.selectedReclaimable))"]
        if review > 0 { sub.append("\(Wording.items(review, "review item")) go to the Trash or run their tool") }
        return HStack(spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                Text("\(Wording.items(rows.count)) selected · \(ByteSize.human(model.selectedBytes))")
                    .font(.system(size: 13, weight: .semibold))
                    .monospacedDigit()
                Text(sub.joined(separator: " · ")).font(.system(size: 12)).foregroundStyle(.secondary)
            }
            Spacer()
            Button("Close", action: close).keyboardShortcut(.cancelAction)
            Button("Clean \(Wording.items(rows.count))…") { model.openSheet() }
                .buttonStyle(.borderedProminent)
                .tint(PopoverView.accent)
                .keyboardShortcut(.defaultAction)
                .disabled(rows.isEmpty || model.rescanning != nil || model.isCleaning)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
    }
}

// MARK: - Sidebar

private struct Sidebar: View {
    @Bindable var model: ReviewModel
    let items: ReviewItems

    var body: some View {
        List(selection: Binding(get: { model.entry }, set: { entry in
            model.entry = entry
            if entry != nil { model.mode = .groups }
        })) {
            ForEach(items.sidebar, id: \.riskClass) { section in
                Section {
                    ForEach(section.entries, id: \.self) { entry in
                        HStack {
                            Text(entry.group).lineLimit(1)
                            Spacer(minLength: 6)
                            Text(label(entry)).font(.system(size: 12)).foregroundStyle(.secondary).monospacedDigit()
                        }
                        .tag(entry)
                    }
                } header: {
                    HStack(spacing: 6) {
                        RoundedRectangle(cornerRadius: 2).fill(ReportClassStyle.color(section.riskClass)).frame(width: 8, height: 8)
                        Text(ReportClassStyle.name(section.riskClass))
                    }
                } footer: {
                    if section.riskClass == .userData {
                        Text("Downloads, media, backups and folders you protect. Shown, never cleaned.")
                            .font(.system(size: 11))
                            .foregroundStyle(.secondary)
                    }
                }
            }
        }
        .listStyle(.sidebar)
    }

    /// "9.2 of 13.1 GiB" when part of the group is selected.
    private func label(_ entry: ReviewItems.Entry) -> String {
        let rows = items.rows(in: entry)
        let total = rows.reduce(0) { $0 + $1.finding.bytes }
        let selected = rows.filter { model.isSelected($0.id) }.reduce(0) { $0 + $1.finding.bytes }
        if selected == 0 || selected == total { return ByteSize.human(total) }
        return "\(ByteSize.human(selected)) of \(ByteSize.human(total))"
    }
}

// MARK: - Groups

/// One group of one class: its rules, each with its items.
private struct GroupView: View {
    @Bindable var model: ReviewModel
    let items: ReviewItems
    let entry: ReviewItems.Entry

    var body: some View {
        let rows = items.rows(in: entry).filter { items.matches($0, model.query) }
        let byRule = Dictionary(grouping: rows, by: { $0.finding.rule })
        let rules = byRule.keys.sorted { a, b in
            let (x, y) = (byRule[a]!.reduce(0) { $0 + $1.finding.bytes }, byRule[b]!.reduce(0) { $0 + $1.finding.bytes })
            return x != y ? x > y : a < b
        }
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .firstTextBaseline) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(entry.group).font(.system(size: 17, weight: .semibold))
                    Text(blurb).font(.system(size: 12)).foregroundStyle(.secondary)
                }
                Spacer()
                Text(ByteSize.human(rows.reduce(0) { $0 + $1.finding.bytes })).font(.system(size: 15, weight: .semibold)).monospacedDigit()
            }
            .padding(16)
            Divider()
            List {
                ForEach(rules, id: \.self) { rule in
                    let members = byRule[rule]!.sorted { $0.finding.bytes > $1.finding.bytes }
                    Section {
                        ForEach(members) { row in
                            ItemRow(model: model, row: row, home: items.report.home)
                        }
                    } header: {
                        RuleHeader(model: model, rule: rule, rows: members)
                    }
                }
            }
            .listStyle(.inset)
        }
    }

    private var blurb: String {
        switch entry.riskClass {
        case .safe: "Caches that refill on the next install or build. Cleaned with the tool's own command when it has one."
        case .rebuildable: "Environments and build folders of projects untouched for a while that have a lock file."
        case .review: "Expensive to restore, or restorability is uncertain. Not selected by default; they go to the Trash."
        case .userData: "Your data. Shown, never cleaned."
        }
    }
}

/// A rule's line: one checkbox for all its selectable items.
private struct RuleHeader: View {
    @Bindable var model: ReviewModel
    let rule: String
    let rows: [ReviewItems.Row]

    var body: some View {
        let selectable = rows.filter(\.selectable)
        let selected = selectable.filter { model.isSelected($0.id) }.count
        let state: CheckState = selected == 0 ? .off : (selected == selectable.count ? .on : .mixed)
        HStack(spacing: 10) {
            CheckBox(state: state, enabled: !selectable.isEmpty, label: "Select all of \(rule)") {
                model.set(selectable.map(\.id), selected: state != .on)
            }
            Text(rule).font(.system(size: 13, weight: .semibold))
            Text("\(Wording.items(rows.count, "folder")) · \(rows.first?.action.map(CleanWording.action) ?? "report only")")
                .font(.system(size: 12))
                .foregroundStyle(.secondary)
            Spacer()
            Text(ByteSize.human(rows.reduce(0) { $0 + $1.finding.bytes })).font(.system(size: 12, weight: .semibold)).monospacedDigit()
        }
        .padding(.vertical, 2)
    }
}

// MARK: - Items

/// Every finding, filtered and sorted; user data stays in its own section.
private struct ItemsView: View {
    @Bindable var model: ReviewModel
    let items: ReviewItems

    var body: some View {
        let rows = items.items(matching: model.query, sort: model.sort)
        VStack(alignment: .leading, spacing: 0) {
            VStack(alignment: .leading, spacing: 2) {
                Text("Largest items").font(.system(size: 17, weight: .semibold))
                Text("Every folder the scan found. Tick or untick single folders; right-click to reveal or protect one.")
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
            }
            .padding(16)
            Divider()
            List(rows) { row in
                ItemRow(model: model, row: row, home: items.report.home)
            }
            .listStyle(.inset)
        }
    }
}

private struct ItemRow: View {
    @Bindable var model: ReviewModel
    let row: ReviewItems.Row
    let home: String
    @State private var confirmProtect = false

    var body: some View {
        let path = Wording.tilde(row.finding.path, home: home)
        HStack(spacing: 10) {
            CheckBox(state: model.isSelected(row.id) ? .on : .off, enabled: row.selectable, label: "Select \(path)") {
                model.toggle(row.id)
            }
            VStack(alignment: .leading, spacing: 1) {
                Text(path).font(.system(size: 13)).lineLimit(1).truncationMode(.middle)
                Text(secondLine).font(.system(size: 11)).foregroundStyle(row.lock == nil ? .secondary : ReportClassStyle.review).lineLimit(1)
            }
            Spacer(minLength: 8)
            VStack(alignment: .trailing, spacing: 1) {
                Text(ByteSize.human(row.finding.bytes)).font(.system(size: 13)).monospacedDigit()
                if let frees = frees {
                    Text("frees ~\(frees)").font(.system(size: 11)).foregroundStyle(.secondary).monospacedDigit()
                }
            }
        }
        .opacity(row.selectable || row.lock == .protected || row.lock == .cleaned ? 1 : 0.6)
        .contentShape(Rectangle())
        .contextMenu {
            Button("Reveal in Finder") {
                NSWorkspace.shared.activateFileViewerSelecting([URL(filePath: row.finding.path)])
            }
            Button("Protect this folder…") { confirmProtect = true }
                .disabled(!row.selectable)
        }
        .confirmationDialog("Protect \(path)?", isPresented: $confirmProtect) {
            Button("Protect") { model.protect(row.id) }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("An empty .ddc-keep file goes into the folder. DeepDriveCleaner then never selects or cleans it; delete the file to undo.")
        }
    }

    private var secondLine: String {
        let detail = CleanWording.detail(row)
        let parts = [row.finding.rule, row.action.map(CleanWording.action), detail.isEmpty ? nil : detail].compactMap { $0 }
        return parts.joined(separator: " · ")
    }

    /// When cleaning it frees visibly less than its size (clones shared elsewhere).
    private var frees: String? {
        let frees = ByteSize.human(row.finding.reclaimableBytes)
        return frees == ByteSize.human(row.finding.bytes) ? nil : frees
    }
}

// MARK: - Parts

enum CheckState { case on, off, mixed }

/// A checkbox with a mixed state, drawn from SF Symbols so it reads the same
/// in a list row and a section header.
struct CheckBox: View {
    let state: CheckState
    let enabled: Bool
    let label: String
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.system(size: 15))
                .foregroundStyle(state == .off ? AnyShapeStyle(.secondary) : AnyShapeStyle(PopoverView.accent))
        }
        .buttonStyle(.plain)
        .disabled(!enabled)
        .opacity(enabled ? 1 : 0.35)
        .accessibilityLabel(label)
        .accessibilityValue(state == .on ? "selected" : state == .mixed ? "partly selected" : "not selected")
    }

    private var symbol: String {
        switch state {
        case .on: "checkmark.square.fill"
        case .mixed: "minus.square.fill"
        case .off: "square"
        }
    }
}

struct Banner: View {
    let systemImage: String?
    let text: String

    var body: some View {
        HStack(spacing: 8) {
            if let systemImage {
                Image(systemName: systemImage).foregroundStyle(.secondary)
            } else {
                ProgressView().controlSize(.small)
            }
            Text(text).font(.system(size: 12))
            Spacer()
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
        .background(Color.secondary.opacity(0.08))
    }
}
