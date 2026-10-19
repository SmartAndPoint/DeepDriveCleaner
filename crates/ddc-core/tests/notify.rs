//! The notify decision's contract (story "CLI: shared settings, types and the
//! notify decision").

use std::path::Path;

use ddc_core::notify::{Decision, NOTIFY_SCHEMA_VERSION, NotifyReason, NotifyReport, NotifyStatus, SkipCause};

#[test]
fn the_decision_and_the_skip_have_the_documented_shape() {
    let scanned = NotifyReport {
        schema_version: NOTIFY_SCHEMA_VERSION,
        status: NotifyStatus::Scanned(Decision {
            notify: true,
            reason: Some(NotifyReason::LowDisk),
            volume: Some("/Volumes/Work".into()),
            reclaimable: 3,
            free: Some(1),
            total: Some(100),
        }),
    };
    assert_eq!(
        serde_json::to_value(&scanned).unwrap(),
        serde_json::json!({
            "schema_version": 1, "status": "scanned", "notify": true, "reason": "low-disk",
            "volume": "/Volumes/Work", "reclaimable": 3, "free": 1, "total": 100,
        })
    );
    let quiet = Decision { notify: false, reason: None, volume: None, reclaimable: 0, free: None, total: None };
    let json = serde_json::to_value(NotifyStatus::Scanned(quiet)).unwrap();
    assert!(json["reason"].is_null() && json["volume"].is_null(), "absent values are written as null");
    let busy = NotifyReport { schema_version: 1, status: NotifyStatus::Skipped { reason: SkipCause::Busy } };
    assert_eq!(
        serde_json::to_value(&busy).unwrap(),
        serde_json::json!({ "schema_version": 1, "status": "skipped", "reason": "busy" })
    );
    for report in [scanned, busy] {
        let back: NotifyReport = serde_json::from_value(serde_json::to_value(&report).unwrap()).unwrap();
        assert_eq!(back, report);
    }
}

#[test]
#[allow(clippy::disallowed_methods)] // DDC_UPDATE_SCHEMA regenerates the committed file
fn committed_notify_schema_is_current() {
    let schema = serde_json::to_string_pretty(&schemars::schema_for!(NotifyReport)).unwrap() + "\n";
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/notify.schema.json");
    if std::env::var_os("DDC_UPDATE_SCHEMA").is_some() {
        std::fs::write(&path, &schema).unwrap();
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(committed == schema, "docs/notify.schema.json is stale: run DDC_UPDATE_SCHEMA=1 cargo test");
}
