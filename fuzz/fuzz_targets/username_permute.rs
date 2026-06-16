#![no_main]

use adler_core::{MAX_VARIANTS, PermuteLevel, Username, permute};
use libfuzzer_sys::fuzz_target;

const MAX_INPUT: usize = 128;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT {
        return;
    }
    let Ok(raw) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(username) = Username::new(raw.trim()) else {
        return;
    };

    for level in [
        PermuteLevel::None,
        PermuteLevel::Basic,
        PermuteLevel::Aggressive,
    ] {
        let variants = permute(&username, level);
        assert!(variants.len() <= MAX_VARIANTS);
        assert!(
            variants
                .iter()
                .all(|variant| Username::new(variant.as_str()).is_ok())
        );
        if level == PermuteLevel::None {
            assert_eq!(variants, [username.clone()]);
        }
    }
});
