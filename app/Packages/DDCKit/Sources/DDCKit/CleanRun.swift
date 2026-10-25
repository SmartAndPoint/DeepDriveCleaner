import Foundation

/// The only code that runs `clean --items`, its dry run included (ADR "Menu
/// bar app: Swift shell over the ddc CLI", §7). The sheet's Clean action is
/// the one caller of `cleanConfirmedItems()`; a structural test holds both.
extension ReviewModel {
    // MARK: - Live warnings

    /// The dry run over the sheet's items: one prediction per line, shown on
    /// its item as it arrives. Check again runs it anew.
    public func checkWarnings() {
        guard let sheet, sheet.stage == .confirm, let items else { return }
        stopWarnings()
        guard let runner else {
            self.sheet?.warnings = .failed("The app's embedded CLI is missing. Reinstall DeepDriveCleaner.")
            return
        }
        let listed = sheet.confirmation.listed(checkedAgainst: currentItems(items))
        guard !listed.isEmpty else {
            self.sheet?.warnings = .failed("None of these items can be cleaned any more.")
            return
        }
        guard let file = writeList(listed.map(\.item)) else {
            self.sheet?.warnings = .failed("The list of items could not be written.")
            return
        }
        self.sheet?.predictions = [:]
        self.sheet?.warnings = .waiting
        let ticket = CLIRunner.Ticket()
        let (events, sink) = AsyncStream<RunEvent>.makeStream()
        let task = Task {
            let consumer = Task { @MainActor in
                var line = 0
                for await event in events {
                    guard dryRun?.ticket == ticket else { continue }
                    switch event {
                    case .started:
                        self.sheet?.warnings = .checking
                    case .line(let text):
                        // The lines follow the list's order, one per item; one that
                        // cannot be read still takes its item's place.
                        let parsed = try? PreflightLine.read(text)
                        if case .summary? = parsed { continue }
                        defer { line += 1 }
                        // The path confirms the match.
                        guard case .item(let item)? = parsed, line < listed.count, listed[line].item.path == item.path else { continue }
                        self.sheet?.predictions[listed[line].id] = item.prediction
                    case .progress:
                        break
                    }
                }
            }
            let result = await runner.run(
                .dryRun,
                ["clean", "--items", file.path, "--dry-run", "--json"],
                options: CLIRunner.Options(
                    ticket: ticket,
                    stdoutLine: { sink.yield(.line($0)) },
                    onStart: { sink.yield(.started(nil)) }
                )
            )
            sink.finish()
            await consumer.value
            try? FileManager.default.removeItem(at: file)
            guard dryRun?.ticket == ticket else { return }
            dryRun = nil
            self.sheet?.warnings = switch result.outcome {
            case .done: .checked
            case .busy: .busy
            case .cancelled: .stopped
            default: .failed(Self.failure(result, doing: "Checking for warnings"))
            }
            if result.outcome != .done, result.outcome != .cancelled {
                log("dry run ended \(result.outcome): \(result.stderrTail.suffix(3).joined(separator: " | "))")
            }
        }
        dryRun = (ticket, task)
    }

    /// Stops a dry run that is waiting or running; it holds the shared lock
    /// and walks every listed tree.
    func stopWarnings() {
        guard let dryRun else { return }
        self.dryRun = nil
        if sheet?.warnings == .waiting || sheet?.warnings == .checking { sheet?.warnings = .stopped }
        let ticket = dryRun.ticket
        if let runner { Task { await runner.cancel(ticket) } }
    }

    // MARK: - The clean

    /// The sheet's Clean action, and nothing else in the app: `clean --items`
    /// with exactly the confirmed items, each with its class, action and
    /// `permanent`. Pressing it twice starts one clean.
    public func cleanConfirmedItems() {
        guard let sheet, sheet.stage == .confirm, !isCleaning, let items else { return }
        stopWarnings()
        guard let runner else {
            self.sheet?.stage = .stopped("The app's embedded CLI is missing, so it cannot clean. Reinstall DeepDriveCleaner.", journal: nil)
            return
        }
        let confirmation = sheet.confirmation
        // A type switched off since the sheet opened leaves the list: the app
        // never lists an ignored item (ADR §6 item 1).
        let listed = confirmation.listed(checkedAgainst: currentItems(items))
        guard !listed.isEmpty else {
            self.sheet?.stage = .stopped("None of these items can be cleaned any more. Nothing was touched.", journal: nil)
            return
        }
        guard let file = writeList(listed.map(\.item)) else {
            self.sheet?.stage = .stopped("The list of items could not be written. Nothing was touched.", journal: nil)
            return
        }
        let volumes = items.report.volumes
        let ticket = CLIRunner.Ticket()
        cleanTicket = ticket
        isCleaning = true
        self.sheet?.stage = .cleaning(.waiting)
        setBusy(true)
        log("clean of \(listed.count) items requested")
        cleanTask = Task {
            await runClean(runner, file: file, ticket: ticket, confirmation: confirmation, volumes: volumes)
            cleanTask = nil
            cleanTicket = nil
            isCleaning = false
        }
    }

    private func runClean(
        _ runner: any CLIRunning,
        file: URL,
        ticket: CLIRunner.Ticket,
        confirmation: Confirmation,
        volumes: [ScanReport.Volume]
    ) async {
        let lastScan = data.lastScan
        let (events, sink) = AsyncStream<RunEvent>.makeStream()
        // The file's date when the process starts, not when Clean was pressed:
        // a scan queued ahead of the clean writes the file too. Nil: never started.
        var atStart: Date??
        var watcher: Task<Void, Never>?
        var progress: CleanProgress?
        let consumer = Task { @MainActor in
            var otherVersion = false
            for await event in events {
                switch event {
                case .started(let baseline):
                    atStart = baseline
                    cleanProcessLive = true
                    setPhase(.preparing(since: clock()))
                    watcher = Task { @MainActor in
                        // A CLI that writes no events saves its rescan before it
                        // touches the first item; while the clean holds the lock
                        // nothing else writes the file.
                        while !Task.isCancelled {
                            if DataDirectory.modified(lastScan) != baseline {
                                setPhase(.cleaning(since: clock()))
                                return
                            }
                            try? await Task.sleep(for: pollInterval)
                        }
                    }
                case .progress(.event(let event)):
                    // The first event hands the phase to the events.
                    watcher?.cancel()
                    if progress == nil { progress = CleanProgress() }
                    // In place: a copy per line would be quadratic over thousands of items.
                    progress?.apply(event)
                    if progress?.cleaning == true { setPhase(.cleaning(since: clock())) }
                    // Item lines can come by the thousand: they show with the next tick.
                    if case .item = event { continue }
                    sheet?.progress = progress
                case .progress(.otherVersion(let version)):
                    if !otherVersion { log("progress events of schema \(version) ignored") }
                    otherVersion = true
                case .line:
                    break
                }
            }
            if let progress { sheet?.progress = progress }
        }
        let result = await runner.run(
            .clean,
            ["clean", "--items", file.path, "--json", "--progress", "json"],
            options: CLIRunner.Options(
                ticket: ticket,
                // On the runner's reader thread: decode and yield, never wait.
                stderrLine: { line in if let read = ProgressEvent.read(line) { sink.yield(.progress(read)) } },
                // Read on the runner's side, the moment the process exists.
                onStart: { sink.yield(.started(DataDirectory.modified(lastScan))) }
            )
        )
        sink.finish()
        await consumer.value
        watcher?.cancel()
        cleanProcessLive = false
        for done in processEnded { done() }
        processEnded = []
        try? FileManager.default.removeItem(at: file)
        log("clean ended \(result.outcome): \(result.stderrTail.suffix(3).joined(separator: " | "))")

        // The CLI says `cleaning` before it touches the first item, and saves
        // its rescan before that: neither means nothing was touched.
        let saved = atStart.map { DataDirectory.modified(lastScan) != $0 } ?? false
        let touched = saved || progress?.cleaning == true
        let stdout = result.stdout
        let journal = data.journal
        // What was done is read from the report whenever there is one (exit 0,
        // a cancel or another signal once the CLI's handler was installed),
        // never from how the process ended.
        var report: CleanReport?
        if !stdout.isEmpty {
            switch await Task.detached(operation: { Result { try CleanReport.read(stdout) } }).value {
            case .success(let read): report = read
            case .failure(let error): log("clean report unreadable: \(error)")
            }
        }
        let stage: Stage
        if let report {
            stage = .result(CleanResult.make(report: report, confirmation: confirmation, volumes: volumes))
            let gone = report.items.filter { $0.outcome == .deleted || $0.outcome == .trashed }
            var identities = Set(gone.map { Identity(path: $0.path, dev: $0.dev, ino: $0.ino) })
            // A finding inside a picked folder went to the Trash with it.
            let folders = gone.filter { $0.class == .userData }.map(\.path)
            if !folders.isEmpty {
                for row in items?.rows ?? [] where folders.contains(where: { ReviewItems.inside(row.finding.path, $0) }) {
                    identities.insert(Identity(row.finding))
                }
            }
            cleaned = (identities, clock())
        } else if result.outcome == .done {
            stage = .stopped("The clean ended, but its report could not be read. The journal lists what was done.", journal: journal)
        } else if touched {
            // Items may be gone: only the journal knows which.
            stage = .stopped(Self.failure(result, doing: "The clean") + " It stopped before it could report; the journal lists what was done.", journal: journal)
        } else {
            stage = .stopped(Self.failure(result, doing: "The clean") + " Nothing was touched.", journal: nil)
        }
        // Unless Cancel already closed it for a clean that never started; a
        // report or a clean that may have touched files always shows.
        if case .cleaning? = sheet?.stage {
            sheet?.stage = stage
        } else if report != nil || touched {
            sheet?.stage = stage
        }

        // The disk changed, or the CLI saved its rescan: the popover shows it
        // after a rescan of its own, and the icon turns until that ends.
        if report != nil || touched || result.outcome == .done, !quitting {
            rescanning = .afterClean
            await rescan()
            rescanning = nil
            if sheet == nil { reload() }
        }
        setBusy(false)
    }

    /// Forward only: a phase already reached keeps its start.
    private func setPhase(_ phase: Phase) {
        guard case .cleaning(let current)? = sheet?.stage, Self.step(phase) > Self.step(current) else { return }
        sheet?.stage = .cleaning(phase)
    }

    private static func step(_ phase: Phase) -> Int {
        switch phase {
        case .waiting: 0
        case .preparing: 1
        case .cleaning: 2
        case .stopping: 3
        }
    }

    /// Cancel during a clean: the CLI stops after the current item. A clean
    /// still waiting behind another run never starts, and the sheet says so at once.
    public func cancelClean() {
        guard let cleanTicket, case .cleaning(let phase)? = sheet?.stage else { return }
        if phase == .waiting, !cleanProcessLive {
            sheet?.stage = .stopped("Cancelled before it started. Nothing was touched.", journal: nil)
        } else {
            sheet?.stage = .cleaning(.stopping)
        }
        if let runner { Task { await runner.cancel(cleanTicket) } }
    }

    /// Quit during a clean: cancel it, then `done` once the CLI has exited,
    /// with no rescan after it: the CLI is a child process and would go on
    /// cleaning unwatched. False when no clean process runs; one still
    /// waiting never starts once the app is gone.
    public func prepareToQuit(_ done: @escaping @MainActor () -> Void) -> Bool {
        stopWarnings()
        quitting = true
        guard cleanProcessLive else { return false }
        cancelClean()
        processEnded.append(done)
        return true
    }

    // MARK: - Parts

    enum RunEvent: Sendable {
        /// `last-scan.json`'s date when the process started (the clean's baseline).
        case started(Date?)
        case line(String)
        /// A stderr line that is an event.
        case progress(ProgressEvent.Line)
    }

    /// The rows as the current `settings.json` makes them, for the payload's
    /// last check while the sheet shows (the window's own rows stay put).
    private func currentItems(_ items: ReviewItems) -> ReviewItems {
        guard settingsChanged else { return items }
        let settings = try? SharedSettings.read(at: data.settings)
        var fresh = ReviewItems.make(report: items.report, settings: settings, rules: builtRules)
        // Locks the window added since the scan stay.
        for lock in [ReviewItems.Lock.protected, .cleaned] {
            fresh = fresh.locking(Set(items.rows.filter { $0.lock == lock }.map(\.id)), as: lock)
        }
        return fresh
    }

    /// The list goes to a file: the runner's stdin is `/dev/null`. Unique per
    /// run, readable by the user only, removed when the run ends (it may wait
    /// behind another run first).
    private func writeList(_ items: [ListedItem]) -> URL? {
        let file = temporaryDirectory.appending(path: "ddc-items-\(UUID().uuidString).json")
        do {
            let data = try ItemList(items: items).encoded()
            guard FileManager.default.createFile(atPath: file.path, contents: data, attributes: [.posixPermissions: 0o600]) else {
                return nil
            }
            return file
        } catch {
            log("item list not written: \(error)")
            return nil
        }
    }

    /// One sentence for a run that did not end well.
    static func failure(_ result: CLIRunner.Result, doing what: String) -> String {
        switch result.outcome {
        case .refused:
            return "\(what) was refused by the CLI (exit 2): a bug in the app."
        case .busy:
            return "Another scan or clean holds the lock. Try again when it ends."
        case .error(let status):
            // `ddc clean: …` for a refused list, `ddc: …` otherwise.
            let line = result.stderrTail.last { ($0.hasPrefix("ddc: ") || $0.hasPrefix("ddc clean: ")) && !$0.hasPrefix("ddc: warning:") }
            let message = line.map { String($0[$0.index(after: $0.firstIndex(of: ":")!)...].dropFirst()) } ?? "exit \(status)"
            return "\(what) failed: \(message)."
        case .crashed(let signal):
            return "\(what) stopped unexpectedly (signal \(signal))."
        case .notLaunched(let reason):
            return "\(what) could not start: \(reason)."
        case .cancelled:
            return "Cancelled."
        case .done:
            return "\(what) ended without a report."
        }
    }
}
