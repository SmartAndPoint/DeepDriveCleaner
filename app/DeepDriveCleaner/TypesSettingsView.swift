import DDCKit
import SwiftUI

/// Settings → Types (artboard 10): one switch per group of `rules --json`,
/// with its classes, then the idle age and the minimum size. A type
/// switched off stays in the report but is never selected; the popover and
/// Review apply it at once, with no scan.
struct TypesSettingsView: View {
    let model: SettingsModel

    var body: some View {
        Form {
            SettingsProblem(model: model)
            Section {
                if model.types.isEmpty {
                    Text("The types show once the command-line tool has listed them.").foregroundStyle(.secondary)
                }
                ForEach(model.types) { row in
                    Toggle(isOn: Binding(get: { row.isOn }, set: { model.setType(row, on: $0) })) {
                        HStack(alignment: .firstTextBaseline) {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(row.label)
                                if let help = row.help { Text(help).settingsHelp() }
                            }
                            Spacer()
                            HStack(spacing: 4) {
                                ForEach(row.classes, id: \.self) { ClassChip(riskClass: $0) }
                            }
                        }
                    }
                }
            } footer: {
                Text("What the scan offers to clean. A type switched off stays in the report but is never selected.")
                    .settingsHelp()
            }
            Section {
                LabeledContent {
                    NumberField(value: Binding(get: { model.minAgeDays }, set: { model.setMinAgeDays($0) }), range: 0...3650, unit: "days")
                } label: {
                    Text("Projects count as idle after")
                    Text("From the next scan; the last report keeps the age it was scanned with.")
                }
                LabeledContent {
                    NumberField(value: Binding(get: { model.minSizeMB }, set: { model.setMinSizeMB($0) }), range: 0...1_000_000, unit: "MB")
                } label: {
                    Text("Hide items smaller than")
                    Text("Smaller items stay in the report but are never selected.")
                }
            }
        }
        .scrollingSettingsPane()
    }
}

/// "Safe", "Rebuildable", "Review": a class in its own ink.
private struct ClassChip: View {
    let riskClass: ScanReport.RiskClass

    var body: some View {
        Text(ReportClassStyle.name(riskClass))
            .font(.system(size: 10, weight: .semibold))
            .foregroundStyle(ReportClassStyle.ink(riskClass))
            .padding(.horizontal, 6)
            .padding(.vertical, 1)
            .background(ReportClassStyle.selectedTint(riskClass), in: Capsule())
    }
}

/// A number with its unit, typed or stepped.
private struct NumberField: View {
    let value: Binding<Int>
    let range: ClosedRange<Int>
    let unit: String

    var body: some View {
        HStack(spacing: 6) {
            TextField(unit, value: value, format: .number)
                .labelsHidden()
                .multilineTextAlignment(.trailing)
                .frame(width: 64)
            Stepper(unit, value: value, in: range).labelsHidden()
            Text(unit)
        }
    }
}
