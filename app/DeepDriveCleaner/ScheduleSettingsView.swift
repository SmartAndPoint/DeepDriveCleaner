import DDCKit
import SwiftUI

/// Settings → Schedule (artboard 10): Scan automatically, Daily or Weekly at
/// a whole hour, Pause, and the next scan. The choice is the app's own
/// (`UserDefaults`); the scheduler of "Background scan and notifications in
/// the app" acts on it.
struct ScheduleSettingsView: View {
    let model: SettingsModel

    var body: some View {
        let schedule = model.schedule
        Form {
            Section {
                Toggle(isOn: binding(\.enabled)) {
                    Text("Scan automatically")
                    Text("A scan only reads. Nothing is removed until you review it.")
                }
                Group {
                    Picker("Repeat", selection: binding(\.repeats)) {
                        ForEach(Schedule.Repeat.allCases, id: \.self) { Text($0.title).tag($0) }
                    }
                    .pickerStyle(.segmented)
                    .fixedSize()
                    if schedule.repeats == .weekly {
                        Picker("Day", selection: Binding(get: { schedule.weekday }, set: { day in edit { $0.setWeekday(day) } })) {
                            ForEach(Schedule.weekdays, id: \.self) { Text(Schedule.weekdayName($0)).tag($0) }
                        }
                    }
                    Picker(selection: Binding(get: { schedule.hour }, set: { hour in edit { $0.setHour(hour) } })) {
                        ForEach(Schedule.hours, id: \.self) { Text(Schedule.hourText($0)).tag($0) }
                    } label: {
                        Text("Time")
                        Text("Whole hours: the Mac checks once an hour.")
                    }
                }
                .disabled(!schedule.enabled)
            }
            Section {
                Toggle(isOn: Binding(get: { model.paused }, set: { model.setPaused($0) })) {
                    Text("Pause scheduled scans")
                    Text("The same as Pause in the menu; the schedule is kept for Resume.")
                }
                if let next = model.nextScan {
                    LabeledContent("Next scan", value: next)
                    if schedule.enabled && !model.paused {
                        Text("If the Mac is asleep at \(Schedule.hourText(schedule.hour)), the scan runs soon after it wakes. Scan now in the menu and ddc scan in Terminal don't move the schedule.")
                            .settingsHelp()
                    }
                }
            }
        }
        .settingsPane()
    }

    private func binding<Value>(_ key: WritableKeyPath<Schedule, Value>) -> Binding<Value> {
        Binding(get: { model.schedule[keyPath: key] }, set: { value in edit { $0[keyPath: key] = value } })
    }

    private func edit(_ change: (inout Schedule) -> Void) {
        var schedule = model.schedule
        change(&schedule)
        model.setSchedule(schedule)
    }
}
