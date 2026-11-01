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
            // While it slides away after Done or Back to Review, the stage it closed on.
            if let sheet = model.sheet ?? model.lastSheet {
                switch sheet.stage {
                case .confirm:
                    ConfirmView(model: model, sheet: sheet)
                case .cleaning(let phase):
                    CleaningView(model: model, phase: phase, confirmation: sheet.confirmation, progress: sheet.progress)
                case .result(let result):
                    CleanResultView(model: model, result: result)
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
                Text(CleanWording.sheetTitle(confirmation)).font(.system(size: 20, weight: .semibold))
                Text(CleanWording.sheetHeader(sheet))
                    .foregroundStyle(.secondary)
                if let notCounted = CleanWording.notCounted(confirmation) {
                    Text(notCounted).font(.system(size: 12)).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                }
                if let sessions = CleanWording.sessions(confirmation) {
                    Text(sessions).font(.system(size: 12)).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                }
            }

            VStack(spacing: 0) {
                ForEach(confirmation.classRows, id: \.riskClass) { row in
                    HStack(alignment: .top, spacing: 10) {
                        RoundedRectangle(cornerRadius: 3).fill(ReportClassStyle.color(row.riskClass)).frame(width: 10, height: 10).padding(.top, 4)
                        VStack(alignment: .leading, spacing: 2) {
                            Text("\(row.riskClass == .userData ? "Your own files" : ReportClassStyle.name(row.riskClass)) · \(Wording.items(row.count))")
                                .font(.system(size: 13, weight: .semibold))
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

            if confirmation.showsAllPermanent {
                HStack(spacing: 6) {
                    let state = confirmation.permanentState
                    CheckBox(state: state, enabled: !model.isCleaning, label: "Delete all permanently") {
                        model.setAllPermanent(state != .on)
                    }
                    Text("Delete all permanently").font(.system(size: 12))
                }
                .help("Removed at once instead of moved to the Trash; they cannot be put back. Settings → Automation decides how the sheet starts.")
            }

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
                Button("Check Again") { model.checkWarnings() }
                    .disabled(sheet.warnings == .waiting || sheet.warnings == .checking)
                Spacer()
                Button("Back to Review") { model.closeSheet() }.keyboardShortcut(.cancelAction)
                // Not the default button: Return never starts a clean. Off while
                // the check runs, and when every item would be skipped.
                Button(CleanWording.sheetAction(confirmation), role: .destructive) { model.cleanConfirmedItems() }
                    .buttonStyle(.borderedProminent)
                    .disabled(model.isCleaning || !sheet.cleanAllowed)
            }
        }
    }

    @ViewBuilder private var warningsLine: some View {
        let skips = sheet.skipSummary
        HStack(spacing: 8) {
            switch sheet.warnings {
            case .waiting:
                ProgressView().controlSize(.small)
                Text("Waiting for the running scan to check for warnings…")
            case .checking:
                ProgressView().controlSize(.small)
                Text("Checking each item for warnings…")
                // A long check never traps the user: Stop leaves Clean on.
                Button("Stop") { model.stopCheck() }.controlSize(.small)
            case .checked where skips.count == 0:
                Image(systemName: "checkmark.circle").foregroundStyle(ReportClassStyle.safe)
                Text("No warnings: nothing is predicted to be skipped.")
            case .checked where skips.nothingToClean:
                Image(systemName: "exclamationmark.triangle").foregroundStyle(ReportClassStyle.ink(.review))
                Text(CleanWording.nothingToClean(skips)).fixedSize(horizontal: false, vertical: true)
            case .checked:
                Image(systemName: "exclamationmark.triangle").foregroundStyle(ReportClassStyle.ink(.review))
                Text("\(Wording.items(skips.count)) will be skipped; the rest is cleaned.")
            case .busy:
                Image(systemName: "hourglass").foregroundStyle(.secondary)
                Text("A scan or clean started in Terminal holds the lock. Check Again when it ends.")
            case .stopped:
                Image(systemName: "pause.circle").foregroundStyle(.secondary)
                Text("The check was stopped.")
            case .failed(let message):
                Image(systemName: "exclamationmark.circle").foregroundStyle(ReportClassStyle.ink(.review))
                Text(message)
            }
        }
        .font(.system(size: 12))
    }

    @ViewBuilder private func prediction(_ item: DDCKit.Confirmation.Item) -> some View {
        if case .wouldSkip(let reason)? = sheet.predictions[item.id] {
            Text(CleanWording.willSkip(reason)).font(.system(size: 11)).foregroundStyle(ReportClassStyle.ink(.review))
        }
    }

    private func explain(_ riskClass: ScanReport.RiskClass, _ confirmation: DDCKit.Confirmation) -> String {
        let items = confirmation.items.filter { $0.riskClass == riskClass }
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
            var text = "They go to the Trash; nothing of yours is deleted permanently. Put Back restores them, and the space returns when you empty the Trash."
            if confirmation.included > 0 {
                let one = confirmation.included == 1
                text += " Includes \(Wording.items(confirmation.included)) selected on \(one ? "its" : "their") own; \(one ? "it goes" : "they go") to the Trash with the folder, and \(one ? "its" : "their") space returns when the Trash is emptied."
            }
            return text
        }
    }

    private func note(_ confirmation: DDCKit.Confirmation) -> String {
        var text = "Every item is checked again right before it is touched: still the same folder, no symlink above it, not in use, its app not running, not protected by a .ddc-keep file. Anything that fails is skipped and listed."
        if confirmation.trashCount > 0 { text += " The Trash keeps the space used until you empty it." }
        text += " Local Time Machine snapshots can hold the space until macOS thins them."
        return text
    }
}

/// Step 1, the CLI's re-check, then step 2, the items (artboard 11). Without
/// events yet the bar is indeterminate.
private struct CleaningView: View {
    @Bindable var model: ReviewModel
    let phase: ReviewModel.Phase
    let confirmation: DDCKit.Confirmation
    let progress: CleanProgress?

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            VStack(alignment: .leading, spacing: 2) {
                if let step {
                    Text(step).font(.system(size: 11, weight: .semibold)).foregroundStyle(.secondary)
                }
                Text(title).font(.system(size: 20, weight: .semibold))
            }
            bar
            if let current = items?.current {
                HStack(spacing: 6) {
                    Text("Now:").foregroundStyle(.secondary)
                    Text(CleanWording.now(current, home: confirmation.home))
                        .font(.system(size: 12, design: current.tool == nil ? .default : .monospaced))
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Spacer(minLength: 8)
                    if current.tool == nil {
                        Text(ByteSize.human(current.bytes)).foregroundStyle(.secondary).monospacedDigit()
                    }
                }
                .font(.system(size: 12))
            }
            Text(detail).font(.system(size: 12)).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            HStack {
                Text("\(Wording.items(confirmation.items.count)) · \(ByteSize.human(confirmation.bytes)) confirmed")
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
            Text("You can keep working meanwhile; the menu bar icon turns until the clean ends.")
                .font(.system(size: 12))
                .foregroundStyle(.secondary)
            HStack {
                Spacer()
                // Stop, as the phase it leads to says; Esc too.
                Button("Stop") { model.cancelClean() }
                    .keyboardShortcut(.cancelAction)
                    .disabled(phase == .stopping)
            }
        }
    }

    /// The item counters, once the CLI has begun cleaning; `.stopping` keeps them.
    private var items: CleanProgress? {
        guard let progress, progress.cleaning else { return nil }
        switch phase {
        case .cleaning, .stopping: return progress.counting(items: confirmation.items.count, bytes: confirmation.counted)
        case .waiting, .preparing: return nil
        }
    }

    /// Step 1's walk while the CLI re-checks.
    private var walk: CleanProgress.Walk? {
        guard case .preparing = phase, let progress, !progress.cleaning else { return nil }
        return progress.walk
    }

    @ViewBuilder private var bar: some View {
        if let items {
            VStack(alignment: .leading, spacing: 6) {
                CleanBar(fraction: items.fraction, inFlight: items.inFlight)
                HStack {
                    Text(CleanWording.counterLine(items))
                    Spacer()
                    Text(CleanWording.percent(items.fraction))
                }
                .font(.system(size: 12))
                .foregroundStyle(.secondary)
                .monospacedDigit()
            }
        } else if let walk {
            VStack(alignment: .leading, spacing: 6) {
                if let fraction = progress?.walkFraction {
                    CleanBar(fraction: fraction, inFlight: 0)
                } else {
                    ProgressView().progressViewStyle(.linear)
                }
                HStack {
                    Text(CleanWording.walkLine(walk))
                    Spacer()
                    if let fraction = progress?.walkFraction { Text(CleanWording.percent(fraction)) }
                }
                .font(.system(size: 12))
                .foregroundStyle(.secondary)
                .monospacedDigit()
            }
        } else {
            ProgressView().progressViewStyle(.linear)
        }
    }

    private var step: String? {
        guard progress != nil else { return nil }
        switch phase {
        case .preparing: return "Step 1 of 2"
        case .cleaning: return "Step 2 of 2"
        case .waiting, .stopping: return nil
        }
    }

    private var title: String {
        switch phase {
        case .waiting: "Waiting for the running scan…"
        case .preparing: progress == nil ? "Preparing…" : "Checking again"
        case .cleaning: progress == nil ? "Cleaning…" : "Cleaning"
        case .stopping: "Stopping after the current item…"
        }
    }

    private var detail: String {
        switch phase {
        case .waiting: "The clean starts when the scan ends. Stop now and it never starts."
        case .preparing: progress == nil ? "Checking the selected folders again before anything is touched." : CleanWording.walkDetail(walk?.step)
        case .cleaning: items.map(CleanWording.cleaningNote) ?? "Removing the confirmed items one by one. Stop ends it after the current item."
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

/// A determinate bar with the item in flight striped after it; the stripes
/// hold still under Reduce Motion.
private struct CleanBar: View {
    let fraction: Double
    let inFlight: Double
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        GeometryReader { geometry in
            let width = geometry.size.width
            ZStack(alignment: .leading) {
                Capsule().fill(Color.secondary.opacity(0.2))
                HStack(spacing: 0) {
                    Rectangle().fill(Color.accentColor).frame(width: width * fraction)
                    if inFlight > 0 {
                        Stripes(moving: !reduceMotion).frame(width: width * inFlight)
                    }
                }
                .clipShape(Capsule())
            }
        }
        .frame(height: 6)
        .accessibilityElement()
        .accessibilityLabel("Clean progress")
        .accessibilityValue(CleanWording.percent(fraction))
    }
}

private struct Stripes: View {
    let moving: Bool

    var body: some View {
        if moving {
            TimelineView(.animation) { context in
                pattern(offset: context.date.timeIntervalSinceReferenceDate.truncatingRemainder(dividingBy: 1) * Self.period)
            }
        } else {
            pattern(offset: 0)
        }
    }

    private static let period = 8.0

    private func pattern(offset: Double) -> some View {
        Canvas { context, size in
            context.fill(Path(CGRect(origin: .zero, size: size)), with: .color(Color.accentColor.opacity(0.35)))
            var x = offset - Self.period - size.height
            while x < size.width {
                var stripe = Path()
                stripe.move(to: CGPoint(x: x, y: size.height))
                stripe.addLine(to: CGPoint(x: x + size.height, y: 0))
                stripe.addLine(to: CGPoint(x: x + size.height + Self.period / 2, y: 0))
                stripe.addLine(to: CGPoint(x: x + Self.period / 2, y: size.height))
                stripe.closeSubpath()
                context.fill(stripe, with: .color(.accentColor))
                x += Self.period
            }
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
                    Button("Show the Journal") { NSWorkspace.shared.activateFileViewerSelecting([journal]) }
                }
                Spacer()
                Button("Done", action: done).keyboardShortcut(.defaultAction)
            }
        }
    }
}
