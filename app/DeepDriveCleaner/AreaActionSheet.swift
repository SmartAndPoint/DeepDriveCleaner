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
            if let sheet = model.freeSheet, let offer = sheet.offers.first {
                VStack(alignment: .leading, spacing: 14) {
                    Text(offer.question).font(.system(size: 20, weight: .semibold))
                    switch sheet.stage {
                    case .previewing:
                        waiting(offer.previewingText)
                        buttons { Button("Cancel") { Task { await model.closeFree() } }.keyboardShortcut(.cancelAction) }
                    case .confirm(let previews):
                        if let preview = previews.first { confirm(offer, preview) }
                    case .running:
                        waiting(offer.runningText)
                        buttons { Button("Stop") { Task { await model.stopFree() } }.keyboardShortcut(.cancelAction) }
                    case .result(let actions):
                        if let action = actions.first { result(offer, action) }
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
            if let needed = BrewWording.neededBy(preview) {
                Text(needed).fixedSize(horizontal: false, vertical: true)
            }
            buttons { Button("Close") { Task { await model.closeFree() } }.keyboardShortcut(.defaultAction) }
        } else if case .failed = preview.result, offer.action == .brewUninstall || offer.action == .brewUninstallOrphans {
            Text(FreeWording.result(preview, name: offer.name)).fixedSize(horizontal: false, vertical: true)
            buttons { Button("Close") { Task { await model.closeFree() } }.keyboardShortcut(.defaultAction) }
        } else {
            Text(offer.consequence).fixedSize(horizontal: false, vertical: true)
            if let estimate = preview.preview?.estimateBytes {
                Text(offer.estimateText(estimate))
                    .font(.system(size: 13, weight: .semibold))
            }
            if !preview.kegs.isEmpty {
                kegs(preview.kegs)
            } else if let lines = preview.preview?.lines, !lines.isEmpty {
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

    /// What brew uninstall removes, as brew listed it right before: each
    /// version, its folder and what ddc measured.
    private func kegs(_ kegs: [FreeReport.Keg]) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 4) {
                ForEach(Array(kegs.enumerated()), id: \.offset) { _, keg in
                    HStack(alignment: .firstTextBaseline) {
                        Text("\(keg.formula) \(keg.version)").font(.system(size: 12, weight: .semibold))
                        Text(keg.path).font(.system(size: 11, design: .monospaced)).foregroundStyle(.secondary).textSelection(.enabled)
                        Spacer(minLength: 8)
                        Text(keg.bytes.map(ByteSize.human) ?? "not measured").font(.system(size: 12)).monospacedDigit()
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .frame(maxHeight: 180)
        .padding(8)
        .background(RoundedRectangle(cornerRadius: 6).fill(Color(nsColor: .textBackgroundColor)))
    }

    @ViewBuilder
    private func result(_ offer: AreaActions.Offer, _ action: FreeReport.ActionReport) -> some View {
        Text(FreeWording.result(action, name: offer.name)).fixedSize(horizontal: false, vertical: true)
        if let needed = BrewWording.neededBy(action) {
            Text(needed).fixedSize(horizontal: false, vertical: true)
        }
        if FreeWording.showsOutput(action), let output = action.output, !output.isEmpty {
            Text(output)
                .font(.system(size: 11, design: .monospaced))
                .foregroundStyle(.secondary)
                .lineLimit(6)
                .textSelection(.enabled)
        }
        if FreeWording.offersTerminal(action) {
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
