//! `settings.json`; settings it accepts read back the same once written.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(settings) = ddc_core::settings::parse(data) {
        let json = serde_json::to_vec(&settings).expect("settings serialize");
        assert_eq!(ddc_core::settings::parse(&json), Ok(settings));
    }
});
