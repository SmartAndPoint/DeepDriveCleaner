import Foundation

/// One row of the Types pane (artboard 10): a group of `rules --json` with
/// switchable rules, the group's label, its help, its class chips, and
/// whether it is on.
public struct TypesRow: Sendable, Equatable, Identifiable {
    public let label: String
    public let help: String?
    /// One chip per class among its switchable rules, in class order: no
    /// row claims to be safer than its least safe rule (decision D2).
    public let classes: [ScanReport.RiskClass]
    /// The ids the switch writes to `disabled_rules`.
    public let ids: [String]
    /// Off only when every one of `ids` is in `disabled_rules`.
    public let isOn: Bool

    public var id: String { label }

    /// "Rebuildable · Review".
    public var chips: String { classes.map(CleanWording.className).joined(separator: " · ") }
}

/// The Types pane's rows and the switch's edit. Switching a type off makes
/// its findings `ignored` (ADR §6 item 7): the scanner still walks there.
public enum TypesRows {
    public static func make(rules: RuleList, disabled: [String]) -> [TypesRow] {
        let disabled = Set(disabled)
        return rules.groups.compactMap { label in
            let members = rules.rules.filter { $0.group == label && $0.switchable }
            guard !members.isEmpty else { return nil }
            let classes = ScanReport.RiskClass.allCases.filter { riskClass in members.contains { $0.class == riskClass } }
            let ids = members.map(\.id)
            return TypesRow(label: label, help: help[label], classes: classes, ids: ids, isOn: !ids.allSatisfy(disabled.contains))
        }
    }

    /// Off writes every switchable id of the row; on removes them all.
    public static func set(_ row: TypesRow, on: Bool, in file: inout SettingsFile) {
        if on {
            file.disabledRules.removeAll { row.ids.contains($0) }
        } else {
            file.disabledRules += row.ids.filter { !file.disabledRules.contains($0) }
        }
    }

    /// The help under each row, keyed by the group label `rules --json`
    /// gives (`GROUPS` in `crates/ddc-core/src/rules/listing.rs`). A contract
    /// test checks that every row has one.
    public static let help: [String: String] = [
        "Python environments": ".venv, venv, conda envs inside projects, environments of the conda in your home folder",
        "node_modules": "Dependencies of JavaScript projects",
        "Build outputs": "target/, CMake and Gradle build folders, Pods",
        "Build caches in projects": "__pycache__, .next, .turbo, Gradle: rebuilt on the next build",
        "Package downloads": "npm, pnpm, yarn, bun, pip, Poetry, conda, Homebrew, CocoaPods",
        "Toolchain caches": "Cargo, uv, Go modules and build cache, Gradle: needed to build offline",
        "Maven and test browsers": "The Maven repository, Playwright, Puppeteer and Cypress browsers: downloaded again when needed",
        "Browser and app caches": "Browsers, Spotify, Slack",
        "IDE caches": "JetBrains, VS Code, Cursor: re-indexed after cleaning",
        "Xcode builds": "DerivedData, simulator caches: a full rebuild next time",
        "Xcode device support": "Symbols of iOS versions you debugged on",
        "Old toolchains": "Node, Python and Rust versions no project uses",
        "ML models": "Ollama, Hugging Face, PyTorch",
        "VM and container images": "Docker Desktop, Podman: image and builder prune only",
    ]
}
