/// What a skip means for the next clean: the table of story "Review window:
/// after a clean, only skips that can clear stay selected". The sheet's
/// advice and the result read it, so they never advise differently for the
/// same item. In this order the sheet's warnings line names them.
public enum AfterClean: Sendable, Hashable, Comparable {
    /// Can clear by itself or by one step of the user's: stays selected.
    case keep
    /// The CLI re-checks against the last scan, so only a new scan clears it.
    case rescan
    /// Never cleanable from here.
    case never
}

extension SkipReason {
    /// No `default:`: a new reason cannot be added without choosing.
    public var afterClean: AfterClean {
        switch self {
        case .ownerRunning, .inUse, .inUseUnknownCwd, .toolNotReady:
            .keep
        // `incomplete`: the walk was partial; nothing in it says a permission would fix it.
        case .changedSinceScan, .parentChanged, .missing, .ruleMismatch, .notAnEntry, .entryLocked, .symlink, .notADirectory,
             .incomplete:
            .rescan
        case .kept, .protected, .projectRoot, .scanRoot, .outsideScanRoots, .actionNotAllowed, .needsConfirmation,
             .insideAnotherItem, .nativeUnavailable:
            .never
        }
    }
}
