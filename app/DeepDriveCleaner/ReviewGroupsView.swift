import DDCKit
import SwiftUI

/// One sidebar entry in the Groups view (story "Review window: sorted
/// sidebar, Select all and folder groups"; board 9): Select all, then
/// subgroups by folder or by rule, largest first, each folding to its
/// heading. What a subgroup's rows share is said on its heading.
struct GroupView: View {
    @Bindable var model: ReviewModel
    let items: ReviewItems
    let entry: ReviewItems.Entry

    /// A heading's checkbox, and the rows' checkboxes under its folder icon,
    /// so a path lines up with its folder's name.
    static let checkIndent: CGFloat = 26
    static let itemIndent: CGFloat = 49

    var body: some View {
        let layout = items.layout(entry, matching: model.query)
        VStack(alignment: .leading, spacing: 0) {
            header(layout)
            Divider()
            if let all = layout.selectAll, !all.isEmpty {
                SelectAllRow(model: model, ids: all, summary: "\(Wording.items(layout.count, "folder")) · \(ByteSize.human(layout.bytes))")
                Divider()
            }
            if layout.count == 0 && layout.lines(collapsed: []).isEmpty {
                Text(model.query.isEmpty ? "Nothing in this group." : "Nothing here matches the filter.")
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                List(layout.lines(collapsed: model.collapsed)) { line in
                    switch line {
                    case .heading(let subgroup, let expanded):
                        SubgroupHeading(model: model, subgroup: subgroup, expanded: expanded)
                    case .item(let item):
                        ItemRow(model: model, row: item.row, home: items.report.home, shown: item.shown, detail: item.detail)
                            .padding(.leading, Self.itemIndent)
                    case .single(let single):
                        ItemRow(
                            model: model,
                            row: single.item.row,
                            home: items.report.home,
                            heading: (single.title, single.subtitle),
                            shown: single.item.shown,
                            detail: single.detail
                        )
                        .padding(.leading, Self.checkIndent)
                    case .entry(let item):
                        ItemRow(model: model, row: item.row, home: items.report.home, shown: item.shown, detail: item.detail)
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

/// Every selectable row the filter shows; locked rows never change.
private struct SelectAllRow: View {
    @Bindable var model: ReviewModel
    let ids: [Int]
    /// "9 folders · 3.8 GiB".
    let summary: String

    var body: some View {
        let state = model.checkState(ids)
        HStack(spacing: 10) {
            CheckBox(state: state, enabled: model.canSelect(ids), label: "Select all") {
                model.toggleAll(ids)
            }
            Text("Select all").font(.system(size: 13, weight: .semibold))
            Spacer()
            Text(summary).font(.system(size: 12)).foregroundStyle(.secondary).monospacedDigit()
        }
        // The list's rows start about 16 pt in: line its checkbox up with theirs.
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
            .accessibilityHint(expanded ? "Hides its folders" : "Shows its folders")
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
            Text(ByteSize.human(subgroup.bytes)).font(.system(size: 13, weight: .semibold)).monospacedDigit()
        }
        .padding(.top, 6)
        .padding(.bottom, 2)
    }
}
