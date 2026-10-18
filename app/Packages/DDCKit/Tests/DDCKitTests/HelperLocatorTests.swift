import Foundation
import Testing
@testable import DDCKit

@Suite("HelperLocator")
struct HelperLocatorTests {
    let root = FileManager.default.temporaryDirectory.appending(path: "ddckit-\(UUID().uuidString)")

    // Swift Testing makes a suite instance per test, so each test removes its own.
    func remove() {
        try? FileManager.default.removeItem(at: root)
    }

    /// A bundle skeleton with `Contents/MacOS/DeepDriveCleaner`.
    func bundle() throws -> (bundle: URL, main: URL) {
        let bundle = root.appending(path: "DeepDriveCleaner.app")
        let main = bundle.appending(path: "Contents/MacOS/DeepDriveCleaner")
        try FileManager.default.createDirectory(at: main.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data().write(to: main)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: main.path)
        return (bundle, main)
    }

    static func helper(in bundle: URL, mode: Int) throws -> URL {
        let cli = bundle.appending(path: HelperLocator.relativePath)
        try FileManager.default.createDirectory(at: cli.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data().write(to: cli)
        try FileManager.default.setAttributes([.posixPermissions: mode], ofItemAtPath: cli.path)
        return cli
    }

    @Test("the CLI is found in Contents/Helpers")
    func found() throws {
        defer { remove() }
        let (bundle, main) = try bundle()
        let cli = try Self.helper(in: bundle, mode: 0o755)
        #expect(try HelperLocator.cli(bundleURL: bundle, mainExecutable: main) == cli)
    }

    @Test("a missing or non-executable CLI is refused")
    func refused() throws {
        defer { remove() }
        let (bundle, main) = try bundle()
        #expect(throws: HelperLocator.LocateError.self) { try HelperLocator.cli(bundleURL: bundle, mainExecutable: main) }
        let cli = try Self.helper(in: bundle, mode: 0o644)
        #expect(throws: HelperLocator.LocateError.notExecutable(cli.path)) {
            try HelperLocator.cli(bundleURL: bundle, mainExecutable: main)
        }
    }

    @Test("the app's own executable is never taken for the CLI")
    func notTheApp() throws {
        defer { remove() }
        let (bundle, _) = try bundle()
        let cli = try Self.helper(in: bundle, mode: 0o755)
        #expect(throws: HelperLocator.LocateError.isMainExecutable(cli.path)) {
            try HelperLocator.cli(bundleURL: bundle, mainExecutable: cli)
        }
    }
}
