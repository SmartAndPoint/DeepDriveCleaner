//! The areas outside the home folder: a fixed table, because the titles and
//! explanations are what the app shows. Paths are relative to the Data
//! volume's root, `/System/Volumes/Data`: through the firmlinks of
//! `/usr/share/firmlinks`, `/Applications` and the Data path are one folder,
//! and the Data path alone counts it once.

use super::model::ActionId;

pub struct AreaDef {
    pub id: &'static str,
    /// Folders measured for this area, relative to the Data root.
    pub paths: &'static [&'static str],
    /// Also every child of this folder whose name ends with this suffix.
    pub children: Option<(&'static str, &'static str)>,
    /// The table's area this one lies in. A walk root has none, nor an area
    /// whose paths lie in several.
    pub parent: Option<&'static str>,
    pub title: &'static str,
    pub explanation: &'static str,
    pub frees: &'static str,
    pub action: Option<ActionId>,
    /// Walked under the entry budget ([`super::DiskOptions::temp_entry_budget`]).
    pub budgeted: bool,
}

const fn area(id: &'static str, paths: &'static [&'static str], title: &'static str) -> AreaDef {
    AreaDef {
        id,
        paths,
        children: None,
        parent: None,
        title,
        explanation: "",
        frees: "",
        action: None,
        budgeted: false,
    }
}

/// Walk roots first, then the areas nested in them.
pub static AREAS: &[AreaDef] = &[
    AreaDef {
        explanation: "Apps installed for every user of this Mac.",
        frees: "Delete an app you no longer use in Finder, or with its own uninstaller.",
        ..area("applications", &["Applications"], "Applications")
    },
    AreaDef {
        explanation: "Support files, caches, fonts and plug-ins of apps for every user, and system settings.",
        frees: "The app that put them there, or its uninstaller.",
        ..area("library", &["Library"], "Library for all users")
    },
    AreaDef {
        explanation: "Data macOS downloads: system assets, language models, voices and update files.",
        frees: "macOS removes assets it no longer needs on its own.",
        ..area("macos-assets", &["System"], "macOS downloaded data")
    },
    AreaDef {
        explanation: "Package managers and tools installed in /opt.",
        frees: "The tool that installed them.",
        ..area("opt", &["opt"], "/opt")
    },
    AreaDef {
        explanation: "Tools installed by hand or by installers.",
        frees: "The installer's uninstaller.",
        ..area("usr-local", &["usr/local"], "/usr/local")
    },
    AreaDef {
        explanation: "System databases, logs and other files macOS keeps in /private.",
        frees: "macOS manages them.",
        ..area("private", &["private"], "System files in /private")
    },
    AreaDef {
        explanation: "Home folders of the other users of this Mac. Folders you cannot read are not counted.",
        frees: "Each user cleans their own home folder.",
        ..area("users", &["Users"], "Other users")
    },
    AreaDef {
        explanation: "Memory dumps written when a process crashes, if core dumps are on.",
        frees: "Delete them in Terminal with an administrator's password.",
        ..area("cores", &["cores"], "Crash dumps")
    },
    AreaDef {
        explanation: "A macOS update downloaded and waiting to be installed.",
        frees: "Installing the update.",
        parent: Some("library"),
        ..area("macos-update", &["Library/Updates"], "Downloaded macOS update")
    },
    AreaDef {
        children: Some(("System/Library/AssetsV2", "SimulatorRuntime")),
        explanation: "Simulator runtimes of Xcode, with the caches and images CoreSimulator keeps beside them.",
        frees: "Deleting a runtime you no longer use with simctl.",
        action: Some(ActionId::SimctlRuntimeDelete),
        ..area("simulator-runtimes", &["Library/Developer/CoreSimulator"], "Simulator runtimes")
    },
    AreaDef {
        explanation: "Homebrew's formulae, casks and their old versions.",
        frees: "brew cleanup removes old versions and old downloads.",
        action: Some(ActionId::BrewCleanup),
        parent: Some("opt"),
        ..area("homebrew", &["opt/homebrew"], "Homebrew")
    },
    AreaDef {
        explanation: "A conda installation with its environments and package cache.",
        frees: "conda clean --all removes the package cache; conda env remove removes an environment.",
        parent: Some("opt"),
        ..area("conda", &["opt/miniconda3", "opt/anaconda3", "opt/miniforge3"], "Conda")
    },
    AreaDef {
        explanation: "Per-user temporary files and caches of apps and tools. Large folders are counted up to a limit.",
        frees: "Quitting the app that owns them, or restarting the Mac.",
        parent: Some("private"),
        budgeted: true,
        ..area("temporary-folders", &["private/var/folders"], "Temporary folders")
    },
    AreaDef {
        explanation: "The shared temporary folder /tmp.",
        frees: "Restarting the Mac clears what is older than three days.",
        parent: Some("private"),
        budgeted: true,
        ..area("tmp", &["private/tmp"], "/tmp")
    },
    AreaDef {
        explanation: "The sleep image, a copy of memory written before the Mac sleeps.",
        frees: "macOS manages it; it is about the size of the memory.",
        parent: Some("private"),
        ..area("sleep-image", &["private/var/vm"], "Sleep image")
    },
    AreaDef {
        explanation: "The Shared folder every user can write to.",
        frees: "Delete what you no longer need in Finder.",
        parent: Some("users"),
        ..area("shared", &["Users/Shared"], "Shared folder")
    },
];
