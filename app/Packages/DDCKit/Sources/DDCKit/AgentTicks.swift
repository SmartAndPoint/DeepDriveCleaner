import Foundation

/// Whether the scheduler agent's hourly ticks stopped arriving (story
/// "Scheduled scans: register the agent again after an update", DDC-002):
/// on the owner's Mac launchd kept a registration it could not start while
/// `SMAppService` said enabled, and nothing told the person.
public enum AgentTicks {
    /// Two hourly ticks missed.
    public static let limit: TimeInterval = 2 * 3600

    /// True when, counted in awake time, more than `limit` passed since the
    /// later of the last agent tick and the watch's start. A mark saved with
    /// more uptime than now is from before a restart: only the time since
    /// boot counts. With neither mark, false: the watch has not started.
    /// `running`, when given, is this process's start: a Mac at the login
    /// window for hours, its person logged out, has no agent ticks either.
    public static func stopped(
        lastTick: AgentTickMark?, since baseline: AgentTickMark?, running: AgentTickMark? = nil, uptime: TimeInterval, limit: TimeInterval = limit
    ) -> Bool {
        guard lastTick != nil || baseline != nil else { return false }
        let marks = [lastTick, baseline, running].compactMap { $0 }
        guard !marks.isEmpty else { return false }
        let awake = marks.map { $0.uptime <= uptime ? uptime - $0.uptime : uptime }.min()
        return awake.map { $0 > limit } ?? false
    }
}
