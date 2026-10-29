import AppKit
import DDCKit
import SwiftUI

/// The Review window (artboard 4): classes and groups on the left, the
/// findings on the right, the selection and Clean N items… at the bottom.
/// Nothing here cleans; Clean N items… opens the confirmation sheet.
struct ReviewView: View {
    @Bindable var model: ReviewModel
    let close: () -> Void
    /// The Whole disk detail, shared with its own window.
    let wholeDisk: () -> DiskView
    /// While the user types a filter, Return stays in it instead of opening
    /// the Clean sheet.
    @FocusState private var filterFocused: Bool

    var body: some View {
        VStack(spacing: 0) {
            // Nothing in the Whole disk detail is filtered, listed or cleaned.
            if !model.wholeDisk {
                header
                Divider()
            }
            if let notice = model.notice {
                Banner(systemImage: "info.circle", text: notice)
            }
            // A report of other roots (`ddc scan <path>` in the terminal), above the groups.
            if let scope = model.scopeNotice, !model.wholeDisk {
                Banner(systemImage: "exclamationmark.triangle", text: scope, action: model.scansPaused ? nil : (
                    "Scan Now", model.rescanning == nil, { Task { await model.scanAgain() } }
                ))
            }
            // The user's "Skip entirely" folders: not read, so nothing there is listed.
            if let notScanned = model.items?.notScanned {
                Banner(systemImage: "eye.slash", text: notScanned)
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
                    if model.wholeDisk { wholeDisk() } else { content(items) }
                }
            } else {
                ContentUnavailableView {
                    Label("No Scan Yet", systemImage: "internaldrive")
                } description: {
                    Text(model.scansPaused
                        ? "Scans are paused. Resume them from the menu bar icon."
                        : "Scan from the menu bar first; the findings show here.")
                } actions: {
                    if !model.scansPaused {
                        Button("Scan Now") { Task { await model.scanAgain() } }.disabled(model.rescanning != nil)
                    }
                }
            }
            Divider()
            if model.wholeDisk {
                // Close and Esc stay: the detail has no selection to clean.
                HStack {
                    Spacer()
                    Button("Close", action: close).keyboardShortcut(.cancelAction)
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 12)
            } else {
                SelectionFooter(model: model, typing: filterFocused && !model.query.isEmpty, close: close)
            }
        }
        .frame(minWidth: 820, minHeight: 520)
        .sheet(isPresented: Binding(get: { model.sheet != nil }, set: { if !$0 { model.closeSheet() } })) {
            ConfirmSheet(model: model)
        }
        // Edit → Find (⌘F) while this window is key.
        .onReceive(NotificationCenter.default.publisher(for: .findInReview)) { _ in filterFocused = true }
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
                    Text("Biggest First").tag(ReviewItems.Sort.size)
                    Text("By Path").tag(ReviewItems.Sort.path)
                    Text("Most Idle First").tag(ReviewItems.Sort.idle)
                }
                .frame(width: 170)
            }
            HStack(spacing: 6) {
                Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
                TextField("Filter by path or rule", text: $model.query)
                    .textFieldStyle(.plain)
                    .focused($filterFocused)
                if !model.query.isEmpty {
                    // Esc clears the filter first; with it empty, Esc is Close's.
                    Button { model.query = "" } label: { Image(systemName: "xmark.circle.fill") }
                        .buttonStyle(.plain)
                        .foregroundStyle(.secondary)
                        .keyboardShortcut(.cancelAction)
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
                Button("Scan Again") { Task { await model.scanAgain() } }
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
                ContentUnavailableView("Nothing Found", systemImage: "checkmark.circle", description: Text("The last scan found nothing to clean."))
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
    /// The filter has the focus and holds text.
    let typing: Bool
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
            // Esc closes the window (owner, 2026-10-19), unless it is
            // clearing the filter.
            Button("Close", action: close).keyboardShortcut(model.query.isEmpty ? .cancelAction : nil)
            Button("Clean \(Wording.items(rows.count))…") { model.openSheet() }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(typing ? nil : .defaultAction)
                .disabled(!model.canClean(scope: nil))
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
                            Text("Your own files. Nothing is selected for you; pick files one by one and they go to the Trash.")
                                .font(.system(size: 11))
                                .foregroundStyle(.secondary)
                                .fixedSize(horizontal: false, vertical: true)
                                .padding(.horizontal, 8)
                                .padding(.top, 6)
                            if items.backgroundSkipped {
                                Label(CleanWording.backgroundSkipped, systemImage: "info.circle")
                                    .font(.system(size: 11))
                                    .foregroundStyle(.secondary)
                                    .fixedSize(horizontal: false, vertical: true)
                                    .padding(.horizontal, 8)
                                    .padding(.top, 4)
                            }
                        }
                    }
                    .padding(EdgeInsets(top: 8, leading: 6, bottom: 6, trailing: 6))
                    .background(ReportClassStyle.sectionTint(section.riskClass), in: RoundedRectangle(cornerRadius: 10))
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 14)
            WholeDiskEntry(model: model).padding(.horizontal, 12).padding(.bottom, 14)
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
        model.wholeDisk = false
    }
}

/// The sidebar's last row: what lies outside the home folder.
private struct WholeDiskEntry: View {
    @Bindable var model: ReviewModel

    var body: some View {
        Button {
            model.wholeDisk = true
        } label: {
            HStack(spacing: 8) {
                Image(systemName: "internaldrive").foregroundStyle(.secondary)
                Text("Whole disk").font(.system(size: 13, weight: model.wholeDisk ? .semibold : .regular))
                Spacer()
            }
            .padding(.horizontal, 8)
            .frame(minHeight: 32)
            .background(model.wholeDisk ? Color.secondary.opacity(0.16) : Color.clear, in: RoundedRectangle(cornerRadius: 7))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(model.wholeDisk ? .isSelected : [])
    }
}

/// A group in the sidebar, "9.2 of 13.1 GiB" when part of it is selected.
private struct SidebarEntry: View {
    @Bindable var model: ReviewModel
    let entry: ReviewItems.Entry

    var body: some View {
        let picked = model.entry == entry && !model.wholeDisk
        Button {
            model.entry = entry
            model.mode = .groups
            model.wholeDisk = false
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
                Text("Every folder the scan found. Tick or untick single folders; right-click to show or protect one.")
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
            }
            .padding(16)
            Divider()
            if rows.isEmpty {
                if model.query.isEmpty {
                    ContentUnavailableView("Nothing Found", systemImage: "checkmark.circle", description: Text("The last scan found nothing to clean."))
                } else {
                    ContentUnavailableView.search(text: model.query)
                }
            } else {
                List(rows) { row in
                    ItemRow(model: model, row: row, home: items.report.home)
                }
                .listStyle(.inset)
            }
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
            if row.userEntry != nil, case .entry? = row.lock {
                // A locked file of the user's own: a lock, its reason on the line below.
                Image(systemName: "lock.fill")
                    .font(.system(size: 13))
                    .foregroundStyle(.secondary)
                    .frame(width: 16)
                    .accessibilityLabel("Locked")
            } else {
                CheckBox(state: model.isSelected(row.id) ? .on : .off, enabled: row.selectable, label: "Select \(path)") {
                    model.toggle(row.id)
                }
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
                    Text(second).font(.system(size: 11)).foregroundStyle(warns ? ReportClassStyle.ink(.review) : .secondary).lineLimit(1)
                }
                // Its tool ran in the last clean: said until the next scan.
                if let note = model.toolNote(for: row.id) {
                    Text(note).font(.system(size: 11)).foregroundStyle(.secondary).lineLimit(1)
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
            Button("Show in Finder") {
                NSWorkspace.shared.activateFileViewerSelecting([URL(filePath: row.finding.path)])
            }
            // Never for a file, nor inside a device backup.
            if model.items?.canProtect(row) ?? true {
                Button("Protect This Folder…") { confirmProtect = true }
                    .disabled(!model.canProtect(row.id))
            }
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
                .foregroundStyle(state == .off ? AnyShapeStyle(.secondary) : AnyShapeStyle(Color.accentColor))
        }
        .buttonStyle(.plain)
        .disabled(!enabled)
        .opacity(enabled ? 1 : 0.35)
        .accessibilityLabel(label)
        .accessibilityAddTraits(.isToggle)
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
    /// A button's title, whether it is enabled, and what it does.
    var action: (title: String, enabled: Bool, run: () -> Void)?

    var body: some View {
        HStack(spacing: 8) {
            if let systemImage {
                Image(systemName: systemImage).foregroundStyle(.secondary)
            } else {
                ProgressView().controlSize(.small)
            }
            Text(text).font(.system(size: 12)).fixedSize(horizontal: false, vertical: true)
            Spacer()
            if let action {
                Button(action.title, action: action.run).controlSize(.small).disabled(!action.enabled)
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
        .background(Color.secondary.opacity(0.08))
    }
}
