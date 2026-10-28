import AppKit
import DDCKit
import SwiftUI

/// The sheet of one area action (story "Whole disk: free space with the
/// owning tool"; menu bar ADR §6 item 11): the tool's own dry run, the exact
/// command, what cannot be undone, then the run and its result. Its confirm
/// button is the only place in the app that runs `disk free` (a structural
/// test holds it).
struct AreaActionSheet: View {
    let model: DiskModel

    var body: some View {
        Group {
            if let sheet = model.freeSheet {
                VStack(alignment: .leading, spacing: 14) {
                    Text(sheet.offer.question).font(.system(size: 20, weight: .semibold))
                    switch sheet.stage {
                    case .previewing:
                        waiting("Asking \(tool(sheet.offer)) what it would remove…")
                        buttons { Button("Cancel") { Task { await model.closeFree() } }.keyboardShortcut(.cancelAction) }
                    case .confirm(let preview):
                        confirm(sheet.offer, preview)
                    case .running:
                        waiting(sheet.offer.action == .simctlRuntimeDelete ? "Deleting the runtime…" : "Running brew cleanup…")
                        buttons { Button("Stop") { Task { await model.stopFree() } }.keyboardShortcut(.cancelAction) }
                    case .result(let action):
                        result(sheet.offer, action)
                    case .failed(let why):
                        Text(why).fixedSize(horizontal: false, vertical: true)
                        buttons { Button("Close") { Task { await model.closeFree() } }.keyboardShortcut(.defaultAction) }
                    }
                }
            }
        }
        .frame(width: 600)
        .padding(24)
    }

    private func tool(_ offer: AreaActions.Offer) -> String {
        offer.action == .simctlRuntimeDelete ? "simctl" : "brew"
    }

    private func waiting(_ text: String) -> some View {
        HStack(spacing: 8) {
            ProgressView().controlSize(.small)
            Text(text).foregroundStyle(.secondary)
        }
    }

    @ViewBuilder
    private func confirm(_ offer: AreaActions.Offer, _ preview: FreeReport.ActionReport) -> some View {
        if case .refused(let reason) = preview.result {
            Text("It cannot run now: \(reason.text).").fixedSize(horizontal: false, vertical: true)
            buttons { Button("Close") { Task { await model.closeFree() } }.keyboardShortcut(.defaultAction) }
        } else {
            Text(offer.consequence).fixedSize(horizontal: false, vertical: true)
            if let estimate = preview.preview?.estimateBytes {
                Text(offer.action == .simctlRuntimeDelete
                    ? "Its folder holds \(ByteSize.human(estimate))."
                    : "brew says this frees about \(ByteSize.human(estimate)).")
                    .font(.system(size: 13, weight: .semibold))
            }
            if let lines = preview.preview?.lines, !lines.isEmpty {
                ScrollView {
                    Text(lines.joined(separator: "\n"))
                        .font(.system(size: 11, design: .monospaced))
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .frame(maxHeight: 180)
                .padding(8)
                .background(RoundedRectangle(cornerRadius: 6).fill(Color(nsColor: .textBackgroundColor)))
            }
            command(preview.terminalCommand)
            buttons {
                Button("Cancel") { Task { await model.closeFree() } }.keyboardShortcut(.cancelAction)
                Button(offer.confirmTitle, role: .destructive) { Task { await model.freeConfirmedActions() } }
                    .keyboardShortcut(.defaultAction)
                    .disabled(model.scanning)
            }
        }
    }

    @ViewBuilder
    private func result(_ offer: AreaActions.Offer, _ action: FreeReport.ActionReport) -> some View {
        Text(FreeWording.result(action, name: offer.name)).fixedSize(horizontal: false, vertical: true)
        if case .failed = action.result {
            if let output = action.output, !output.isEmpty {
                Text(output)
                    .font(.system(size: 11, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .lineLimit(6)
                    .textSelection(.enabled)
            }
            command(action.terminalCommand)
        }
        buttons { Button("Done") { Task { await model.closeFree() } }.keyboardShortcut(.defaultAction) }
    }

    /// The exact command, as it runs: for a person to check, or to paste
    /// into Terminal when it needs an administrator.
    private func command(_ text: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Text(text)
                .font(.system(size: 11, design: .monospaced))
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 8)
            Button("Copy Command") {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(text, forType: .string)
            }
        }
    }

    private func buttons(@ViewBuilder _ content: () -> some View) -> some View {
        HStack {
            Spacer()
            content()
        }
    }
}
