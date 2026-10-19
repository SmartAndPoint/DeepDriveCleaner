//! The built-in rule table. Ids, classes and paths follow the tables in story
//! "Detection rule catalog" (epic DDC-001) and `bench/make_baseline.py`.
//! Order matters for specialized rules: the first match wins.

use super::{Clones, Matcher, Rule, TOMBSTONE};
use crate::class::Category::*;
use crate::class::RiskClass::*;
use crate::class::{Fallback, LocationCompare, LocationProbe, NativeCommand, Removal};

const PY_LOCKS: &[&str] = &["uv.lock", "poetry.lock", "Pipfile.lock", "requirements*.txt"];
const JS_LOCKS: &[&str] = &["package-lock.json", "pnpm-lock.yaml", "yarn.lock", "bun.lock", "bun.lockb"];
const JS_BUILD: &[&str] = &[".next", ".nuxt", ".svelte-kit", ".turbo", ".parcel-cache", ".angular"];
const GRADLE: &[&str] = &["build.gradle", "build.gradle.kts"];
const GRADLE_ANY: &[&str] = &["build.gradle", "build.gradle.kts", "settings.gradle", "settings.gradle.kts"];
const NONE: &[&str] = &[];
/// App bundles of the JetBrains IDEs; editions match too (`PyCharm CE`, `IntelliJ IDEA Ultimate`).
const JETBRAINS: &[&str] = &[
    "IntelliJ IDEA",
    "PyCharm",
    "WebStorm",
    "GoLand",
    "CLion",
    "RustRover",
    "DataGrip",
    "Rider",
    "PhpStorm",
    "RubyMine",
    "DataSpell",
    "Fleet",
];

const fn rule(
    id: &'static str,
    category: crate::class::Category,
    class: crate::class::RiskClass,
    removal: Removal,
    matcher: Matcher,
) -> Rule {
    Rule {
        id,
        category,
        class,
        removal,
        matcher,
        lockfiles: None,
        owners: &[],
        hint: "",
        listed: true,
        clones: Clones::Never,
    }
}

const fn name(names: &'static [&'static str], siblings: &'static [&'static str]) -> Matcher {
    Matcher::Name { names, siblings, children_any: NONE }
}

const fn native(
    program: &'static str,
    args: &'static [&'static str],
    probe: Option<LocationProbe>,
    fallback: Fallback,
) -> Removal {
    Removal::Native { cmd: NativeCommand { program, args, location: probe }, fallback }
}

const fn probe(
    program: &'static str,
    args: &'static [&'static str],
    compare: LocationCompare,
) -> Option<LocationProbe> {
    Some(LocationProbe { program, args, compare })
}

const DOCKER_PRUNE: &[NativeCommand] = &[
    NativeCommand {
        program: "docker",
        args: &["--context", "desktop-linux", "builder", "prune", "-f"],
        location: None,
    },
    NativeCommand { program: "docker", args: &["--context", "desktop-linux", "image", "prune", "-f"], location: None },
];
const PODMAN_PRUNE: &[NativeCommand] =
    &[NativeCommand { program: "podman", args: &["image", "prune", "-f"], location: None }];

pub static RULES: &[Rule] = &[
    // ---- synthetic -------------------------------------------------------
    Rule {
        hint: "protected by --exclude or a .ddc-keep marker",
        ..rule(super::KEPT, Personal, UserData, Removal::None, Matcher::Synthetic)
    },
    // ---- project artifacts (specialized, in match order) -----------------
    Rule {
        hint: "left by an interrupted ddc clean",
        ..rule(TOMBSTONE, ProjectArtifact, Safe, Removal::Delete, Matcher::NameContains(".ddc-deleting-"))
    },
    // App bundles are sealed: rules never apply inside (`.app/Contents/Resources/node_modules`).
    Rule {
        hint: "macOS application bundle",
        listed: false,
        ..rule("macos.app-bundle", Personal, UserData, Removal::None, Matcher::NameSuffix(".app"))
    },
    Rule {
        hint: "global packages; managed by the package manager",
        listed: false,
        ..rule(
            "node.global-packages",
            ProjectArtifact,
            UserData,
            Removal::None,
            Matcher::Under {
                name: "node_modules",
                prefixes: &[".npm-global", ".config/yarn/global", ".bun/install/global", ".volta", ".fnm"],
            },
        )
    },
    Rule {
        lockfiles: Some(PY_LOCKS),
        clones: Clones::UvVenv,
        hint: "recreate: uv sync / pip install -r requirements.txt",
        ..rule("python.venv", ProjectArtifact, Rebuildable, Removal::Delete, Matcher::ContainsFile("pyvenv.cfg"))
    },
    Rule {
        hint: "recreated by tox/nox on the next run",
        ..rule(
            "python.tox",
            ProjectArtifact,
            Safe,
            Removal::Delete,
            name(&[".tox", ".nox"], &["tox.ini", "noxfile.py", "pyproject.toml", "setup.cfg"]),
        )
    },
    rule("python.pycache", ProjectArtifact, Safe, Removal::Delete, name(&["__pycache__"], NONE)),
    rule(
        "python.tool-cache",
        ProjectArtifact,
        Safe,
        Removal::Delete,
        name(&[".pytest_cache", ".mypy_cache", ".ruff_cache"], NONE),
    ),
    Rule {
        lockfiles: Some(JS_LOCKS),
        clones: Clones::PnpmModules,
        hint: "recreate: npm ci / pnpm install / yarn install",
        ..rule(
            "node.node_modules",
            ProjectArtifact,
            Rebuildable,
            Removal::Delete,
            name(&["node_modules"], &["package.json"]),
        )
    },
    Rule {
        clones: Clones::PnpmModules,
        hint: "no package.json next to it: origin unknown",
        ..rule("node.node_modules-orphan", ProjectArtifact, Review, Removal::Trash, name(&["node_modules"], NONE))
    },
    Rule {
        hint: "rebuilt by the next build",
        ..rule("js.build-cache", ProjectArtifact, Safe, Removal::Delete, name(JS_BUILD, &["package.json"]))
    },
    Rule {
        hint: "rebuilt by cargo build",
        ..rule("rust.target", ProjectArtifact, Rebuildable, Removal::Delete, name(&["target"], &["Cargo.toml"]))
    },
    Rule {
        hint: "rebuilt by mvn package",
        ..rule("maven.target", ProjectArtifact, Rebuildable, Removal::Delete, name(&["target"], &["pom.xml"]))
    },
    Rule {
        hint: "rebuilt by gradle build",
        ..rule(
            "gradle.build",
            ProjectArtifact,
            Rebuildable,
            Removal::Delete,
            Matcher::Name { names: &["build"], siblings: GRADLE, children_any: &["intermediates", "classes", "tmp"] },
        )
    },
    rule("gradle.project-cache", ProjectArtifact, Safe, Removal::Delete, name(&[".gradle"], GRADLE_ANY)),
    Rule {
        hint: "rebuilt by cmake --build",
        ..rule("cmake.build", ProjectArtifact, Rebuildable, Removal::Delete, Matcher::CmakeBuild)
    },
    Rule {
        lockfiles: Some(&["Podfile.lock"]),
        hint: "recreate: pod install",
        ..rule("cocoapods.pods", ProjectArtifact, Rebuildable, Removal::Delete, name(&["Pods"], &["Podfile"]))
    },
    // ---- package caches ---------------------------------------------------
    rule("cache.npm", PackageCache, Safe, Removal::Delete, Matcher::KnownPaths(&[".npm/_cacache", ".npm/_npx"])),
    rule(
        "cache.yarn",
        PackageCache,
        Safe,
        Removal::Delete,
        Matcher::KnownPaths(&["Library/Caches/Yarn", ".yarn/berry/cache"]),
    ),
    Rule {
        clones: Clones::Always,
        ..rule("cache.bun", PackageCache, Safe, Removal::Delete, Matcher::KnownPaths(&[".bun/install/cache"]))
    },
    Rule {
        clones: Clones::Always,
        hint: "node_modules installed by pnpm are clones and stay intact",
        ..rule(
            "cache.pnpm",
            PackageCache,
            Safe,
            Removal::Delete,
            Matcher::KnownPaths(&["Library/pnpm/store", ".pnpm-store"]),
        )
    },
    Rule {
        clones: Clones::Always,
        ..rule(
            "cache.uv",
            PackageCache,
            Safe,
            native(
                "uv",
                &["cache", "clean"],
                probe("uv", &["cache", "dir"], LocationCompare::Equals),
                Fallback::Delete,
            ),
            Matcher::KnownPaths(&[".cache/uv"]),
        )
    },
    rule("cache.pip", PackageCache, Safe, Removal::Delete, Matcher::KnownPaths(&["Library/Caches/pip"])),
    rule(
        "cache.poetry",
        PackageCache,
        Safe,
        Removal::Delete,
        Matcher::KnownPaths(&["Library/Caches/pypoetry/cache", "Library/Caches/pypoetry/artifacts"]),
    ),
    rule("cache.homebrew", PackageCache, Safe, Removal::Delete, Matcher::KnownPaths(&["Library/Caches/Homebrew"])),
    rule(
        "cache.go-build",
        PackageCache,
        Safe,
        native("go", &["clean", "-cache"], probe("go", &["env", "GOCACHE"], LocationCompare::Equals), Fallback::Delete),
        Matcher::KnownPaths(&["Library/Caches/go-build"]),
    ),
    rule(
        "cache.go-mod",
        PackageCache,
        Safe,
        native(
            "go",
            &["clean", "-modcache"],
            probe("go", &["env", "GOMODCACHE"], LocationCompare::Equals),
            Fallback::DeleteWritable,
        ),
        Matcher::KnownPaths(&["go/pkg/mod"]),
    ),
    rule(
        "cache.cargo",
        PackageCache,
        Safe,
        Removal::Delete,
        Matcher::KnownPaths(&[".cargo/registry/cache", ".cargo/registry/src", ".cargo/git/checkouts"]),
    ),
    rule("cache.gradle", PackageCache, Safe, Removal::Delete, Matcher::KnownPaths(&[".gradle/caches"])),
    Rule {
        hint: "may contain artifacts installed locally with mvn install",
        ..rule("repo.maven", PackageCache, Review, Removal::Trash, Matcher::KnownPaths(&[".m2/repository"]))
    },
    rule("cache.cocoapods", PackageCache, Safe, Removal::Delete, Matcher::KnownPaths(&["Library/Caches/CocoaPods"])),
    Rule {
        hint: "reinstall: npx playwright install",
        ..rule(
            "browsers.playwright",
            PackageCache,
            Review,
            Removal::Trash,
            Matcher::KnownPaths(&["Library/Caches/ms-playwright"]),
        )
    },
    Rule {
        hint: "reinstall: npx puppeteer browsers install",
        ..rule("browsers.puppeteer", PackageCache, Review, Removal::Trash, Matcher::KnownPaths(&[".cache/puppeteer"]))
    },
    Rule {
        hint: "reinstall: npx cypress install",
        ..rule(
            "browsers.cypress",
            PackageCache,
            Review,
            Removal::Trash,
            Matcher::KnownPaths(&["Library/Caches/Cypress"]),
        )
    },
    // ---- Xcode ------------------------------------------------------------
    Rule {
        owners: &["Xcode"],
        ..rule(
            "xcode.derived-data",
            Xcode,
            Safe,
            Removal::Delete,
            Matcher::KnownPaths(&["Library/Developer/Xcode/DerivedData"]),
        )
    },
    rule(
        "xcode.simulator-caches",
        Xcode,
        Safe,
        Removal::Delete,
        Matcher::KnownPaths(&["Library/Developer/CoreSimulator/Caches"]),
    ),
    Rule {
        hint: "re-created when the device is connected again",
        ..rule(
            "xcode.device-support",
            Xcode,
            Review,
            Removal::Trash,
            Matcher::KnownPaths(&["Library/Developer/Xcode/iOS DeviceSupport"]),
        )
    },
    Rule {
        hint: "archives contain dSYMs needed to symbolicate crash reports",
        ..rule(
            "xcode.archives",
            Xcode,
            UserData,
            Removal::None,
            Matcher::KnownPaths(&["Library/Developer/Xcode/Archives"]),
        )
    },
    // ---- VMs and containers: native-only ----------------------------------
    Rule {
        hint: "frees space inside Docker.raw; Docker Desktop shrinks the image later",
        ..rule(
            "vm.docker-desktop",
            VmContainer,
            Review,
            Removal::NativeOnly(DOCKER_PRUNE),
            Matcher::KnownPaths(&["Library/Containers/com.docker.docker"]),
        )
    },
    Rule {
        hint: "removes dangling images only",
        ..rule(
            "vm.podman",
            VmContainer,
            Review,
            Removal::NativeOnly(PODMAN_PRUNE),
            Matcher::KnownPaths(&[".local/share/containers"]),
        )
    },
    Rule {
        hint: "remove manually: colima delete (destroys containers and volumes)",
        ..rule("vm.colima", VmContainer, Review, Removal::NativeOnly(&[]), Matcher::KnownPaths(&[".colima"]))
    },
    Rule {
        hint: "remove manually: limactl delete <instance>",
        ..rule("vm.lima", VmContainer, Review, Removal::NativeOnly(&[]), Matcher::KnownPaths(&[".lima"]))
    },
    Rule {
        hint: "manage in the OrbStack app",
        ..rule("vm.orbstack", VmContainer, Review, Removal::NativeOnly(&[]), Matcher::KnownPaths(&[".orbstack"]))
    },
    Rule {
        hint: "manage in Rancher Desktop (Troubleshooting → Factory Reset)",
        ..rule(
            "vm.rancher-desktop",
            VmContainer,
            Review,
            Removal::NativeOnly(&[]),
            Matcher::KnownPaths(&["Library/Application Support/rancher-desktop"]),
        )
    },
    rule("cache.colima", PackageCache, Safe, Removal::Delete, Matcher::KnownPaths(&["Library/Caches/colima"])),
    // ---- toolchains (each version separately; protected versions dropped after the walk)
    Rule {
        hint: "reinstall: nvm install <version>",
        ..rule("toolchain.nvm", Toolchain, Review, Removal::Trash, Matcher::KnownChildren(".nvm/versions/node"))
    },
    Rule {
        hint: "reinstall: pyenv install <version>",
        ..rule(
            "toolchain.pyenv",
            Toolchain,
            Review,
            native(
                "pyenv",
                &["uninstall", "-f", "{name}"],
                probe("pyenv", &["root"], LocationCompare::Contains),
                Fallback::Trash,
            ),
            Matcher::KnownChildren(".pyenv/versions"),
        )
    },
    Rule {
        hint: "reinstall: rustup toolchain install <name>",
        ..rule(
            "toolchain.rustup",
            Toolchain,
            Review,
            native(
                "rustup",
                &["toolchain", "uninstall", "{name}"],
                probe("rustup", &["show", "home"], LocationCompare::Contains),
                Fallback::Trash,
            ),
            Matcher::KnownChildren(".rustup/toolchains"),
        )
    },
    // ---- ML models -----------------------------------------------------------
    Rule {
        hint: "re-download with ollama pull; remove one model with ollama rm",
        ..rule("model.ollama", MlModel, Review, Removal::Trash, Matcher::KnownPaths(&[".ollama/models"]))
    },
    rule("model.huggingface", MlModel, Review, Removal::Trash, Matcher::KnownPaths(&[".cache/huggingface"])),
    rule("model.lmstudio", MlModel, Review, Removal::Trash, Matcher::KnownPaths(&[".lmstudio/models"])),
    rule("model.torch", MlModel, Review, Removal::Trash, Matcher::KnownPaths(&[".cache/torch"])),
    // ---- IDEs ------------------------------------------------------------------
    Rule {
        owners: JETBRAINS,
        ..rule("ide.jetbrains-cache", Ide, Safe, Removal::Delete, Matcher::KnownPaths(&["Library/Caches/JetBrains"]))
    },
    Rule {
        hint: "settings of an older IDE version; a newer one exists",
        ..rule(
            "ide.jetbrains-old-data",
            Ide,
            Review,
            Removal::Trash,
            Matcher::KnownChildren("Library/Application Support/JetBrains"),
        )
    },
    // One rule per editor, so a running VS Code does not hold back Cursor's caches.
    Rule {
        owners: &["Visual Studio Code"],
        ..rule(
            "ide.vscode-cache",
            Ide,
            Safe,
            Removal::Delete,
            Matcher::KnownPaths(&[
                "Library/Application Support/Code/Cache",
                "Library/Application Support/Code/CachedData",
                "Library/Application Support/Code/CachedExtensionVSIXs",
                "Library/Application Support/Code/GPUCache",
                "Library/Application Support/Code/logs",
            ]),
        )
    },
    Rule {
        owners: &["Cursor"],
        ..rule(
            "ide.cursor-cache",
            Ide,
            Safe,
            Removal::Delete,
            Matcher::KnownPaths(&[
                "Library/Application Support/Cursor/Cache",
                "Library/Application Support/Cursor/CachedData",
                "Library/Application Support/Cursor/CachedExtensionVSIXs",
                "Library/Application Support/Cursor/GPUCache",
                "Library/Application Support/Cursor/logs",
            ]),
        )
    },
    Rule {
        owners: &["Windsurf"],
        ..rule(
            "ide.windsurf-cache",
            Ide,
            Safe,
            Removal::Delete,
            Matcher::KnownPaths(&[
                "Library/Application Support/Windsurf/Cache",
                "Library/Application Support/Windsurf/CachedData",
                "Library/Application Support/Windsurf/CachedExtensionVSIXs",
                "Library/Application Support/Windsurf/GPUCache",
                "Library/Application Support/Windsurf/logs",
            ]),
        )
    },
    Rule {
        hint: "chat history and workspace state",
        ..rule(
            "ide.workspace-storage",
            Ide,
            UserData,
            Removal::None,
            Matcher::KnownPaths(&[
                "Library/Application Support/Code/User/workspaceStorage",
                "Library/Application Support/Cursor/User/workspaceStorage",
                "Library/Application Support/Windsurf/User/workspaceStorage",
            ]),
        )
    },
    // ---- application caches -------------------------------------------------------
    Rule {
        owners: &["Google Chrome"],
        ..rule(
            "app.chrome-cache",
            AppCache,
            Safe,
            Removal::Delete,
            Matcher::KnownPaths(&["Library/Caches/Google/Chrome"]),
        )
    },
    Rule {
        owners: &["Brave Browser"],
        ..rule(
            "app.brave-cache",
            AppCache,
            Safe,
            Removal::Delete,
            Matcher::KnownPaths(&["Library/Caches/BraveSoftware"]),
        )
    },
    Rule {
        owners: &["Firefox"],
        ..rule("app.firefox-cache", AppCache, Safe, Removal::Delete, Matcher::KnownPaths(&["Library/Caches/Firefox"]))
    },
    Rule {
        owners: &["Spotify"],
        ..rule(
            "app.spotify-cache",
            AppCache,
            Safe,
            Removal::Delete,
            Matcher::KnownPaths(&["Library/Caches/com.spotify.client"]),
        )
    },
    Rule {
        owners: &["Slack"],
        ..rule(
            "app.slack-cache",
            AppCache,
            Safe,
            Removal::Delete,
            Matcher::KnownPaths(&[
                "Library/Application Support/Slack/Cache",
                "Library/Application Support/Slack/Service Worker/CacheStorage",
            ]),
        )
    },
    Rule {
        owners: &["Adobe Lightroom", "Adobe Photoshop"],
        ..rule(
            "app.adobe-camera-raw-cache",
            AppCache,
            Safe,
            Removal::Delete,
            Matcher::KnownPaths(&["Library/Caches/Adobe Camera Raw 2"]),
        )
    },
    Rule {
        hint: "empty the Trash in Finder",
        ..rule("trash", Trash, Review, Removal::None, Matcher::KnownPaths(&[".Trash"]))
    },
    // ---- user data (report only) --------------------------------------------------
    rule("user.pictures", Personal, UserData, Removal::None, Matcher::KnownPaths(&["Pictures"])),
    rule("user.movies", Personal, UserData, Removal::None, Matcher::KnownPaths(&["Movies"])),
    rule("user.music", Personal, UserData, Removal::None, Matcher::KnownPaths(&["Music"])),
    rule("user.downloads", Personal, UserData, Removal::None, Matcher::KnownRegion(&["Downloads"])),
    rule(
        "user.ios-backups",
        Personal,
        UserData,
        Removal::None,
        Matcher::KnownPaths(&["Library/Application Support/MobileSync/Backup"]),
    ),
    rule(
        "user.imazing-backups",
        Personal,
        UserData,
        Removal::None,
        Matcher::KnownPaths(&["Library/Application Support/iMazing/Backups"]),
    ),
    rule(
        "user.telegram",
        Personal,
        UserData,
        Removal::None,
        Matcher::KnownPaths(&["Library/Group Containers/6N38VWS5BX.ru.keepcoder.Telegram"]),
    ),
    rule(
        "user.whatsapp",
        Personal,
        UserData,
        Removal::None,
        Matcher::KnownPaths(&["Library/Group Containers/group.net.whatsapp.WhatsApp.shared"]),
    ),
    Rule {
        hint: "cloud folders are not scanned",
        ..rule(
            "user.cloud",
            Personal,
            UserData,
            Removal::None,
            Matcher::NoTraverse(&["Library/CloudStorage", "Library/Mobile Documents"]),
        )
    },
    // ---- generic, lowest priority -------------------------------------------------------
    Rule {
        hint: "tagged as a cache by its owner; regeneration may be slow",
        ..rule("generic.cachedir-tag", GenericCache, Review, Removal::Trash, Matcher::CacheDirTag)
    },
];
