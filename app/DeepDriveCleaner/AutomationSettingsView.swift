import DDCKit
import SwiftUI

/// Settings → Automation (artboard 10): what a scheduled scan does, when it
/// notifies, and how a clean treats Review items. A scheduled scan never
/// cleans: the automatic clean of artboard 10 is parked (2026-10-28).
struct AutomationSettingsView: View {
    let model: SettingsModel

    var body: some View {
        let off = model.automationNote != nil
        Form {
            if let note = model.automationNote {
                Section {
                    Label(note, systemImage: "info.circle").foregroundStyle(.secondary)
                }
            }
            SettingsProblem(model: model)
            Section("After a scheduled scan") {
                Choice(
                    title: "Notify me",
                    help: "Nothing is removed until you review it. The default.",
                    selected: model.afterScheduledScan == .notify
                ) { model.setAfterScheduledScan(.notify) }
                Choice(
                    title: "Only show it in the menu bar",
                    help: "No notifications. The popover shows what can be freed.",
                    selected: model.afterScheduledScan == .menuBarOnly
                ) { model.setAfterScheduledScan(.menuBarOnly) }
            }
            .disabled(off)
            Section("Notify me when") {
                LabeledContent {
                    HStack(spacing: 6) {
                        TextField("GiB", value: Binding(get: { model.thresholdGiB }, set: { model.setThresholdGiB($0) }), format: .number)
                            .labelsHidden()
                            .multilineTextAlignment(.trailing)
                            .frame(width: 56)
                        Stepper("GiB", value: Binding(get: { model.thresholdGiB }, set: { model.setThresholdGiB($0) }), in: 1...10_000)
                            .labelsHidden()
                        Text("GiB")
                    }
                } label: {
                    Text("At least this much can be freed")
                    Text("Again only when it grows by a fifth, or after a week.")
                }
                Toggle(isOn: Binding(get: { model.lowDisk }, set: { model.setLowDisk($0) })) {
                    Text("The disk is almost full")
                    Text("Under 10% free: at every scheduled scan while it lasts.")
                }
                Toggle(isOn: Binding(get: { model.workingHoursOnly }, set: { model.setWorkingHoursOnly($0) })) {
                    Text("Only during working hours")
                    Text("9:00 to 19:00 on weekdays. A notification due later waits until then.")
                }
                .disabled(model.afterScheduledScan == .menuBarOnly)
            }
            .disabled(off)
            Section("When you clean") {
                Toggle(isOn: Binding(get: { model.deletePermanently }, set: { model.setDeletePermanently($0) })) {
                    Text("Delete Review items instead of moving them to the Trash")
                    Text("Off: they go to the Trash and can be put back.")
                }
            }
        }
        .settingsPane()
    }
}

/// One choice of a radio group with its help under its title.
private struct Choice: View {
    let title: String
    let help: String
    let selected: Bool
    let pick: () -> Void

    var body: some View {
        Button(action: pick) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Image(systemName: selected ? "largecircle.fill.circle" : "circle")
                    .foregroundStyle(selected ? Color.accentColor : .secondary)
                VStack(alignment: .leading, spacing: 2) {
                    Text(title)
                    Text(help).settingsHelp()
                }
                Spacer(minLength: 0)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(selected ? [.isSelected] : [])
    }
}
