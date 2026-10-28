//! The progress events' contract (story "CLI: progress events").

use std::path::Path;

use ddc_core::exec::{CommandResult, Outcome, SkipReason};
use ddc_core::plan::ActionKind;
use ddc_core::progress::{
    CurrentEvent, ItemEvent, ItemsEvent, PROGRESS_SCHEMA_VERSION, Phase, PhaseEvent, ProgressEvent, ProgressLine,
    WalkEvent, WalkStep,
};
use serde_json::json;

fn line(event: ProgressEvent) -> ProgressLine {
    ProgressLine::new(event)
}

#[test]
fn every_event_has_the_documented_shape_and_reads_back() {
    let cases = [
        (
            line(ProgressEvent::Phase(PhaseEvent { phase: Phase::Cleaning })),
            json!({"schema_version": 1, "event": "phase", "phase": "cleaning"}),
        ),
        (
            line(ProgressEvent::Walk(WalkEvent {
                step: WalkStep::Recheck,
                files: 3,
                dirs: 2,
                bytes: 4096,
                files_expected: Some(5),
            })),
            json!({"schema_version": 1, "event": "walk", "step": "recheck", "files": 3, "dirs": 2, "bytes": 4096,
                   "files_expected": 5}),
        ),
        (
            line(ProgressEvent::Walk(WalkEvent {
                step: WalkStep::Scan,
                files: 1,
                dirs: 1,
                bytes: 0,
                files_expected: None,
            })),
            json!({"schema_version": 1, "event": "walk", "step": "scan", "files": 1, "dirs": 1, "bytes": 0}),
        ),
        (
            line(ProgressEvent::Walk(WalkEvent {
                step: WalkStep::Disk,
                files: 7,
                dirs: 3,
                bytes: 8192,
                files_expected: None,
            })),
            json!({"schema_version": 1, "event": "walk", "step": "disk", "files": 7, "dirs": 3, "bytes": 8192}),
        ),
        (
            line(ProgressEvent::Items(ItemsEvent {
                done: 0,
                n: 2,
                bytes_done: 0,
                bytes_total: 10,
                current: Some(CurrentEvent {
                    i: 1,
                    path: "/Users/example/.cache/uv".into(),
                    action: ActionKind::Native,
                    bytes: 10,
                    tool: Some("uv".into()),
                }),
            })),
            json!({"schema_version": 1, "event": "items", "done": 0, "n": 2, "bytes_done": 0, "bytes_total": 10,
                   "current": {"i": 1, "path": "/Users/example/.cache/uv", "action": "native", "bytes": 10, "tool": "uv"}}),
        ),
        (
            line(ProgressEvent::Item(ItemEvent {
                i: 2,
                n: 2,
                path: "/Users/example/p/node_modules".into(),
                action: ActionKind::Delete,
                bytes: 20,
                outcome: Outcome::Skipped(SkipReason::InUse { pid: 7, name: "node".into() }),
            })),
            json!({"schema_version": 1, "event": "item", "i": 2, "n": 2, "path": "/Users/example/p/node_modules",
                   "action": "delete", "bytes": 20, "outcome": "skipped", "reason": "in-use", "pid": 7, "name": "node"}),
        ),
        (
            line(ProgressEvent::Item(ItemEvent {
                i: 1,
                n: 2,
                path: "/Users/example/.cache/uv".into(),
                action: ActionKind::Native,
                bytes: 10,
                outcome: Outcome::Native {
                    commands: vec![CommandResult {
                        program: "uv".into(),
                        args: vec!["cache".into(), "clean".into()],
                        success: true,
                        code: Some(0),
                        output: String::new(),
                    }],
                },
            })),
            json!({"schema_version": 1, "event": "item", "i": 1, "n": 2, "path": "/Users/example/.cache/uv",
                   "action": "native", "bytes": 10, "outcome": "native",
                   "commands": [{"program": "uv", "args": ["cache", "clean"], "success": true, "code": 0}]}),
        ),
    ];
    for (event, expected) in cases {
        assert_eq!(event.schema_version, PROGRESS_SCHEMA_VERSION);
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value, expected);
        let back: ProgressLine = serde_json::from_value(value).unwrap();
        assert_eq!(back, event, "reads back as the same event");
    }
}

#[test]
#[allow(clippy::disallowed_methods)] // DDC_UPDATE_SCHEMA regenerates the committed file
fn committed_progress_schema_is_current() {
    let schema = serde_json::to_string_pretty(&schemars::schema_for!(ProgressLine)).unwrap() + "\n";
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/progress.schema.json");
    if std::env::var_os("DDC_UPDATE_SCHEMA").is_some() {
        std::fs::write(&path, &schema).unwrap();
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(committed == schema, "docs/progress.schema.json is stale: run DDC_UPDATE_SCHEMA=1 cargo test");
}
