import DDCKit
import SwiftUI

/// The update line (ADR "Updates through Sparkle, alongside the Homebrew
/// cask", §2 and §3): "Version X is available", its release notes and Update
/// and Relaunch, then the download's progress, the wait for the runner, or an
/// error with the release page as the manual route. The popover shows it above
/// its footer, and Settings → General under the update rows. It shows nothing
/// while no update is known, or when the updater did not start.
struct UpdateLineView: View {
    let model: UpdateModel
    /// The runner's state: Update and Relaunch waits for it, and says what for.
    let activity: RunnerActivity?

    var body: some View {
        if let item = model.item, !isUnavailable {
            VStack(alignment: .leading, spacing: 6) {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Image(systemName: "arrow.down.circle").foregroundStyle(.secondary).accessibilityHidden(true)
                    Text(title(item.shortVersion)).font(.system(size: 13, weight: .semibold))
                    Spacer(minLength: 8)
                    if let url = item.releaseURL {
                        Link(model.informationOnly ? "Release Page" : "Release Notes", destination: url)
                            .font(.system(size: 12))
                    }
                }
                detail(item)
            }
            .padding(10)
            .background(Color.secondary.opacity(0.1), in: RoundedRectangle(cornerRadius: 8))
        }
    }

    private var isUnavailable: Bool {
        if case .unavailable = model.state { return true }
        return false
    }

    private func title(_ version: String) -> String {
        if model.informationOnly { return "Version \(version) needs a manual install" }
        switch model.state {
        case .downloading: return "Downloading version \(version)…"
        case .extracting: return "Preparing version \(version)…"
        case .waitingForIdle, .installing, .installingOnQuit: return "Installing version \(version)"
        default: return "Version \(version) is available"
        }
    }

    @ViewBuilder private func detail(_ item: UpdateFlow.Item) -> some View {
        if model.informationOnly {
            Text("It is signed by a different team, so it does not install itself. Download it from the release page.")
                .font(.system(size: 12)).foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        } else {
            switch model.state {
            case .downloading, .extracting:
                if let progress = model.progress {
                    ProgressView(value: progress).progressViewStyle(.linear)
                } else {
                    ProgressView().progressViewStyle(.linear)
                }
            case .waitingForIdle:
                Text(activity?.reason == .cleaning ? "Waiting for the clean to finish" : "Waiting for the scan to finish")
                    .font(.system(size: 12)).foregroundStyle(.secondary)
            case .installing, .installingOnQuit:
                Text("DeepDriveCleaner quits and opens again.")
                    .font(.system(size: 12)).foregroundStyle(.secondary)
            default:
                HStack(spacing: 8) {
                    Button("Update and Relaunch") { model.press() }
                        .controlSize(.small)
                        .disabled(!model.canPress)
                    if model.state == .checking {
                        ProgressView().controlSize(.small)
                    } else if let reason = activity?.reason {
                        Text(reason == .cleaning ? "after the clean finishes" : "after the scan finishes")
                            .font(.system(size: 12)).foregroundStyle(.secondary)
                    }
                }
                if let message = model.flow.message {
                    Text("\(message) The release page has the download too.")
                        .font(.system(size: 12)).foregroundStyle(ReportClassStyle.ink(.review))
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
    }
}
