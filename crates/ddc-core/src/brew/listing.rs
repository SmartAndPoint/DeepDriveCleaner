//! `ddc disk brew-packages`: the formulae installed on request under the
//! Homebrew area, each with its kegs measured by ddc ("measured, never
//! assumed": brew's JSON has no sizes), what it needs, what needs it, its
//! service and whether a removal is offered (menu bar ADR §6 item 10,
//! amendment of 2026-10-29). It takes no lock, writes nothing and saves
//! nothing; the re-read of `ddc disk free` decides again right before a
//! removal.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::model::{BREW_PACKAGES_SCHEMA_VERSION, BrewPackages, Dependency, Keg, NotOffered, Package, ServicesState};
use super::{AskError, Budget, ENV, Graph, Service, Services, ask_info, ask_services};
use crate::disk::Sources;
use crate::exec::{CommandRunner, NATIVE_TIMEOUT};
use crate::free::{Refusal, measure, probe};

/// Why nothing could be listed: exit 1.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ListError {
    /// The location probe refused the area's `brew`.
    #[error("brew could not list its packages: the location check refused it ({0:?})")]
    Probe(Refusal),
    /// `brew info` failed, timed out or printed what is not JSON.
    #[error("brew could not list its packages: {0}")]
    Info(String),
}

/// Lists the formulae under the area's `brew`, within one
/// [`NATIVE_TIMEOUT`]. `skips` bind the measuring of the kegs.
pub fn list(
    commands: &dyn CommandRunner,
    sources: &Sources,
    home: &Path,
    skips: &[PathBuf],
) -> Result<BrewPackages, ListError> {
    let budget = Budget::new(NATIVE_TIMEOUT);
    let target = probe::brew(commands, sources, home, &ENV, &budget).map_err(ListError::Probe)?;
    let installed = ask_info(commands, &target.exe, budget.remaining()).map_err(|e| match e {
        AskError::Failed(why) => ListError::Info(why),
        AskError::TimedOut => ListError::Info(format!("timed out after {} s", budget.total().as_secs())),
    })?;
    let services = ask_services(commands, &target.exe, &target.prefix, budget.remaining());
    let graph = Graph::new(&installed);
    Ok(build(&graph, &services, &target.prefix, &target.cellar, home, skips))
}

/// The listing of `graph`, its kegs measured in one walk.
pub fn build(
    graph: &Graph,
    services: &Services,
    prefix: &Path,
    cellar: &Path,
    home: &Path,
    skips: &[PathBuf],
) -> BrewPackages {
    let nodes = graph.nodes();
    let paths: Vec<Vec<PathBuf>> =
        nodes.iter().map(|n| n.versions.iter().map(|v| cellar.join(&n.name).join(v)).collect()).collect();
    let mut sizes = measure::regions(&paths.concat(), skips).into_iter();
    let kegs: Vec<Vec<Keg>> = nodes
        .iter()
        .zip(&paths)
        .map(|(n, ps)| {
            n.versions
                .iter()
                .zip(ps)
                .map(|(v, p)| Keg { version: v.clone(), path: p.clone(), bytes: sizes.next().flatten() })
                .collect()
        })
        .collect();
    let bytes = |i: usize| kegs[i].iter().map(|k| k.bytes).sum::<Option<u64>>();
    let full_names = |set: &BTreeSet<usize>| set.iter().map(|&i| nodes[i].full_name.clone()).collect::<Vec<_>>();
    let state = match services {
        Services::Listed(_) => ServicesState::Listed,
        Services::NotKnown => ServicesState::NotKnown,
        Services::Unreadable => ServicesState::Unreadable,
    };

    let mut formulae: Vec<Package> = (0..nodes.len())
        .filter(|&i| nodes[i].on_request)
        .map(|i| {
            let n = &nodes[i];
            let needed = graph.needed_by(i, &BTreeSet::new());
            let orphaned = graph.would_orphan(i);
            let service = services.of(&n.name, home);
            let not_offered = if !graph.incomplete.is_empty() {
                Some(NotOffered::FieldMissing)
            } else if !needed.is_empty() {
                Some(NotOffered::Needed)
            } else if n.pinned {
                Some(NotOffered::Pinned)
            } else if service == Service::Unreadable {
                Some(NotOffered::ServicesUnreadable)
            } else if !service.allows_removal() {
                Some(NotOffered::Service)
            } else {
                None
            };
            let mut dependencies: Vec<Dependency> = graph
                .closure(i)
                .into_iter()
                .map(|d| Dependency {
                    full_name: nodes[d].full_name.clone(),
                    bytes: bytes(d),
                    on_request: nodes[d].on_request,
                    pinned: nodes[d].pinned,
                    alone: orphaned.orphans.contains(&d),
                })
                .collect();
            dependencies.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.full_name.cmp(&b.full_name)));
            Package {
                name: n.name.clone(),
                full_name: n.full_name.clone(),
                pinned: n.pinned,
                kegs: kegs[i].clone(),
                bytes: bytes(i),
                service,
                needed_by: needed.formulae,
                needed_by_casks: needed.casks,
                dependencies,
                would_orphan: full_names(&orphaned.orphans),
                would_orphan_bytes: orphaned.orphans.iter().map(|&d| bytes(d)).sum(),
                kept_pinned: full_names(&orphaned.kept_pinned),
                not_offered,
            }
        })
        .collect();
    // Largest first; one not measured after every measured one.
    formulae.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.full_name.cmp(&b.full_name)));
    BrewPackages {
        schema_version: BREW_PACKAGES_SCHEMA_VERSION,
        ddc_version: env!("CARGO_PKG_VERSION").to_owned(),
        prefix: prefix.to_path_buf(),
        cellar: cellar.to_path_buf(),
        services: state,
        formulae,
        orphans: full_names(&graph.orphans()),
        incomplete: graph.incomplete.clone(),
    }
}
