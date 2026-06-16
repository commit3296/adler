#![no_main]

use adler_core::Registry;
use libfuzzer_sys::fuzz_target;

const MAX_INPUT: usize = 16 * 1024;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT {
        return;
    }
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let _ = Registry::from_json_str(text);
});
