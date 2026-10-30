//! The platform seam's constants and the error envelope (ADR "Platform seam:
//! one platform module per OS with declared capabilities"; story "Platform
//! seam: ADR and capability model", DDC-007), in the normal build. The
//! refusals themselves are tested in a build with `simulate-unported`
//! (`tests/unported.rs`, `make unported-check`).

use std::path::Path;

use ddc_core::platform::PLATFORM;
use ddc_core::refusal::{ERROR_SCHEMA_VERSION, ErrorReport};

#[test]
fn the_platform_is_the_build_targets() {
    assert_eq!(serde_json::to_value(PLATFORM).unwrap(), std::env::consts::OS);
}

/// On macOS nothing ever skips: the switch is on, every capability is
/// declared, in declaration order. Left out of the build that simulates an
/// unported platform, where `make unported-check` compiles it.
#[cfg(all(target_os = "macos", not(feature = "simulate-unported")))]
#[test]
fn macos_is_supported_with_every_capability() {
    use ddc_core::platform::{self, CAPABILITIES, Capability, SUPPORTED};

    const { assert!(SUPPORTED, "macOS refuses: a build with simulate-unported runs only make unported-check") };
    assert_eq!(platform::require_supported(), Ok(()));
    assert_eq!(CAPABILITIES, Capability::ALL);
    let names: Vec<String> =
        CAPABILITIES.iter().map(|c| serde_json::to_value(c).unwrap().as_str().unwrap().to_owned()).collect();
    assert_eq!(
        names,
        [
            "private-size",
            "in-use-check",
            "dataless-files",
            "full-disk-access",
            "local-snapshots",
            "whole-disk",
            "coding-agents",
            "homebrew",
            "schedule",
            "notifications"
        ]
    );
}

#[test]
fn the_envelope_has_its_own_version_key() {
    let schema = serde_json::to_value(schemars::schema_for!(ErrorReport)).unwrap();
    let required: Vec<&str> = schema["required"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(required, ["error_schema_version", "error"]);
    assert!(schema["properties"].get("schema_version").is_none(), "no report decoder may read it as its own");
    assert_eq!(ERROR_SCHEMA_VERSION, 1);
    for name in ["Platform", "Capability", "ErrorCode"] {
        assert!(schema["$defs"].get(name).is_some(), "$defs/{name}");
    }
}

#[test]
#[allow(clippy::disallowed_methods)] // DDC_UPDATE_SCHEMA regenerates the committed file
fn committed_error_schema_is_current() {
    let schema = serde_json::to_string_pretty(&schemars::schema_for!(ErrorReport)).unwrap() + "\n";
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/error.schema.json");
    if std::env::var_os("DDC_UPDATE_SCHEMA").is_some() {
        std::fs::write(&path, &schema).unwrap();
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(committed == schema, "docs/error.schema.json is stale: run DDC_UPDATE_SCHEMA=1 cargo test");
}

/// The three reports name the same `$defs` the app's contract test looks up.
#[test]
fn the_report_schemas_define_platform_and_capability() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs");
    for name in ["scan", "clean", "disk"] {
        let text = std::fs::read_to_string(root.join(format!("{name}.schema.json"))).unwrap();
        let schema: serde_json::Value = serde_json::from_str(&text).unwrap();
        for def in ["Platform", "Capability"] {
            assert!(schema["$defs"].get(def).is_some(), "docs/{name}.schema.json has no $defs/{def}");
        }
        let required: Vec<&str> = schema["required"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert!(required.contains(&"platform") && required.contains(&"capabilities"), "{name}: {required:?}");
    }
}
