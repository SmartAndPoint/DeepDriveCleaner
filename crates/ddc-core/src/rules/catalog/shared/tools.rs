//! Shared rules: VM runtimes, toolchains and ML models.

use super::super::{DOCKER_PRUNE, PODMAN_PRUNE, native, probe, rule};
use super::PerOs;
use crate::class::Category::*;
use crate::class::RiskClass::*;
use crate::class::{Fallback, LocationCompare, Removal};
use crate::platform::Platform;
use crate::rules::{Matcher, Rule, Scope};

// VMs and containers: native-only.

/// Docker Desktop's VM disk: under `~/Library/Containers/com.docker.docker`
/// on macOS, `~/.docker/desktop` on Linux, `%LOCALAPPDATA%\Docker\wsl` on
/// Windows (docs.docker.com, the Desktop settings, "Disk image location").
pub(crate) const fn vm_docker_desktop(p: Platform) -> Rule {
    let paths = PerOs {
        macos: &["Library/Containers/com.docker.docker"],
        linux: &[".docker/desktop"],
        windows: &["AppData/Local/Docker/wsl"],
    };
    Rule {
        hint: "frees space inside the VM's disk image; Docker Desktop shrinks the image later",
        ..rule(
            "vm.docker-desktop",
            VmContainer,
            Review,
            Removal::NativeOnly(DOCKER_PRUNE),
            Matcher::KnownPaths(paths.of(p)),
        )
    }
}

pub(crate) const VM_PODMAN: Rule = Rule {
    hint: "removes dangling images only",
    ..rule(
        "vm.podman",
        VmContainer,
        Review,
        Removal::NativeOnly(PODMAN_PRUNE),
        Matcher::KnownPaths(&[".local/share/containers"]),
    )
};

pub(crate) const VM_COLIMA: Rule = Rule {
    hint: "remove manually: colima delete (destroys containers and volumes)",
    ..rule("vm.colima", VmContainer, Review, Removal::NativeOnly(&[]), Matcher::KnownPaths(&[".colima"]))
};

pub(crate) const VM_LIMA: Rule = Rule {
    hint: "remove manually: limactl delete <instance>",
    ..rule("vm.lima", VmContainer, Review, Removal::NativeOnly(&[]), Matcher::KnownPaths(&[".lima"]))
};

// Toolchains: each version separately; protected versions dropped after the walk.

pub(crate) const TOOLCHAIN_NVM: Rule = Rule {
    hint: "reinstall: nvm install <version>",
    scope: Scope::Tree,
    ..rule("toolchain.nvm", Toolchain, Review, Removal::Trash, Matcher::KnownChildren(&[".nvm/versions/node"]))
};

pub(crate) const TOOLCHAIN_PYENV: Rule = Rule {
    hint: "reinstall: pyenv install <version>",
    scope: Scope::Tree,
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
        Matcher::KnownChildren(&[".pyenv/versions"]),
    )
};

pub(crate) const TOOLCHAIN_RUSTUP: Rule = Rule {
    hint: "reinstall: rustup toolchain install <name>",
    scope: Scope::Tree,
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
        Matcher::KnownChildren(&[".rustup/toolchains"]),
    )
};

// ML models.

pub(crate) const MODEL_OLLAMA: Rule = Rule {
    hint: "re-download with ollama pull; remove one model with ollama rm",
    ..rule("model.ollama", MlModel, Review, Removal::Trash, Matcher::KnownPaths(&[".ollama/models"]))
};

// The downloaded models, datasets and Xet chunks only: the sign-in token
// (`token`, `stored_tokens`) sits beside them at the cache's root.
pub(crate) const MODEL_HUGGINGFACE: Rule = rule(
    "model.huggingface",
    MlModel,
    Review,
    Removal::Trash,
    Matcher::KnownPaths(&[".cache/huggingface/hub", ".cache/huggingface/xet", ".cache/huggingface/datasets"]),
);

pub(crate) const MODEL_LMSTUDIO: Rule =
    rule("model.lmstudio", MlModel, Review, Removal::Trash, Matcher::KnownPaths(&[".lmstudio/models"]));

pub(crate) const MODEL_TORCH: Rule =
    rule("model.torch", MlModel, Review, Removal::Trash, Matcher::KnownPaths(&[".cache/torch"]));
