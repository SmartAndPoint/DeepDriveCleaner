//! Shared rules: VM runtimes, toolchains and ML models.

use super::super::{PODMAN_PRUNE, native, probe, rule};
use crate::class::Category::*;
use crate::class::RiskClass::*;
use crate::class::{Fallback, LocationCompare, Removal};
use crate::rules::{Matcher, Rule, Scope};

// VMs and containers: native-only.
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
    ..rule("toolchain.nvm", Toolchain, Review, Removal::Trash, Matcher::KnownChildren(".nvm/versions/node"))
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
        Matcher::KnownChildren(".pyenv/versions"),
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
        Matcher::KnownChildren(".rustup/toolchains"),
    )
};

// ML models.

pub(crate) const MODEL_OLLAMA: Rule = Rule {
    hint: "re-download with ollama pull; remove one model with ollama rm",
    ..rule("model.ollama", MlModel, Review, Removal::Trash, Matcher::KnownPaths(&[".ollama/models"]))
};

pub(crate) const MODEL_HUGGINGFACE: Rule =
    rule("model.huggingface", MlModel, Review, Removal::Trash, Matcher::KnownPaths(&[".cache/huggingface"]));

pub(crate) const MODEL_LMSTUDIO: Rule =
    rule("model.lmstudio", MlModel, Review, Removal::Trash, Matcher::KnownPaths(&[".lmstudio/models"]));

pub(crate) const MODEL_TORCH: Rule =
    rule("model.torch", MlModel, Review, Removal::Trash, Matcher::KnownPaths(&[".cache/torch"]));
