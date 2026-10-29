import DDCKit
import SwiftUI

/// The Homebrew row's packages sheet (story "Whole disk: Homebrew packages,
/// their sizes, and brew uninstall"; menu bar ADR §6 items 10–11): the
/// formulae installed on request with what their versions take, why one is
/// not offered, and Uninstall… on each one brew lets go. Uninstall… opens the
/// area action sheet in its place, through `DiskModel.previewAction`; after a
/// formula was uninstalled, what it alone needed is offered once.
struct BrewPackagesSheet: View {
    let model: DiskModel

    private var packages: BrewPackagesModel { model.brewPackages }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(alignment: .firstTextBaseline) {
                Text("Homebrew packages").font(.system(size: 20, weight: .semibold))
                Spacer()
                if packages.asking, packages.listing != nil {
                    ProgressView().controlSize(.small)
                    Text("Asking Homebrew…").font(.system(size: 12)).foregroundStyle(.secondary)
                }
            }
            if let message = packages.message {
                Text(message).fixedSize(horizontal: false, vertical: true)
            }
            if let listing = packages.listing {
                notes(listing)
                if let orphans = packages.orphans {
                    orphansStep(orphans)
                }
                list(listing)
            } else if packages.asking {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text("Asking Homebrew…").foregroundStyle(.secondary)
                }
            }
            HStack {
                Button("Refresh") { Task { await packages.list() } }.disabled(packages.asking)
                Spacer()
                Button("Done") { packages.close() }.keyboardShortcut(.defaultAction)
            }
        }
        .frame(width: 640)
        .padding(24)
    }

    @ViewBuilder
    private func notes(_ listing: BrewPackages) -> some View {
        Text("The formulae you installed, with what their versions take in \(listing.cellar). Uninstalling one removes every version of it; brew install brings it back.")
            .font(.system(size: 12))
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
        if !listing.incomplete.isEmpty {
            Text("brew listed \(BrewWording.dependents(formulae: listing.incomplete, casks: [])) without a field DeepDriveCleaner needs, so nothing is offered.")
                .font(.system(size: 12))
                .foregroundStyle(ReportClassStyle.ink(.review))
                .fixedSize(horizontal: false, vertical: true)
        }
        if listing.services == .unreadable {
            Text("brew services could not be read, so nothing is offered.")
                .font(.system(size: 12))
                .foregroundStyle(ReportClassStyle.ink(.review))
        }
    }

    private func orphansStep(_ orphans: AreaActions.Offer) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            VStack(alignment: .leading, spacing: 2) {
                Text("Nothing installed needs these now").font(.system(size: 13, weight: .semibold))
                Text(BrewWording.dependents(formulae: orphans.names ?? [], casks: []) + (orphans.bytes.map { ", \(ByteSize.human($0))" } ?? ""))
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 12)
            Button(orphans.buttonTitle) { Task { await model.previewAction(orphans) } }
                .disabled(!model.canFree)
        }
        .padding(10)
        .background(RoundedRectangle(cornerRadius: 6).fill(Color(nsColor: .controlBackgroundColor)))
    }

    private func list(_ listing: BrewPackages) -> some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 0) {
                if listing.formulae.isEmpty {
                    Text("No formula is installed on request.").foregroundStyle(.secondary).padding(.vertical, 8)
                }
                ForEach(listing.formulae) { package in
                    PackageRow(package: package, offer: packages.offer(package), model: model)
                    Divider()
                }
            }
        }
        .frame(minHeight: 200, maxHeight: 420)
    }
}

private struct PackageRow: View {
    let package: BrewPackages.Package
    let offer: AreaActions.Offer?
    let model: DiskModel

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            VStack(alignment: .leading, spacing: 2) {
                Text(package.fullName).font(.system(size: 13, weight: .semibold))
                ForEach(lines, id: \.self) { line in
                    Text(line).font(.system(size: 12)).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                }
                if let why = BrewWording.notOffered(package) {
                    Text(why).font(.system(size: 12)).foregroundStyle(ReportClassStyle.ink(.review)).fixedSize(horizontal: false, vertical: true)
                }
                if let offer, let result = model.freeResults[offer.id] {
                    Text(FreeWording.result(result, name: offer.name))
                        .font(.system(size: 12))
                        .foregroundStyle(result.result == .freed ? Color.secondary : ReportClassStyle.ink(.review))
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            Spacer(minLength: 12)
            if let offer {
                Button(offer.buttonTitle) { Task { await model.previewAction(offer) } }
                    .controlSize(.small)
                    .disabled(!model.canFree)
            }
            Text(package.bytes.map(ByteSize.human) ?? "not measured").font(.system(size: 13)).monospacedDigit().fixedSize()
        }
        .padding(.vertical, 8)
        .accessibilityElement(children: .combine)
    }

    /// Its versions when there are several, what only it needs, the pinned
    /// ones kept, and a service brew could not be asked about.
    private var lines: [String] {
        var out: [String] = []
        if package.kegs.count > 1 {
            out.append("\(package.kegs.count) versions: " + package.kegs.map { keg in
                keg.version + (keg.bytes.map { " (\(ByteSize.human($0)))" } ?? "")
            }.joined(separator: ", "))
        }
        if !package.wouldOrphan.isEmpty {
            let size = package.wouldOrphanBytes.map { ", \(ByteSize.human($0))" } ?? ""
            out.append("Only it needs \(BrewWording.dependents(formulae: package.wouldOrphan, casks: []))\(size): offered after it is uninstalled.")
        }
        if !package.keptPinned.isEmpty {
            out.append("Kept, pinned: \(BrewWording.dependents(formulae: package.keptPinned, casks: [])).")
        }
        if let note = BrewWording.note(package) { out.append(note) }
        return out
    }
}
