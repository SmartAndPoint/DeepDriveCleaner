//! `diskutil -plist` output: text, so input that is not UTF-8 never reaches it.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = ddc_core::disk::plist::parse(text);
    }
});
