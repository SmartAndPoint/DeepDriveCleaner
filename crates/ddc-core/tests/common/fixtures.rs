//! One minimal tree per rule of the catalog: `tests/rules.rs` checks each
//! produces its rule, `tests/recheck.rs` that the pruned and the full
//! re-check agree on it. A new rule without a fixture fails both.

use ddc_core::rules::{Matcher, Rule};

use super::TreeBuilder;

/// A valid `CACHEDIR.TAG`.
pub const TAG: &str = "Signature: 8a477f597d28d172789f06886806bc55\n# cache\n";

/// Builds `rule`'s fixture in `t`; the finding it must produce, relative to
/// the root, or `None` for an unlisted or untraversed rule.
pub fn rule_fixture(t: &TreeBuilder, rule: &Rule) -> Option<String> {
    match rule.matcher {
        // A sealed folder: a project inside it is no finding either.
        Matcher::KnownPaths(paths) if !rule.listed => {
            for p in paths {
                t.write(&format!("{p}/tool/package.json"), "{}");
                t.write(&format!("{p}/tool/package-lock.json"), "{}");
                t.file(&format!("{p}/tool/node_modules/x.js"), 100);
            }
            None
        }
        Matcher::KnownPaths(paths) | Matcher::KnownRegion(paths) => {
            for p in paths {
                t.file(&format!("{p}/payload"), 100);
            }
            Some(paths[0].to_owned())
        }
        Matcher::NoTraverse(paths) => {
            t.file(&format!("{}/payload", paths[0]), 100);
            None
        }
        Matcher::KnownChildren(parent) => {
            let (old, new, setup): (&str, &str, &[(&str, &str)]) = match rule.id {
                "toolchain.nvm" => ("v18.0.0", "v20.0.0", &[(".nvm/alias/default", "20")]),
                "toolchain.pyenv" => ("3.11.0", "3.12.0", &[(".pyenv/version", "3.12.0")]),
                "toolchain.rustup" => (
                    "nightly-aarch64-apple-darwin",
                    "stable-aarch64-apple-darwin",
                    &[(".rustup/settings.toml", "default_toolchain = \"stable-aarch64-apple-darwin\"\n")],
                ),
                "ide.jetbrains-old-data" => ("PyCharm2023.1", "PyCharm2024.1", &[]),
                "agent.claude-code-transcripts" => ("-Users-example-a", "-Users-example-b", &[]),
                other => panic!("no fixture for KnownChildren rule {other}"),
            };
            for v in [old, new] {
                t.file(&format!("{parent}/{v}/payload"), 100);
            }
            for (rel, content) in setup {
                t.write(rel, content);
            }
            Some(format!("{parent}/{old}"))
        }
        Matcher::Synthetic => {
            t.write("kept/.ddc-keep", "");
            Some("kept".to_owned())
        }
        _ => {
            let (files, at): (&[(&str, &str)], Option<&str>) = match rule.id {
                "ddc.tombstone" => (&[("p/x.ddc-deleting-1-1/f", "")], Some("p/x.ddc-deleting-1-1")),
                "macos.app-bundle" => (&[("A.app/Contents/Resources/node_modules/x.js", "")], None),
                "node.global-packages" => (&[(".npm-global/lib/node_modules/tool/node_modules/y.js", "")], None),
                "python.venv" => (&[("p/uv.lock", ""), ("p/.venv/pyvenv.cfg", "")], Some("p/.venv")),
                "python.conda-env" => (
                    &[("miniconda3/envs/old/conda-meta/history", ""), ("miniconda3/envs/old/bin/python", "")],
                    Some("miniconda3/envs/old"),
                ),
                "python.tox" => (&[("p/tox.ini", ""), ("p/.tox/py/f", "")], Some("p/.tox")),
                "python.pycache" => (&[("p/__pycache__/m.pyc", "")], Some("p/__pycache__")),
                "python.tool-cache" => (&[("p/.mypy_cache/f", "")], Some("p/.mypy_cache")),
                "node.node_modules" => (
                    &[("p/package.json", ""), ("p/yarn.lock", ""), ("p/node_modules/x.js", "")],
                    Some("p/node_modules"),
                ),
                "node.node_modules-orphan" => (&[("p/node_modules/x.js", "")], Some("p/node_modules")),
                "js.build-cache" => (&[("p/package.json", ""), ("p/.svelte-kit/x", "")], Some("p/.svelte-kit")),
                "rust.target" => (&[("p/Cargo.toml", ""), ("p/target/debug/x", "")], Some("p/target")),
                "maven.target" => (&[("p/pom.xml", ""), ("p/target/classes/A.class", "")], Some("p/target")),
                "gradle.build" => (&[("p/build.gradle", ""), ("p/build/tmp/x", "")], Some("p/build")),
                "gradle.project-cache" => {
                    (&[("p/settings.gradle.kts", ""), ("p/.gradle/8.0/x", "")], Some("p/.gradle"))
                }
                "cmake.build" => (
                    &[("p/CMakeLists.txt", ""), ("p/cmake-build-debug/CMakeCache.txt", "@SRC@")],
                    Some("p/cmake-build-debug"),
                ),
                "cocoapods.pods" => (&[("p/Podfile", ""), ("p/Podfile.lock", ""), ("p/Pods/x", "")], Some("p/Pods")),
                "generic.cachedir-tag" => (&[("p/somecache/CACHEDIR.TAG", TAG)], Some("p/somecache")),
                other => panic!("no fixture for rule {other}: add one here"),
            };
            for (rel, content) in files {
                let content =
                    content.replace("@SRC@", &format!("CMAKE_HOME_DIRECTORY:INTERNAL={}\n", t.path("p").display()));
                t.write(rel, &content);
            }
            at.map(str::to_owned)
        }
    }
}
