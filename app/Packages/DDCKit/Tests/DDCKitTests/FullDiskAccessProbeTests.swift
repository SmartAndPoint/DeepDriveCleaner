import Foundation
import Testing
@testable import DDCKit

/// The probe over a temporary home: no test opens the real TCC.db or
/// `~/Library/Safari` (story "Full Disk Access check on macOS 27", DDC-002).
@Suite("Full Disk Access probe")
struct FullDiskAccessProbeTests {
    let home: TempDataDirectory

    init() throws {
        home = try TempDataDirectory()
    }

    var probe: FullDiskAccessProbe { FullDiskAccessProbe(locations: FullDiskAccessProbe.locations(home: home.url)) }
    var tcc: URL { home.url.appending(path: "Library/Application Support/com.apple.TCC/TCC.db") }
    var safari: URL { home.url.appending(path: "Library/Safari") }

    func makeTCC() throws {
        try FileManager.default.createDirectory(at: tcc.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data().write(to: tcc)
    }

    func makeSafari(withEntry: Bool) throws {
        try FileManager.default.createDirectory(at: safari, withIntermediateDirectories: true)
        if withEntry { try Data().write(to: safari.appending(path: "History.db")) }
    }

    @Test("the locations are the home's TCC.db file, then its Safari folder")
    func locations() {
        #expect(probe.locations == [
            .init(url: tcc, kind: .file),
            .init(url: safari, kind: .folder),
        ])
    }

    @Test("a readable TCC.db grants, and the Safari folder is not tried")
    func readableTCC() throws {
        try makeTCC()
        #expect(probe.check() == .granted)
        #expect(probe.steps().count == 1)
    }

    @Test("without TCC.db a listable Safari folder grants, empty or not")
    func safariAnswers() throws {
        try makeSafari(withEntry: false)
        #expect(probe.check() == .granted)
        try makeSafari(withEntry: true)
        #expect(probe.check() == .granted)
        #expect(probe.steps().map(\.errno) == [ENOENT, nil])
    }

    @Test("with neither, access is missing with ENOENT")
    func neither() {
        #expect(probe.check() == .missing(errno: ENOENT))
        #expect(probe.steps().count == 2)
    }

    @Test("a refused TCC.db and a listable Safari folder grant, as macOS 27 may refuse TCC.db")
    func refusedTCCThenSafari() throws {
        try makeTCC()
        try makeSafari(withEntry: true)
        try FileManager.default.setAttributes([.posixPermissions: 0], ofItemAtPath: tcc.path)
        defer { try? FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: tcc.path) }
        #expect(probe.check() == .granted)
        #expect(probe.steps().map(\.errno) == [EACCES, nil])
    }

    @Test("a refused TCC.db with no Safari folder misses access, with the last step's errno")
    func refusedTCCAlone() throws {
        try makeTCC()
        try FileManager.default.setAttributes([.posixPermissions: 0], ofItemAtPath: tcc.path)
        defer { try? FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: tcc.path) }
        #expect(probe.check() == .missing(errno: ENOENT))
        #expect(probe.steps().map(\.errno) == [EACCES, ENOENT])
    }

    @Test("a refused TCC.db and a refused Safari folder miss access, as without the grant on any macOS")
    func bothRefused() throws {
        try makeTCC()
        try makeSafari(withEntry: true)
        try FileManager.default.setAttributes([.posixPermissions: 0], ofItemAtPath: tcc.path)
        try FileManager.default.setAttributes([.posixPermissions: 0], ofItemAtPath: safari.path)
        defer {
            try? FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: tcc.path)
            try? FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: safari.path)
        }
        #expect(probe.check() == .missing(errno: EACCES))
    }

    @Test("without TCC.db an unlistable Safari folder is missing access")
    func unlistableSafari() throws {
        try makeSafari(withEntry: true)
        try FileManager.default.setAttributes([.posixPermissions: 0], ofItemAtPath: safari.path)
        defer { try? FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: safari.path) }
        #expect(probe.check() == .missing(errno: EACCES))
    }

    @Test("an empty list is missing access")
    func empty() {
        #expect(FullDiskAccessProbe(locations: []).check() == .missing(errno: ENOENT))
    }
}
