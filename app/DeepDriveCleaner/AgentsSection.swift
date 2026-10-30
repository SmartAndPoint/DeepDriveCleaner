import DDCKit
import SwiftUI

/// The Whole disk view's Coding agents section (story "Coding agents in the
/// Whole disk view"): per harness, its folders with their sizes and its
/// sessions live now. Read-only: it names and sizes, and offers no button
/// that removes anything; a folder's or a harness's context menu shows it in
/// Finder. Its bytes are part of "Rest of the home folder".
struct AgentsSection: View {
    let agents: AgentsModel

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Coding Agents").font(.system(size: 13, weight: .semibold))
                    Text("What coding-agent tools keep in the home folder, and their sessions running now. Part of the rest of the home folder.")
                        .font(.system(size: 12))
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Spacer(minLength: 12)
                if agents.refreshing {
                    ProgressView().controlSize(.small)
                    Text(agents.measuring ? "Measuring…" : "Reading…").font(.system(size: 12)).foregroundStyle(.secondary)
                }
                Button("Refresh") { Task { await agents.refresh() } }
                    .disabled(!agents.canRefresh || agents.refreshing)
            }
            if let message = agents.message {
                Text(message)
                    .font(.system(size: 12))
                    .foregroundStyle(ReportClassStyle.ink(.review))
                    .fixedSize(horizontal: false, vertical: true)
            }
            if let summary = agents.summary {
                if summary.rows.isEmpty {
                    Text("No coding-agent tool keeps a folder in this home folder.")
                        .font(.system(size: 12))
                        .foregroundStyle(.secondary)
                } else {
                    VStack(spacing: 0) {
                        ForEach(summary.rows) { row in
                            AgentsRow(row: row)
                            if row.kind == .harness || summary.rows.last == row { Divider() }
                        }
                    }
                }
            }
        }
    }
}

private struct AgentsRow: View {
    let row: AgentsSummary.Row

    var body: some View {
        if row.paths.isEmpty {
            line
        } else {
            line.contextMenu { RevealButton(reveal: .finder(row.paths)) }
        }
    }

    private var line: some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            mark
            VStack(alignment: .leading, spacing: 2) {
                Text(row.title)
                    .font(.system(size: row.kind == .harness ? 13 : 12, weight: row.kind == .harness ? .semibold : .regular))
                    .lineLimit(1)
                    .truncationMode(.middle)
                if !row.note.isEmpty {
                    Text(row.note).font(.system(size: 11)).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                }
            }
            Spacer(minLength: 12)
            Text(row.size).font(.system(size: 12)).monospacedDigit().fixedSize()
        }
        .padding(.leading, row.kind == .harness ? 0 : 24)
        .padding(.vertical, row.kind == .harness ? 8 : 4)
        .accessibilityElement(children: .combine)
    }

    @ViewBuilder private var mark: some View {
        switch row.kind {
        case .harness:
            Image(systemName: "terminal").font(.system(size: 11)).foregroundStyle(.secondary).frame(width: 12)
        case .session:
            Circle().fill(.green).frame(width: 7, height: 7).frame(width: 12).accessibilityLabel("Live")
        case .records:
            Image(systemName: "exclamationmark.triangle").font(.system(size: 10)).foregroundStyle(ReportClassStyle.ink(.review)).frame(width: 12)
        case .folder, .transcripts:
            Color.clear.frame(width: 12, height: 1)
        }
    }
}
