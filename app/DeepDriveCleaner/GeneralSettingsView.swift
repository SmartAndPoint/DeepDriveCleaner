import DDCKit
import SwiftUI

/// Settings → General (artboard 10): Open at login, the two icon switches,
/// updates, and where the command-line tool is.
struct GeneralSettingsView: View {
    let model: SettingsModel
    let loginItem: LoginItem
    let updates: UpdaterSettings

    var body: some View {
        Form {
            Section {
                Toggle(isOn: Binding(get: { loginItem.isOn }, set: { loginItem.set($0) })) {
                    Text("Open at login")
                    Text("On by default, as now: the menu bar icon is back after you log in.")
                }
                if loginItem.status == .requiresApproval {
                    LabeledContent("Waiting for your approval in System Settings.") {
                        Button("Open Login Items…") { loginItem.openLoginItemsSettings() }
                    }
                }
                if let failure = loginItem.failure {
                    Text(failure).foregroundStyle(ReportClassStyle.ink(.review))
                }
            }
            Section {
                Toggle(isOn: Binding(get: { model.showSize }, set: { model.setShowSize($0) })) {
                    Text("Show the size you can free next to the icon")
                    Text("Only when it is over your notification threshold.")
                }
                Toggle(isOn: Binding(get: { model.showBadge }, set: { model.setShowBadge($0) })) {
                    Text("Show a badge on the menu bar icon")
                    Text("After a notification, until you open Review.")
                }
            }
            UpdatesSection(updates: updates)
            Section {
                LabeledContent {
                    Text(model.commandLineTool)
                        .font(.system(size: 12, design: .monospaced))
                        .textSelection(.enabled)
                        .lineLimit(1)
                        .truncationMode(.middle)
                } label: {
                    Text("Command-line tool")
                    Text("A scan from Terminal uses the folders, types and sizes set here.")
                }
            }
        }
        .settingsPane()
        .onAppear {
            loginItem.refresh()
            updates.refresh()
        }
    }
}

/// The daily check, the last one, Check Now, and the update line (ADR
/// "Updates through Sparkle, alongside the Homebrew cask", §2 and §5).
private struct UpdatesSection: View {
    let updates: UpdaterSettings

    var body: some View {
        Section {
            if let reason = updates.unavailable {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Updates are unavailable in this build")
                    Text(reason).font(.system(size: 12)).foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                .foregroundStyle(ReportClassStyle.ink(.review))
            }
            Toggle(isOn: Binding(get: { updates.checksDaily }, set: { updates.setChecksDaily($0) })) {
                Text("Check for updates daily")
                Text("One request to GitHub a day. Nothing installs until you press Update and Relaunch.")
            }
            .disabled(updates.unavailable != nil)
            LabeledContent {
                if updates.model.state == .checking {
                    ProgressView().controlSize(.small)
                } else {
                    Button("Check Now") { updates.checkNow() }
                        .disabled(!updates.checkNowEnabled)
                }
            } label: {
                Text("Last checked")
                Text(updates.lastCheck.map { $0.formatted(date: .abbreviated, time: .shortened) } ?? "Never")
            }
            if updates.model.item != nil {
                UpdateLineView(model: updates.model, activity: updates.activity)
            } else if updates.model.state == .failed, let message = updates.model.flow.message {
                Text(message).foregroundStyle(ReportClassStyle.ink(.review))
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }
}

extension View {
    /// Every pane: a grouped form of one width, as tall as its rows.
    func settingsPane() -> some View {
        formStyle(.grouped)
            .frame(width: 560)
            .fixedSize(horizontal: false, vertical: true)
    }

    /// A pane whose rows can outgrow a small screen (Types): a fixed height,
    /// and the form scrolls inside it.
    func scrollingSettingsPane() -> some View {
        formStyle(.grouped)
            .frame(width: 560, height: 620)
    }
}

/// Why `settings.json` cannot be changed, at the top of a pane that writes it.
struct SettingsProblem: View {
    let model: SettingsModel

    var body: some View {
        if let problem = model.problem {
            Section {
                Label(problem, systemImage: "exclamationmark.triangle")
                    .foregroundStyle(ReportClassStyle.ink(.review))
            }
        }
    }
}
