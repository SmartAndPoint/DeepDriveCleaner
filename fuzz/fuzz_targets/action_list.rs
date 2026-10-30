//! The `disk free --actions` list; a list it accepts reads back the same once written.

#![no_main]

use ddc_core::free::ActionList;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(list) = ActionList::parse(data) {
        let json = serde_json::to_vec(&list).expect("an action list serializes");
        assert_eq!(ActionList::parse(&json), Ok(list));
    }
});
