import Foundation

/// Finds the CLI embedded in the app bundle (ADR §1).
///
/// It lives in `Contents/Helpers`, not `Contents/MacOS`: on a case-insensitive
/// volume `deepdrivecleaner` and the main executable `DeepDriveCleaner` are the
/// same name. `Bundle.url(forAuxiliaryExecutable:)` looks next to the main
/// executable and would return the app itself, so the URL is built here.
public enum HelperLocator {
    public static let relativePath = "Contents/Helpers/deepdrivecleaner"

    public enum LocateError: Error, Equatable {
        case missing(String)
        case notExecutable(String)
        case isMainExecutable(String)
    }

    public static func cli(bundleURL: URL, mainExecutable: URL?) throws -> URL {
        let url = bundleURL.appending(path: relativePath, directoryHint: .notDirectory)
        let files = FileManager.default
        guard files.fileExists(atPath: url.path) else { throw LocateError.missing(url.path) }
        guard files.isExecutableFile(atPath: url.path) else { throw LocateError.notExecutable(url.path) }
        if let mainExecutable, sameFile(url, mainExecutable) { throw LocateError.isMainExecutable(url.path) }
        return url
    }

    /// Where Homebrew's cask links `ddc`.
    public static let linkCandidates = ["/opt/homebrew/bin/ddc", "/usr/local/bin/ddc"]

    /// The General pane's "Command-line tool" row: the first of `candidates`
    /// that resolves into this bundle, else the bundled helper's path. Only
    /// paths are resolved; no process is started.
    public static func commandLineTool(bundleURL: URL, candidates: [String] = linkCandidates) -> String {
        let helper = bundleURL.appending(path: relativePath, directoryHint: .notDirectory).path
        let bundle = FolderList.resolved(bundleURL.path)
        let link = candidates.first { candidate in
            guard FileManager.default.fileExists(atPath: candidate) else { return false }
            let target = FolderList.resolved(candidate)
            return target.hasPrefix(bundle + "/")
        }
        return link ?? helper
    }

    private static func sameFile(_ a: URL, _ b: URL) -> Bool {
        let key = URLResourceKey.fileResourceIdentifierKey
        guard let ida = try? a.resourceValues(forKeys: [key]).fileResourceIdentifier,
              let idb = try? b.resourceValues(forKeys: [key]).fileResourceIdentifier
        else { return false }
        return ida.isEqual(idb)
    }
}
