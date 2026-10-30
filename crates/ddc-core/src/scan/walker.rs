//! The parallel walk: one rayon task per directory. A directory's descriptor
//! is closed before its children are spawned, so open files stay bounded by
//! the worker count (launchd agents start with a soft limit of 256).

use std::fs::Metadata;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::inodes::{Holder, InodeTable};
use super::reader::EntryMeta;
use super::reader::{DirReader, EntryKind, ReadError, ReadErrorKind, SF_DATALESS, StdReader};
use super::{
    DirCtx, EntryResult, OwnerEntries, OwnerKind, OwnerResult, Post, Pre, Progress, ReaderKind, RootResult, SkipReason,
    Skipped, Tag, Visitor, WalkError, WalkOptions, WalkOutput, WalkSetupError,
};
use crate::sys::bulk::BulkReader;

struct OwnerAcc {
    id: usize,
    tag: Tag,
    kind: OwnerKind,
    path: PathBuf,
    dev: u64,
    ino: u64,
    parent: (u64, u64),
    want_private: bool,
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
        ReaderKind::Bulk => &BulkReader,
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
            .map(|(path, md)| RootAcc {
                path: path.clone(),
                dev: md.dev(),
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
    };

    pool.install(|| {
        rayon::scope(|s| {
            for (i, (path, md)) in roots.iter().enumerate() {
                if md.is_dir() {
                    let task = Task {
                        dir: path.clone(),
                        expect: (md.dev(), md.ino()),
                        parent: path
                            .parent()
                            .and_then(|p| std::fs::symlink_metadata(p).ok())
                            .map_or((0, 0), |m| (m.dev(), m.ino())),
                        depth: 0,
                        root: i,
                        owner: None,
                        entry: None,
                        hooks: true,
                    };
                    let sh = &sh;
                    s.spawn(move |s| visit(s, sh, task));
                } else {
                    // A file given as a root is counted like `du` does.
                    let bytes = md.blocks() * 512;
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
    normalize_roots(roots).map(|roots| roots.into_iter().map(|(path, _)| path).collect())
}

fn normalize_roots(roots: &[PathBuf]) -> Result<Vec<(PathBuf, Metadata)>, WalkSetupError> {
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
        .map(|path| match std::fs::symlink_metadata(&path) {
            Ok(md) => Ok((path, md)),
            Err(source) => Err(WalkSetupError::Root { path, source }),
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
    let Task { dir, expect, parent, depth, root, mut owner, entry, mut hooks } = task;
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
                let private = sh.visitor.wants_private(&ctx, tag);
                lists = sh.visitor.lists_entries(&ctx, tag);
                owner = Some(sh.new_owner(tag, OwnerKind::Claim, &dir, expect, parent, private));
                hooks = false;
            }
            Pre::Region(tag) => {
                lists = sh.visitor.lists_entries(&ctx, tag);
                owner = Some(sh.new_owner(tag, OwnerKind::Region, &dir, expect, parent, false));
            }
        }
    }

    let want_private = sh.opts.private_everywhere || owner.as_ref().is_some_and(|o| o.want_private);
    let read = if lists {
        sh.reader.read_with_times(&dir, Some(expect), want_private)
    } else {
        sh.reader.read(&dir, Some(expect), want_private)
    };
    let mut listing = match read {
        Ok(listing) => listing,
        Err(err) => {
            sh.read_failed(dir, err, root, owner.as_deref(), entry.as_deref());
            return;
        }
    };

    if hooks {
        let ctx = DirCtx { path: &dir, name, depth, dev: expect.0, ino: expect.1, owner: owner_ctx(&owner) };
        if let Post::Claim(tag) = sh.visitor.after_read(&ctx, &listing) {
            let private = sh.visitor.wants_private(&ctx, tag);
            let claim = sh.new_owner(tag, OwnerKind::Claim, &dir, expect, parent, private);
            if !want_private && claim.want_private {
                // The listing was read before we knew this is a finding: re-read
                // this one directory with private sizes.
                match sh.reader.read(&dir, Some(expect), true) {
                    Ok(again) => listing = again,
                    Err(err) => {
                        // Keep the first listing; its files then count as "private unknown".
                        claim.errors.fetch_add(1, Relaxed);
                        sh.error(dir.clone(), err);
                    }
                }
            }
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
    let (mut linked_bytes, mut skipped, mut private_missing) = (0u64, 0u64, 0u64);

    let here = (listing.dev, listing.ino);
    for (i, meta) in listing.entries.iter().enumerate() {
        let within = entry_of(i);
        // The child's own counts go to its accumulator; deeper ones to the
        // entry this directory lies in, after the loop.
        let own = children.as_ref().map(|c| &c[i]);
        if let Some(errno) = meta.error {
            if errno == libc::ENOENT {
                skipped += 1;
                sh.vanished();
            } else {
                errors += 1;
                sh.error(dir.join(&meta.name), ReadError::from_errno(errno));
            }
            continue;
        }
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
                    };
                    s.spawn(move |s| visit(s, sh, task));
                }
            }
            _ => {
                files += 1;
                sh.visitor.on_file(&dir, meta, ctx_owner);
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
    fn new_owner(
        &self,
        tag: Tag,
        kind: OwnerKind,
        path: &Path,
        (dev, ino): (u64, u64),
        parent: (u64, u64),
        private: bool,
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
            want_private: private,
            bytes: AtomicU64::new(0),
            shared: AtomicU64::new(0),
            private: AtomicU64::new(0),
            files: AtomicU64::new(0),
            dirs: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            skipped: AtomicU64::new(0),
            private_missing: AtomicU64::new(0),
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

        let measure_private =
            |o: &OwnerAcc| (o.want_private || self.opts.private_everywhere) && o.private_missing.load(Relaxed) == 0;
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
