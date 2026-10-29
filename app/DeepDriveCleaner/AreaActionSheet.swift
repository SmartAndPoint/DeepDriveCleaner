import AppKit
import DDCKit
import SwiftUI

/// The sheet of an area action, or of the checked rows' list (stories "Whole
/// disk: free space with the owning tool" and "Whole disk: free the checked
/// areas in one sheet"; menu bar ADR §6 item 11): each tool's own dry run,
/// the exact command, what cannot be undone, then the run and each result.
/// Its confirm button is the only place in the app that runs `disk free` (a
/// structural test holds it).
struct AreaActionSheet: View {
    let model: DiskModel

    var body: some View {
        Group {
            if let sheet = model.freeSheet, let offer = sheet.offers.first {
                VStack(alignment: .leading, spacing: 14) {
                    Text(sheet.question).font(.system(size: 20, weight: .semibold))
                    switch sheet.stage {
                    case .previewing:
                        waiting(sheet.previewingText)
                        buttons { Button("Cancel") { Task { await model.closeFree() } }.keyboardShortcut(.cancelAction) }
                    case .confirm(let previews):
                        if sheet.offers.count == 1, let preview = previews.first {
                            confirm(offer, preview)
                        } else {
                            confirmList(sheet, previews)
                        }
                    case .running:
                        waiting(sheet.runningText)
                        buttons { Button("Stop") { Task { await model.stopFree() } }.keyboardShortcut(.cancelAction) }
                    case .result(let actions):
                        if sheet.offers.count == 1, let action = actions.first {
                            result(offer, action)
                        } else {
                            resultList(sheet, actions)
                        }
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

    /// One action: its dry run, or why it cannot run, which leaves nothing
    /// to confirm.
    @ViewBuilder
    private func confirm(_ offer: AreaActions.Offer, _ preview: FreeReport.ActionReport) -> some View {
        if preview.result == .wouldRun {
            dryRun(offer, preview, linesHeight: 180)
            confirmButtons(offer.confirmTitle)
        } else {
            notRunning(offer, preview)
            buttons { Button("Close") { Task { await model.closeFree() } }.keyboardShortcut(.defaultAction) }
        }
    }

    /// A list: each action's dry run in run order, or why it cannot run;
    /// the tools' estimates summed; one confirmation for those that would
    /// run, none when nothing would.
    @ViewBuilder
    private func confirmList(_ sheet: DiskModel.FreeSheet, _ previews: [FreeReport.ActionReport]) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                ForEach(Array(zip(sheet.offers, previews).enumerated()), id: \.offset) { index, pair in
                    let (offer, preview) = pair
                    if index > 0 { Divider() }
                    VStack(alignment: .leading, spacing: 8) {
                        Text(offer.name).font(.system(size: 14, weight: .semibold))
                        if preview.result == .wouldRun {
                            dryRun(offer, preview, linesHeight: 90)
                        } else {
                            notRunning(offer, preview)
                        }
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .frame(maxHeight: 420)
        if let total = sheet.totalText {
            Text(total).font(.system(size: 13, weight: .semibold))
        }
        if let title = sheet.confirmTitle {
            if sheet.confirmed.count < sheet.offers.count {
                Text("Only the \(sheet.confirmed.count == 1 ? "one" : "\(sheet.confirmed.count)") that can run now will run; DeepDriveCleaner checks each again right before it runs.")
                    .fixedSize(horizontal: false, vertical: true)
            }
            confirmButtons(title)
        } else {
            Text("None of them can run now: there is nothing to confirm.").fixedSize(horizontal: false, vertical: true)
            buttons { Button("Close") { Task { await model.closeFree() } }.keyboardShortcut(.defaultAction) }
        }
    }

    /// What the action removes and cannot undo, the tool's estimate and dry
    /// run, and the exact command.
    @ViewBuilder
    private func dryRun(_ offer: AreaActions.Offer, _ preview: FreeReport.ActionReport, linesHeight: CGFloat) -> some View {
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
            .frame(maxHeight: linesHeight)
            .padding(8)
            .background(RoundedRectangle(cornerRadius: 6).fill(Color(nsColor: .textBackgroundColor)))
        }
        command(preview.terminalCommand)
    }

    /// Why a previewed action does not run: refused with its reason, or
    /// its dry run failed.
    @ViewBuilder
    private func notRunning(_ offer: AreaActions.Offer, _ preview: FreeReport.ActionReport) -> some View {
        if case .refused(let reason) = preview.result {
            Text("It cannot run now: \(reason.text).").fixedSize(horizontal: false, vertical: true)
            if let needed = BrewWording.neededBy(preview) {
                Text(needed).fixedSize(horizontal: false, vertical: true)
            }
        } else if offer.action == .brewUninstall || offer.action == .brewUninstallOrphans {
            Text(FreeWording.result(preview, name: offer.name)).fixedSize(horizontal: false, vertical: true)
        } else {
            Text("Its dry run did not succeed, so it does not run.").fixedSize(horizontal: false, vertical: true)
            if let output = preview.output, !output.isEmpty {
                Text(output)
                    .font(.system(size: 11, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .lineLimit(6)
                    .textSelection(.enabled)
            }
        }
    }

    private func confirmButtons(_ title: String) -> some View {
        buttons {
            Button("Cancel") { Task { await model.closeFree() } }.keyboardShortcut(.cancelAction)
            Button(title, role: .destructive) { Task { await model.freeConfirmedActions() } }
                .keyboardShortcut(.defaultAction)
                .disabled(model.scanning)
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
        outcome(offer, action)
        buttons { Button("Done") { Task { await model.closeFree() } }.keyboardShortcut(.defaultAction) }
    }

    /// Each action's result, as the CLI measured it, and no total: ddc
    /// reports space per action.
    @ViewBuilder
    private func resultList(_ sheet: DiskModel.FreeSheet, _ actions: [FreeReport.ActionReport]) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                ForEach(Array(zip(sheet.offers, actions).enumerated()), id: \.offset) { index, pair in
                    let (offer, action) = pair
                    if index > 0 { Divider() }
                    VStack(alignment: .leading, spacing: 8) {
                        Text(offer.name).font(.system(size: 14, weight: .semibold))
                        outcome(offer, action)
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .frame(maxHeight: 420)
        buttons { Button("Done") { Task { await model.closeFree() } }.keyboardShortcut(.defaultAction) }
    }

    /// The result's sentence, what still needs a formula, the tool's output
    /// and the Terminal command when they help.
    @ViewBuilder
    private func outcome(_ offer: AreaActions.Offer, _ action: FreeReport.ActionReport) -> some View {
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
