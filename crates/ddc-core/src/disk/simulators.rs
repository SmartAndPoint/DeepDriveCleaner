//! Simulator runtimes, from the real `simctl` of CoreSimulator. Never
//! `xcrun`, which opens the Command Line Tools dialog on a Mac without
//! developer tools, and never the `simctl` shim under `xcode-select -p`,
//! which runs `xcodebuild -runFirstLaunch` and asks for an administrator's
//! password after an Xcode update.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

use super::Sources;
use super::model::{Device, Runtime, SimulatorStatus, Simulators};
use crate::exec::CommandRunner;

const TOOL_TIMEOUT: Duration = Duration::from_secs(60);

/// `None` without developer tools: `xcode-select -p` fails or has no
/// `xcodebuild` under it (Command Line Tools only).
pub(super) fn simulators(commands: &dyn CommandRunner, sources: &Sources) -> Option<Simulators> {
    let select = commands.at(&sources.xcode_select)?;
    let out = commands.run(&select, &["-p".to_owned()], &[], TOOL_TIMEOUT).ok().filter(|o| o.success)?;
    let developer = PathBuf::from(out.stdout.trim());
    if !developer.is_absolute() {
        return None;
    }
    let xcodebuild = commands.at(&developer.join("usr/bin/xcodebuild"))?;
    let env = [("DEVELOPER_DIR", developer.to_str()?)];
    let ready =
        commands.run(&xcodebuild, &["-checkFirstLaunchStatus".to_owned()], &env, TOOL_TIMEOUT).is_ok_and(|o| o.success);
    if !ready {
        return Some(failed(SimulatorStatus::FirstLaunchPending, "not measured until Xcode finishes its first launch"));
    }
    let Some(simctl) = commands.at(&sources.simctl) else {
        return Some(failed(SimulatorStatus::Failed, "CoreSimulator's simctl is missing"));
    };
    let run = |args: &[&str]| {
        let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        commands.run(&simctl, &args, &env, TOOL_TIMEOUT).ok().filter(|o| o.success).map(|o| o.stdout)
    };
    let Some(runtimes) = run(&["runtime", "list", "-j"]).and_then(|s| parse_runtimes(&s)) else {
        return Some(failed(SimulatorStatus::Failed, "simctl runtime list failed"));
    };
    let devices = run(&["list", "devices", "-j"]).and_then(|s| parse_devices(&s)).unwrap_or_default();
    let runtimes: Vec<Runtime> = runtimes
        .into_iter()
        .map(|r| {
            let devices: Vec<Device> = r
                .runtime_identifier
                .as_ref()
                .and_then(|id| devices.get(id))
                .map(|ds| {
                    ds.iter()
                        .map(|d| Device { udid: d.udid.clone(), name: d.name.clone(), state: d.state.clone() })
                        .collect()
                })
                .unwrap_or_default();
            let booted = devices.iter().any(|d| d.state == "Booted");
            Runtime {
                identifier: r.identifier,
                runtime_identifier: r.runtime_identifier,
                platform: r.platform_identifier,
                version: r.version,
                build: r.build,
                size_bytes: r.size_bytes,
                path: r.path,
                parent_mount_path: r.parent_mount_path,
                deletable: r.deletable,
                state: r.state,
                devices,
                booted,
            }
        })
        .collect();
    let listed_bytes = runtimes.iter().filter_map(|r| r.size_bytes).sum();
    Some(Simulators { status: SimulatorStatus::Listed, message: None, runtimes, listed_bytes })
}

fn failed(status: SimulatorStatus, message: &str) -> Simulators {
    Simulators { status, message: Some(message.to_owned()), runtimes: Vec::new(), listed_bytes: 0 }
}

/// One entry of `simctl runtime list -j`; every field optional but the id.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRuntime {
    identifier: String,
    runtime_identifier: Option<String>,
    platform_identifier: Option<String>,
    version: Option<String>,
    build: Option<String>,
    size_bytes: Option<u64>,
    path: Option<PathBuf>,
    parent_mount_path: Option<PathBuf>,
    deletable: Option<bool>,
    state: Option<String>,
}

/// A map keyed by the runtime's id (Xcode 15 and later), or a list.
fn parse_runtimes(json: &str) -> Option<Vec<RawRuntime>> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum List {
        Map(BTreeMap<String, RawRuntime>),
        Array(Vec<RawRuntime>),
    }
    let mut runtimes = match serde_json::from_str::<List>(json).ok()? {
        List::Map(map) => map.into_values().collect::<Vec<_>>(),
        List::Array(list) => list,
    };
    runtimes.sort_by(|a, b| (&a.runtime_identifier, &a.identifier).cmp(&(&b.runtime_identifier, &b.identifier)));
    Some(runtimes)
}

#[derive(Debug, Clone, Deserialize)]
struct RawDevice {
    udid: String,
    name: String,
    state: String,
}

/// `{"devices": {"<runtimeIdentifier>": [device, …]}}`.
fn parse_devices(json: &str) -> Option<BTreeMap<String, Vec<RawDevice>>> {
    #[derive(Deserialize)]
    struct List {
        devices: BTreeMap<String, Vec<RawDevice>>,
    }
    serde_json::from_str::<List>(json).ok().map(|l| l.devices)
}
