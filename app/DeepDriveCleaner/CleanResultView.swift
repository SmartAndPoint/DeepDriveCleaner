import AppKit
import DDCKit
import SwiftUI

/// The result of a clean (artboard 6): the free space before and after, what
/// was done, and what was left alone grouped by cause.
struct CleanResultView: View {
    let result: CleanResult
    let done: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Label(result.cancelled ? "Stopped" : "Done", systemImage: result.cancelled ? "pause.circle.fill" : "checkmark.circle.fill")
                .font(.system(size: 20, weight: .semibold))
                .foregroundStyle(result.cancelled ? AnyShapeStyle(.primary) : AnyShapeStyle(ReportClassStyle.safe))

            ForEach(result.volumes, id: \.name) { volume in
                VolumeView(volume: volume)
            }
            if result.localSnapshots > 0 {
                Text("Local Time Machine snapshots hold part of the space until macOS thins them.")
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
            }

            HStack(spacing: 0) {
                Tile(value: result.deleted.count, label: "deleted · \(ByteSize.human(result.deleted.bytes))")
                if result.trashed.count > 0 {
                    Tile(value: result.trashed.count, label: "to the Trash · \(ByteSize.human(result.trashed.bytes))")
                }
                Tile(value: result.toolsRun.count, label: "tool commands")
                Tile(value: result.skipped.count, label: "skipped")
                Tile(value: result.failed, label: "failed", alert: result.failed > 0)
                if result.notRun.count > 0 {
                    Tile(value: result.notRun.count, label: "not run")
                }
            }

            if !result.causes.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    Text("Skipped, nothing was lost").font(.system(size: 13, weight: .semibold))
                    ScrollView {
                        VStack(alignment: .leading, spacing: 4) {
                            ForEach(result.causes, id: \.title) { cause in
                                DisclosureGroup {
                                    VStack(alignment: .leading, spacing: 2) {
                                        ForEach(cause.paths, id: \.self) { Text($0).font(.system(size: 11)).foregroundStyle(.secondary) }
                                    }
                                    .padding(.leading, 4)
                                } label: {
                                    HStack {
                                        VStack(alignment: .leading, spacing: 1) {
                                            Text(cause.title).font(.system(size: 12))
                                            Text(cause.count == 1 ? cause.paths[0] : Wording.items(cause.count))
                                                .font(.system(size: 11))
                                                .foregroundStyle(.secondary)
                                        }
                                        Spacer()
                                        Text(ByteSize.human(cause.bytes)).font(.system(size: 12)).monospacedDigit()
                                    }
                                }
                            }
                        }
                    }
                    .frame(maxHeight: 160)
                    Text("They stay selected for the next clean.").font(.system(size: 12)).foregroundStyle(.secondary)
                }
            }

            if !result.failures.isEmpty {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Failed").font(.system(size: 13, weight: .semibold))
                    ForEach(Array(result.failures.enumerated()), id: \.offset) { _, failure in
                        Text("\(failure.path): \(failure.error)").font(.system(size: 11)).foregroundStyle(ReportClassStyle.ink(.review))
                    }
                }
            }

            HStack {
                Button("Show the Journal") {
                    NSWorkspace.shared.activateFileViewerSelecting([URL(filePath: result.journal)])
                }
                Spacer()
                Button("Done", action: done).keyboardShortcut(.defaultAction)
            }
        }
    }
}

private struct VolumeView: View {
    let volume: CleanResult.Volume

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text("Free space\(volume.name == "The startup disk" ? "" : " on \(volume.name)")")
                Spacer()
                Text(ByteSize.human(volume.freeBefore)).foregroundStyle(.secondary)
                Text("→")
                Text(ByteSize.human(volume.freeAfter)).fontWeight(.semibold)
            }
            .font(.system(size: 13))
            .monospacedDigit()
            if let total = volume.total, total > 0 {
                GeometryReader { geometry in
                    let width = geometry.size.width
                    ZStack(alignment: .leading) {
                        Color.secondary.opacity(0.15)
                        ReportClassStyle.safe.opacity(0.45).frame(width: width * CGFloat(min(volume.freeAfter, total)) / CGFloat(total))
                        ReportClassStyle.safe.frame(width: width * CGFloat(min(volume.freeBefore, total)) / CGFloat(total))
                    }
                }
                .frame(height: 8)
                .clipShape(RoundedRectangle(cornerRadius: 4))
                .accessibilityElement()
                .accessibilityLabel("Free space grew from \(ByteSize.human(volume.freeBefore)) to \(ByteSize.human(volume.freeAfter)) of \(ByteSize.human(total))")
            }
            Text("APFS may take a moment to give the space back.").font(.system(size: 11)).foregroundStyle(.secondary)
        }
    }
}

private struct Tile: View {
    let value: Int
    let label: String
    var alert = false

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(Wording.number(value)).font(.system(size: 20, weight: .semibold)).monospacedDigit()
                .foregroundStyle(alert ? AnyShapeStyle(ReportClassStyle.ink(.review)) : AnyShapeStyle(.primary))
            Text(label).font(.system(size: 11)).foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        // "12 cleaned", not "12" and "cleaned" apart.
        .accessibilityElement(children: .combine)
    }
}
