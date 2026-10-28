import DDCKit
import ServiceManagement

/// `SchedulerAgent`'s port to `SMAppService.agent(plistName:)`: the plists
/// in `Contents/Library/LaunchAgents`. DDCKit never imports ServiceManagement.
@MainActor
final class SystemAgentService: AgentService {
    func status(_ plist: String) -> AgentStatus {
        switch SMAppService.agent(plistName: plist).status {
        case .enabled: .enabled
        case .requiresApproval: .requiresApproval
        case .notFound: .notFound
        case .notRegistered: .notRegistered
        @unknown default: .notRegistered
        }
    }

    func register(_ plist: String) throws {
        try SMAppService.agent(plistName: plist).register()
    }

    func unregister(_ plist: String) async throws {
        try await SMAppService.agent(plistName: plist).unregister()
    }
}
