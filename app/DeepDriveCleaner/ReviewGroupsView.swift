import DDCKit
import SwiftUI

/// One sidebar entry in the Groups view (story "Review window: sorted
/// sidebar, Select all and folder groups"; board 9): Select all, then
/// subgroups by folder or by rule, in the sort header's order (story "Review
/// window: sort a group's rows by name, size or last change"), each folding
/// to its heading. What a subgroup's rows share is said on its heading.
struct GroupView: View {
    @Bindable var model: ReviewModel
    let items: ReviewItems
    let entry: ReviewItems.Entry

    /// A heading's checkbox, and the rows' checkboxes under its folder icon,
    /// so a path lines up with its folder's name.
    static let checkIndent: CGFloat = 26
    static let itemIndent: CGFloat = 49

    var body: some View {
        let layout = items.layout(entry, matching: model.query, sort: model.sort)
        let scanned = Wording.date(rfc3339: items.report.generatedAt)
        VStack(alignment: .leading, spacing: 0) {
            header(layout)
            Divider()
            if let all = layout.selectAll, !all.isEmpty {
                SelectAllRow(model: model, ids: all, group: entry.group, total: layout.selectAllTotal ?? "")
                Divider()
            }
            // A User data folder's picks, or a coding agent's sessions beside Select all.
            if !layout.pickScope.isEmpty {
                PickedRow(model: model, ids: layout.pickScope, group: entry.group)
                Divider()
            }
            if layout.count == 0 && layout.lines(collapsed: []).isEmpty {
                if model.query.isEmpty {
                    ContentUnavailableView("Nothing in This Group", systemImage: "tray")
                } else {
                    ContentUnavailableView.search(text: model.query)
                }
            } else {
                if layout.count > 0 {
                    // Under Select all, above the rows: the Name title over
                    // the paths, Size and Changed over the rows' columns.
                    SortHeader(model: model, leading: 16 + Self.checkIndent + 26)
                    Divider()
                }
                List(layout.lines(collapsed: model.collapsed)) { line in
                    switch line {
                    case .heading(let subgroup, let expanded):
                        SubgroupHeading(model: model, subgroup: subgroup, expanded: expanded, scanned: scanned)
                    case .item(let item):
                        ItemRow(model: model, row: item.row, home: items.report.home, scanned: scanned, shown: item.shown, detail: item.detail)
                            .padding(.leading, Self.itemIndent)
                    case .single(let single):
                        ItemRow(
                            model: model,
                            row: single.item.row,
                            home: items.report.home,
                            scanned: scanned,
                            heading: (single.title, single.subtitle),
                            shown: single.item.shown,
                            detail: single.detail
                        )
                        .padding(.leading, Self.checkIndent)
                    case .entry(let item):
                        ItemRow(model: model, row: item.row, home: items.report.home, scanned: scanned, shown: item.shown, detail: item.detail)
                            .padding(.leading, Self.checkIndent)
                    case .note(let text):
                        Text(text)
                            .font(.system(size: 12))
                            .foregroundStyle(.secondary)
                            .padding(.leading, Self.checkIndent + 26)
                            .padding(.vertical, 4)
                    }
                }
                .listStyle(.inset)
            }
        }
        .id(entry)
    }

    private func header(_ layout: GroupLayout) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 24) {
            VStack(alignment: .leading, spacing: 3) {
                Text(entry.group).font(.system(size: 20, weight: .semibold))
                ForEach([layout.blurb, layout.facts, layout.hint].compactMap { $0 }, id: \.self) { line in
                    Text(line)
                        .font(.system(size: 12))
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                if layout.opensTrash {
                    OpenTrashButton().controlSize(.small).padding(.top, 4)
                }
            }
            Spacer(minLength: 0)
            GroupAmount(model: model, entry: entry)
        }
        .padding(EdgeInsets(top: 16, leading: 20, bottom: 12, trailing: 20))
    }
}

/// "9.2 of 13.1 GiB": its own view, so ticking redraws it and not the list.
private struct GroupAmount: View {
    @Bindable var model: ReviewModel
    let entry: ReviewItems.Entry

    var body: some View {
        Text(model.amount(of: entry))
            .font(.system(size: 15, weight: .semibold))
            .monospacedDigit()
            .lineLimit(1)
    }
}

/// Every selectable row the filter shows; locked rows never change. Its
/// button cleans the selection among them only (story "Review window: clean
/// only the selection in this group"; board 12, variant B).
private struct SelectAllRow: View {
    @Bindable var model: ReviewModel
    let ids: [Int]
    /// The sidebar group, for "Clean 2 from Browser and app caches…".
    let group: String
    /// "9 folders · 3.8 GiB", when nothing here can be selected.
    let total: String

    var body: some View {
        let state = model.checkState(ids)
        let selectable = model.canSelect(ids)
        let label = Wording.cleanFrom(model.selectedRows(in: ids).count, group: group)
        HStack(spacing: 10) {
            CheckBox(state: state, enabled: selectable, label: "Select all") {
                model.toggleAll(ids)
            }
            Text("Select all").font(.system(size: 13, weight: .semibold)).fixedSize()
            Spacer()
            if selectable {
                // "2 of 3 · 7.2 GiB": the rows the filter shows, so it can be
                // less than the group's amount in the header.
                Text(model.scopeSummary(ids)).font(.system(size: 12)).foregroundStyle(.secondary).monospacedDigit().fixedSize()
                // The same sheet as Clean N items…, holding this group's ticks
                // only. No shortcut: Return stays the footer's.
                Button { model.openSheet(scope: ids) } label: {
                    Text(label).lineLimit(1).truncationMode(.middle)
                }
                .buttonStyle(.bordered)
                .frame(maxWidth: 280)
                .help(label)
                .disabled(!model.canClean(scope: ids))
            } else {
                Text(total).font(.system(size: 12)).foregroundStyle(.secondary).monospacedDigit()
            }
        }
        // The list's rows start about 16 pt in: line its checkbox up with theirs.
        .padding(.leading, 16 + GroupView.checkIndent)
        .padding(.trailing, 20)
        .frame(minHeight: 40)
        .background(Color.primary.opacity(0.04))
    }
}

/// A User data folder's picked entries: no checkbox, since nothing here is
/// picked for the user; its button moves the picks among them only (story
/// "User data: move a folder's picked files to the Trash from that folder").
private struct PickedRow: View {
    @Bindable var model: ReviewModel
    let ids: [Int]
    /// The sidebar group, for "Move 2 from Movies to the Trash…".
    let group: String

    var body: some View {
        let label = Wording.moveFrom(model.selectedRows(in: ids).count, group: group)
        HStack(spacing: 10) {
            Text(model.pickedSummary(ids)).font(.system(size: 12)).foregroundStyle(.secondary).monospacedDigit().fixedSize()
            Spacer()
            // The same sheet as Clean N items…, holding this folder's picks
            // only. No shortcut: Return stays the footer's.
            Button { model.openSheet(scope: ids) } label: {
                Text(label).lineLimit(1).truncationMode(.middle)
            }
            .buttonStyle(.bordered)
            .frame(maxWidth: 320)
            .help(label)
            .disabled(!model.canClean(scope: ids))
        }
        // In line with the rows' checkboxes.
        .padding(.leading, 16 + GroupView.checkIndent)
        .padding(.trailing, 20)
        .frame(minHeight: 40)
        .background(Color.primary.opacity(0.04))
    }
}

/// A subgroup's heading: fold, select its rows, its name and what its rows share.
private struct SubgroupHeading: View {
    @Bindable var model: ReviewModel
    let subgroup: GroupLayout.Subgroup
    let expanded: Bool
    /// The scan's time: the Changed column's Today and Yesterday.
    let scanned: Date?

    var body: some View {
        HStack(spacing: 6) {
            // No animation: a subgroup of hundreds of rows folds at once.
            Button { model.toggleCollapsed(subgroup.key) } label: {
                Image(systemName: "chevron.right")
                    .font(.system(size: 11, weight: .semibold))
                    .rotationEffect(.degrees(expanded ? 90 : 0))
                    .frame(width: GroupView.checkIndent - 6, height: 24)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .foregroundStyle(.secondary)
            .accessibilityLabel(subgroup.title)
            .accessibilityValue(expanded ? "expanded" : "collapsed")
            .accessibilityHint(subgroup.foldHint(expanded: expanded))
            if subgroup.checkable {
                CheckBox(state: model.checkState(subgroup.ids), enabled: model.canSelect(subgroup.ids), label: "Select all in \(subgroup.title)") {
                    model.toggleAll(subgroup.ids)
                }
            }
            if subgroup.isFolder {
                Image(systemName: "folder")
                    .font(.system(size: 14))
                    .foregroundStyle(.secondary)
                    .accessibilityHidden(true)
            }
            VStack(alignment: .leading, spacing: 1) {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(subgroup.title).font(.system(size: 15, weight: .semibold)).lineLimit(1).truncationMode(.middle)
                    Text(subgroup.subtitle).font(.system(size: 12)).foregroundStyle(.secondary).lineLimit(1)
                }
                if let hint = subgroup.hint {
                    Text(hint).font(.system(size: 11)).foregroundStyle(.secondary).lineLimit(1)
                }
            }
            .accessibilityElement(children: .combine)
            .accessibilityAddTraits(.isHeader)
            Spacer(minLength: 8)
            // Its total, and the newest change among its folders.
            SortColumns(
                changed: CleanWording.changed(subgroup.modified, scanned: scanned),
                tooltip: CleanWording.subgroupChangedHelp(subgroup.modified),
                weight: .semibold
            ) {
                Text(ByteSize.human(subgroup.bytes)).font(.system(size: 13, weight: .semibold)).monospacedDigit()
            }
        }
        .padding(.top, 6)
        .padding(.bottom, 2)
    }
}
