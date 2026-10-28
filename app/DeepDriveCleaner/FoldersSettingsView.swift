import AppKit
import DDCKit
import SwiftUI

/// Settings → Folders (artboard 10): Full Disk Access, Always scan, Not
/// readable and Exclude. The lists live in `settings.json`, so a scan from
/// Terminal uses them too; `SettingsModel` checks every change.
struct FoldersSettingsView: View {
    let model: SettingsModel
    let privacy: PrivacySettings
    @State private var root: String?
    @State private var exclusion: String?

    var body: some View {
        let folders = model.folders
        Form {
            Section {
                if model.fullDiskAccess {
                    Label("Full Disk Access is on: every folder is read, scheduled scans included, and macOS asks nothing.", systemImage: "checkmark.shield")
                } else {
                    VStack(alignment: .leading, spacing: 8) {
                        Label(
                            "Full Disk Access is off. Scheduled scans then skip the folders macOS protects (Desktop, Documents, Downloads, other apps' data), so they never stop at a macOS question. Turn it on to scan everything.",
                            systemImage: "lock.shield"
                        )
                        Button("Open Privacy Settings") { privacy.openFullDiskAccess() }
                    }
                }
            }
            SettingsProblem(model: model)
            Section {
                List(selection: $root) {
                    ForEach(folders.roots) { root in
                        HStack {
                            Text(root.shown).lineLimit(1).truncationMode(.middle)
                            Spacer()
                            if let detail = root.detail { Text(detail).foregroundStyle(.secondary) }
                        }
                        .tag(root.path)
                    }
                }
                .frame(height: 90)
                PlusMinus(
                    add: { pick(startingAt: "/Volumes") { model.addRoot($0) } },
                    remove: { if let root { model.removeRoot(root); self.root = nil } },
                    canRemove: root != nil && folders.canRemoveRoot,
                    noun: "Folder"
                )
            } header: {
                Text("Always scan")
            } footer: {
                Text("The home folder by default. Add external disks here; a folder inside one already listed is covered by it.")
                    .settingsHelp()
            }
            // With Full Disk Access only the confirmation line: a stale list would contradict it.
            if !model.fullDiskAccess && !model.notReadable.isEmpty {
                Section {
                    ForEach(model.notReadable, id: \.path) { folder in
                        HStack {
                            Text(folder.shown).lineLimit(1).truncationMode(.middle)
                            Text("Denied").foregroundStyle(ReportClassStyle.ink(.review))
                            Spacer()
                            Button("Open Privacy Settings") { privacy.openFilesAndFolders() }
                        }
                    }
                } header: {
                    Text("Not readable")
                } footer: {
                    Text("The last Scan now could not read these: you said no when macOS asked. Only System Settings can change that.")
                        .settingsHelp()
                }
            }
            Section {
                List(selection: $exclusion) {
                    ForEach(folders.exclusions) { row in
                        HStack {
                            Text(row.shown).lineLimit(1).truncationMode(.middle)
                            Spacer()
                            Picker("Mode", selection: Binding(get: { row.mode }, set: { model.setMode($0, of: row.path) })) {
                                ForEach(FolderList.Mode.allCases, id: \.self) { Text($0.title).tag($0) }
                            }
                            .labelsHidden()
                            .fixedSize()
                        }
                        .tag(row.path)
                    }
                }
                .frame(height: 110)
                PlusMinus(
                    add: { pick(startingAt: model.home) { model.addExclusion($0, mode: .count) } },
                    remove: { if let exclusion { model.removeExclusion(exclusion); self.exclusion = nil } },
                    canRemove: exclusion != nil,
                    noun: "Exclusion"
                )
            } header: {
                Text("Exclude")
            } footer: {
                Text("Never cleaned. \"Count, never clean\" still shows the size, marked as yours, as a .ddc-keep file inside a folder does. \"Skip entirely\" is not read at all, so scans get faster, and Review says it was not scanned. A folder skipped inside a cache or a project's build folder keeps that whole folder: it is read and never cleaned.")
                    .settingsHelp()
            }
            if let message = model.folderMessage {
                Section {
                    Label(message, systemImage: "exclamationmark.triangle")
                        .foregroundStyle(ReportClassStyle.ink(.review))
                }
            }
        }
        .settingsPane()
    }

    /// The folder picker; an external volume can be chosen from the sidebar.
    private func pick(startingAt directory: String, _ add: @escaping @MainActor (String) -> Void) {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.directoryURL = URL(filePath: directory, directoryHint: .isDirectory)
        panel.prompt = "Add"
        panel.begin { response in
            guard response == .OK, let url = panel.url else { return }
            MainActor.assumeIsolated { add(url.path) }
        }
    }
}

/// The + and − under a list, as macOS settings draw them.
private struct PlusMinus: View {
    let add: () -> Void
    let remove: () -> Void
    let canRemove: Bool
    let noun: String

    var body: some View {
        HStack(spacing: 0) {
            Button(action: add) { Image(systemName: "plus").frame(width: 22, height: 18) }
                .accessibilityLabel("Add \(noun)…")
            Button(action: remove) { Image(systemName: "minus").frame(width: 22, height: 18) }
                .disabled(!canRemove)
                .accessibilityLabel("Remove \(noun)")
            Spacer()
        }
        .buttonStyle(.borderless)
    }
}

extension Text {
    /// The help under a row or a section, as the canvas sets it.
    func settingsHelp() -> some View {
        font(.system(size: 11)).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
    }
}
