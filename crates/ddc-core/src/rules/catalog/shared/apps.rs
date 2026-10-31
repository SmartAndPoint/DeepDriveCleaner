//! Shared rules: the browsers' and apps' caches, and the Trash.

use super::super::rule;
use super::PerOs;
use crate::class::Category::*;
use crate::class::Removal;
use crate::class::RiskClass::*;
use crate::platform::Platform;
use crate::rules::{Matcher, Rule};

/// Chrome's disk cache: `~/Library/Caches/Google/Chrome` on macOS,
/// `~/.cache/google-chrome` on Linux, the default profile's `Cache` on
/// Windows (chromium.googlesource.com, "User Data Directory", the cache
/// directory).
pub(crate) const fn app_chrome_cache(p: Platform) -> Rule {
    let paths = PerOs {
        macos: &["Library/Caches/Google/Chrome"],
        linux: &[".cache/google-chrome"],
        windows: &["AppData/Local/Google/Chrome/User Data/Default/Cache"],
    };
    Rule {
        owners: &["Google Chrome"],
        ..rule("app.chrome-cache", AppCache, Safe, Removal::Delete, Matcher::KnownPaths(paths.of(p)))
    }
}

/// Brave keeps Chromium's layout under its own name.
pub(crate) const fn app_brave_cache(p: Platform) -> Rule {
    let paths = PerOs {
        macos: &["Library/Caches/BraveSoftware"],
        linux: &[".cache/BraveSoftware"],
        windows: &["AppData/Local/BraveSoftware/Brave-Browser/User Data/Default/Cache"],
    };
    Rule {
        owners: &["Brave Browser"],
        ..rule("app.brave-cache", AppCache, Safe, Removal::Delete, Matcher::KnownPaths(paths.of(p)))
    }
}

/// Firefox's local profile folders, which hold its cache:
/// `~/Library/Caches/Firefox` on macOS, `~/.cache/mozilla/firefox` on
/// Linux, `%LOCALAPPDATA%\Mozilla\Firefox\Profiles` on Windows
/// (support.mozilla.org, "Profiles - Where Firefox stores your bookmarks,
/// passwords and other user data", the local folder).
pub(crate) const fn app_firefox_cache(p: Platform) -> Rule {
    let paths = PerOs {
        macos: &["Library/Caches/Firefox"],
        linux: &[".cache/mozilla/firefox"],
        windows: &["AppData/Local/Mozilla/Firefox/Profiles"],
    };
    Rule {
        owners: &["Firefox"],
        ..rule("app.firefox-cache", AppCache, Safe, Removal::Delete, Matcher::KnownPaths(paths.of(p)))
    }
}

/// Spotify's offline and streaming cache: `~/Library/Caches/com.spotify.client`
/// on macOS, `~/.cache/spotify` on Linux, `%LOCALAPPDATA%\Spotify\Data` on
/// Windows (support.spotify.com, "Storage", the cache location).
pub(crate) const fn app_spotify_cache(p: Platform) -> Rule {
    let paths = PerOs {
        macos: &["Library/Caches/com.spotify.client"],
        linux: &[".cache/spotify"],
        windows: &["AppData/Local/Spotify/Data"],
    };
    Rule {
        owners: &["Spotify"],
        ..rule("app.spotify-cache", AppCache, Safe, Removal::Delete, Matcher::KnownPaths(paths.of(p)))
    }
}

/// The caches in Slack's data folder: `~/Library/Application Support/Slack`
/// on macOS, `~/.config/Slack` on Linux, `%APPDATA%\Slack` on Windows
/// (slack.com help, "Where Slack stores data on your computer").
pub(crate) const fn app_slack_cache(p: Platform) -> Rule {
    let paths = PerOs {
        macos: &[
            "Library/Application Support/Slack/Cache",
            "Library/Application Support/Slack/Service Worker/CacheStorage",
        ],
        linux: &[".config/Slack/Cache", ".config/Slack/Service Worker/CacheStorage"],
        windows: &["AppData/Roaming/Slack/Cache", "AppData/Roaming/Slack/Service Worker/CacheStorage"],
    };
    Rule {
        owners: &["Slack"],
        ..rule("app.slack-cache", AppCache, Safe, Removal::Delete, Matcher::KnownPaths(paths.of(p)))
    }
}

/// The Trash of the home's volume: `~/.Trash` on macOS, the trashed files
/// in `~/.local/share/Trash` on Linux (freedesktop.org, "Trash
/// specification", the home trash). Windows keeps its Recycle Bin at each
/// drive's root, outside the home: no path there.
pub(crate) const fn trash(p: Platform) -> Rule {
    let paths = PerOs { macos: &[".Trash"], linux: &[".local/share/Trash/files"], windows: &[] };
    Rule { hint: "empty the Trash", ..rule("trash", Trash, Review, Removal::None, Matcher::KnownPaths(paths.of(p))) }
}
