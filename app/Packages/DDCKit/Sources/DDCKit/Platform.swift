import Foundation

/// The platform a report's CLI was built for (`Platform` in
/// `crates/ddc-core/src/platform/mod.rs`; ADR "Platform seam: one platform
/// module per OS with declared capabilities", §4). The app embeds the macOS
/// CLI, so its reports say `macos`; an unknown value is a decoding error.
public enum Platform: String, Decodable, Sendable, CaseIterable {
    case macos, linux, windows
}

/// What the CLI's build can do on its platform (`Capability`, ADR §3). The
/// scan, clean and disk reports list the declared ones in declaration order;
/// one not listed is absent, and the other fields read against that (no
/// `private_bytes` without `privateSize`, `full_disk_access` true without
/// `fullDiskAccess`). An unknown value is a decoding error.
public enum Capability: String, Decodable, Sendable, CaseIterable {
    case privateSize = "private-size"
    case inUseCheck = "in-use-check"
    case datalessFiles = "dataless-files"
    case fullDiskAccess = "full-disk-access"
    case localSnapshots = "local-snapshots"
    case wholeDisk = "whole-disk"
    case codingAgents = "coding-agents"
    case homebrew
    case schedule
    case notifications
}
