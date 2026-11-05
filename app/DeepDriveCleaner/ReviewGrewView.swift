import AppKit
import DDCKit
import SwiftUI

/// What the "Grew since …" entry's not-read section opens in the app.
@MainActor
struct GrewLinks {
    let openPrivacy: @MainActor () -> Void
    let openFolders: @MainActor () -> Void

    static let none = GrewLinks(openPrivacy: {}, openFolders: {})
}

/// The sidebar row above the class sections (story "Popover and Review: what
/// grew since the earlier scan").
struct GrewSidebarEntry: View {
    @Bindable var model: ReviewModel
    let grew: GrewEntry

    var body: some View {
        let picked = model.grewSince && !model.wholeDisk && model.mode == .groups
        Button {
            model.showGrew()
        } label: {
            HStack(spacing: 8) {
                Image(systemName: "arrow.up.right").foregroundStyle(.secondary)
                Text("What grew").font(.system(size: 13, weight: picked ? .semibold : .regular)).lineLimit(1)
                Spacer(minLength: 6)
                if let amount = grew.amount {
                    Text(amount).font(.system(size: 12)).foregroundStyle(Color.primary.opacity(0.72)).monospacedDigit().lineLimit(1)
                        .fixedSize()
                }
            }
            .padding(.horizontal, 8)
            .frame(minHeight: 32)
            .background(picked ? Color.secondary.opacity(0.16) : Color.clear, in: RoundedRectangle(cornerRadius: 7))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help(grew.title)
        .accessibilityLabel(grew.title)
        .accessibilityValue(grew.amount ?? "")
        .accessibilityAddTraits(picked ? .isSelected : [])
    }
}

/// The entry's detail: the same rows as their groups, by their change, then
/// what no row explains. Ticks are the window's; the footer's Clean N
/// items… stays the only way to the sheet.
struct GrewView: View {
    @Bindable var model: ReviewModel
    let items: ReviewItems
    let grew: GrewEntry
    let links: GrewLinks

    private enum Anchor: Hashable {
        case top, notRead
    }

    var body: some View {
        let scanned = Wording.date(rfc3339: items.report.generatedAt)
        ScrollViewReader { proxy in
            List {
                VStack(alignment: .leading, spacing: 4) {
                    Text(grew.title).font(.system(size: 17, weight: .semibold))
                    Text(grew.summary).font(.system(size: 12)).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                    if let note = grew.rootsNote {
                        Text(note).font(.system(size: 12)).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                    }
                }
                .padding(.vertical, 6)
                .id(Anchor.top)
                if !grew.groups.isEmpty {
                    GrewHeader(model: model)
                }
                ForEach(grew.groups) { group in
                    Section {
                        ForEach(group.rows) { grewRow in
                            if let row = items.row(grewRow.id) {
                                ItemRow(model: model, row: row, home: items.report.home, scanned: scanned, change: grewRow.change)
                            }
                        }
                    } header: {
                        GrewGroupHeader(model: model, group: group)
                    }
                }
                if let elsewhere = grew.elsewhere {
                    Section {
                        Text("ddc does not clean these. Their folders are not listed yet.")
                            .font(.system(size: 12)).foregroundStyle(.secondary)
                    } header: {
                        SectionTitle(title: "Elsewhere in your home folder", amount: elsewhere)
                    }
                }
                if grew.rest != nil || !grew.notRead.isEmpty {
                    Section {
                        ForEach(grew.notRead, id: \.kind) { kind in
                            HStack(spacing: 8) {
                                Text(kind.text).font(.system(size: 12)).foregroundStyle(.secondary)
                                Spacer(minLength: 8)
                                action(for: kind.kind)
                            }
                        }
                        HStack(spacing: 8) {
                            Text("Outside your home folder: see what takes the rest of the disk.")
                                .font(.system(size: 12)).foregroundStyle(.secondary)
                            Spacer(minLength: 8)
                            Button("Whole Disk…") {
                                model.leaveGrew()
                                model.wholeDisk = true
                            }
                            .buttonStyle(.link)
                        }
                        if let note = grew.cleanedNote {
                            Text(note).font(.system(size: 12)).foregroundStyle(.secondary)
                        }
                    } header: {
                        SectionTitle(title: "In folders ddc did not read, or outside your home folder", amount: grew.rest)
                    }
                    .id(Anchor.notRead)
                }
                if !grew.gone.isEmpty || grew.cleanedSince != nil {
                    Section {
                        DisclosureGroup("Cleaned by you or gone (\(grew.gone.count))") {
                            if let since = grew.cleanedSince {
                                Text(since).font(.system(size: 12)).foregroundStyle(.secondary)
                            }
                            ForEach(grew.gone) { gone in
                                HStack {
                                    Text(gone.path).font(.system(size: 12)).lineLimit(1).truncationMode(.middle)
                                    Spacer(minLength: 8)
                                    Text(gone.text).font(.system(size: 12)).foregroundStyle(.secondary).monospacedDigit()
                                }
                            }
                        }
                        .font(.system(size: 13))
                    }
                }
            }
            .listStyle(.inset)
            .onAppear { scroll(proxy) }
            .onChange(of: model.grewSection) { scroll(proxy) }
        }
    }

    private func scroll(_ proxy: ScrollViewProxy) {
        guard let section = model.grewSection else { return }
        proxy.scrollTo(section == .notRead ? Anchor.notRead : Anchor.top, anchor: .top)
        model.grewSection = nil
    }

    @ViewBuilder private func action(for kind: ChangesReport.NotReadKind) -> some View {
        switch kind {
        case .prompting:
            HStack(spacing: 10) {
                if grew.offersPrivacy {
                    Button("Open Privacy Settings", action: links.openPrivacy).buttonStyle(.link)
                }
                if !model.scansPaused {
                    Button("Scan Now") { Task { await model.scanAgain() } }
                        .buttonStyle(.link)
                        .disabled(model.rescanning != nil || model.isCleaning)
                }
            }
        case .skip:
            Button("Settings…", action: links.openFolders).buttonStyle(.link)
        default:
            EmptyView()
        }
    }
}

/// The entry's column titles: Name, Size and Change, its own order.
private struct GrewHeader: View {
    @Bindable var model: ReviewModel

    var body: some View {
        HStack(spacing: SortHeader.columnSpacing) {
            title(.name, "Name").frame(maxWidth: .infinity, alignment: .leading)
            title(.size, "Size").frame(width: SortHeader.sizeWidth, alignment: .trailing)
            title(.change, "Change").frame(width: SortHeader.changedWidth, alignment: .trailing)
        }
        .padding(.leading, 26)
        .frame(minHeight: 24)
    }

    private func title(_ column: GrewEntry.Sort.Column, _ text: String) -> some View {
        let sorted = model.grewSort.column == column
        return Button { model.sortGrew(by: column) } label: {
            HStack(spacing: 3) {
                Text(text)
                if sorted {
                    Image(systemName: model.grewSort.ascending ? "chevron.up" : "chevron.down")
                        .font(.system(size: 9, weight: .bold))
                        .accessibilityHidden(true)
                }
            }
            .font(.system(size: 11, weight: sorted ? .semibold : .regular))
            .foregroundStyle(sorted ? AnyShapeStyle(.primary) : AnyShapeStyle(.secondary))
            .contentShape(Rectangle())
        }
        .buttonStyle(.borderless)
        .help("Sort by \(text)")
        .accessibilityLabel(sorted ? "\(text), sorted \(model.grewSort.ascending ? "ascending" : "descending")" : text)
        .accessibilityHint("Sorts the rows by \(text)")
        .accessibilityAddTraits(sorted ? .isSelected : [])
    }
}

/// A group's heading in the entry: its checkbox only where the group's own
/// entry has Select all, its name and its change.
private struct GrewGroupHeader: View {
    @Bindable var model: ReviewModel
    let group: GrewEntry.Group

    var body: some View {
        HStack(spacing: 10) {
            if let ids = group.selectAll {
                CheckBox(state: model.checkState(ids), enabled: model.canSelect(ids), label: "Select the folders of \(group.entry.group) that grew") {
                    model.toggleAll(ids)
                }
            }
            Text(group.entry.group).font(.system(size: 13, weight: .semibold))
            Text(ReportClassStyle.name(group.entry.riskClass)).font(.system(size: 11)).foregroundStyle(.secondary)
            Spacer(minLength: 8)
            Text(GrewEntry.amount(group.delta)).font(.system(size: 12)).foregroundStyle(.secondary).monospacedDigit()
        }
    }
}

private struct SectionTitle: View {
    let title: String
    let amount: String?

    var body: some View {
        HStack {
            Text(title).font(.system(size: 13, weight: .semibold))
            Spacer(minLength: 8)
            if let amount {
                Text(amount).font(.system(size: 12)).foregroundStyle(.secondary).monospacedDigit()
            }
        }
    }
}
