//! `last-scan.json`; a report it accepts reads back the same once written.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(Some(report)) = ddc_core::store::decode_last_scan(data) {
        let json = serde_json::to_vec(&report).expect("a scan report serializes");
        let again = ddc_core::store::decode_last_scan(&json).expect("a written report reads back");
        assert_eq!(again, Some(report));
    }
});
