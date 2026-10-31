//! The parallel walk: one rayon task per directory. A directory's descriptor
//! is closed before its children are spawned, so open files stay bounded by
//! the worker count (launchd agents start with a soft limit of 256).

use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::inodes::{Holder, InodeTable};
use super::reader::EntryMeta;
use super::reader::{DirReader, EntryKind, ReadError, ReadErrorKind, SF_DATALESS, StdReader, SystemReader, Times};
use super::{
    DirCtx, EntryResult, OwnerEntries, OwnerKind, OwnerResult, Post, Pre, Progress, ReaderKind, RootResult, SkipReason,
    Skipped, Tag, Visitor, WalkError, WalkOptions, WalkOutput, WalkSetupError,
};
use crate::platform::meta;

/// [`OwnerAcc::modified`] before any time is folded in.
const NO_TIME: i64 = i64::MIN;

/// What an owner's listings read besides the defaults.
#[derive(Clone, Copy)]
struct Wants {
    private: bool,
    times: bool,
}

struct OwnerAcc {
    id: usize,
    tag: Tag,
    kind: OwnerKind,
    path: PathBuf,
    dev: u64,
    ino: u64,
    parent: (u64, u64),
    want_private: bool,
    /// Its listings read [`Times::Modified`] for [`Self::modified`].
    want_times: bool,
    /// The newest modification time folded in so far, or [`NO_TIME`].
    modified: AtomicI64,
    bytes: AtomicU64,
    shared: AtomicU64,
    private: AtomicU64,
    files: AtomicU64,
    dirs: AtomicU64,
    errors: AtomicU64,
    /// Subdirectories skipped or vanished inside this owner.
    skipped: AtomicU64,
    /// Exclusive files whose private size was not returned.
    private_missing: AtomicU64,
    /// Entries inside, its own directory included, owned by another user
    /// (safety ADR, amendment of 2026-10-31).
    foreign: AtomicU64,
}

/// A direct child of an owner that lists entries; deeper tasks inherit it,
/// across claims inside too, so its size includes them.
struct EntryAcc {
    id: usize,
    owner: usize,
    meta: EntryMeta,
    bytes: AtomicU64,
    errors: AtomicU64,
    skipped: AtomicU64,
    other_fs: AtomicU64,
}

struct RootAcc {
    path: PathBuf,
    dev: u64,
    bytes: AtomicU64,
    files: AtomicU64,
    dirs: AtomicU64,
}

#[derive(Default)]
struct Log {
    errors: Vec<WalkError>,
    errors_total: u64,
    skipped: Vec<Skipped>,
    skipped_total: u64,
    vanished: u64,
}

struct Shared<'a> {
    reader: &'a dyn DirReader,
    visitor: &'a dyn Visitor,
    progress: &'a Progress,
    opts: &'a WalkOptions,
    roots: Vec<RootAcc>,
    owners: Mutex<Vec<Arc<OwnerAcc>>>,
    entries: Mutex<Vec<Arc<EntryAcc>>>,
    inodes: InodeTable,
    log: Mutex<Log>,
    unowned: AtomicU64,
    pruned: AtomicU64,
    /// The scanning user, whose entries are not foreign.
    me: u32,
}

struct Task {
    dir: PathBuf,
    expect: (u64, u64),
    /// `(dev, ino)` of the parent directory.
    parent: (u64, u64),
    depth: u32,
    root: usize,
    owner: Option<Arc<OwnerAcc>>,
    /// The listed entry this directory lies in.
    entry: Option<Arc<EntryAcc>>,
    hooks: bool,
    /// The directory's owner as its parent's listing reported it; `None`
    /// for a root and where the reader does not tell.
    uid: Option<u32>,
}

/// Walks `roots` (canonicalized; roots nested in other roots are dropped)
/// without following symlinks or crossing filesystems.
pub fn walk(
    roots: &[PathBuf],
    visitor: &dyn Visitor,
    opts: &WalkOptions,
    progress: &Progress,
) -> Result<WalkOutput, WalkSetupError> {
    let reader: &dyn DirReader = match opts.reader {
        ReaderKind::Bulk => &SystemReader,
        ReaderKind::Std => &StdReader,
    };
    walk_with_reader(roots, visitor, opts, progress, reader)
}

/// [`walk`] through a given reader (`opts.reader` is ignored), so a test can
/// see every directory the walk opens.
pub fn walk_with_reader(
    roots: &[PathBuf],
    visitor: &dyn Visitor,
    opts: &WalkOptions,
    progress: &Progress,
    reader: &dyn DirReader,
) -> Result<WalkOutput, WalkSetupError> {
    crate::platform::require_supported().map_err(WalkSetupError::Unsupported)?;
    let started = Instant::now();
    let roots = normalize_roots(roots)?;
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(opts.threads.unwrap_or(0))
        .thread_name(|i| format!("ddc-walk-{i}"))
        .build()?;
    let sh = Shared {
        reader,
        visitor,
        progress,
        opts,
        roots: roots
            .iter()
            .map(|(path, _, id)| RootAcc {
                path: path.clone(),
                dev: id.0,
                bytes: AtomicU64::new(0),
                files: AtomicU64::new(0),
                dirs: AtomicU64::new(0),
            })
            .collect(),
        owners: Mutex::new(Vec::new()),
        entries: Mutex::new(Vec::new()),
        inodes: InodeTable::new(),
        log: Mutex::new(Log::default()),
        unowned: AtomicU64::new(0),
        pruned: AtomicU64::new(0),
        me: crate::platform::procs::uid(),
    };

    pool.install(|| {
        rayon::scope(|s| {
            for (i, (path, md, id)) in roots.iter().enumerate() {
                if md.is_dir() {
                    let task = Task {
                        dir: path.clone(),
                        expect: *id,
                        parent: path
                            .parent()
                            .and_then(|p| std::fs::symlink_metadata(p).ok())
                            .and_then(|m| meta::file_id(&m).ok())
                            .unwrap_or((0, 0)),
                        depth: 0,
                        root: i,
                        owner: None,
                        entry: None,
                        uid: None,
                        hooks: true,
                    };
                    let sh = &sh;
                    s.spawn(move |s| visit(s, sh, task));
                } else {
                    // A file given as a root is counted like `du` does.
                    let bytes = meta::alloc_bytes(md);
                    sh.roots[i].bytes.fetch_add(bytes, Relaxed);
                    sh.roots[i].files.fetch_add(1, Relaxed);
                    sh.unowned.fetch_add(bytes, Relaxed);
                    sh.progress.add(1, 0, bytes, 0);
                }
            }
        });
    });
    Ok(sh.finish(started))
}

/// `roots` as [`walk`] takes them: canonicalized, sorted, roots nested in
/// others dropped.
pub fn normalized_roots(roots: &[PathBuf]) -> Result<Vec<PathBuf>, WalkSetupError> {
    normalize_roots(roots).map(|roots| roots.into_iter().map(|(path, _, _)| path).collect())
}

/// Each root with its metadata and its `(dev, ino)`; a root whose identity
/// cannot be told is refused, never walked under a made-up one.
#[allow(clippy::type_complexity)]
fn normalize_roots(roots: &[PathBuf]) -> Result<Vec<(PathBuf, Metadata, (u64, u64))>, WalkSetupError> {
    let mut canon = Vec::with_capacity(roots.len());
    for root in roots {
        let path = root.canonicalize().map_err(|source| WalkSetupError::Root { path: root.clone(), source })?;
        canon.push(path);
    }
    canon.sort();
    canon.dedup();
    let mut kept: Vec<PathBuf> = Vec::with_capacity(canon.len());
    for path in canon {
        if !kept.iter().any(|k| path.starts_with(k)) {
            kept.push(path);
        }
    }
    kept.into_iter()
        .map(|path| {
            let found = std::fs::symlink_metadata(&path).and_then(|md| meta::file_id(&md).map(|id| (md, id)));
            match found {
                Ok((md, id)) => Ok((path, md, id)),
                Err(source) => Err(WalkSetupError::Root { path, source }),
            }
        })
        .collect()
}

fn visit<'s>(s: &rayon::Scope<'s>, sh: &'s Shared<'s>, task: Task) {
    if sh.progress.is_cancelled() {
        return;
    }
    // Before any rule hook, inside claims too: a pruned directory is never opened.
    if sh.visitor.prunes(&task.dir) {
        sh.pruned.fetch_add(1, Relaxed);
        return;
    }
    let Task { dir, expect, parent, depth, root, mut owner, entry, mut hooks, uid } = task;
    let foreign_dir = uid.is_some_and(|u| u != sh.me);
    let name = dir.file_name().unwrap_or(dir.as_os_str());
    let mut lists = false;

    if hooks {
        let ctx = DirCtx { path: &dir, name, depth, dev: expect.0, ino: expect.1, owner: owner_ctx(&owner) };
        match sh.visitor.before_read(&ctx) {
            Pre::Continue => {}
            Pre::Skip(reason) => {
                sh.skip(dir, reason);
                return;
            }
            Pre::Claim(tag) => {
                let wants = sh.claim_wants(&ctx, tag);
                lists = sh.visitor.lists_entries(&ctx, tag);
                let claim = sh.new_owner(tag, OwnerKind::Claim, &dir, expect, parent, wants);
                claim.foreign.fetch_add(u64::from(foreign_dir), Relaxed);
                owner = Some(claim);
                hooks = false;
            }
            Pre::Region(tag) => {
                lists = sh.visitor.lists_entries(&ctx, tag);
                let wants = Wants { private: false, times: false };
                owner = Some(sh.new_owner(tag, OwnerKind::Region, &dir, expect, parent, wants));
            }
        }
    }

    let want_private =
        sh.opts.private_size && (sh.opts.private_everywhere || owner.as_ref().is_some_and(|o| o.want_private));
    let times = if lists {
        Times::ModifiedAndAdded
    } else if owner.as_ref().is_some_and(|o| o.want_times) {
        Times::Modified
    } else {
        Times::None
    };
    let mut listing = match sh.reader.read(&dir, Some(expect), want_private, times) {
        Ok(listing) => listing,
        Err(err) => {
            sh.read_failed(dir, err, root, owner.as_deref(), entry.as_deref());
            return;
        }
    };

    if hooks {
        let ctx = DirCtx { path: &dir, name, depth, dev: expect.0, ino: expect.1, owner: owner_ctx(&owner) };
        if let Post::Claim(tag) = sh.visitor.after_read(&ctx, &listing) {
            let wants = sh.claim_wants(&ctx, tag);
            let claim = sh.new_owner(tag, OwnerKind::Claim, &dir, expect, parent, wants);
            let for_private = !want_private && claim.want_private;
            let for_times = times == Times::None && claim.want_times;
            if for_private || for_times {
                // The listing was read before we knew this is a finding: re-read
                // this one directory with what the claim wants.
                let again_times = if claim.want_times { Times::Modified } else { Times::None };
                match sh.reader.read(&dir, Some(expect), want_private || claim.want_private, again_times) {
                    Ok(again) => listing = again,
                    Err(err) if for_private => {
                        // Keep the first listing; its files then count as "private unknown".
                        claim.errors.fetch_add(1, Relaxed);
                        sh.error(dir.clone(), err);
                    }
                    // For the times alone: the first listing is whole, and only its
                    // files' times are missing. The date stays a lower bound, and the
                    // size is not made incomplete for it.
                    Err(_) => {}
                }
            }
            claim.foreign.fetch_add(u64::from(foreign_dir), Relaxed);
            owner = Some(claim);
            hooks = false;
            // The region's own directory became another finding: no entries.
            lists = false;
        }
    }

    // One accumulator per direct child, in listing order.
    let children: Option<Vec<Arc<EntryAcc>>> = match (&owner, lists) {
        (Some(o), true) => Some(sh.new_entries(o.id, &listing.entries)),
        _ => None,
    };
    let entry_of = |i: usize| children.as_ref().map_or(entry.as_ref(), |c| Some(&c[i]));

    let ctx_owner = owner_ctx(&owner);
    let owner_id = owner.as_ref().map(|o| o.id);
    let root_dev = sh.roots[root].dev;
    let (mut files, mut bytes, mut private, mut errors) = (0u64, 0u64, 0u64, 0u64);
    let (mut linked_bytes, mut skipped, mut private_missing, mut foreign) = (0u64, 0u64, 0u64, 0u64);
    // The directory's own time, then each entry's, which the listing has only
    // when it asked for times.
    let mut newest = listing.modified;

    let here = (listing.dev, listing.ino);
    for (i, meta) in listing.entries.iter().enumerate() {
        let within = entry_of(i);
        // The child's own counts go to its accumulator; deeper ones to the
        // entry this directory lies in, after the loop.
        let own = children.as_ref().map(|c| &c[i]);
        if let Some(errno) = meta.error {
            if errno == crate::platform::errno::ENOENT {
                skipped += 1;
                sh.vanished();
            } else {
                errors += 1;
                sh.error(dir.join(&meta.name), ReadError::from_errno(errno));
            }
            continue;
        }
        // Before the kind decides where the bytes go: a hard-linked file (most
        // of a pnpm or uv tree), a skipped or unreadable directory still tells
        // when it last changed.
        newest = newest.max(meta.modified);
        foreign += u64::from(meta.uid.is_some_and(|u| u != sh.me));
        match meta.kind {
            EntryKind::Dir => {
                let child = dir.join(&meta.name);
                if meta.flags & SF_DATALESS != 0 {
                    skipped += 1;
                    sh.skip(child, SkipReason::Dataless);
                } else if meta.dev != root_dev {
                    skipped += 1;
                    if let Some(e) = within {
                        e.other_fs.fetch_add(1, Relaxed);
                    }
                    sh.skip(child, SkipReason::OtherFilesystem);
                } else {
                    let task = Task {
                        dir: child,
                        expect: (meta.dev, meta.ino),
                        parent: here,
                        depth: depth + 1,
                        root,
                        owner: owner.clone(),
                        entry: within.cloned(),
                        hooks,
                        uid: meta.uid,
                    };
                    s.spawn(move |s| visit(s, sh, task));
                }
            }
            _ => {
                files += 1;
                sh.visitor.on_file(&dir, here, meta, ctx_owner);
                if meta.nlink > 1 && meta.kind == EntryKind::File {
                    let holder = Holder { root, owner: owner_id, entry: within.map(|e| e.id) };
                    sh.inodes.record(meta.dev, meta.ino, meta.alloc, meta.private, meta.nlink, holder);
                    linked_bytes += meta.alloc;
                } else {
                    bytes += meta.alloc;
                    if let Some(e) = own {
                        e.bytes.fetch_add(meta.alloc, Relaxed);
                    }
                    match meta.private {
                        Some(p) => private += p,
                        // Not returned: the owner's lower bound becomes unknown.
                        None => private_missing += 1,
                    }
                }
            }
        }
    }

    let acc = &sh.roots[root];
    acc.bytes.fetch_add(bytes, Relaxed);
    acc.files.fetch_add(files, Relaxed);
    acc.dirs.fetch_add(1, Relaxed);
    match &owner {
        Some(o) => {
            o.bytes.fetch_add(bytes, Relaxed);
            o.private.fetch_add(private, Relaxed);
            o.files.fetch_add(files, Relaxed);
            o.dirs.fetch_add(1, Relaxed);
            o.errors.fetch_add(errors, Relaxed);
            o.skipped.fetch_add(skipped, Relaxed);
            o.private_missing.fetch_add(private_missing, Relaxed);
            o.foreign.fetch_add(foreign, Relaxed);
            if o.want_times
                && let Some(t) = newest
            {
                o.modified.fetch_max(t, Relaxed);
            }
        }
        None => {
            sh.unowned.fetch_add(bytes, Relaxed);
        }
    }
    if let Some(e) = &entry {
        e.bytes.fetch_add(bytes, Relaxed);
        e.errors.fetch_add(errors, Relaxed);
        e.skipped.fetch_add(skipped, Relaxed);
    }
    sh.progress.add(files, 1, bytes + linked_bytes, errors);
}

fn owner_ctx(owner: &Option<Arc<OwnerAcc>>) -> Option<(OwnerKind, Tag)> {
    owner.as_ref().map(|o| (o.kind, o.tag))
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Shared<'_> {
    /// What the visitor asks for a claim of `tag` at `ctx`, the times only
    /// while [`WalkOptions::claim_times`] is on.
    fn claim_wants(&self, ctx: &DirCtx<'_>, tag: Tag) -> Wants {
        Wants {
            private: self.opts.private_size && self.visitor.wants_private(ctx, tag),
            times: self.opts.claim_times && self.visitor.wants_times(ctx, tag),
        }
    }

    fn new_owner(
        &self,
        tag: Tag,
        kind: OwnerKind,
        path: &Path,
        (dev, ino): (u64, u64),
        parent: (u64, u64),
        wants: Wants,
    ) -> Arc<OwnerAcc> {
        let mut owners = lock(&self.owners);
        let acc = Arc::new(OwnerAcc {
            id: owners.len(),
            tag,
            kind,
            path: path.to_path_buf(),
            dev,
            ino,
            parent,
            want_private: wants.private,
            want_times: wants.times,
            modified: AtomicI64::new(NO_TIME),
            bytes: AtomicU64::new(0),
            shared: AtomicU64::new(0),
            private: AtomicU64::new(0),
            files: AtomicU64::new(0),
            dirs: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            skipped: AtomicU64::new(0),
            private_missing: AtomicU64::new(0),
            foreign: AtomicU64::new(0),
        });
        owners.push(Arc::clone(&acc));
        acc
    }

    fn new_entries(&self, owner: usize, listing: &[EntryMeta]) -> Vec<Arc<EntryAcc>> {
        let mut entries = lock(&self.entries);
        listing
            .iter()
            .map(|meta| {
                let acc = Arc::new(EntryAcc {
                    id: entries.len(),
                    owner,
                    meta: meta.clone(),
                    bytes: AtomicU64::new(0),
                    errors: AtomicU64::new(0),
                    skipped: AtomicU64::new(0),
                    other_fs: AtomicU64::new(0),
                });
                entries.push(Arc::clone(&acc));
                acc
            })
            .collect()
    }

    fn error(&self, path: PathBuf, error: ReadError) {
        let mut log = lock(&self.log);
        log.errors_total += 1;
        log.errors.push(WalkError { path, error });
        if log.errors.len() >= 2 * self.opts.max_recorded.max(1) {
            // Keep the smallest paths so the recorded subset does not depend on timing.
            log.errors.sort_by(|a, b| a.path.cmp(&b.path));
            log.errors.truncate(self.opts.max_recorded);
        }
    }

    fn skip(&self, path: PathBuf, reason: SkipReason) {
        let mut log = lock(&self.log);
        log.skipped_total += 1;
        log.skipped.push(Skipped { path, reason });
        if log.skipped.len() >= 2 * self.opts.max_recorded.max(1) {
            log.skipped.sort_by(|a, b| a.path.cmp(&b.path));
            log.skipped.truncate(self.opts.max_recorded);
        }
    }

    fn vanished(&self) {
        lock(&self.log).vanished += 1;
    }

    fn read_failed(
        &self,
        dir: PathBuf,
        err: ReadError,
        root: usize,
        owner: Option<&OwnerAcc>,
        entry: Option<&EntryAcc>,
    ) {
        match err.kind {
            ReadErrorKind::NotFound
            | ReadErrorKind::Dataless
            | ReadErrorKind::Changed
            | ReadErrorKind::OtherFilesystem => {
                if let Some(o) = owner {
                    o.skipped.fetch_add(1, Relaxed);
                }
                if let Some(e) = entry {
                    let count = if err.kind == ReadErrorKind::OtherFilesystem { &e.other_fs } else { &e.skipped };
                    count.fetch_add(1, Relaxed);
                }
                match err.kind {
                    ReadErrorKind::NotFound => self.vanished(),
                    ReadErrorKind::Dataless => self.skip(dir, SkipReason::Dataless),
                    ReadErrorKind::Changed => self.skip(dir, SkipReason::Changed),
                    _ => self.skip(dir, SkipReason::OtherFilesystem),
                }
            }
            ReadErrorKind::PermissionDenied | ReadErrorKind::Other => {
                // The directory exists and is counted; its contents are unknown.
                self.roots[root].dirs.fetch_add(1, Relaxed);
                if let Some(o) = owner {
                    o.dirs.fetch_add(1, Relaxed);
                    o.errors.fetch_add(1, Relaxed);
                }
                if let Some(e) = entry {
                    e.errors.fetch_add(1, Relaxed);
                }
                self.progress.add(0, 1, 0, 1);
                self.error(dir, err);
            }
        }
    }

    fn finish(self, started: Instant) -> WalkOutput {
        let owners = self.owners.into_inner().unwrap_or_else(|p| p.into_inner());
        let entries = self.entries.into_inner().unwrap_or_else(|p| p.into_inner());
        let mut unowned = self.unowned.into_inner();

        for inode in self.inodes.resolve() {
            // Each inode counts once, under the first root that saw it.
            if let Some(first) = inode.holders.first() {
                self.roots[first.root].bytes.fetch_add(inode.alloc, Relaxed);
            }
            // An entry, independent of owners: every link inside that one entry.
            let mut within: Vec<Option<usize>> = inode.holders.iter().map(|h| h.entry).collect();
            within.sort();
            within.dedup();
            if let [Some(e)] = within.as_slice()
                && inode.complete
            {
                entries[*e].bytes.fetch_add(inode.alloc, Relaxed);
            }
            let mut holders: Vec<Option<usize>> = inode.holders.iter().map(|h| h.owner).collect();
            holders.sort();
            holders.dedup();
            match holders.as_slice() {
                [Some(o)] if inode.complete => {
                    owners[*o].bytes.fetch_add(inode.alloc, Relaxed);
                    match inode.private {
                        Some(p) => owners[*o].private.fetch_add(p, Relaxed),
                        None => owners[*o].private_missing.fetch_add(1, Relaxed),
                    };
                }
                [None] => unowned += inode.alloc,
                shared => {
                    for o in shared.iter().flatten() {
                        owners[*o].shared.fetch_add(inode.alloc, Relaxed);
                    }
                }
            }
        }

        let measure_private = |o: &OwnerAcc| {
            self.opts.private_size
                && (o.want_private || self.opts.private_everywhere)
                && o.private_missing.load(Relaxed) == 0
        };
        let mut owner_results: Vec<OwnerResult> = owners
            .iter()
            .map(|o| OwnerResult {
                tag: o.tag,
                kind: o.kind,
                path: o.path.clone(),
                dev: o.dev,
                ino: o.ino,
                parent_dev: o.parent.0,
                parent_ino: o.parent.1,
                bytes: o.bytes.load(Relaxed),
                shared_bytes: o.shared.load(Relaxed),
                private_bytes: measure_private(o).then(|| o.private.load(Relaxed)),
                files: o.files.load(Relaxed),
                dirs: o.dirs.load(Relaxed),
                errors: o.errors.load(Relaxed),
                skipped: o.skipped.load(Relaxed),
                foreign: o.foreign.load(Relaxed),
                modified: Some(o.modified.load(Relaxed)).filter(|&t| t != NO_TIME),
            })
            .collect();
        owner_results.sort_by(|a, b| a.path.cmp(&b.path));

        let mut listed: Vec<OwnerEntries> = Vec::new();
        for e in &entries {
            let owner = &owners[e.owner].path;
            let result = EntryResult {
                name: e.meta.name.clone(),
                kind: e.meta.kind,
                dev: e.meta.dev,
                ino: e.meta.ino,
                flags: e.meta.flags,
                error: e.meta.error,
                modified: e.meta.modified,
                added: e.meta.added,
                bytes: e.bytes.load(Relaxed),
                errors: e.errors.load(Relaxed),
                skipped: e.skipped.load(Relaxed),
                other_fs: e.other_fs.load(Relaxed),
            };
            match listed.iter_mut().find(|l| &l.owner == owner) {
                Some(l) => l.entries.push(result),
                None => listed.push(OwnerEntries { owner: owner.clone(), entries: vec![result] }),
            }
        }
        for l in &mut listed {
            l.entries.sort_by(|a, b| a.name.cmp(&b.name));
        }
        listed.sort_by(|a, b| a.owner.cmp(&b.owner));

        let roots: Vec<RootResult> = self
            .roots
            .iter()
            .map(|r| RootResult {
                path: r.path.clone(),
                bytes: r.bytes.load(Relaxed),
                files: r.files.load(Relaxed),
                dirs: r.dirs.load(Relaxed),
            })
            .collect();

        let mut log = self.log.into_inner().unwrap_or_else(|p| p.into_inner());
        log.errors.sort_by(|a, b| a.path.cmp(&b.path));
        log.errors.truncate(self.opts.max_recorded);
        log.skipped.sort_by(|a, b| a.path.cmp(&b.path));
        log.skipped.truncate(self.opts.max_recorded);

        WalkOutput {
            total_bytes: roots.iter().map(|r| r.bytes).sum(),
            roots,
            owners: owner_results,
            entries: listed,
            unowned_bytes: unowned,
            errors: log.errors,
            errors_total: log.errors_total,
            skipped: log.skipped,
            skipped_total: log.skipped_total,
            vanished: log.vanished,
            pruned: self.pruned.into_inner(),
            cancelled: self.progress.is_cancelled(),
            elapsed: started.elapsed(),
        }
    }
}
