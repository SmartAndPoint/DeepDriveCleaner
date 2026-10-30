import DDCKit
import SwiftUI

/// The column titles above a group's rows and above the Items list (story
/// "Review window: sort a group's rows by name, size or last change"): Name,
/// Size and Changed, each a button that sorts by its column and, pressed
/// again, reverses it. One sort for both views (`ReviewModel.sort`).
///
/// No `Table`: the rows stay `ItemRow`s in the `List`, whose Size and Changed
/// columns take these widths, right-aligned, so the titles sit over them.
struct SortHeader: View {
    @Bindable var model: ReviewModel
    /// Where the Name title starts: over the rows' paths.
    let leading: CGFloat

    /// The rows' and subgroup headings' fixed columns, and the gap before each.
    static let sizeWidth: CGFloat = 104
    static let changedWidth: CGFloat = 92
    static let columnSpacing: CGFloat = 10
    /// Where the list's rows end, as the Select all row has it.
    static let trailing: CGFloat = 20

    var body: some View {
        HStack(spacing: Self.columnSpacing) {
            title(.name).frame(maxWidth: .infinity, alignment: .leading)
            title(.size).frame(width: Self.sizeWidth, alignment: .trailing)
            title(.changed).frame(width: Self.changedWidth, alignment: .trailing)
        }
        .padding(.leading, leading)
        .padding(.trailing, Self.trailing)
        .frame(minHeight: 28)
    }

    /// A column title: a borderless button, so with keyboard navigation on
    /// Tab reaches it with a focus ring and Space presses it.
    private func title(_ column: ReviewSort.Column) -> some View {
        let sort = model.sort
        let sorted = sort.column == column
        return Button { model.sortBy(column) } label: {
            HStack(spacing: 3) {
                Text(CleanWording.sortTitle(column))
                if sorted {
                    Image(systemName: sort.ascending ? "chevron.up" : "chevron.down")
                        .font(.system(size: 9, weight: .bold))
                        .accessibilityHidden(true)
                }
            }
            .font(.system(size: 11, weight: sorted ? .semibold : .regular))
            .foregroundStyle(sorted ? AnyShapeStyle(.primary) : AnyShapeStyle(.secondary))
            .contentShape(Rectangle())
        }
        .buttonStyle(.borderless)
        .help(CleanWording.sortHint(column, sort: sort))
        // "Changed, sorted oldest first".
        .accessibilityLabel(CleanWording.sortLabel(column, sort: sort))
        .accessibilityHint(CleanWording.sortHint(column, sort: sort))
        .accessibilityAddTraits(sorted ? .isSelected : [])
    }
}

/// A row's or a subgroup heading's Size and Changed columns: fixed widths,
/// right-aligned, under the sort header's titles.
struct SortColumns<Size: View>: View {
    let changed: String
    /// The Changed column's tooltip; none with no date.
    let tooltip: String?
    var weight: Font.Weight = .regular
    @ViewBuilder let size: () -> Size

    var body: some View {
        HStack(spacing: SortHeader.columnSpacing) {
            size().frame(width: SortHeader.sizeWidth, alignment: .trailing)
            Text(changed)
                .font(.system(size: 12, weight: weight))
                .foregroundStyle(.secondary)
                .monospacedDigit()
                .lineLimit(1)
                .frame(width: SortHeader.changedWidth, alignment: .trailing)
                .help(tooltip ?? "")
        }
    }
}
