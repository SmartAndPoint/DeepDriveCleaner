import DDCKit
import SwiftUI

/// Who holds the CLI's lock when the sheet's check, its Clean or Scan now
/// could not start (story "Busy: name the scan or clean that holds the lock,
/// and offer to stop it"): one line per holder with what Stop does and its
/// Stop, then Wait. With no holder named, only Wait.
struct LockBusyPanel: View {
    let busy: LockBusy
    let stop: (LockHolder.ID) -> Void
    let wait: () -> Void
    let stopWaiting: () -> Void

    var body: some View {
        TimelineView(.periodic(from: .now, by: 30)) { context in
            VStack(alignment: .leading, spacing: 8) {
                if busy.purpose == .cleaning {
                    Text(Wording.lockCleanNotStarted).font(.system(size: 12, weight: .semibold))
                }
                if busy.holders.isEmpty {
                    Text(Wording.lockNoHolder).font(.system(size: 12)).foregroundStyle(.secondary)
                }
                ForEach(busy.holders) { holder in
                    HStack(alignment: .top, spacing: 8) {
                        Image(systemName: "lock").foregroundStyle(.secondary)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(Wording.lockHolder(holder, now: context.date)).font(.system(size: 12))
                            Text(busy.stopping.contains(holder.id) ? Wording.lockStopping(holder) : Wording.lockStopEffect(holder))
                                .font(.system(size: 11)).foregroundStyle(.secondary)
                        }
                        .fixedSize(horizontal: false, vertical: true)
                        Spacer(minLength: 8)
                        Button("Stop") { stop(holder.id) }
                            .controlSize(.small)
                            .disabled(busy.stopping.contains(holder.id))
                    }
                }
                HStack(spacing: 8) {
                    if busy.waiting {
                        ProgressView().controlSize(.small)
                        Text(Wording.lockWaiting(busy.purpose)).font(.system(size: 12)).foregroundStyle(.secondary)
                        Button("Stop Waiting", action: stopWaiting).controlSize(.small)
                    } else {
                        Button("Wait", action: wait).controlSize(.small)
                        Text(Wording.lockWaitHint(busy.purpose)).font(.system(size: 11)).foregroundStyle(.secondary)
                    }
                }
            }
            .padding(10)
            .background(Color.secondary.opacity(0.06), in: RoundedRectangle(cornerRadius: 8))
        }
    }
}
