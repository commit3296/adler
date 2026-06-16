#![no_main]

use adler_core::{Site, Username};
use libfuzzer_sys::fuzz_target;

const MAX_INPUT: usize = 16 * 1024;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT {
        return;
    }
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(site) = serde_json::from_str::<Site>(text) else {
        return;
    };

    let _ = site.validate();
    let username = Username::new("alice").expect("static username is valid");
    let _ = site.url_for(&username);
});
