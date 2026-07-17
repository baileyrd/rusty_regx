//! Compiling any pattern must return `Ok` or a structured `Err` — never
//! panic, abort, or overflow the stack. This is the target that would have
//! caught the deep-nesting stack overflow fixed in #8.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(pattern) = std::str::from_utf8(data) {
        let _ = rusty_regx::Regex::new(pattern);
        let _ = rusty_regx::Regex::new_ci(pattern);
        let _ = rusty_regx::Regex::new_posix(pattern);
        let _ = rusty_regx::Regex::new_posix_ci(pattern);
    }
});
