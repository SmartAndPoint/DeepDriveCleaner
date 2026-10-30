//! Shared rules: the user's own data, reported only, and what the user kept.

use super::super::rule;
use super::PerOs;
use crate::class::Category::*;
use crate::class::Removal;
use crate::class::RiskClass::*;
use crate::platform::Platform;
use crate::rules::{KEPT, Matcher, Rule};

pub(crate) const USER_KEPT: Rule = Rule {
    hint: "protected by --exclude or a .ddc-keep marker",
    ..rule(KEPT, Personal, UserData, Removal::None, Matcher::Synthetic)
};

pub(crate) const USER_PICTURES: Rule =
    rule("user.pictures", Personal, UserData, Removal::None, Matcher::KnownPaths(&["Pictures"]));

/// The videos folder: `~/Movies` on macOS; `~/Videos` on Linux (the XDG
/// user dirs' `XDG_VIDEOS_DIR` default) and on Windows (the Videos known
/// folder).
pub(crate) const fn user_movies(p: Platform) -> Rule {
    let paths = PerOs { macos: &["Movies"], linux: &["Videos"], windows: &["Videos"] };
    rule("user.movies", Personal, UserData, Removal::None, Matcher::KnownPaths(paths.of(p)))
}

pub(crate) const USER_MUSIC: Rule =
    rule("user.music", Personal, UserData, Removal::None, Matcher::KnownPaths(&["Music"]));

pub(crate) const USER_DOWNLOADS: Rule =
    rule("user.downloads", Personal, UserData, Removal::None, Matcher::KnownRegion(&["Downloads"]));
