//! Rule matching during the walk, in the precedence order of ADR §2:
//! exclusions and keep markers → exact known paths (before reading) →
//! ancestors of known paths only descend → specialized rules → CACHEDIR.TAG.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::entries::has_entries;
use super::matcher::{self, ListingView};
use super::{Catalog, Clones, NeverOpen};
use crate::class::RiskClass;
use crate::model::{ExclusionOrigin, MarkerFolder};
use crate::scan::{DirCtx, DirListing, EntryKind, EntryMeta, OwnerKind, Post, Pre, SkipReason, Tag, Visitor};

/// A file with this name protects its directory and everything below.
pub const KEEP_MARKER: &str = ".ddc-keep";

/// Files at least this large outside every finding are listed for manual review.
pub const LARGE_FILE_BYTES: u64 = 500 << 20;
/// How many large files are kept.
pub const LARGE_FILE_LIMIT: usize = 20;

/// Toolchain versions pinned by project files found during the walk.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Pins {
    pub node: Vec<String>,
    pub python: Vec<String>,
    pub rust: Vec<String>,
}

/// What the walk knew of a folder it claimed as `user.kept`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeptClaim {
    /// The rule the claim displaced, when the walk knows it.
    pub rule: Option<Tag>,
    /// A keep marker at its top kept it.
    pub marker: bool,
}

pub struct RuleVisitor<'c> {
    catalog: &'c Catalog,
    excludes: Vec<PathBuf>,
    /// Where the excludes that are not user skips came from.
    excludes_origin: ExclusionOrigin,
    /// Directories inside claimed findings that contain a keep marker, with
    /// the identity the walk held when it listed each.
    markers: Mutex<Vec<(PathBuf, (u64, u64))>>,
    kept: Mutex<HashMap<PathBuf, KeptClaim>>,
    pins: Mutex<Pins>,
    large: Mutex<Vec<(u64, PathBuf)>>,
    large_threshold: u64,
    /// Never opened (TCC-protected areas in background scans).
    skips: Vec<PathBuf>,
    /// The user's skips: never opened, and recorded when the walk stops at one.
    user_skips: Vec<PathBuf>,
    /// The user skips only a flag gave.
    flag_skips: Vec<PathBuf>,
    not_scanned: Mutex<Vec<PathBuf>>,
}

impl<'c> RuleVisitor<'c> {
    /// `excludes` must be canonical paths.
    pub fn new(catalog: &'c Catalog, excludes: Vec<PathBuf>) -> Self {
        Self {
            catalog,
            excludes,
            excludes_origin: ExclusionOrigin::Settings,
            markers: Mutex::new(Vec::new()),
            kept: Mutex::new(HashMap::new()),
            pins: Mutex::new(Pins::default()),
            large: Mutex::new(Vec::new()),
            large_threshold: LARGE_FILE_BYTES,
            skips: Vec::new(),
            user_skips: Vec::new(),
            flag_skips: Vec::new(),
            not_scanned: Mutex::new(Vec::new()),
        }
    }

    /// Where the excludes came from, and the user skips only a flag gave
    /// (canonical): what a kept finding's `protection` names. By default
    /// everything is the settings'.
    pub fn with_origins(mut self, excludes: ExclusionOrigin, flag_skips: Vec<PathBuf>) -> Self {
        self.excludes_origin = excludes;
        self.flag_skips = flag_skips;
        self
    }

    /// Directories that are skipped without being opened.
    pub fn with_skips(mut self, skips: Vec<PathBuf>) -> Self {
        self.skips = skips;
        self
    }

    /// The user's skips, canonical: a directory at or under one is not opened,
    /// and the skip is recorded for [`Self::not_scanned`].
    pub fn with_user_skips(mut self, skips: Vec<PathBuf>) -> Self {
        self.user_skips = skips;
        self
    }

    /// Lists files of at least `bytes` as large (default [`LARGE_FILE_BYTES`]).
    pub fn with_large_file_threshold(mut self, bytes: u64) -> Self {
        self.large_threshold = bytes;
        self
    }

    pub fn catalog(&self) -> &'c Catalog {
        self.catalog
    }

    pub fn excludes(&self) -> &[PathBuf] {
        &self.excludes
    }

    pub fn excludes_origin(&self) -> ExclusionOrigin {
        self.excludes_origin
    }

    pub fn user_skips(&self) -> &[PathBuf] {
        &self.user_skips
    }

    pub fn flag_skips(&self) -> &[PathBuf] {
        &self.flag_skips
    }

    pub fn keep_markers(&self) -> Vec<PathBuf> {
        let mut m: Vec<PathBuf> =
            self.markers.lock().unwrap_or_else(|p| p.into_inner()).iter().map(|(dir, _)| dir.clone()).collect();
        m.sort();
        m
    }

    /// [`Self::keep_markers`] with each folder's identity, sorted by folder.
    pub fn marker_folders(&self) -> Vec<MarkerFolder> {
        let mut m: Vec<MarkerFolder> = self
            .markers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .map(|(folder, (dev, ino))| MarkerFolder { folder: folder.clone(), dev: *dev, ino: *ino })
            .collect();
        m.sort_by(|a, b| a.folder.cmp(&b.folder));
        m
    }

    /// The folders the walk claimed as `user.kept`, with what it knew of each.
    pub fn kept_claims(&self) -> HashMap<PathBuf, KeptClaim> {
        self.kept.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// The user's skips the walk stopped at, sorted, each once.
    pub fn not_scanned(&self) -> Vec<PathBuf> {
        let mut skipped = self.not_scanned.lock().unwrap_or_else(|p| p.into_inner()).clone();
        skipped.sort();
        skipped.dedup();
        skipped
    }

    /// Largest files outside findings (or in the remainder of a region),
    /// biggest first, at most `LARGE_FILE_LIMIT`.
    pub fn large_files(&self) -> Vec<(u64, PathBuf)> {
        let mut large = self.large.lock().unwrap_or_else(|p| p.into_inner()).clone();
        sort_large(&mut large);
        large
    }

    pub fn pins(&self) -> Pins {
        self.pins.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn claim_unless_kept(&self, dir: &Path, tag: Tag) -> Tag {
        if fs::symlink_metadata(dir.join(KEEP_MARKER)).is_err() {
            return tag;
        }
        self.kept(dir, Some(tag), true)
    }

    /// Records a `user.kept` claim of `dir` and returns its tag.
    fn kept(&self, dir: &Path, rule: Option<Tag>, marker: bool) -> Tag {
        self.kept.lock().unwrap_or_else(|p| p.into_inner()).insert(dir.to_path_buf(), KeptClaim { rule, marker });
        self.catalog.kept
    }

    /// The specialized or generic rule that claims `dir` from its listing.
    fn matched(&self, dir: &DirCtx<'_>, view: &ListingView<'_>) -> Option<Tag> {
        let c = self.catalog;
        let has = |n: &str| matcher::DirView::has_child(view, n);
        if c.ancestors.contains(dir.path) || !c.claimable(dir.path, dir.depth, has) {
            return None;
        }
        c.specialized.iter().chain((c.cachedir != Tag::MAX).then_some(&c.cachedir)).copied().find(|&tag| {
            let m = &c.rule(tag).matcher;
            Catalog::content_matcher_allowed(m, has) && matcher::matches(m, view, &c.home)
        })
    }
}

impl Visitor for RuleVisitor<'_> {
    fn before_read(&self, dir: &DirCtx<'_>) -> Pre {
        let c = self.catalog;
        match c.never_open(dir.path, &self.user_skips) {
            Some(NeverOpen::UserSkip(skip)) => {
                self.not_scanned.lock().unwrap_or_else(|p| p.into_inner()).push(skip.clone());
                return Pre::Skip(SkipReason::Excluded);
            }
            Some(NeverOpen::NoTraverse) => return Pre::Skip(SkipReason::Excluded),
            None => {}
        }
        if self.skips.iter().any(|s| s == dir.path) {
            return Pre::Skip(SkipReason::Excluded);
        }
        // An exclude equal to this directory or above it (above a scan root).
        if self.excludes.iter().any(|e| dir.path.starts_with(e)) {
            // Nothing below is listed: only a table rule is known here, never
            // one that matches a listing (`node_modules`).
            let rule = c
                .known
                .get(dir.path)
                .or_else(|| c.regions.get(dir.path))
                .or_else(|| dir.path.parent().and_then(|p| c.children_of.get(p)))
                .copied();
            // A marker at its top protects it too (a file, a symlink or a
            // folder of that name), and `ddc keep remove` needs its identity.
            let marker = fs::symlink_metadata(dir.path.join(KEEP_MARKER)).is_ok();
            return Pre::Claim(self.kept(dir.path, rule, marker));
        }
        if let Some(&tag) = c.known.get(dir.path) {
            return Pre::Claim(self.claim_unless_kept(dir.path, tag));
        }
        if let Some(&tag) = c.regions.get(dir.path) {
            let tag = self.claim_unless_kept(dir.path, tag);
            return if tag == c.kept { Pre::Claim(tag) } else { Pre::Region(tag) };
        }
        if let Some(&tag) = dir.path.parent().and_then(|p| c.children_of.get(p)) {
            return Pre::Claim(self.claim_unless_kept(dir.path, tag));
        }
        Pre::Continue
    }

    fn after_read(&self, dir: &DirCtx<'_>, listing: &DirListing) -> Post {
        let view = ListingView { path: dir.path, listing };
        let matched = self.matched(dir, &view);
        // A marker always keeps the folder; the matchers ran first so that
        // the report names the rule it displaced.
        if matcher::DirView::has_child(&view, KEEP_MARKER) {
            return Post::Claim(self.kept(dir.path, matched, true));
        }
        matched.map_or(Post::Descend, Post::Claim)
    }

    fn wants_private(&self, dir: &DirCtx<'_>, tag: Tag) -> bool {
        let rule = self.catalog.rule(tag);
        if rule.class == RiskClass::UserData || !rule.removal.removes_files() {
            return false;
        }
        match rule.clones {
            Clones::Never => false,
            Clones::Always => true,
            Clones::UvVenv => fs::read_to_string(dir.path.join("pyvenv.cfg")).is_ok_and(|cfg| {
                cfg.lines().any(|l| l.trim_start().strip_prefix("uv").is_some_and(|r| r.trim_start().starts_with('=')))
            }),
            Clones::PnpmModules => {
                [".pnpm", ".modules.yaml"].iter().any(|n| fs::symlink_metadata(dir.path.join(n)).is_ok())
                    || dir.path.parent().is_some_and(|p| {
                        ["bun.lock", "bun.lockb"].iter().any(|n| fs::symlink_metadata(p.join(n)).is_ok())
                    })
            }
        }
    }

    /// A finding shows the newest change inside it (story "Review window:
    /// sort a group's rows by name, size or last change"). By the rule's
    /// class, as [`Self::wants_private`]: a user-data folder's entries carry
    /// their own times, and a protected or unlisted claim (an app bundle)
    /// shows no date.
    fn wants_times(&self, _dir: &DirCtx<'_>, tag: Tag) -> bool {
        let rule = self.catalog.rule(tag);
        tag != self.catalog.kept && rule.listed && rule.class != RiskClass::UserData
    }

    /// The user's own folders list their entries (story "User data: pick
    /// your own files for the Trash"), and so do the coding agents' session
    /// folders, whose sessions are entries (story "Harness data rules"):
    /// any rule with an entry policy.
    fn lists_entries(&self, _dir: &DirCtx<'_>, tag: Tag) -> bool {
        has_entries(self.catalog.rule(tag).id)
    }

    fn on_file(&self, dir: &Path, dir_id: (u64, u64), entry: &EntryMeta, owner: Option<(OwnerKind, Tag)>) {
        let in_claim = matches!(owner, Some((OwnerKind::Claim, _)));
        // A keep marker of any kind (file, symlink) inside a finding protects it.
        if in_claim && entry.name == KEEP_MARKER {
            self.markers.lock().unwrap_or_else(|p| p.into_inner()).push((dir.to_path_buf(), dir_id));
            return;
        }
        if entry.kind != EntryKind::File {
            return;
        }
        // Large files outside findings — including inside unlisted claims (app bundles).
        let in_finding = in_claim && matches!(owner, Some((_, tag)) if self.catalog.rule(tag).listed);
        if !in_finding && entry.alloc >= self.large_threshold {
            let mut large = self.large.lock().unwrap_or_else(|p| p.into_inner());
            large.push((entry.alloc, dir.join(&entry.name)));
            if large.len() > 4 * LARGE_FILE_LIMIT {
                sort_large(&mut large);
            }
        }
        let Some(name) = entry.name.to_str() else {
            return;
        };
        // Pins inside claimed artifacts (packages shipping .nvmrc) do not count;
        // pins inside projects the user protected do.
        let in_artifact = in_claim && !matches!(owner, Some((_, tag)) if tag == self.catalog.kept);
        if name == KEEP_MARKER
            || in_artifact
            || !matches!(
                name,
                ".nvmrc" | ".node-version" | ".python-version" | "rust-toolchain" | "rust-toolchain.toml"
            )
        {
            return;
        }
        let Ok(text) = fs::read_to_string(dir.join(name)) else {
            return;
        };
        let mut pins = self.pins.lock().unwrap_or_else(|p| p.into_inner());
        match name {
            ".nvmrc" | ".node-version" => pins.node.extend(
                text.lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty() && !l.starts_with('#'))
                    .map(|l| l.split_whitespace().next().unwrap_or_default().to_owned()),
            ),
            ".python-version" => pins.python.extend(text.split_whitespace().map(str::to_owned)),
            _ => pins.rust.extend(rust_channel(&text)),
        }
    }
}

/// Channel from `rust-toolchain.toml` (`channel = "1.99.1"`) or a legacy `rust-toolchain` file.
fn rust_channel(text: &str) -> Option<String> {
    let toml = text.lines().find_map(|l| {
        let l = l.trim();
        l.strip_prefix("channel")
            .map(|rest| rest.trim_start().trim_start_matches('=').trim().trim_matches(['"', '\'']).to_owned())
    });
    toml.or_else(|| {
        let line = text.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('['))?;
        (!line.contains('=')).then(|| line.to_owned())
    })
    .filter(|c| !c.is_empty())
}

fn sort_large(large: &mut Vec<(u64, PathBuf)>) {
    large.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    large.truncate(LARGE_FILE_LIMIT);
}
