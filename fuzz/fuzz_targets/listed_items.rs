//! The `clean --items` list; a list it accepts reads back the same once written.

#![no_main]

use ddc_core::plan::ListedItems;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(list) = ListedItems::parse(data) {
        let json = serde_json::to_vec(&list).expect("an item list serializes");
        assert_eq!(ListedItems::parse(&json), Ok(list));
    }
});
