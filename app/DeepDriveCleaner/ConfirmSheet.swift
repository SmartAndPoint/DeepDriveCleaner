import AppKit
import DDCKit
import SwiftUI

/// The confirmation sheet (artboard 5), then the clean's progress, then the
/// result (artboard 6). Its Clean button is the only place in the app that
/// starts a clean (ADR §7; a structural test holds it).
struct ConfirmSheet: View {
    @Bindable var model: ReviewModel

    var body: some View {
        Group {
            if let sheet = model.sheet {
                switch sheet.stage {
                case .confirm:
                    ConfirmView(model: model, sheet: sheet)
                case .cleaning(let phase):
                    CleaningView(model: model, phase: phase, confirmation: sheet.confirmation)
                case .result(let result):
                    CleanResultView(result: result, done: model.closeSheet)
                case .stopped(let text, let journal):
                    StoppedView(text: text, journal: journal, done: model.closeSheet)
                }
            }
        }
        .frame(width: 640)
        .padding(24)
    }
}

private struct ConfirmView: View {
    @Bindable var model: ReviewModel
    let sheet: ReviewModel.Sheet

    var body: some View {
        let confirmation = sheet.confirmation
        let home = confirmation.home
        VStack(alignment: .leading, spacing: 16) {
            VStack(alignment: .leading, spacing: 4) {
                Text("Clean \(Wording.items(confirmation.items.count))?").font(.system(size: 20, weight: .semibold))
                Text("\(ByteSize.human(confirmation.bytes)) selected · frees about \(ByteSize.human(confirmation.reclaimable))")
                    .foregroundStyle(.secondary)
            }

            VStack(spacing: 0) {
                ForEach(confirmation.classRows, id: \.riskClass) { row in
                    HStack(alignment: .top, spacing: 10) {
                        RoundedRectangle(cornerRadius: 3).fill(ReportClassStyle.color(row.riskClass)).frame(width: 10, height: 10).padding(.top, 4)
                        VStack(alignment: .leading, spacing: 2) {
                            Text("\(ReportClassStyle.name(row.riskClass)) · \(Wording.items(row.count))").font(.system(size: 13, weight: .semibold))
                            Text(explain(row.riskClass, confirmation)).font(.system(size: 12)).foregroundStyle(.secondary)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                        Spacer()
                        Text(ByteSize.human(row.bytes)).font(.system(size: 13)).monospacedDigit()
                    }
                    .padding(.vertical, 8)
                }
            }

            warningsLine

            ScrollView {
                LazyVStack(alignment: .leading, spacing: 6) {
                    ForEach(confirmation.items) { item in
                        HStack(alignment: .top, spacing: 8) {
                            VStack(alignment: .leading, spacing: 1) {
                                Text(Wording.tilde(item.finding.path, home: home)).font(.system(size: 12)).lineLimit(1).truncationMode(.middle)
                                prediction(item)
                            }
                            Spacer(minLength: 8)
                            if item.canBePermanent {
                                Toggle("Delete permanently", isOn: Binding(get: { item.permanent }, set: { _ in model.togglePermanent(item.id) }))
                                    .toggleStyle(.checkbox)
                                    .font(.system(size: 11))
                            }
                            Text(CleanWording.action(item.effectiveAction)).font(.system(size: 11)).foregroundStyle(.secondary)
                            Text(ByteSize.human(item.finding.bytes)).font(.system(size: 12)).monospacedDigit().frame(width: 76, alignment: .trailing)
                        }
                    }
                }
                .padding(10)
            }
            .frame(maxHeight: 220)
            .background(Color.secondary.opacity(0.06), in: RoundedRectangle(cornerRadius: 8))

            HStack(alignment: .top, spacing: 8) {
                Image(systemName: "checkmark.shield").foregroundStyle(.secondary)
                Text(note(confirmation)).font(.system(size: 12)).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            }

            HStack {
                Button("Check again") { model.checkWarnings() }
                    .disabled(sheet.warnings == .waiting || sheet.warnings == .checking)
                Spacer()
                Button("Back to review") { model.closeSheet() }.keyboardShortcut(.cancelAction)
                // Not the default button: Return never starts a clean.
                Button("Clean \(Wording.items(confirmation.items.count))") { model.cleanConfirmedItems() }
                    .buttonStyle(.borderedProminent)
                    .tint(PopoverView.accent)
                    .disabled(model.isCleaning)
            }
        }
    }

    @ViewBuilder private var warningsLine: some View {
        let skips = sheet.predictions.values.filter { if case .wouldSkip = $0 { true } else { false } }.count
        HStack(spacing: 8) {
            switch sheet.warnings {
            case .waiting:
                ProgressView().controlSize(.small)
                Text("Waiting for the running scan to check for warnings…")
            case .checking:
                ProgressView().controlSize(.small)
                Text("Checking each item for warnings…")
            case .checked where skips == 0:
                Image(systemName: "checkmark.circle").foregroundStyle(ReportClassStyle.safe)
                Text("No warnings: nothing is predicted to be skipped.")
            case .checked where skips == sheet.confirmation.items.count:
                Image(systemName: "exclamationmark.triangle").foregroundStyle(ReportClassStyle.review)
                Text("Nothing can be cleaned right now: every item would be skipped.")
            case .checked:
                Image(systemName: "exclamationmark.triangle").foregroundStyle(ReportClassStyle.review)
                Text("\(Wording.items(skips)) will be skipped; the rest is cleaned.")
            case .busy:
                Image(systemName: "hourglass").foregroundStyle(.secondary)
                Text("A scan or clean started in Terminal holds the lock. Check again when it ends.")
            case .stopped:
                Image(systemName: "pause.circle").foregroundStyle(.secondary)
                Text("The check was stopped.")
            case .failed(let message):
                Image(systemName: "exclamationmark.circle").foregroundStyle(ReportClassStyle.review)
                Text(message)
            }
        }
        .font(.system(size: 12))
    }

    @ViewBuilder private func prediction(_ item: DDCKit.Confirmation.Item) -> some View {
        if case .wouldSkip(let reason)? = sheet.predictions[item.id] {
            Text(CleanWording.willSkip(reason)).font(.system(size: 11)).foregroundStyle(ReportClassStyle.review)
        }
    }

    private func explain(_ riskClass: ScanReport.RiskClass, _ confirmation: DDCKit.Confirmation) -> String {
        let items = confirmation.items.filter { $0.finding.class == riskClass }
        let tools = items.filter { $0.effectiveAction == .native || $0.effectiveAction == .nativeOnly }.count
        let toolNote = tools == 0 ? "" : " \(Wording.items(tools)) \(tools == 1 ? "is" : "are") cleaned by \(tools == 1 ? "its" : "their") own tool."
        switch riskClass {
        case .safe:
            return "Deleted; caches refill on the next install or build." + toolNote
        case .rebuildable:
            return "Deleted. A reinstall or build in each project restores them from its lock file." + toolNote
        case .review:
            let trash = items.filter { $0.effectiveAction == .trash }.count
            let permanent = items.filter { $0.permanent }.count
            var parts: [String] = []
            if trash > 0 { parts.append("\(Wording.items(trash)) go\(trash == 1 ? "es" : "") to the Trash, which frees nothing until you empty it.") }
            if permanent > 0 { parts.append("\(Wording.items(permanent)) \(permanent == 1 ? "is" : "are") deleted permanently.") }
            return parts.joined(separator: " ") + toolNote
        case .userData:
            return "Never cleaned."
        }
    }

    private func note(_ confirmation: DDCKit.Confirmation) -> String {
        var text = "Every item is checked again right before it is touched: still the same folder, no symlink above it, not in use, its app not running, not protected by a .ddc-keep file. Anything that fails is skipped and listed."
        if confirmation.trashCount > 0 { text += " The Trash keeps the space used until you empty it." }
        text += " Local Time Machine snapshots can hold the space until macOS thins them."
        return text
    }
}

private struct CleaningView: View {
    @Bindable var model: ReviewModel
    let phase: ReviewModel.Phase
    let confirmation: DDCKit.Confirmation

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text(title).font(.system(size: 20, weight: .semibold))
            ProgressView().progressViewStyle(.linear).tint(PopoverView.accent)
            HStack {
                Text("\(Wording.items(confirmation.items.count)) · \(ByteSize.human(confirmation.bytes))")
                Spacer()
                if let since {
                    TimelineView(.periodic(from: since, by: 1)) { context in
                        Text(Wording.elapsed(context.date.timeIntervalSince(since)))
                    }
                }
            }
            .font(.system(size: 12))
            .foregroundStyle(.secondary)
            .monospacedDigit()
            Text(detail).font(.system(size: 12)).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            Text("You can keep working meanwhile; the menu bar icon turns until the clean ends.")
                .font(.system(size: 12))
                .foregroundStyle(.secondary)
            HStack {
                Spacer()
                Button("Cancel") { model.cancelClean() }
                    .disabled(phase == .stopping)
            }
        }
    }

    private var title: String {
        switch phase {
        case .waiting: "Waiting for the running scan…"
        case .preparing: "Preparing…"
        case .cleaning: "Cleaning…"
        case .stopping: "Stopping after the current item…"
        }
    }

    private var detail: String {
        switch phase {
        case .waiting: "The clean starts when the scan ends. Cancel now and it never starts."
        case .preparing: "Checking the selected folders again before anything is touched."
        case .cleaning: "Removing the confirmed items one by one. Cancel stops after the current item."
        case .stopping: "Nothing after the current item is touched."
        }
    }

    private var since: Date? {
        switch phase {
        case .preparing(let since), .cleaning(let since): since
        case .waiting, .stopping: nil
        }
    }
}

private struct StoppedView: View {
    let text: String
    let journal: URL?
    let done: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Label("The clean did not finish", systemImage: "exclamationmark.circle")
                .font(.system(size: 20, weight: .semibold))
            Text(text).fixedSize(horizontal: false, vertical: true)
            HStack {
                if let journal {
                    Button("Show the journal") { NSWorkspace.shared.activateFileViewerSelecting([journal]) }
                }
                Spacer()
                Button("Done", action: done).keyboardShortcut(.defaultAction)
            }
        }
    }
}
