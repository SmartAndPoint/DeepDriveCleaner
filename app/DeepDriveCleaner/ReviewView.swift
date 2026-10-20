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
                    Sidebar(model: model, items: items).frame(width: 330)
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
            SelectionFooter(model: model, close: close)
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
}

/// The selection and Clean N items…: its own view, so a tick redraws it and
/// not the window around it.
private struct SelectionFooter: View {
    @Bindable var model: ReviewModel
    let close: () -> Void

    var body: some View {
        let rows = model.selectedRows
        let review = rows.filter { $0.riskClass == .review }.count
        var sub = ["frees about \(ByteSize.human(model.selectedReclaimable))"]
        if review > 0 { sub.append("\(Wording.items(review, "review item")) \(review == 1 ? "goes to the Trash or runs its tool" : "go to the Trash or run their tool")") }
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

/// Classes and their groups, largest first, on the window's own background;
/// each class a tinted section (board 9, `tint`).
private struct Sidebar: View {
    @Bindable var model: ReviewModel
    let items: ReviewItems

    var body: some View {
        let sections = items.sidebar
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                ForEach(sections, id: \.riskClass) { section in
                    VStack(alignment: .leading, spacing: 1) {
                        HStack(spacing: 6) {
                            RoundedRectangle(cornerRadius: 2).fill(ReportClassStyle.color(section.riskClass)).frame(width: 8, height: 8)
                            Text(ReportClassStyle.name(section.riskClass))
                                .textCase(.uppercase)
                                .font(.system(size: 11, weight: .bold))
                                .tracking(0.6)
                                .foregroundStyle(ReportClassStyle.ink(section.riskClass))
                        }
                        .padding(.horizontal, 8)
                        .padding(.bottom, 6)
                        .accessibilityElement(children: .combine)
                        .accessibilityAddTraits(.isHeader)
                        ForEach(section.entries, id: \.self) { entry in
                            SidebarEntry(model: model, entry: entry)
                        }
                        if section.riskClass == .userData {
                            Text("Downloads, media, backups and folders you protect. Shown, never cleaned.")
                                .font(.system(size: 11))
                                .foregroundStyle(.secondary)
                                .fixedSize(horizontal: false, vertical: true)
                                .padding(.horizontal, 8)
                                .padding(.top, 6)
                        }
                    }
                    .padding(EdgeInsets(top: 8, leading: 6, bottom: 6, trailing: 6))
                    .background(ReportClassStyle.sectionTint(section.riskClass), in: RoundedRectangle(cornerRadius: 10))
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 14)
        }
        .background(Color(nsColor: .controlBackgroundColor))
        .focusable()
        .onMoveCommand { move($0, in: sections.flatMap(\.entries)) }
    }

    /// Up and down walk the entries, as the list did.
    private func move(_ direction: MoveCommandDirection, in entries: [ReviewItems.Entry]) {
        guard !entries.isEmpty else { return }
        let index = model.entry.flatMap(entries.firstIndex(of:))
        let next: Int
        switch direction {
        case .up: next = max((index ?? entries.count) - 1, 0)
        case .down: next = min((index ?? -1) + 1, entries.count - 1)
        default: return
        }
        model.entry = entries[next]
        model.mode = .groups
    }
}

/// A group in the sidebar, "9.2 of 13.1 GiB" when part of it is selected.
private struct SidebarEntry: View {
    @Bindable var model: ReviewModel
    let entry: ReviewItems.Entry

    var body: some View {
        let picked = model.entry == entry
        Button {
            model.entry = entry
            model.mode = .groups
        } label: {
            HStack(spacing: 8) {
                Text(entry.group).font(.system(size: 13, weight: picked ? .semibold : .regular)).lineLimit(1)
                Spacer(minLength: 6)
                // Darker than .secondary: 4.5:1 on the picked tint too.
                Text(model.amount(of: entry)).font(.system(size: 12)).foregroundStyle(Color.primary.opacity(0.72)).monospacedDigit().lineLimit(1)
                    .fixedSize()
            }
            .padding(.horizontal, 8)
            .frame(minHeight: 32)
            .background(picked ? ReportClassStyle.selectedTint(entry.riskClass) : Color.clear, in: RoundedRectangle(cornerRadius: 7))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        // A group can sit in two classes: say which.
        .accessibilityLabel("\(entry.group), \(ReportClassStyle.name(entry.riskClass))")
        .accessibilityValue(model.amount(of: entry))
        .accessibilityAddTraits(picked ? .isSelected : [])
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

/// A finding's row. The Items view shows its full line; the Groups view
/// passes the path relative to its folder and the row's own facts, and a
/// one-item rule its title above them.
struct ItemRow: View {
    @Bindable var model: ReviewModel
    let row: ReviewItems.Row
    let home: String
    var heading: (title: String, subtitle: String)?
    var shown: String?
    var detail: String?
    @State private var confirmProtect = false

    var body: some View {
        let path = Wording.tilde(row.finding.path, home: home)
        let second = detail ?? secondLine
        // In the Groups view a lock the heading states is not repeated, nor coloured.
        let warns = detail == nil ? row.lock != nil : CleanWording.ownLock(row) != nil
        HStack(spacing: 10) {
            CheckBox(state: model.isSelected(row.id) ? .on : .off, enabled: row.selectable, label: "Select \(path)") {
                model.toggle(row.id)
            }
            VStack(alignment: .leading, spacing: 1) {
                if let heading {
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Text(heading.title).font(.system(size: 15, weight: .semibold)).lineLimit(1)
                        Text(heading.subtitle).font(.system(size: 12)).foregroundStyle(.secondary).lineLimit(1)
                    }
                }
                Text(shown ?? path).font(.system(size: 13)).lineLimit(1).truncationMode(.middle)
                if !second.isEmpty {
                    Text(second).font(.system(size: 11)).foregroundStyle(warns ? ReportClassStyle.review : .secondary).lineLimit(1)
                }
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
